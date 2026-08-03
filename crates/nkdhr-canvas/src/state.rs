use std::collections::BTreeMap;
use std::os::unix::io::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat, wl_surface::WlSurface};
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_seat,
    delegate_shm, delegate_xdg_shell,
};

use crate::canvas::marks::CanvasMarks;
use crate::canvas::output_group::OutputLayout;
use crate::canvas::world::{Animation, Canvas, Drag, Viewport};
use crate::keybindings::Keybindings;

const DEFAULT_GROUP: &str = "default";
const DEFAULT_CANVAS: &str = "default";

/// Camera and transient interaction state shared by every physical output
/// in one rigid output group.
pub struct GroupView {
    pub canvas: String,
    pub viewport: Viewport,
    pub in_overview: bool,
    pub pre_overview_viewport: Viewport,
    pub animation: Option<Animation>,
    /// Last keyboard-focused surface in this group, restored when pointer
    /// activity makes the group active again.
    pub keyboard_focus: Option<WlSurface>,
}

impl GroupView {
    fn new(canvas: String) -> Self {
        Self {
            canvas,
            viewport: Viewport::WORK,
            in_overview: false,
            pre_overview_viewport: Viewport::WORK,
            animation: None,
            keyboard_focus: None,
        }
    }
}

/// Everything the compositor's protocol handlers need. Owns every piece of
/// `wayland_frontend` state COMP-2 registers a global for; the renderer
/// itself stays in `main.rs`'s `WinitGraphicsBackend`, not here — nothing
/// in protocol handling needs to touch pixels directly (buffer import
/// happens lazily at render time, on whichever renderer the caller passes
/// in), so there's no reason to entangle the two.
pub struct App {
    pub start_time: Instant,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,
    /// First-class canvas worlds, keyed by the stable names used in the
    /// output-group configuration. Disconnected canvases stay here so a
    /// hotplug cannot discard their windows.
    pub canvases: BTreeMap<String, Canvas>,
    /// One camera per output group. Every physical output in a group reads
    /// the same entry; separate groups can move independently.
    pub group_views: BTreeMap<String, GroupView>,
    /// Keyboard actions and newly mapped windows target the most recently
    /// entered/clicked output group.
    pub active_group: String,
    /// The in-progress `super+drag` move/resize interaction, if any —
    /// `input.rs` is the only thing that reads or writes this.
    pub drag: Option<Drag>,
    /// Hot-reloadable copy of the `canvas` CTRL-5 namespace's keybindings
    /// (see `crate::keybindings`), shared with the background thread that
    /// watches `Config1.Changed` for it.
    pub keybindings: Arc<Mutex<Keybindings>>,
    /// COMP-4 position marks, now namespaced by first-class canvas.
    pub marks: CanvasMarks,
}

impl App {
    pub fn new(
        display_handle: &DisplayHandle,
        dmabuf_state: DmabufState,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display_handle, "nkdhr-canvas");
        seat.add_keyboard(Default::default(), 200, 200)?;
        seat.add_pointer();

        let marks = crate::canvas::marks::load();
        let mark_count = marks.values().map(|marks| marks.len()).sum::<usize>();
        println!("nkdhr-canvas: loaded {mark_count} saved mark(s)");

        let canvases = BTreeMap::from([(DEFAULT_CANVAS.to_owned(), Canvas::new())]);
        let group_views = BTreeMap::from([(
            DEFAULT_GROUP.to_owned(),
            GroupView::new(DEFAULT_CANVAS.to_owned()),
        )]);

        Ok(Self {
            start_time: Instant::now(),
            compositor_state: CompositorState::new::<Self>(display_handle),
            xdg_shell_state: XdgShellState::new::<Self>(display_handle),
            shm_state: ShmState::new::<Self>(display_handle, Vec::new()),
            dmabuf_state,
            seat_state,
            data_device_state: DataDeviceState::new::<Self>(display_handle),
            seat,
            canvases,
            group_views,
            active_group: DEFAULT_GROUP.to_owned(),
            drag: None,
            keybindings: crate::keybindings::watch(),
            marks,
        })
    }

    /// Reconcile hotplug/config output identities without deleting stale
    /// worlds or views. Reconnecting a group resumes exactly where it was.
    pub fn reconcile_output_layout(&mut self, layout: &OutputLayout) {
        for group in &layout.groups {
            self.canvases.entry(group.canvas.clone()).or_default();
            self.group_views
                .entry(group.name.clone())
                .and_modify(|view| view.canvas.clone_from(&group.canvas))
                .or_insert_with(|| GroupView::new(group.canvas.clone()));
        }
        if !layout
            .groups
            .iter()
            .any(|group| group.name == self.active_group)
            && let Some(group) = layout.groups.first()
        {
            let group = group.name.clone();
            self.activate_group(&group);
        }
    }

    pub fn activate_group(&mut self, group: &str) {
        if self.group_views.contains_key(group) && self.active_group != group {
            let focus = self.group_views[group].keyboard_focus.clone();
            self.active_group = group.to_owned();
            self.drag = None;
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, focus, smithay::utils::SERIAL_COUNTER.next_serial());
            }
        }
    }

    pub fn active_view(&self) -> &GroupView {
        self.group_views
            .get(&self.active_group)
            .expect("the active output group must have view state")
    }

    pub fn active_view_mut(&mut self) -> &mut GroupView {
        self.group_views
            .get_mut(&self.active_group)
            .expect("the active output group must have view state")
    }

    pub fn active_canvas(&self) -> &Canvas {
        let canvas = &self.active_view().canvas;
        self.canvases
            .get(canvas)
            .expect("an output group must reference a live canvas")
    }

    pub fn active_canvas_mut(&mut self) -> &mut Canvas {
        let canvas = self.active_view().canvas.clone();
        self.canvases
            .get_mut(&canvas)
            .expect("an output group must reference a live canvas")
    }
}

/// Per-client state `wayland_server` asks every client to carry. Only the
/// compositor's own bookkeeping (buffer-commit double-buffering state)
/// belongs here; nothing else has needed per-client data yet.
#[derive(Default)]
pub struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
    }
}

impl OutputHandler for App {}

impl BufferHandler for App {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for App {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for App {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    /// Just creates the `wl_buffer` object; actual GPU import happens
    /// lazily the first time this buffer is used in a render element
    /// (`ImportDmaWl`, driven by whichever renderer is rendering that
    /// frame), the same as every other buffer type. Rejecting a dmabuf
    /// here would need eagerly importing it into the renderer to check —
    /// but the renderer lives in `main.rs`'s backend, not in `App`, and
    /// real per-buffer validation isn't worth threading that through for
    /// COMP-2's scope; a genuinely incompatible buffer surfaces as a
    /// render-time import error later instead of a protocol-level one
    /// now.
    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let _ = notifier.successful::<Self>();
    }
}

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    /// Places the new window on the canvas (COMP-3's world-coordinate
    /// model, `crate::world::Canvas::map`) and gives it keyboard focus —
    /// a newly launched app grabbing focus is standard desktop behavior,
    /// not a COMP-2-era placeholder; it coexists with (doesn't replace)
    /// the click-to-focus and `cycle_focus` keybinding `main.rs`'s
    /// `handle_input` implements for switching between windows already
    /// mapped.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();

        let group = self.active_group.clone();
        let canvas_name = self.active_view().canvas.clone();
        let position = self.active_canvas_mut().map(surface.clone());
        println!(
            "nkdhr-canvas: mapped window on canvas {canvas_name:?} via group {group:?} at world {position:?}"
        );

        if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, Some(surface.wl_surface().clone()), serial);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        surface.send_configure().ok();
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        for canvas in self.canvases.values_mut() {
            canvas.unmap(surface.wl_surface());
        }
        for view in self.group_views.values_mut() {
            if view.keyboard_focus.as_ref() == Some(surface.wl_surface()) {
                view.keyboard_focus = None;
            }
        }
    }
}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        self.active_view_mut().keyboard_focus = focused.cloned();
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}

impl SelectionHandler for App {
    type SelectionUserData = ();
}

impl DataDeviceHandler for App {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for App {}
impl ServerDndGrabHandler for App {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

delegate_compositor!(App);
delegate_shm!(App);
delegate_dmabuf!(App);
delegate_xdg_shell!(App);
delegate_seat!(App);
delegate_data_device!(App);
delegate_output!(App);

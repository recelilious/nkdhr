use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::io::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::input::KeyState;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, Window, find_popup_root_surface,
};
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::pointer::{CursorImageStatus, Focus};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::{
    wl_buffer, wl_data_source::WlDataSource, wl_seat, wl_surface::WlSurface,
};
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{IsAlive, Logical, Point, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::wayland::selection::primary_selection::set_primary_focus;
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::xwayland::{X11Surface, XWaylandClientData};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_seat,
    delegate_shm, delegate_xdg_shell,
};

use crate::canvas::marks::CanvasMarks;
use crate::canvas::output_group::OutputLayout;
use crate::canvas::world::{Animation, Canvas, Drag, Viewport};
use crate::cursor::CursorState;
use crate::protocols::ProtocolState;
use crate::settings::InteractionSettings;

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
    pub keyboard_focus: Option<KeyboardFocusTarget>,
}

/// A common keyboard target for native windows, X11 windows and the
/// protocol-only ext-session-lock surfaces.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyboardFocusTarget {
    Wayland(WlSurface),
    X11(X11Surface),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionOwner {
    XWayland(SelectionTarget),
}

pub struct DndIcon {
    pub surface: WlSurface,
    pub offset: Point<i32, Logical>,
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Wayland(surface) => surface.is_alive(),
            Self::X11(surface) => surface.alive(),
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Wayland(surface) => Some(Cow::Borrowed(surface)),
            Self::X11(surface) => surface.wl_surface().map(Cow::Owned),
        }
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(popup: PopupKind) -> Self {
        Self::Wayland(popup.wl_surface().clone())
    }
}

impl From<KeyboardFocusTarget> for WlSurface {
    fn from(target: KeyboardFocusTarget) -> Self {
        target
            .wl_surface()
            .expect("a live popup grab always has a Wayland surface")
            .into_owned()
    }
}

impl KeyboardTarget<App> for KeyboardFocusTarget {
    fn enter(&self, seat: &Seat<App>, app: &mut App, keys: Vec<KeysymHandle<'_>>, serial: Serial) {
        match self {
            Self::Wayland(surface) => {
                KeyboardTarget::<App>::enter(surface, seat, app, keys, serial)
            }
            Self::X11(surface) => KeyboardTarget::<App>::enter(surface, seat, app, keys, serial),
        }
    }

    fn leave(&self, seat: &Seat<App>, app: &mut App, serial: Serial) {
        match self {
            Self::Wayland(surface) => KeyboardTarget::<App>::leave(surface, seat, app, serial),
            Self::X11(surface) => KeyboardTarget::<App>::leave(surface, seat, app, serial),
        }
    }

    fn key(
        &self,
        seat: &Seat<App>,
        app: &mut App,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            Self::Wayland(surface) => {
                KeyboardTarget::<App>::key(surface, seat, app, key, state, serial, time)
            }
            Self::X11(surface) => {
                KeyboardTarget::<App>::key(surface, seat, app, key, state, serial, time)
            }
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<App>,
        app: &mut App,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        match self {
            Self::Wayland(surface) => {
                KeyboardTarget::<App>::modifiers(surface, seat, app, modifiers, serial)
            }
            Self::X11(surface) => {
                KeyboardTarget::<App>::modifiers(surface, seat, app, modifiers, serial)
            }
        }
    }
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
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub protocols: ProtocolState,
    pub popup_manager: PopupManager,
    pub seat: Seat<Self>,
    pub cursor: CursorState,
    pub dnd_icon: Option<DndIcon>,
    /// First-class canvas worlds, keyed by the stable names used in the
    /// output-group configuration. Disconnected canvases stay here so a
    /// hotplug cannot discard their windows.
    pub canvases: BTreeMap<String, Canvas>,
    /// One camera per output group. Every physical output in a group reads
    /// the same entry; separate groups can move independently.
    pub group_views: BTreeMap<String, GroupView>,
    /// Canvases referenced by at least one currently connected output
    /// group. Disconnected canvases remain in `canvases`, but are not
    /// visible for protocol policy such as idle inhibition.
    visible_canvases: BTreeSet<String>,
    /// Keyboard actions and newly mapped windows target the most recently
    /// entered/clicked output group.
    pub active_group: String,
    /// The in-progress `super+drag` move/resize interaction, if any —
    /// `input.rs` is the only thing that reads or writes this.
    pub drag: Option<Drag>,
    /// Whether the current libinput swipe belongs to the compositor's
    /// exactly-three-finger canvas-pan gesture. Other swipe counts are
    /// forwarded through the Wayland pointer-gestures protocol.
    pub canvas_swipe_active: bool,
    /// Hot-reloadable keybindings and grid policy from CTRL-5's `canvas`
    /// namespace, shared with the `Config1.Changed` watcher.
    pub interaction_settings: Arc<Mutex<InteractionSettings>>,
    /// COMP-4 position marks, now namespaced by first-class canvas.
    pub marks: CanvasMarks,
    /// TTY-only backend control seam. The shared input layer records a
    /// requested VT, while the backend that owns libseat performs it.
    vt_switching_enabled: bool,
    pending_vt_switch: Option<i32>,
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

        let mut default_canvas = Canvas::new();
        if let Some(node) = crate::widget_host::demo_node_from_env() {
            println!(
                "nkdhr-canvas: enabling opt-in COMP-7 pinned image fixture {:?}",
                node.id()
            );
            default_canvas.add_pinned(node);
        }
        let canvases = BTreeMap::from([(DEFAULT_CANVAS.to_owned(), default_canvas)]);
        let group_views = BTreeMap::from([(
            DEFAULT_GROUP.to_owned(),
            GroupView::new(DEFAULT_CANVAS.to_owned()),
        )]);

        Ok(Self {
            start_time: Instant::now(),
            display_handle: display_handle.clone(),
            compositor_state: CompositorState::new::<Self>(display_handle),
            xdg_shell_state: XdgShellState::new::<Self>(display_handle),
            shm_state: ShmState::new::<Self>(display_handle, Vec::new()),
            dmabuf_state,
            _output_manager_state: OutputManagerState::new_with_xdg_output::<Self>(display_handle),
            seat_state,
            protocols: ProtocolState::new(display_handle),
            popup_manager: PopupManager::default(),
            seat,
            cursor: CursorState::default(),
            dnd_icon: None,
            canvases,
            group_views,
            visible_canvases: BTreeSet::new(),
            active_group: DEFAULT_GROUP.to_owned(),
            drag: None,
            canvas_swipe_active: false,
            interaction_settings: crate::settings::watch(),
            marks,
            vt_switching_enabled: false,
            pending_vt_switch: None,
        })
    }

    pub fn enable_vt_switching(&mut self) {
        self.vt_switching_enabled = true;
    }

    pub fn vt_switching_enabled(&self) -> bool {
        self.vt_switching_enabled
    }

    pub fn request_vt_switch(&mut self, vt: i32) {
        if self.vt_switching_enabled {
            self.pending_vt_switch = Some(vt);
        }
    }

    pub fn take_vt_switch_request(&mut self) -> Option<i32> {
        self.pending_vt_switch.take()
    }

    /// Reconcile hotplug/config output identities without deleting stale
    /// worlds or views. Reconnecting a group resumes exactly where it was.
    pub fn reconcile_output_layout(&mut self, layout: &OutputLayout) {
        self.visible_canvases = layout
            .groups
            .iter()
            .map(|group| group.canvas.clone())
            .collect();
        self.protocols.reconcile_outputs(
            layout
                .groups
                .iter()
                .flat_map(|group| &group.outputs)
                .map(|output| output.name.clone()),
        );
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

    /// Whether a currently visible, mapped client has requested that the
    /// session remain awake. Dead or unmapped surfaces never keep it alive.
    #[allow(
        dead_code,
        reason = "the SESS idle/DPMS policy consumes this COMP-6 API"
    )]
    pub fn idle_inhibited(&self) -> bool {
        self.protocols.idle_inhibitors.iter().any(|surface| {
            surface.is_alive()
                && self.visible_canvases.iter().any(|canvas_name| {
                    self.canvases.get(canvas_name).is_some_and(|canvas| {
                        canvas
                            .windows()
                            .iter()
                            .any(|window| window.matches_surface(surface))
                    })
                })
        })
    }

    pub fn session_locked(&self) -> bool {
        self.protocols.is_locked()
    }

    pub fn lock_surface_for_output(&self, output_name: &str) -> Option<WlSurface> {
        self.protocols.lock_surface(output_name)
    }

    pub fn note_protected_frame(&mut self, output_name: &str) {
        self.protocols.note_protected_frame(output_name);
    }

    /// Defensive cleanup for abrupt client death. Graceful protocol/XWM
    /// destroy callbacks still remove state immediately; this catches the
    /// paths where a disconnected resource becomes dead without one.
    pub fn cleanup_dead_client_state(&mut self) -> usize {
        let removed = self
            .canvases
            .values_mut()
            .map(Canvas::remove_dead_windows)
            .sum();
        self.protocols.idle_inhibitors.retain(WlSurface::is_alive);

        for view in self.group_views.values_mut() {
            if view
                .keyboard_focus
                .as_ref()
                .is_some_and(|focus| !focus.alive())
            {
                view.keyboard_focus = None;
            }
        }
        if self.drag.as_ref().is_some_and(|drag| match drag {
            Drag::Move { surface, .. } | Drag::Resize { surface, .. } => !surface.is_alive(),
            Drag::Pan { .. } => false,
        }) {
            self.drag = None;
        }
        if self
            .dnd_icon
            .as_ref()
            .is_some_and(|icon| !icon.surface.is_alive())
        {
            self.dnd_icon = None;
        }

        removed
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
        if let Some(state) = client.get_data::<ClientState>() {
            &state.compositor_state
        } else {
            &client
                .get_data::<XWaylandClientData>()
                .expect("every compositor client must carry recognized client data")
                .compositor_state
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popup_manager.commit(surface);
        for window in self
            .canvases
            .values()
            .flat_map(|canvas| canvas.windows())
            .filter(|window| window.matches_surface(surface))
        {
            window.window.on_commit();
        }
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
        let grid = { self.interaction_settings.lock().unwrap().grid };
        let position = self
            .active_canvas_mut()
            .map(Window::new_wayland_window(surface.clone()), grid);
        println!(
            "nkdhr-canvas: mapped window on canvas {canvas_name:?} via group {group:?} at world {position:?}"
        );

        if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            keyboard.set_focus(
                self,
                Some(KeyboardFocusTarget::Wayland(surface.wl_surface().clone())),
                serial,
            );
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        if let Err(error) = self
            .popup_manager
            .track_popup(PopupKind::Xdg(surface.clone()))
        {
            eprintln!("nkdhr-canvas: rejected popup with a dead parent: {error}");
            surface.send_popup_done();
            return;
        }
        surface.send_configure().ok();
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let valid_seat =
            Seat::<Self>::from_resource(&seat).is_some_and(|candidate| candidate == self.seat);
        let valid_grab = self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.has_grab(serial));
        if valid_seat && valid_grab {
            crate::input::begin_client_move(self, surface.wl_surface());
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let valid_seat =
            Seat::<Self>::from_resource(&seat).is_some_and(|candidate| candidate == self.seat);
        let valid_grab = self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.has_grab(serial));
        let edge = match edges {
            xdg_toplevel::ResizeEdge::Top => crate::canvas::world::ResizeEdge::Top,
            xdg_toplevel::ResizeEdge::Bottom => crate::canvas::world::ResizeEdge::Bottom,
            xdg_toplevel::ResizeEdge::Left => crate::canvas::world::ResizeEdge::Left,
            xdg_toplevel::ResizeEdge::Right => crate::canvas::world::ResizeEdge::Right,
            xdg_toplevel::ResizeEdge::TopLeft => crate::canvas::world::ResizeEdge::TopLeft,
            xdg_toplevel::ResizeEdge::TopRight => crate::canvas::world::ResizeEdge::TopRight,
            xdg_toplevel::ResizeEdge::BottomLeft => crate::canvas::world::ResizeEdge::BottomLeft,
            xdg_toplevel::ResizeEdge::BottomRight => crate::canvas::world::ResizeEdge::BottomRight,
            _ => return,
        };
        if valid_seat && valid_grab {
            crate::input::begin_client_resize(self, surface.wl_surface(), edge);
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        if seat != self.seat
            || !seat
                .get_pointer()
                .is_some_and(|pointer| pointer.has_grab(serial))
        {
            return;
        }

        let popup = PopupKind::Xdg(surface);
        let Ok(root_surface) = find_popup_root_surface(&popup) else {
            return;
        };
        let same_client = seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .and_then(|focus| focus.wl_surface().map(Cow::into_owned))
            .is_some_and(|focus| focus.id().same_client_as(&root_surface.id()));
        if !same_client {
            return;
        }

        let root = KeyboardFocusTarget::Wayland(root_surface);
        let Ok(grab) = self.popup_manager.grab_popup(root, popup, &seat, serial) else {
            return;
        };
        let pointer = seat.get_pointer();
        let keyboard = seat.get_keyboard();
        if let Some(pointer) = pointer {
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        if let Some(keyboard) = keyboard {
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = positioner.get_geometry();
        });
        surface.send_repositioned(token);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        for canvas in self.canvases.values_mut() {
            canvas.unmap(surface.wl_surface());
        }
        for view in self.group_views.values_mut() {
            if view
                .keyboard_focus
                .as_ref()
                .and_then(WaylandFocus::wl_surface)
                .as_deref()
                == Some(surface.wl_surface())
            {
                view.keyboard_focus = None;
            }
        }
    }
}

impl SeatHandler for App {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&KeyboardFocusTarget>) {
        if !self.session_locked() {
            self.active_view_mut().keyboard_focus = focused.cloned();
        }
        let focused_surface = focused.and_then(|target| target.wl_surface().map(Cow::into_owned));
        for window in self.canvases.values().flat_map(|canvas| canvas.windows()) {
            let activated = window
                .wl_surface()
                .as_ref()
                .is_some_and(|surface| Some(surface) == focused_surface.as_ref());
            if window.window.set_activated(activated)
                && let Some(toplevel) = window.window.toplevel()
            {
                toplevel.send_configure();
            }
        }
        let client = (!self.session_locked())
            .then(|| {
                focused_surface
                    .as_ref()
                    .and_then(|surface| self.display_handle.get_client(surface.id()).ok())
            })
            .flatten();
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor.set_status(image);
    }
}

impl SelectionHandler for App {
    type SelectionUserData = SelectionOwner;

    fn new_selection(
        &mut self,
        target: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        let Some(xwm) = self.protocols.xwm.as_mut() else {
            return;
        };
        if let Err(error) = xwm.new_selection(target, source.map(|source| source.mime_types())) {
            eprintln!("nkdhr-canvas: failed to publish Wayland selection to X11: {error}");
            return;
        }

        // Smithay 0.7 queues the X11 selection-owner request without flushing it.
        // A harmless RANDR round trip publishes the change before an X11 client
        // tries to read the new Wayland-owned selection.
        if let Err(error) = xwm.get_randr_primary_output() {
            eprintln!("nkdhr-canvas: failed to flush the X11 selection update: {error}");
        }
    }

    fn send_selection(
        &mut self,
        target: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        owner: &SelectionOwner,
    ) {
        if *owner != SelectionOwner::XWayland(target) {
            return;
        }
        let Some(writer) = self.protocols.x_selection_writer.take() else {
            return;
        };
        let result = self
            .protocols
            .xwm
            .as_mut()
            .ok_or_else(|| "XWM is unavailable".into())
            .and_then(|xwm| writer(xwm, target, mime_type, fd));
        self.protocols.x_selection_writer = Some(writer);
        if let Err(error) = result {
            eprintln!("nkdhr-canvas: failed to transfer X11 selection to Wayland: {error}");
        }
    }
}

impl DataDeviceHandler for App {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.protocols.data_device
    }
}

impl ClientDndGrabHandler for App {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        let hotspot = self.cursor.hotspot();
        self.dnd_icon = icon.map(|surface| DndIcon {
            surface,
            offset: (-hotspot.x, -hotspot.y).into(),
        });
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.dnd_icon = None;
    }
}
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

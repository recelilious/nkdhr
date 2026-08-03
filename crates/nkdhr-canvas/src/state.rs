use std::os::unix::io::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Client;
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

use crate::keybindings::Keybindings;
use crate::marks::Marks;
use crate::world::{Animation, Canvas, Drag, Viewport};

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
    /// COMP-3's world-coordinate window model: every mapped toplevel's
    /// position, in stacking order.
    pub canvas: Canvas,
    /// The in-progress `super+drag` move/resize interaction, if any —
    /// `main.rs`'s `handle_input` is the only thing that reads or writes
    /// this.
    pub drag: Option<Drag>,
    /// Hot-reloadable copy of the `canvas` CTRL-5 namespace's keybindings
    /// (see `crate::keybindings`), shared with the background thread that
    /// watches `Config1.Changed` for it.
    pub keybindings: Arc<Mutex<Keybindings>>,
    /// COMP-4's camera onto the canvas — always what's actually rendered,
    /// including mid-animation values (`main.rs`'s render loop advances
    /// `animation` into this every frame).
    pub viewport: Viewport,
    /// Whether the canvas is currently in the zoomed-out overview state
    /// (or animating into/out of it) rather than the normal 1:1 work
    /// state — ROADMAP.md's sharpness policy (§2.4) ties directly to this:
    /// scaling blur is only ever accepted while this is `true`.
    pub in_overview: bool,
    /// The work-state viewport to return to when overview is dismissed
    /// without picking a window (Escape, or clicking empty space) —
    /// captured the moment overview is entered.
    pub pre_overview_viewport: Viewport,
    /// The in-progress eased transition between two viewports, if any
    /// (overview enter/exit, jumping to a mark). `None` means `viewport`
    /// is already exactly where it should be.
    pub animation: Option<Animation>,
    /// COMP-4's position marks (ROADMAP.md §2.3), loaded once at startup
    /// (`crate::marks::load`) and written back (`crate::marks::save`)
    /// whenever one is set.
    pub marks: Marks,
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

        let position = self.canvas.map(surface.clone());
        println!("nkdhr-canvas: mapped window at world {position:?}");

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
        self.canvas.unmap(surface.wl_surface());
    }
}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::fd::OwnedFd;

mod screencopy;
mod xwayland;

pub use screencopy::SCREENCOPY_FORMAT;
use screencopy::ScreencopyState;

use smithay::input::pointer::PointerHandle;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::reexports::wayland_server::protocol::{
    wl_output::WlOutput, wl_surface::WlSurface,
};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Logical, Point};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState};
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use smithay::wayland::pointer_constraints::{
    PointerConstraintsHandler, PointerConstraintsState, with_pointer_constraint,
};
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::{
    delegate_fractional_scale, delegate_idle_inhibit, delegate_pointer_constraints,
    delegate_primary_selection, delegate_relative_pointer, delegate_session_lock,
    delegate_xdg_decoration,
};
use smithay::xwayland::xwm::X11Wm;

use crate::state::{App, KeyboardFocusTarget};

pub(crate) type XSelectionWriter = Box<
    dyn Fn(&mut X11Wm, SelectionTarget, String, OwnedFd) -> Result<(), Box<dyn std::error::Error>>,
>;

/// Smithay deliberately scrubs Xwayland's process environment. Preserve
/// the dynamic-loader path when the compositor itself was launched from a
/// non-system runtime (for example a Nix profile or an unpacked diagnostic
/// package); packaged installations normally have no such variable.
pub fn xwayland_environment() -> Vec<(OsString, OsString)> {
    std::env::var_os("LD_LIBRARY_PATH")
        .map(|value| [(OsString::from("LD_LIBRARY_PATH"), value)].into())
        .unwrap_or_default()
}

/// Protocol globals and policy state shared by the compositor backends.
///
/// Keeping this separate from backend state prevents the nested and DRM
/// implementations from quietly acquiring different desktop semantics.
pub struct ProtocolState {
    pub data_device: DataDeviceState,
    pub primary_selection: PrimarySelectionState,
    _xdg_decoration: XdgDecorationState,
    _fractional_scale: FractionalScaleManagerState,
    _pointer_constraints: PointerConstraintsState,
    _relative_pointer: RelativePointerManagerState,
    _idle_inhibit: IdleInhibitManagerState,
    pub idle_inhibitors: Vec<WlSurface>,
    pub session_lock: SessionLockManagerState,
    pub xwayland_shell: XWaylandShellState,
    pub xwm: Option<X11Wm>,
    pub x_display: Option<u32>,
    pub pending_x11_maps: BTreeSet<u32>,
    pub(crate) x_selection_writer: Option<XSelectionWriter>,
    pub screencopy: ScreencopyState,
    lock: SessionLockRuntime,
}

#[derive(Default)]
struct SessionLockRuntime {
    protected: bool,
    confirmed: bool,
    confirmation: Option<SessionLocker>,
    connected_outputs: BTreeSet<String>,
    presented_outputs: BTreeSet<String>,
    surfaces: BTreeMap<String, LockSurface>,
}

impl ProtocolState {
    pub fn new(display_handle: &DisplayHandle) -> Self {
        Self {
            data_device: DataDeviceState::new::<App>(display_handle),
            primary_selection: PrimarySelectionState::new::<App>(display_handle),
            _xdg_decoration: XdgDecorationState::new::<App>(display_handle),
            _fractional_scale: FractionalScaleManagerState::new::<App>(display_handle),
            _pointer_constraints: PointerConstraintsState::new::<App>(display_handle),
            _relative_pointer: RelativePointerManagerState::new::<App>(display_handle),
            _idle_inhibit: IdleInhibitManagerState::new::<App>(display_handle),
            idle_inhibitors: Vec::new(),
            session_lock: SessionLockManagerState::new::<App, _>(display_handle, |_| true),
            xwayland_shell: XWaylandShellState::new::<App>(display_handle),
            xwm: None,
            x_display: None,
            pending_x11_maps: BTreeSet::new(),
            x_selection_writer: None,
            screencopy: ScreencopyState::new(display_handle),
            lock: SessionLockRuntime::default(),
        }
    }

    pub fn reconcile_outputs(&mut self, outputs: impl Iterator<Item = String>) {
        self.lock.connected_outputs = outputs.collect();
        self.screencopy
            .reconcile_outputs(&self.lock.connected_outputs);
        self.lock
            .presented_outputs
            .retain(|name| self.lock.connected_outputs.contains(name));
        self.lock
            .surfaces
            .retain(|name, surface| self.lock.connected_outputs.contains(name) && surface.alive());
        self.confirm_lock_if_ready();
    }

    pub fn is_locked(&self) -> bool {
        self.lock.protected
    }

    pub fn lock_surface(&self, output_name: &str) -> Option<WlSurface> {
        self.lock
            .surfaces
            .get(output_name)
            .filter(|surface| surface.alive())
            .map(|surface| surface.wl_surface().clone())
    }

    pub fn note_protected_frame(&mut self, output_name: &str) {
        if self.lock.protected && self.lock.connected_outputs.contains(output_name) {
            self.lock.presented_outputs.insert(output_name.to_owned());
            self.confirm_lock_if_ready();
        }
    }

    fn confirm_lock_if_ready(&mut self) {
        if self.lock.protected
            && !self.lock.confirmed
            && self
                .lock
                .connected_outputs
                .is_subset(&self.lock.presented_outputs)
            && let Some(confirmation) = self.lock.confirmation.take()
        {
            confirmation.lock();
            self.lock.confirmed = true;
            println!("nkdhr-canvas: session lock confirmed after protected presentation");
        }
    }
}

impl PrimarySelectionHandler for App {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.protocols.primary_selection
    }
}

impl XdgDecorationHandler for App {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        configure_server_side_decoration(toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: Mode) {
        configure_server_side_decoration(toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        configure_server_side_decoration(toplevel);
    }
}

fn configure_server_side_decoration(toplevel: ToplevelSurface) {
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(Mode::ServerSide);
    });
    toplevel.send_configure();
}

impl FractionalScaleHandler for App {}

impl IdleInhibitHandler for App {
    fn inhibit(&mut self, surface: WlSurface) {
        if !self.protocols.idle_inhibitors.contains(&surface) {
            self.protocols.idle_inhibitors.push(surface);
        }
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.protocols
            .idle_inhibitors
            .retain(|candidate| candidate != &surface);
    }
}

impl PointerConstraintsHandler for App {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
        // Smithay commits and retains the hint on the locked-pointer
        // object. The compositor cursor stays fixed while the lock is
        // active; a later motion event resumes from that fixed position.
    }
}

impl SessionLockHandler for App {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.protocols.session_lock
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if self.protocols.lock.protected {
            // Dropping this second request sends `finished`; the active lock
            // remains fail-closed.
            return;
        }

        self.protocols.lock.protected = true;
        self.protocols.lock.confirmed = false;
        self.protocols.lock.confirmation = Some(confirmation);
        self.protocols.lock.presented_outputs.clear();
        self.protocols.lock.surfaces.clear();
        self.drag = None;
        self.dnd_icon = None;

        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial());
        }
        if let Some(pointer) = self.seat.get_pointer() {
            let location = pointer.current_location();
            pointer.motion(
                self,
                None,
                &smithay::input::pointer::MotionEvent {
                    location,
                    serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                    time: self.start_time.elapsed().as_millis() as u32,
                },
            );
            pointer.frame(self);
        }

        println!("nkdhr-canvas: protecting session while lock client prepares surfaces");
        self.protocols.confirm_lock_if_ready();
    }

    fn unlock(&mut self) {
        self.protocols.lock = SessionLockRuntime {
            connected_outputs: self.protocols.lock.connected_outputs.clone(),
            ..SessionLockRuntime::default()
        };

        let focus = self.active_view().keyboard_focus.clone();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, focus, smithay::utils::SERIAL_COUNTER.next_serial());
        }
        println!("nkdhr-canvas: session unlocked");
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        if !self.protocols.lock.protected {
            return;
        }
        let Some(output) = smithay::output::Output::from_resource(&output) else {
            return;
        };
        let Some(mode) = output.current_mode() else {
            return;
        };
        let logical_size = mode
            .size
            .to_f64()
            .to_logical(output.current_scale().fractional_scale());
        surface.with_pending_state(|state| {
            state.size = Some(
                (
                    logical_size.w.round().max(1.0) as u32,
                    logical_size.h.round().max(1.0) as u32,
                )
                    .into(),
            );
        });
        surface.send_configure();

        let output_name = output.name();
        let lock_focus = surface.wl_surface().clone();
        self.protocols.lock.surfaces.insert(output_name, surface);
        if self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .is_none()
            && let Some(keyboard) = self.seat.get_keyboard()
        {
            keyboard.set_focus(
                self,
                Some(KeyboardFocusTarget::Wayland(lock_focus)),
                smithay::utils::SERIAL_COUNTER.next_serial(),
            );
        }
    }
}

delegate_primary_selection!(App);
delegate_xdg_decoration!(App);
delegate_fractional_scale!(App);
delegate_idle_inhibit!(App);
delegate_pointer_constraints!(App);
delegate_relative_pointer!(App);
delegate_session_lock!(App);

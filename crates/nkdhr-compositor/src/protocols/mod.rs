//! The protocol long tail and the session lock.
//!
//! Registering a global is rarely enough on its own: selection follows
//! keyboard focus, constraints alter motion delivery, lock surfaces replace
//! the scene and screencopy reads back the composition. The policy that
//! makes each protocol behave lives next to its registration here, shared
//! by both backends.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::fd::OwnedFd;

mod screencopy;
mod xwayland;

pub use screencopy::SCREENCOPY_FORMAT;
use screencopy::ScreencopyState;

use smithay::desktop::{PopupManager, PopupUngrabStrategy};
use smithay::input::pointer::{MotionEvent, PointerHandle};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::{
    wl_output::WlOutput, wl_surface::WlSurface,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, Resource, delegate_dispatch,
    delegate_global_dispatch,
};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::compositor::with_states;
use smithay::wayland::fractional_scale::{
    FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
};
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use smithay::wayland::pointer_constraints::{
    PointerConstraintsHandler, PointerConstraintsState,
};
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::session_lock::{
    ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
    SessionLockManagerState, SessionLockState, SessionLocker,
};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::{
    delegate_fractional_scale, delegate_idle_inhibit, delegate_pointer_constraints,
    delegate_pointer_gestures, delegate_primary_selection, delegate_relative_pointer,
    delegate_viewporter, delegate_xdg_decoration,
};
use smithay::xwayland::xwm::X11Wm;

use crate::input::{self, UserActionSerials};
use crate::outputs::{OutputLayout, ResolvedOutput};
use crate::state::App;

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
    /// `wp_viewporter` needs no policy of its own: Smithay validates a
    /// surface's viewport and applies its crop and destination size in
    /// `on_commit_buffer_handler`, and everything that sizes, hit-tests or
    /// draws a surface reads the resulting surface view rather than the
    /// buffer. It is what makes fractional scaling usable: a client draws at
    /// the preferred fractional scale and sets its logical size as the
    /// viewport destination.
    _viewporter: ViewporterState,
    _pointer_constraints: PointerConstraintsState,
    _relative_pointer: RelativePointerManagerState,
    _pointer_gestures: PointerGesturesState,
    _idle_inhibit: IdleInhibitManagerState,
    pub idle_inhibitors: IdleInhibitors,
    pub session_lock: SessionLockManagerState,
    pub xwayland_shell: XWaylandShellState,
    pub xwm: Option<X11Wm>,
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
    /// The lock object of the lock in force. It decides which
    /// `unlock_and_destroy` may end the lock, and its client is the only one
    /// whose lock surfaces are shown and whose drags are allowed.
    lock: Option<ExtSessionLockV1>,
    connected_outputs: BTreeSet<String>,
    presented_outputs: BTreeSet<String>,
    surfaces: BTreeMap<String, LockSurface>,
}

/// Whether `unlock_and_destroy` sent on `sender` may end the session lock:
/// only the lock in force may end itself, and only once `locked` has been
/// sent for it (`confirmed`); before that, the request is a protocol error.
/// Generic over the lock object so the rule is testable without a display.
fn may_unlock<L: PartialEq>(in_force: Option<&L>, confirmed: bool, sender: &L) -> bool {
    confirmed && in_force == Some(sender)
}

/// The surfaces that hold idle inhibitors, one entry per live
/// `zwp_idle_inhibitor_v1`. The protocol allows several inhibitors on one
/// surface, and Smithay reports each creation and destruction separately,
/// so a surface keeps inhibiting until its last inhibitor is gone. Generic
/// over the surface type only so that the counting is testable without a
/// display.
#[derive(Debug)]
pub struct IdleInhibitors<S = WlSurface> {
    surfaces: Vec<S>,
}

impl<S> Default for IdleInhibitors<S> {
    fn default() -> Self {
        Self {
            surfaces: Vec::new(),
        }
    }
}

impl<S: PartialEq> IdleInhibitors<S> {
    fn inhibit(&mut self, surface: S) {
        self.surfaces.push(surface);
    }

    /// Forgets one inhibitor of `surface`.
    fn uninhibit(&mut self, surface: &S) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|candidate| candidate == surface)
        {
            self.surfaces.swap_remove(index);
        }
    }

    /// Keeps only the inhibitors whose surface passes `keep`; reaping uses
    /// it to forget surfaces whose client died without destroying them.
    pub fn retain(&mut self, keep: impl FnMut(&S) -> bool) {
        self.surfaces.retain(keep);
    }

    /// Every surface with at least one inhibitor, once per inhibitor.
    pub fn surfaces(&self) -> impl Iterator<Item = &S> {
        self.surfaces.iter()
    }
}

impl ProtocolState {
    pub fn new(display_handle: &DisplayHandle) -> Self {
        Self {
            data_device: DataDeviceState::new::<App>(display_handle),
            primary_selection: PrimarySelectionState::new::<App>(display_handle),
            _xdg_decoration: XdgDecorationState::new::<App>(display_handle),
            _fractional_scale: FractionalScaleManagerState::new::<App>(display_handle),
            _viewporter: ViewporterState::new::<App>(display_handle),
            _pointer_constraints: PointerConstraintsState::new::<App>(display_handle),
            _relative_pointer: RelativePointerManagerState::new::<App>(display_handle),
            _pointer_gestures: PointerGesturesState::new::<App>(display_handle),
            _idle_inhibit: IdleInhibitManagerState::new::<App>(display_handle),
            idle_inhibitors: IdleInhibitors::default(),
            session_lock: SessionLockManagerState::new::<App, _>(display_handle, |_| true),
            xwayland_shell: XWaylandShellState::new::<App>(display_handle),
            xwm: None,
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

    /// Fits every lock surface to its output in `layout`, after a layout
    /// change. A scale or position change hot-applied from the `outputs`
    /// namespace, or a resized nested window, changes an output's logical
    /// size while the session stays locked; a lock surface left at the old
    /// size would leave part of its output black, or put part of the
    /// locker, such as a centred password prompt, off-screen. Surfaces
    /// whose output's size and scale did not change receive nothing.
    pub fn fit_lock_surfaces(&self, layout: &OutputLayout) {
        for (name, surface) in &self.lock.surfaces {
            if let Some(output) = layout.output(name)
                && surface.alive()
            {
                configure_lock_surface(surface, output);
            }
        }
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

    /// Whether `surface` is the lock surface of some output.
    pub fn is_lock_surface(&self, surface: &WlSurface) -> bool {
        self.lock
            .surfaces
            .values()
            .any(|lock_surface| lock_surface.wl_surface() == surface)
    }

    /// Whether `object` belongs to the client that holds the lock in force.
    pub fn is_locker_object(&self, object: &ObjectId) -> bool {
        self.lock
            .lock
            .as_ref()
            .is_some_and(|lock| lock.id().same_client_as(object))
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
            println!("nkdhr-compositor: session lock confirmed after protected presentation");
        }
    }
}

impl App {
    /// Protects the session the moment a lock begins: no interaction that
    /// started before the lock may survive into it.
    ///
    /// The keyboard focus is remembered for unlock first, while the popup
    /// grab that may hold the keyboard still names its window.
    ///
    /// The popup-grab chain is dismissed next, as xdg-shell requires when a
    /// popup grab ends; otherwise a menu would stay open with nothing left
    /// to close it. It is dismissed through Smithay's `PopupGrab::ungrab`,
    /// as a click outside a menu is, which also makes Smithay's per-seat
    /// chain forget the popups: a client that ignores `popup_done` (one
    /// that is hung or stopped) would otherwise still have the chain's
    /// topmost popup after the unlock, and the next client to open a menu
    /// would receive a protocol error for opening it over another client's
    /// popup. The root window's other popups, which hold no grab, are
    /// dismissed with them, so the window comes back from the lock with no
    /// popup open.
    ///
    /// The keyboard grab ends before anything else can move keyboard focus:
    /// Smithay's `PopupKeyboardGrab` ignores focus changes while it is
    /// active, so an open popup would keep receiving keys during the lock,
    /// and a `PopupPointerGrab` that ends while it is active ends it too and
    /// hands keyboard focus to the menu's window for a moment. Then every
    /// surface loses pointer focus, so a drag-and-drop grab ends over no
    /// target and is cancelled rather than dropped into a window, and every
    /// pointer grab (move, resize, popup, drag-and-drop) and every touch
    /// sequence end. The drag-and-drop icon is cleared, the keyboard focus
    /// is cleared and the pointer is left without focus. Finally the record
    /// of user actions is cleared, so no serial from before the lock can
    /// take a popup grab once it ends.
    fn protect_input_for_lock(&mut self) {
        self.remember_focus_for_lock();
        if let Some(mut grab) = self.take_popup_grab() {
            let _ = grab.ungrab(PopupUngrabStrategy::All);
            if let Some((root, _)) = grab.pointer_grab_start_data().focus.as_ref() {
                dismiss_popups(root);
            }
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.unset_grab(self);
        }
        let time = input::synthetic_event_time();
        if let Some(pointer) = self.seat.get_pointer() {
            leave_all_surfaces(self, &pointer, time);
            pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
        }
        input::cancel_touch_sequences(self);
        self.dnd_icon = None;
        self.set_keyboard_focus(None);
        if let Some(pointer) = self.seat.get_pointer() {
            leave_all_surfaces(self, &pointer, time);
        }
        self.user_actions = UserActionSerials::default();
    }
}

/// Moves pointer focus to nothing without moving the pointer.
fn leave_all_surfaces(app: &mut App, pointer: &PointerHandle<App>, time: u32) {
    let location = pointer.current_location();
    pointer.motion(
        app,
        None,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    pointer.frame(app);
}

/// Dismisses every popup of a toplevel surface. Smithay lists nested
/// popups before their parents, so each one is dismissed topmost first, as
/// xdg-shell requires.
fn dismiss_popups(root: &WlSurface) {
    let popups = PopupManager::popups_for_surface(root)
        .map(|(popup, _)| popup)
        .collect::<Vec<_>>();
    for popup in popups {
        let _ = PopupManager::dismiss_popup(root, &popup);
    }
}

impl PrimarySelectionHandler for App {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.protocols.primary_selection
    }
}

/// Decorations are always client-side: every negotiation is answered with
/// `ClientSide`, and the compositor draws no title bar or border of any
/// kind. Clients that never bind the protocol draw their own, as xdg-shell
/// requires.
impl XdgDecorationHandler for App {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        configure_client_side_decoration(&toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: Mode) {
        configure_client_side_decoration(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        configure_client_side_decoration(&toplevel);
    }
}

/// Records the mode in the pending state. Before the toplevel's initial
/// configure, that configure carries it; afterwards the client is answered
/// with a configure right away.
fn configure_client_side_decoration(toplevel: &ToplevelSurface) {
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(Mode::ClientSide);
    });
    if toplevel.is_initial_configure_sent() {
        toplevel.send_configure();
    }
}

/// Preferred scales are maintained by the output-membership pass after each
/// render (`render::update_output_membership`). A surface that asks for its
/// preferred scale before it is shown anywhere, typically a new window
/// before its first buffer, starts with the scale of the output a new
/// window is placed on: the one under the pointer, or the first output. Its
/// first frame is then drawn at the right resolution instead of being
/// redrawn once the window appears.
impl FractionalScaleHandler for App {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let layout = self.output_layout();
        let Some(scale) = layout
            .output_at(self.pointer_location())
            .or_else(|| layout.outputs().first())
            .map(|output| output.scale)
        else {
            return;
        };
        with_states(&surface, |states| {
            with_fractional_scale(states, |state| {
                if state.preferred_scale().is_none() {
                    state.set_preferred_scale(scale);
                }
            });
        });
    }
}

impl IdleInhibitHandler for App {
    fn inhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.inhibit(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.uninhibit(&surface);
    }
}

/// A new constraint starts inactive unless its surface already holds
/// pointer focus with the pointer inside its region; otherwise the next
/// motion that brings the pointer there activates it.
impl PointerConstraintsHandler for App {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        input::activate_pointer_constraint(self, pointer, surface);
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
        self.protocols.lock.lock = Some(confirmation.ext_session_lock().clone());
        self.protocols.lock.confirmation = Some(confirmation);
        self.protocols.lock.presented_outputs.clear();
        self.protocols.lock.surfaces.clear();
        self.protect_input_for_lock();

        println!("nkdhr-compositor: protecting session while lock client prepares surfaces");
        self.protocols.confirm_lock_if_ready();
    }

    /// Reached only for `unlock_and_destroy` on the confirmed lock in force:
    /// every other unlock request is refused before Smithay sees it (see the
    /// `Dispatch<ExtSessionLockV1, _>` implementation below).
    fn unlock(&mut self) {
        if !self.protocols.lock.protected {
            return;
        }
        self.protocols.lock = SessionLockRuntime {
            connected_outputs: self.protocols.lock.connected_outputs.clone(),
            ..SessionLockRuntime::default()
        };
        self.restore_focus_after_unlock();
        // Hand pointer focus back to whatever window is under the pointer,
        // so the first click after unlocking reaches it without a motion.
        input::refresh_pointer_focus(self);
        println!("nkdhr-compositor: session unlocked");
    }

    /// Accepts a lock surface only from the client that holds the lock in
    /// force. Smithay hands over `get_lock_surface` from any lock object,
    /// including one this compositor refused, and checks the output only
    /// against that client's own `wl_output` bindings; a surface from any
    /// other client would replace the locker's on screen and receive the
    /// user's typing. Such a surface is ignored: never configured, shown or
    /// focused. So is one for an output that is no longer in the layout.
    ///
    /// An accepted surface is configured to fit its output
    /// ([`configure_lock_surface`]), and fitted again after every output
    /// layout change ([`ProtocolState::fit_lock_surfaces`]).
    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        if !self.protocols.lock.protected
            || !self.protocols.is_locker_object(&surface.wl_surface().id())
        {
            return;
        }
        let Some(output_name) = Output::from_resource(&output).map(|output| output.name()) else {
            return;
        };
        let Some(resolved) = self.output_layout().output(&output_name) else {
            return;
        };
        configure_lock_surface(&surface, resolved);

        self.protocols.lock.surfaces.insert(output_name, surface);
        self.keep_keyboard_on_lock_surface();
    }
}

/// Fits a lock surface to its output: the output's scale becomes its
/// preferred fractional scale, and a configure carries the output's logical
/// size from the layout, the size xdg-output advertises for that output.
///
/// The scale is set first, so the locker knows it before it draws for the
/// configure and its frame already matches the output. Smithay sends the
/// scale only when it changed, and the configure only when the size
/// differs from the last one sent or acknowledged, so fitting a surface
/// that already fits sends nothing.
fn configure_lock_surface(surface: &LockSurface, output: &ResolvedOutput) {
    with_states(surface.wl_surface(), |states| {
        with_fractional_scale(states, |state| state.set_preferred_scale(output.scale));
    });
    surface.with_pending_state(|state| {
        state.size = Some(
            (
                output.logical_size.w.max(1).unsigned_abs(),
                output.logical_size.h.max(1).unsigned_abs(),
            )
                .into(),
        );
    });
    surface.send_configure();
}

/// `ext_session_lock_v1` requests pass through this check before Smithay
/// handles them.
///
/// Smithay 0.7 accepts `unlock_and_destroy` on any lock object, including
/// one whose `lock` this compositor refused: it posts `invalid_unlock`, but
/// then goes on to call `SessionLockHandler::unlock`, which cannot tell
/// which lock asked. Any client could end the lock in force that way. Here
/// the request is checked first ([`may_unlock`]): on any lock object but
/// the confirmed lock in force it receives the protocol error and nothing
/// else happens. Every other request goes to Smithay unchanged. The other
/// session-lock interfaces are delegated to Smithay as
/// `delegate_session_lock!` would.
impl Dispatch<ExtSessionLockV1, SessionLockState> for App {
    fn request(
        app: &mut Self,
        client: &Client,
        lock: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, ext_session_lock_v1::Request::UnlockAndDestroy)
            && !may_unlock(
                app.protocols.lock.lock.as_ref(),
                app.protocols.lock.confirmed,
                lock,
            )
        {
            lock.post_error(
                ext_session_lock_v1::Error::InvalidUnlock,
                "only the confirmed lock in force can unlock the session",
            );
            return;
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            app, client, lock, request, data, display, data_init,
        );
    }
}

delegate_primary_selection!(App);
delegate_xdg_decoration!(App);
delegate_fractional_scale!(App);
delegate_viewporter!(App);
delegate_idle_inhibit!(App);
delegate_pointer_constraints!(App);
delegate_pointer_gestures!(App);
delegate_relative_pointer!(App);
delegate_global_dispatch!(
    App: [ExtSessionLockManagerV1: SessionLockManagerGlobalData] => SessionLockManagerState
);
delegate_dispatch!(App: [ExtSessionLockManagerV1: ()] => SessionLockManagerState);
delegate_dispatch!(
    App: [ExtSessionLockSurfaceV1: ExtLockSurfaceUserData] => SessionLockManagerState
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_confirmed_lock_in_force_may_unlock() {
        let (in_force, refused) = (1, 2);

        assert!(may_unlock(Some(&in_force), true, &in_force));
        // A lock whose `lock` was refused, while another is in force.
        assert!(!may_unlock(Some(&in_force), true, &refused));
        // The lock in force before `locked` was sent.
        assert!(!may_unlock(Some(&in_force), false, &in_force));
        // No lock in force: an old refused lock changes nothing.
        assert!(!may_unlock(None, false, &refused));
        assert!(!may_unlock(None, true, &refused));
    }

    #[test]
    fn a_surface_inhibits_until_its_last_inhibitor_is_gone() {
        let mut inhibitors = IdleInhibitors::default();
        inhibitors.inhibit("video");
        inhibitors.inhibit("video");
        inhibitors.inhibit("call");

        inhibitors.uninhibit(&"video");
        assert!(inhibitors.surfaces().any(|surface| *surface == "video"));

        inhibitors.uninhibit(&"video");
        assert!(!inhibitors.surfaces().any(|surface| *surface == "video"));
        assert!(inhibitors.surfaces().any(|surface| *surface == "call"));

        // Destroying an inhibitor that is not tracked changes nothing.
        inhibitors.uninhibit(&"video");
        assert_eq!(inhibitors.surfaces().count(), 1);

        inhibitors.retain(|surface| *surface != "call");
        assert_eq!(inhibitors.surfaces().count(), 0);
    }
}

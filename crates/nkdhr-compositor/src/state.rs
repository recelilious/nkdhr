//! `App`, the Smithay state type every protocol handler is implemented on,
//! and the focus policy.
//!
//! `App` holds protocol state and the window store and nothing else. The
//! renderer is never stored here: nothing in protocol handling touches
//! pixels, and buffer import happens lazily at render time on whichever
//! renderer the backend passes in. That is what lets one `App` serve the
//! nested backend's `GlesRenderer` and the TTY backend's multi-GPU renderer.

use std::borrow::Cow;
use std::os::unix::io::OwnedFd;
use std::time::Instant;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::input::KeyState;
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::desktop::{
    PopupGrab, PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy,
    Window, find_popup_root_surface,
};
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::pointer::{ClickGrab, CursorImageStatus, Focus, PointerGrab};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::{
    wl_buffer, wl_data_source::WlDataSource, wl_seat, wl_surface::WlSurface,
};
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, DnDGrab, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::wayland::selection::primary_selection::set_primary_focus;
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::shell::xdg::{
    Configure, PopupSurface, PositionerState, ToplevelStateSet, ToplevelSurface, XdgShellHandler,
    XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::xwayland::{X11Surface, XWaylandClientData};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_seat,
    delegate_shm, delegate_xdg_shell,
};

use crate::cursor::CursorState;
use crate::grabs::{self, ResizeEdges, ResizeState};
use crate::input::{self, TouchPoints, UserActionSerials};
use crate::outputs::OutputLayout;
use crate::protocols::ProtocolState;
use crate::windows::{self, ManagedWindow, WindowId, WindowStore};

/// The keyboard focus target: a native surface (a window, a popup holding an
/// explicit grab, or a session-lock surface) or an X11 window.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyboardFocusTarget {
    Wayland(WlSurface),
    X11(X11Surface),
}

/// Marks a selection that XWayland owns, so the compositor can tell it
/// apart from a Wayland client's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionOwner {
    XWayland(SelectionTarget),
}

/// The surface a client attached to an ongoing drag-and-drop, drawn at the
/// pointer.
pub struct DndIcon {
    pub surface: WlSurface,
    /// Offset from the pointer: the cursor hotspot recorded at drag start,
    /// so the icon keeps its position relative to the cursor image.
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

/// Everything the compositor's protocol handlers need.
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
    /// The touch points that are down, recorded by the input router so that
    /// a session lock can cancel every touch sequence in progress.
    pub touch_points: TouchPoints,
    /// The latest user action of each kind and the client it reached,
    /// recorded by the input router: `xdg_popup.grab` is honoured only for
    /// one of these.
    pub user_actions: UserActionSerials,
    /// The popup grab granted most recently, kept until the compositor's
    /// side of it is released. Its chain decides whether another client's
    /// menu may open ([`App::popup_grab_may_start`]); a touch outside it
    /// ([`App::dismiss_popup_grab`]) and a session lock dismiss it; and
    /// [`App::settle_popup_grab`] releases its pointer and keyboard grabs
    /// once the chain is over.
    popup_grab: Option<PopupGrab<Self>>,
    /// Set when a client other than the locker starts a drag-and-drop during
    /// a session lock. Smithay installs the drag's grab right after telling
    /// the handler, with no way to refuse it, so
    /// [`App::finish_client_dispatch`] cancels it once the request is done.
    cancel_drag_after_dispatch: bool,
    pub windows: WindowStore,
    /// A copy of the backend's current layout. Protocol handlers, the
    /// commit-time placement rule in particular, have no access to the
    /// backend, so `App` keeps its own copy, replaced through
    /// [`App::set_output_layout`].
    output_layout: OutputLayout,
    /// The keyboard focus to restore when the session is unlocked. While
    /// the session is locked the focus policy writes here instead of to the
    /// live focus, which belongs to the lock surface.
    focus_before_lock: Option<KeyboardFocusTarget>,
    /// The VT-switch seam. Only the TTY backend enables switching; the
    /// input router records a request and the backend, which owns libseat,
    /// performs it, so the shared input layer never touches the session.
    vt_switching_enabled: bool,
    pending_vt_switch: Option<i32>,
}

impl App {
    pub fn new(
        display_handle: &DisplayHandle,
        dmabuf_state: DmabufState,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display_handle, "nkdhr-compositor");
        seat.add_keyboard(Default::default(), 200, 200)?;
        seat.add_pointer();
        seat.add_touch();

        Ok(Self {
            start_time: Instant::now(),
            display_handle: display_handle.clone(),
            compositor_state: CompositorState::new::<Self>(display_handle),
            // No window-management capabilities are advertised: the protocol
            // layer offers no maximize, fullscreen, minimize or window menu,
            // and clients that honour `wm_capabilities` hide those controls
            // instead of showing buttons that do nothing.
            xdg_shell_state: XdgShellState::new_with_capabilities::<Self>(
                display_handle,
                Vec::<xdg_toplevel::WmCapabilities>::new(),
            ),
            shm_state: ShmState::new::<Self>(display_handle, Vec::new()),
            dmabuf_state,
            _output_manager_state: OutputManagerState::new_with_xdg_output::<Self>(display_handle),
            seat_state,
            protocols: ProtocolState::new(display_handle),
            popup_manager: PopupManager::default(),
            seat,
            cursor: CursorState::default(),
            dnd_icon: None,
            touch_points: TouchPoints::default(),
            user_actions: UserActionSerials::default(),
            popup_grab: None,
            cancel_drag_after_dispatch: false,
            windows: WindowStore::default(),
            output_layout: OutputLayout::default(),
            focus_before_lock: None,
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

    pub fn output_layout(&self) -> &OutputLayout {
        &self.output_layout
    }

    /// Adopts a newly resolved output layout.
    ///
    /// Reconciles the protocol bookkeeping keyed by output name (session-lock
    /// coverage, lock-surface sizes and keyboard focus, pending screencopy
    /// requests) and then applies the keep-reachable rule: a placed, managed
    /// window whose frame no longer intersects any output is placed again by
    /// the initial-placement rule. Every other window keeps its global
    /// location; a window does not travel with an output. Pointer focus is
    /// recomputed last, since outputs and windows may have moved under the
    /// pointer.
    pub fn set_output_layout(&mut self, layout: OutputLayout) {
        self.protocols
            .reconcile_outputs(layout.outputs().iter().map(|output| output.name.clone()));
        self.output_layout = layout;
        self.protocols.fit_lock_surfaces(&self.output_layout);
        self.keep_keyboard_on_lock_surface();

        let pointer = self.pointer_location();
        let stranded = self
            .windows
            .placed()
            .filter(|window| !window.is_override_redirect())
            .filter_map(|window| {
                let location = windows::location_to_keep_reachable(
                    window.location()?,
                    window.geometry(),
                    &self.output_layout,
                    pointer,
                )?;
                Some((window.id(), location))
            })
            .collect::<Vec<_>>();
        for (id, location) in stranded {
            println!(
                "nkdhr-compositor: window {id} is on no output; placing it at ({}, {})",
                location.x, location.y
            );
            self.move_window(id, location);
        }
        input::refresh_pointer_focus(self);
    }

    pub fn session_locked(&self) -> bool {
        self.protocols.is_locked()
    }

    /// The pointer position in the global space.
    pub fn pointer_location(&self) -> Point<f64, Logical> {
        self.seat
            .get_pointer()
            .map_or_else(|| (0.0, 0.0).into(), |pointer| pointer.current_location())
    }

    /// Moves a placed window. Managed X11 windows are reconfigured at the
    /// new position, so the X server's idea of each window's position is
    /// always the compositor's. Returns `false` when the window is gone,
    /// dead or unplaced.
    ///
    /// `MoveGrab` calls this from inside its own pointer callbacks, so it
    /// leaves pointer focus alone; other callers refresh it once their
    /// change is complete.
    pub fn move_window(&mut self, id: WindowId, location: Point<i32, Logical>) -> bool {
        let Some(window) = self
            .windows
            .get(id)
            .filter(|window| window.is_placed() && window.window().alive())
        else {
            return false;
        };
        match window.x11_surface() {
            Some(surface) if !surface.is_override_redirect() => {
                let size = surface.geometry().size;
                self.set_x11_geometry(id, Rectangle::new(location, size));
            }
            _ => {
                self.windows.set_location(id, location);
            }
        }
        // A pending resize correction is relative to where the resize began;
        // once the window has been moved it no longer applies.
        if let Some(window) = self.windows.get_mut(id) {
            window.set_resize_state(ResizeState::Idle);
        }
        true
    }

    /// Sets a managed X11 window's position and size together: the store
    /// location and the X11 configure always agree.
    pub fn set_x11_geometry(&mut self, id: WindowId, geometry: Rectangle<i32, Logical>) -> bool {
        let Some(surface) = self
            .windows
            .get(id)
            .and_then(ManagedWindow::x11_surface)
            .filter(|surface| !surface.is_override_redirect())
            .cloned()
        else {
            return false;
        };
        if let Err(error) = surface.configure(geometry) {
            eprintln!(
                "nkdhr-compositor: failed to configure X11 window {}: {error}",
                surface.window_id()
            );
        }
        self.windows.set_location(id, geometry.loc)
    }

    /// Raises a window and gives it keyboard focus. Mapping a window and
    /// clicking one both go through here.
    ///
    /// While the session is locked the live focus belongs to the lock
    /// surface, so the window becomes the focus restored on unlock instead.
    /// Override-redirect windows and unplaced toplevels are never focused.
    ///
    /// A popup grab in place is dismissed first, as a click outside its menu
    /// would: Smithay's `PopupKeyboardGrab` keeps keyboard focus on the menu
    /// and silently drops any other focus, so a window opening while a menu
    /// is open would be raised above the menu's window yet stay unfocused.
    ///
    /// Raising can change what lies under the pointer. Callers refresh
    /// pointer focus (`input::refresh_pointer_focus`) once their whole
    /// change is done, so that a single motion describes its outcome.
    pub fn focus_window(&mut self, id: WindowId) {
        let Some(target) = self
            .windows
            .get(id)
            .filter(|window| window.is_focusable())
            .and_then(ManagedWindow::keyboard_target)
        else {
            return;
        };
        if self.popup_grab.is_some() {
            self.dismiss_popup_grab();
        }
        self.windows.raise(id);
        // Keep the X server's stacking order in step with the store's: X11
        // clients rely on it, for example to find the target of a
        // drag-and-drop between X11 windows.
        if let KeyboardFocusTarget::X11(surface) = &target
            && let Some(xwm) = self.protocols.xwm.as_mut()
            && let Err(error) = xwm.raise_window(surface)
        {
            eprintln!(
                "nkdhr-compositor: failed to restack X11 window {}: {error}",
                surface.window_id()
            );
        }
        self.apply_policy_focus(Some(target));
    }

    /// The focus fallback: focuses the topmost focusable window, or nothing.
    /// It runs when the window holding keyboard focus goes away; while the
    /// session is locked it updates the remembered focus instead.
    pub fn focus_topmost(&mut self) {
        let target = self
            .windows
            .topmost_focusable()
            .and_then(ManagedWindow::keyboard_target);
        self.apply_policy_focus(target);
    }

    /// Returns keyboard focus to the root window of a popup-grab chain that
    /// held it, when the chain closes: the window the menu belongs to, which
    /// the grant raised, rather than whatever window is topmost. That is
    /// also where Smithay's popup grabs return focus when the chain ends
    /// through pointer input or a key. When the root window is gone or
    /// unmapped, the focus fallback runs instead.
    fn focus_popup_root(&mut self, grab: &PopupGrab<Self>) {
        match self.popup_root_target(grab) {
            Some(target) => self.apply_policy_focus(Some(target)),
            None => self.focus_topmost(),
        }
    }

    /// The focus target of a popup-grab chain's root window, while that
    /// window is alive and may be focused.
    fn popup_root_target(&self, grab: &PopupGrab<Self>) -> Option<KeyboardFocusTarget> {
        grab.keyboard_grab_start_data()
            .focus
            .as_ref()
            .and_then(|root| root.wl_surface())
            .and_then(|surface| self.windows.find_root(&surface))
            .filter(|window| window.is_focusable() && window.window().alive())
            .and_then(ManagedWindow::keyboard_target)
    }

    /// Applies a focus-policy decision: to the live focus, or while the
    /// session is locked, to the focus restored on unlock.
    fn apply_policy_focus(&mut self, target: Option<KeyboardFocusTarget>) {
        if self.session_locked() {
            self.focus_before_lock = target;
        } else {
            self.set_keyboard_focus(target);
        }
    }

    /// Whether a window holds keyboard focus: the live focus, or while the
    /// session is locked, the focus remembered for unlock.
    fn window_holds_focus(&self, id: WindowId) -> bool {
        let focus = if self.session_locked() {
            self.focus_before_lock.clone()
        } else {
            self.seat
                .get_keyboard()
                .and_then(|keyboard| keyboard.current_focus())
        };
        focus.is_some_and(|focus| {
            self.windows
                .get(id)
                .is_some_and(|window| window.owns_focus(&focus))
        })
    }

    /// Sets the live keyboard focus through the seat.
    ///
    /// Smithay calls `SeatHandler::focus_changed` only for a new focus, not
    /// when focus is cleared, so clearing applies the same protocol
    /// bookkeeping explicitly.
    pub(crate) fn set_keyboard_focus(&mut self, target: Option<KeyboardFocusTarget>) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let cleared = target.is_none();
        keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        if cleared {
            self.sync_focus_state(None);
        }
    }

    /// Keeps protocol state consistent with the keyboard focus: exactly the
    /// focused window carries xdg `Activated` (or X11 activation), and only
    /// toplevels whose state actually changed receive a configure. Clipboard
    /// and primary-selection focus follow the focused client, and are
    /// cleared while the session is locked.
    fn sync_focus_state(&mut self, focused: Option<&KeyboardFocusTarget>) {
        for window in self.windows.iter() {
            let activated =
                window.is_placed() && focused.is_some_and(|focused| window.owns_focus(focused));
            if window.window().set_activated(activated)
                && let Some(toplevel) = window.window().toplevel()
                && toplevel.is_initial_configure_sent()
            {
                toplevel.send_pending_configure();
            }
        }
        let client = (!self.session_locked())
            .then(|| {
                focused
                    .and_then(|target| target.wl_surface())
                    .and_then(|surface| self.display_handle.get_client(surface.id()).ok())
            })
            .flatten();
        set_data_device_focus(&self.display_handle, &self.seat, client.clone());
        set_primary_focus(&self.display_handle, &self.seat, client);
    }

    /// Remembers the live keyboard focus as the focus to restore on unlock,
    /// when a session lock begins.
    ///
    /// What is remembered is the window that owns the focus. While a popup
    /// holds the keyboard through its grab, that is the popup chain's root
    /// window, which is where focus goes when a chain closes (see
    /// [`App::focus_popup_root`]): the lock dismisses the chain, and the
    /// focused popup may even have been destroyed a moment ago, before its
    /// chain was settled. This must run before the lock takes the popup
    /// grab.
    pub(crate) fn remember_focus_for_lock(&mut self) {
        let popup_root = self
            .popup_grab
            .as_ref()
            .filter(|_| self.keyboard_held_by_popup_grab())
            .and_then(|grab| self.popup_root_target(grab));
        self.focus_before_lock = popup_root.or_else(|| {
            let focus = self
                .seat
                .get_keyboard()
                .and_then(|keyboard| keyboard.current_focus())?;
            let owner = self
                .windows
                .iter()
                .find(|window| window.owns_focus(&focus))
                .and_then(ManagedWindow::keyboard_target);
            Some(owner.unwrap_or(focus))
        });
    }

    /// Restores the remembered focus after a valid unlock. If the remembered
    /// surface died meanwhile, the focus fallback picks the topmost window.
    pub(crate) fn restore_focus_after_unlock(&mut self) {
        match self.focus_before_lock.take() {
            Some(focus) if focus.alive() => self.set_keyboard_focus(Some(focus)),
            Some(_) => self.focus_topmost(),
            None => self.set_keyboard_focus(None),
        }
    }

    /// While the session is locked, keeps keyboard focus on a live lock
    /// surface: when the focus is dead, missing or anything but one of the
    /// current lock surfaces, it moves to the lock surface of the output
    /// under the pointer, otherwise to that of the first output in layout
    /// order that has one, otherwise to nothing.
    ///
    /// Smithay keeps a destroyed surface as the keyboard focus until the
    /// focus is set again, so without this a locker that replaces the lock
    /// surface holding focus (an output slept or was unplugged) could not
    /// be typed into until a click. It runs whenever lock surfaces or
    /// outputs can have changed: for a new lock surface, after dead-client
    /// reaping and after each output layout change.
    pub(crate) fn keep_keyboard_on_lock_surface(&mut self) {
        if !self.session_locked() {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let current = keyboard.current_focus();
        let on_lock_surface = current.as_ref().is_some_and(|focus| match focus {
            KeyboardFocusTarget::Wayland(surface) => {
                surface.alive() && self.protocols.is_lock_surface(surface)
            }
            KeyboardFocusTarget::X11(_) => false,
        });
        if on_lock_surface {
            return;
        }
        let layout = &self.output_layout;
        let target = layout
            .output_at(self.pointer_location())
            .and_then(|output| self.protocols.lock_surface(&output.name))
            .or_else(|| {
                layout
                    .outputs()
                    .iter()
                    .find_map(|output| self.protocols.lock_surface(&output.name))
            })
            .map(KeyboardFocusTarget::Wayland);
        if target.is_some() || current.is_some() {
            self.set_keyboard_focus(target);
        }
    }

    /// Removes a window and runs the focus fallback if it held focus.
    pub fn remove_window(&mut self, id: WindowId) {
        let held_focus = self.window_holds_focus(id);
        if self.windows.remove(id).is_none() {
            return;
        }
        if held_focus {
            self.focus_topmost();
        }
        input::refresh_pointer_focus(self);
    }

    /// Places an unplaced xdg toplevel by the initial-placement rule, then
    /// raises and focuses it.
    fn place_toplevel(&mut self, id: WindowId) {
        let Some(geometry) = self.windows.get(id).map(ManagedWindow::geometry) else {
            return;
        };
        let location =
            windows::initial_location(geometry, &self.output_layout, self.pointer_location());
        self.windows.place(id, location);
        println!(
            "nkdhr-compositor: mapped window {id} at ({}, {})",
            location.x, location.y
        );
        self.focus_window(id);
        input::refresh_pointer_focus(self);
    }

    /// A placed toplevel committed a null buffer: it unmapped itself. It
    /// becomes unplaced, and its next buffer commit places it like a new
    /// window.
    fn unplace_toplevel(&mut self, id: WindowId) {
        let held_focus = self.window_holds_focus(id);
        self.windows.unplace(id);
        if held_focus {
            self.focus_topmost();
        }
        input::refresh_pointer_focus(self);
    }

    /// The commit handling specific to an xdg toplevel's root surface.
    fn toplevel_committed(&mut self, id: WindowId, surface: &WlSurface) {
        let Some(window) = self.windows.get(id) else {
            return;
        };
        let Some(toplevel) = window.window().toplevel().cloned() else {
            return;
        };
        if !toplevel.is_initial_configure_sent() {
            // The first commit, or the first after a null-buffer unmap, which
            // Smithay's buffer handling marks for reinitialization. The
            // initial configure carries no size, so the client picks its
            // own, and no states, so a remapped window starts afresh.
            toplevel.with_pending_state(|state| {
                state.size = None;
                state.states = ToplevelStateSet::default();
            });
            toplevel.send_configure();
            return;
        }
        let has_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
        match (window.is_placed(), has_buffer) {
            (false, true) => self.place_toplevel(id),
            (true, false) => self.unplace_toplevel(id),
            (true, true) => {
                if let Some(location) = self.windows.get_mut(id).and_then(grabs::commit_resize) {
                    self.windows.set_location(id, location);
                }
            }
            (false, false) => {}
        }
    }

    /// Defensive cleanup for abrupt client death: drops dead windows, dead
    /// idle-inhibitor surfaces and a dead DnD icon, and runs the focus
    /// fallback when the focus died (while locked, when the remembered
    /// focus died, and the live focus moves to a live lock surface).
    /// Pointer focus is recomputed when windows were reclaimed. Returns the
    /// number of windows reclaimed.
    ///
    /// It runs after [`App::settle_popup_grab`], which has already moved a
    /// focus that died with a grabbing popup to the chain's next popup or
    /// its root window, so the fallback here never takes focus away from
    /// the window a closed menu belonged to.
    pub fn reap_dead_clients(&mut self) -> usize {
        let reclaimed = self.windows.reap_dead().len();
        self.protocols.idle_inhibitors.retain(WlSurface::is_alive);
        if self
            .dnd_icon
            .as_ref()
            .is_some_and(|icon| !icon.surface.alive())
        {
            self.dnd_icon = None;
        }
        let focus_died = if self.session_locked() {
            self.focus_before_lock
                .as_ref()
                .is_some_and(|focus| !focus.alive())
        } else {
            self.seat
                .get_keyboard()
                .and_then(|keyboard| keyboard.current_focus())
                .is_some_and(|focus| !focus.alive())
        };
        if focus_died {
            self.focus_topmost();
        }
        self.keep_keyboard_on_lock_surface();
        if reclaimed > 0 {
            input::refresh_pointer_focus(self);
        }
        reclaimed
    }

    /// Completes what the requests of one client dispatch started, where
    /// Smithay 0.7 gives no handler a say while the request is processed.
    /// Both backends call it right after dispatching clients.
    ///
    /// Today that is one case: a drag-and-drop that a client other than the
    /// locker started during a session lock (see
    /// `ClientDndGrabHandler::started`) is cancelled, which ends the grab
    /// Smithay installed for it. The drag source receives `cancelled`, and
    /// pointer focus returns to the lock surface the drag took it from.
    pub fn finish_client_dispatch(&mut self) {
        if std::mem::take(&mut self.cancel_drag_after_dispatch) {
            self.cancel_drag();
        }
    }

    /// Ends a client drag-and-drop grab on the pointer or on touch.
    fn cancel_drag(&mut self) {
        let mut cancelled = false;
        if let Some(pointer) = self.seat.get_pointer()
            && pointer
                .with_grab(|_, grab| grab.is::<DnDGrab<Self>>())
                .unwrap_or(false)
        {
            pointer.unset_grab(
                self,
                SERIAL_COUNTER.next_serial(),
                input::synthetic_event_time(),
            );
            pointer.frame(self);
            cancelled = true;
        }
        if let Some(touch) = self.seat.get_touch()
            && touch
                .with_grab(|_, grab| grab.is::<DnDGrab<Self>>())
                .unwrap_or(false)
        {
            touch.unset_grab(self);
            cancelled = true;
        }
        if cancelled {
            println!("nkdhr-compositor: cancelled a drag started during the session lock");
        }
    }

    /// Whether a visible client has asked the session to stay awake: any
    /// live inhibitor surface that belongs to a placed window overlapping a
    /// connected output. Dead, unmapped or off-screen surfaces never keep
    /// the session awake.
    #[allow(
        dead_code,
        reason = "the query the session's idle and display-power policy will consume; that policy does not exist yet"
    )]
    pub fn idle_inhibited(&self) -> bool {
        self.protocols.idle_inhibitors.surfaces().any(|surface| {
            surface.is_alive()
                && self.windows.placed().any(|window| {
                    window.matches_surface(surface)
                        && window.global_bbox().is_some_and(|bbox| {
                            self.output_layout
                                .outputs()
                                .iter()
                                .any(|output| output.rect().overlaps(bbox))
                        })
                })
        })
    }

    pub fn lock_surface_for_output(&self, output_name: &str) -> Option<WlSurface> {
        self.protocols.lock_surface(output_name)
    }

    pub fn note_protected_frame(&mut self, output_name: &str) {
        self.protocols.note_protected_frame(output_name);
    }

    /// Installs an `xdg_popup.grab` and returns whether it was granted.
    ///
    /// The grab is honoured only on the compositor's seat, and only for the
    /// serial of the latest key press, key release, button press or
    /// touch-down when that action was delivered to the popup's client
    /// ([`UserActionSerials`]). The action may be over by then: toolkits
    /// open menus when a button is released or a finger lifted and name the
    /// press, so whether the pointer or touch still holds a grab with that
    /// serial does not matter, and neither does which client holds keyboard
    /// focus, which touch never moves. The grab must also be free to start
    /// ([`App::popup_grab_may_start`]). While the session is locked no grab
    /// is granted at all: a popup grab routes keys and clicks to the popup,
    /// which is not a lock surface.
    ///
    /// A granted grab installs `PopupPointerGrab` and `PopupKeyboardGrab`
    /// and gives the popup keyboard focus, since xdg-shell gives it to the
    /// topmost grabbing popup. It also raises the popup's root window:
    /// popups are drawn and hit-tested at their window's stacking level, so
    /// a menu of a window that is not on top (one opened by touch, which
    /// does not raise) would otherwise be partly hidden under other
    /// windows, and touching the hidden part would dismiss it. The grab is
    /// kept in `App::popup_grab` until its chain is over and released.
    fn grant_popup_grab(
        &mut self,
        popup: &PopupKind,
        seat: &wl_seat::WlSeat,
        serial: Serial,
    ) -> bool {
        if self.session_locked() {
            return false;
        }
        let Some(seat) = Seat::<Self>::from_resource(seat).filter(|seat| *seat == self.seat) else {
            return false;
        };
        let Ok(root_surface) = find_popup_root_surface(popup) else {
            return false;
        };
        let Some(client) = input::client_of(&root_surface) else {
            return false;
        };
        if !self.user_actions.delivered_to(serial, &client) || !self.popup_grab_may_start(&client) {
            return false;
        }

        let root_window = self.windows.find_root(&root_surface).map(ManagedWindow::id);
        let root = KeyboardFocusTarget::Wayland(root_surface);
        let Ok(grab) = self
            .popup_manager
            .grab_popup(root, popup.clone(), &seat, serial)
        else {
            return false;
        };
        if let Some(id) = root_window {
            self.windows.raise(id);
        }
        if let Some(pointer) = seat.get_pointer() {
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        if let Some(keyboard) = seat.get_keyboard() {
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        self.set_keyboard_focus(grab.current_grab());
        self.popup_grab = Some(grab);
        true
    }

    /// Whether a popup grab of `client` may start now
    /// ([`may_start_popup_grab`]), given what the pointer is doing and
    /// which client's popup-grab chain is open.
    fn popup_grab_may_start(&self, client: &ClientId) -> bool {
        may_start_popup_grab(
            &self.pointer_activity(),
            self.popup_chain_owner().as_ref(),
            client,
        )
    }

    /// What the pointer is doing, as far as a popup grab is concerned.
    fn pointer_activity(&self) -> PointerActivity<ClientId> {
        let Some(pointer) = self.seat.get_pointer() else {
            return PointerActivity::Free;
        };
        pointer
            .with_grab(|_, grab| PointerActivity::of(grab))
            .unwrap_or(PointerActivity::Free)
    }

    /// The client whose popup-grab chain is open: one of its popups still
    /// holds the grab ([`grabbing_popup`]). `None` when no chain is open,
    /// including while Smithay's pointer and keyboard popup grabs outlive a
    /// chain that is over, until [`App::settle_popup_grab`] releases them.
    pub(crate) fn popup_chain_owner(&self) -> Option<ClientId> {
        let popup = self.popup_grab.as_ref().and_then(grabbing_popup)?;
        popup
            .wl_surface()
            .and_then(|surface| input::client_of(&surface))
    }

    /// Brings the compositor's side of the popup grab in line with its
    /// chain. `render::refresh` runs it once per loop iteration, after
    /// `PopupManager::cleanup` has noted the popups clients destroyed and
    /// before dead-client reaping.
    ///
    /// While the chain is open, keyboard focus stays on its topmost
    /// grabbing popup. When a client destroys a nested popup, Smithay's
    /// `PopupKeyboardGrab` moves focus to the parent popup only with the
    /// next key and leaves the destroyed surface focused until then, so
    /// focus is moved here.
    ///
    /// Once the chain is over, its grabs are released and keyboard focus
    /// returns to the chain's root window ([`App::release_popup_grab`]).
    /// Smithay ends its `PopupPointerGrab` only with the next pointer event
    /// and its `PopupKeyboardGrab` only with the next key. Until then a
    /// menu closed by touch or from the keyboard would keep pointer focus
    /// on its client and keep the next click from focusing a window, and
    /// the reaping that follows would hand the focus the destroyed popup
    /// held to the topmost window.
    pub fn settle_popup_grab(&mut self) {
        let Some(topmost) = self.popup_grab.as_ref().map(grabbing_popup) else {
            return;
        };
        match topmost {
            Some(popup) => {
                let focus = self
                    .seat
                    .get_keyboard()
                    .and_then(|keyboard| keyboard.current_focus());
                if self.keyboard_held_by_popup_grab() && focus.as_ref() != Some(&popup) {
                    self.set_keyboard_focus(Some(popup));
                }
            }
            None => {
                if let Some(grab) = self.popup_grab.take() {
                    self.release_popup_grab(&grab);
                }
            }
        }
    }

    /// Dismisses the open popup-grab chain, as Smithay's `PopupPointerGrab`
    /// does for a click outside it: every grabbing popup receives
    /// `popup_done`, and the grab is released at once
    /// ([`App::release_popup_grab`]), which returns keyboard focus to the
    /// chain's root window. The input router calls it for a touch outside
    /// the chain's client, since Smithay has no touch counterpart of
    /// `PopupPointerGrab`. Dismissed popups no longer count as the chain's
    /// topmost popup, so another client's menu can open right away, before
    /// the dismissed client destroys its popups.
    pub(crate) fn dismiss_popup_grab(&mut self) {
        if let Some(mut grab) = self.popup_grab.take() {
            let _ = grab.ungrab(PopupUngrabStrategy::All);
            self.release_popup_grab(&grab);
        }
    }

    /// Takes the popup grab for a session lock, which dismisses its chain
    /// and ends Smithay's grabs together with every other grab.
    pub(crate) fn take_popup_grab(&mut self) -> Option<PopupGrab<Self>> {
        self.popup_grab.take()
    }

    /// Ends Smithay's pointer and keyboard popup grabs of a chain that is
    /// over or has just been dismissed. Pointer focus returns to the
    /// surface under the pointer. Keyboard focus, if the chain held it,
    /// returns to the chain's root window ([`App::focus_popup_root`]).
    ///
    /// The keyboard grab is ended first. Ending the `PopupPointerGrab` also
    /// ends a keyboard grab of the same serial and moves focus to the root
    /// surface unconditionally, even when that window is unmapped.
    fn release_popup_grab(&mut self, grab: &PopupGrab<Self>) {
        let keyboard_held = self.keyboard_held_by_popup_grab();
        if keyboard_held && let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.unset_grab(self);
        }
        if let Some(pointer) = self.seat.get_pointer()
            && pointer
                .with_grab(|_, current| current.is::<PopupPointerGrab<Self>>())
                .unwrap_or(false)
        {
            pointer.unset_grab(
                self,
                SERIAL_COUNTER.next_serial(),
                input::synthetic_event_time(),
            );
            pointer.frame(self);
        }
        if keyboard_held {
            self.focus_popup_root(grab);
        }
        input::refresh_pointer_focus(self);
    }

    /// Whether Smithay's `PopupKeyboardGrab` holds the keyboard.
    fn keyboard_held_by_popup_grab(&self) -> bool {
        self.seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.with_grab(|_, grab| grab.is::<PopupKeyboardGrab<Self>>()))
            .unwrap_or(false)
    }

    /// Validates an `xdg_toplevel.move`/`resize` request: it must come
    /// through the compositor's seat, for the serial of the pointer's
    /// current grab, and target a known window. `grabs` additionally
    /// requires that grab to be the implicit grab of a press on the window.
    fn client_grab_target(
        &self,
        surface: &ToplevelSurface,
        seat: &wl_seat::WlSeat,
        serial: Serial,
    ) -> Option<WindowId> {
        let same_seat = Seat::<Self>::from_resource(seat).is_some_and(|seat| seat == self.seat);
        let grabbed = self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.has_grab(serial));
        if !same_seat || !grabbed {
            return None;
        }
        self.windows
            .find_root(surface.wl_surface())
            .map(ManagedWindow::id)
    }
}

/// Dismisses a popup whose grab was not granted. The popup was tracked when
/// it was created, so it is dismissed through its tree, which also stops it
/// from being drawn; a popup without a known root is told directly.
fn dismiss_popup(popup: &PopupKind) {
    let dismissed = find_popup_root_surface(popup)
        .ok()
        .is_some_and(|root| PopupManager::dismiss_popup(&root, popup).is_ok());
    if !dismissed && let PopupKind::Xdg(surface) = popup {
        surface.send_popup_done();
    }
}

/// The topmost popup that still holds `grab`, or `None` once its chain is
/// over: every grabbing popup has been destroyed or dismissed, or the
/// chain's root is gone.
///
/// Smithay's `PopupGrab::has_ended` turns true only once the client has
/// also destroyed every popup that was dismissed, but the chain is over as
/// soon as they are dismissed: Smithay no longer counts them as the
/// chain's topmost popup, and a click outside one menu often opens another
/// menu before the first client has destroyed its popups.
fn grabbing_popup(grab: &PopupGrab<App>) -> Option<KeyboardFocusTarget> {
    if grab.has_ended() {
        return None;
    }
    // With no grabbing popup left, Smithay reports the root instead.
    let root = grab.keyboard_grab_start_data().focus.as_ref();
    grab.current_grab().filter(|popup| Some(popup) != root)
}

/// What the pointer is doing when a client asks for a popup grab. Generic
/// over the client identity only so that the grant rule is testable
/// without a display.
#[derive(Debug)]
enum PointerActivity<C> {
    /// No grab: the pointer only follows motion.
    Free,
    /// Smithay's implicit `ClickGrab`, the grab of a button held since a
    /// press, which started on a surface of this client; `None` when it
    /// started on no surface or on one that is gone.
    HeldButton(Option<C>),
    /// A menu's `PopupPointerGrab`, whether its chain is still open or over
    /// and not yet released.
    Menu,
    /// A move, a resize or a drag-and-drop.
    Interaction,
}

impl PointerActivity<ClientId> {
    fn of(grab: &dyn PointerGrab<App>) -> Self {
        if grab.is::<ClickGrab<App>>() {
            let pressed = grab
                .start_data()
                .focus
                .as_ref()
                .filter(|(surface, _)| surface.alive())
                .and_then(|(surface, _)| input::client_of(surface));
            Self::HeldButton(pressed)
        } else if grab.is::<PopupPointerGrab<App>>() {
            Self::Menu
        } else {
            Self::Interaction
        }
    }
}

/// Whether a popup grab of `client` may start, given what the pointer is
/// doing and the client whose popup-grab chain is open, if any.
///
/// Another client's open chain refuses the grab. Smithay keeps one chain
/// per seat and answers a grab whose parent is not the chain's topmost
/// popup with a protocol error (`not_the_topmost_popup`) that would
/// disconnect the client asking. A chain that is over counts for nothing,
/// even while its `PopupPointerGrab` has not been released yet.
///
/// The grant also takes the pointer. It may when the pointer is free, when
/// it holds a menu's grab (the client's own enclosing menu, which is how
/// menus nest and replace one another, or a chain that is over), or when
/// it holds the implicit grab of a press on one of the client's surfaces
/// or on none, which is how a menu opened on a press takes over that
/// press. Another client's held button keeps the pointer, and so do a
/// move, a resize and a drag.
fn may_start_popup_grab<C: PartialEq>(
    pointer: &PointerActivity<C>,
    open_chain_owner: Option<&C>,
    client: &C,
) -> bool {
    if open_chain_owner.is_some_and(|owner| owner != client) {
        return false;
    }
    match pointer {
        PointerActivity::Free | PointerActivity::Menu => true,
        PointerActivity::HeldButton(pressed) => pressed.as_ref().is_none_or(|on| on == client),
        PointerActivity::Interaction => false,
    }
}

/// Per-client state `wayland_server` asks every client to carry: the
/// compositor's buffer-commit bookkeeping.
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

    /// Wayland clients carry [`ClientState`]; the Xwayland client carries
    /// Smithay's `XWaylandClientData`.
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
        // A synchronized subsurface's state is applied with its parent's
        // commit, which runs this handler again for the root.
        if is_sync_subsurface(surface) {
            return;
        }
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let Some(id) = self.windows.find_root(&root).map(ManagedWindow::id) else {
            return;
        };
        if let Some(window) = self.windows.get(id) {
            window.window().on_commit();
        }
        if &root == surface {
            self.toplevel_committed(id, surface);
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

    /// Accepts the buffer without importing it. Real import happens the
    /// first time a render element uses the buffer, on whichever renderer
    /// draws that frame; validating here would mean importing eagerly into
    /// a renderer `App` deliberately does not own. A genuinely incompatible
    /// buffer surfaces as a render-time import error instead.
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

    /// A new toplevel enters the store unplaced. It receives its initial
    /// configure on its first commit and is placed once it commits a buffer,
    /// when its size is known.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.windows
            .insert_unplaced(Window::new_wayland_window(surface));
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        if let Err(error) = self
            .popup_manager
            .track_popup(PopupKind::Xdg(surface.clone()))
        {
            eprintln!("nkdhr-compositor: rejected popup with a dead parent: {error}");
            surface.send_popup_done();
            return;
        }
        surface.send_configure().ok();
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        if let Some(id) = self.client_grab_target(&surface, &seat, serial) {
            grabs::begin_move(self, id, serial);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        if let Some(id) = self.client_grab_target(&surface, &seat, serial)
            && let Some(edges) = ResizeEdges::from_xdg(edges)
        {
            grabs::begin_resize(self, id, edges, serial);
        }
    }

    /// Handles `xdg_popup.grab`. A grab that is not granted dismisses the
    /// popup at once, as xdg-shell requires; otherwise the client would show
    /// a menu that nothing closes.
    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let popup = PopupKind::Xdg(surface);
        if !self.grant_popup_grab(&popup, &seat, serial) {
            dismiss_popup(&popup);
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

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        let Configure::Toplevel(configure) = configure else {
            return;
        };
        let Some(id) = self.windows.find_root(&surface).map(ManagedWindow::id) else {
            return;
        };
        if let Some(window) = self.windows.get_mut(id) {
            grabs::ack_resize_configure(window, configure.serial);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self
            .windows
            .find_root(surface.wl_surface())
            .map(ManagedWindow::id)
        {
            self.remove_window(id);
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&KeyboardFocusTarget>) {
        self.sync_focus_state(focused);
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
            eprintln!("nkdhr-compositor: failed to publish Wayland selection to X11: {error}");
            return;
        }

        // Smithay 0.7 queues the X11 selection-owner request without flushing
        // it. A harmless RANDR round trip publishes the change before an X11
        // client tries to read the new Wayland-owned selection.
        if let Err(error) = xwm.get_randr_primary_output() {
            eprintln!("nkdhr-compositor: failed to flush the X11 selection update: {error}");
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
            eprintln!("nkdhr-compositor: failed to transfer X11 selection to Wayland: {error}");
        }
    }
}

impl DataDeviceHandler for App {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.protocols.data_device
    }
}

impl ClientDndGrabHandler for App {
    /// During a session lock, a drag may only come from the locker. Smithay
    /// starts a drag for any client that names the serial of the pointer's
    /// or touch's current grab, which during the lock is the locker's own
    /// press, so a background client could otherwise take the press away
    /// from the lock surface and follow the user's clicks through its data
    /// source. Such a drag is cancelled right after the dispatch
    /// ([`App::finish_client_dispatch`]) and its icon is never shown. The
    /// drag's client is known from its data source or its icon; a drag with
    /// neither is treated as foreign.
    fn started(
        &mut self,
        source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        if self.session_locked() {
            let client_object = source
                .as_ref()
                .map(Resource::id)
                .or_else(|| icon.as_ref().map(Resource::id));
            if !client_object.is_some_and(|object| self.protocols.is_locker_object(&object)) {
                self.cancel_drag_after_dispatch = true;
                return;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_grabs_start_only_where_they_disturb_no_other_client() {
        let (editor, terminal) = ("editor", "terminal");
        let may_start = may_start_popup_grab::<&str>;

        // A free pointer, or the implicit grab of a press on the client's
        // own surface or on none: a menu opened on a press takes it over.
        assert!(may_start(&PointerActivity::Free, None, &editor));
        assert!(may_start(
            &PointerActivity::HeldButton(Some(editor)),
            None,
            &editor
        ));
        assert!(may_start(&PointerActivity::HeldButton(None), None, &editor));
        // Another client's held button keeps the pointer.
        assert!(!may_start(
            &PointerActivity::HeldButton(Some(terminal)),
            None,
            &editor
        ));

        // A client's own open chain: nested menus and menus that replace
        // one another.
        assert!(may_start(&PointerActivity::Menu, Some(&editor), &editor));
        // Another client's open chain refuses the grab, whatever holds the
        // pointer; Smithay would answer it with a protocol error.
        assert!(!may_start(&PointerActivity::Menu, Some(&editor), &terminal));
        assert!(!may_start(&PointerActivity::Free, Some(&editor), &terminal));
        assert!(!may_start(
            &PointerActivity::HeldButton(Some(terminal)),
            Some(&editor),
            &terminal
        ));
        // A menu grab whose chain is over, not yet released, yields to any
        // client.
        assert!(may_start(&PointerActivity::Menu, None, &terminal));

        // A move, a resize or a drag keeps the pointer.
        assert!(!may_start(&PointerActivity::Interaction, None, &editor));
        assert!(!may_start(
            &PointerActivity::Interaction,
            Some(&editor),
            &editor
        ));
    }
}

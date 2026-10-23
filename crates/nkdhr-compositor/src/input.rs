//! The input router: the single entry point for every input event.
//!
//! Its order is fixed. While a session lock is in force, the fail-closed
//! lock path handles the event and nothing else runs. Otherwise every key
//! passes the VT-switch intercept, the only compositor shortcut, and every
//! event then takes the default routing: forwarding to the seat, plus the
//! click-to-focus rule. A future interaction layer handles events between
//! the intercept and the default routing (see `docs/compositor/EXTENDING.md`);
//! the lock path and the VT intercept always run before it.
//!
//! The router consults nothing but the seat, the window store, the output
//! layout, the lock state and the open popup-grab chain, which a touch
//! outside it dismisses. It records only which touch points are down and
//! the latest user action of each kind, with the client it reached.

use std::collections::HashMap;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
    GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    TouchEvent, TouchSlot,
};
use smithay::input::keyboard::{FilterResult, Keysym, ModifiersState};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent as PointerPinchUpdateEvent,
    GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent as PointerSwipeUpdateEvent, MotionEvent, PointerHandle,
    RelativeMotionEvent,
};
use smithay::input::touch::{DownEvent, MotionEvent as TouchMotionEvent, TouchDownGrab, UpEvent};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Clock, IsAlive, Logical, Monotonic, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::compositor::RegionAttributes;
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};
use smithay::wayland::seat::WaylandFocus;

use crate::outputs::OutputLayout;
use crate::state::{App, KeyboardFocusTarget};
use crate::windows::ManagedWindow;

/// The surface under a point and that surface's global origin.
type SurfaceFocus = (WlSurface, Point<f64, Logical>);

/// The touch points that are down, each with its last position in the
/// global space.
///
/// Smithay delivers every event of a touch point to the surface it went
/// down on, and while one finger is down it sends further fingers to that
/// surface too. Nothing that began before a session lock may keep reaching
/// a window during it, so the lock cancels every touch sequence in progress
/// ([`cancel_touch_sequences`]), and for that it needs to know which points
/// are down. Smithay keeps that list privately, so the router keeps its own.
#[derive(Debug, Default)]
pub struct TouchPoints(HashMap<TouchSlot, Point<f64, Logical>>);

/// The latest user action of each kind (key press, key release, button
/// press and touch-down): its serial and the client it was delivered to.
///
/// `xdg_popup.grab` must name the serial of a user action the popup's
/// client received, and toolkits name the latest one they saw, often once
/// the action is over. GTK opens a menu button's popover when the button
/// is released or the finger lifted, naming the press or touch-down; a
/// slow client's grab can arrive after the release even for a menu it
/// opened on the press; a menu opened from the keyboard (the Menu key,
/// Shift+F10, an accelerator) names the latest key press or release.
/// Smithay can only tell whether a serial belongs to the pointer or touch
/// grab active now, which the release or the lift has already ended, so
/// the router keeps this record, as Mutter does.
///
/// Each kind keeps only its latest action. An action that reached no
/// client (a press during a compositor move, an intercepted VT key) still
/// replaces the previous one, so an older serial can never take a grab
/// again. Generic over the client identity only so that the rule is
/// testable without a display.
#[derive(Debug, Clone)]
pub struct UserActionSerials<C = ClientId> {
    key_press: Option<UserAction<C>>,
    key_release: Option<UserAction<C>>,
    button_press: Option<UserAction<C>>,
    touch_down: Option<UserAction<C>>,
}

#[derive(Debug, Clone)]
struct UserAction<C> {
    serial: Serial,
    /// The client the action was delivered to, if any.
    client: Option<C>,
}

impl<C> Default for UserActionSerials<C> {
    fn default() -> Self {
        Self {
            key_press: None,
            key_release: None,
            button_press: None,
            touch_down: None,
        }
    }
}

impl<C: PartialEq> UserActionSerials<C> {
    fn record_key(&mut self, serial: Serial, state: KeyState, client: Option<C>) {
        let action = Some(UserAction { serial, client });
        match state {
            KeyState::Pressed => self.key_press = action,
            KeyState::Released => self.key_release = action,
        }
    }

    fn record_button_press(&mut self, serial: Serial, client: Option<C>) {
        self.button_press = Some(UserAction { serial, client });
    }

    fn record_touch_down(&mut self, serial: Serial, client: Option<C>) {
        self.touch_down = Some(UserAction { serial, client });
    }

    /// Whether `serial` is the latest action of its kind and was delivered
    /// to `client`.
    pub fn delivered_to(&self, serial: Serial, client: &C) -> bool {
        [
            &self.key_press,
            &self.key_release,
            &self.button_press,
            &self.touch_down,
        ]
        .into_iter()
        .flatten()
        .any(|action| action.serial == serial && action.client.as_ref() == Some(client))
    }
}

/// The client that owns `surface`, while it is connected.
pub fn client_of(surface: &WlSurface) -> Option<ClientId> {
    surface.client().map(|client| client.id())
}

/// The timestamp of an input event the compositor synthesizes, such as the
/// focus changes and cancellations at a session lock: the monotonic clock in
/// milliseconds, the time base of libinput's events and of the events
/// Smithay synthesizes itself.
pub fn synthetic_event_time() -> u32 {
    Clock::<Monotonic>::new().now().as_millis()
}

/// Routes one input event. The TTY backend passes every libinput event
/// except device addition and removal; the nested backend passes winit's
/// input events.
pub fn handle<B: InputBackend>(app: &mut App, layout: &OutputLayout, event: InputEvent<B>) {
    if app.session_locked() {
        handle_locked(app, layout, event);
        return;
    }

    let pointer = pointer(app);
    match event {
        InputEvent::Keyboard { event } => keyboard::<B>(app, event),
        InputEvent::PointerMotion { event } => {
            let target = layout.clamp(pointer.current_location() + event.delta());
            let relative = RelativeMotionEvent {
                delta: event.delta(),
                delta_unaccel: event.delta_unaccel(),
                utime: event.time(),
            };
            pointer_motion(app, &pointer, target, event.time_msec(), Some(relative));
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let target = layout.clamp(absolute_position(layout, &event));
            pointer_motion(app, &pointer, target, event.time_msec(), None);
        }
        InputEvent::PointerButton { event } => {
            if event.state() == ButtonState::Pressed && !pointer.is_grabbed() {
                click_to_focus(app);
                // The press goes to the surface holding pointer focus, which
                // only motion updates. The scene may have changed under a
                // pointer that has not moved since (a client resized, a popup
                // appeared), so focus is recomputed before the press is
                // forwarded: it reaches exactly the surface the focus rule
                // hit-tested.
                refresh_pointer_focus(app);
            }
            forward_button::<B>(app, &pointer, &event);
        }
        InputEvent::PointerAxis { event } => forward_axis::<B>(app, &pointer, &event),
        InputEvent::TouchDown { event } => {
            let location = event.position_transformed(layout.bounding_size());
            let focus = surface_under(app, location);
            dismiss_popup_grab_outside_touch(app, focus.as_ref().map(|(surface, _)| surface));
            touch_down::<B>(app, focus, location, &event);
        }
        InputEvent::TouchMotion { event } => {
            let location = event.position_transformed(layout.bounding_size());
            let focus = surface_under(app, location);
            touch_motion::<B>(app, focus, location, &event);
        }
        event => forward_common(app, &pointer, event),
    }
}

/// Dismisses the open popup-grab chain when a touch starts outside it, as a
/// click outside does through Smithay's `PopupPointerGrab`; Smithay has no
/// touch counterpart, and without this a menu stays open under every touch
/// elsewhere. The touch itself is then delivered as usual.
///
/// Only a touch that starts a new touch sequence decides: while a finger is
/// down, Smithay sends further fingers to the surface the first went down
/// on, whatever is under them ([`touch_dismisses_popup_chain`]).
fn dismiss_popup_grab_outside_touch(app: &mut App, touched: Option<&WlSurface>) {
    let touch = app.seat.get_touch().expect("App::new always creates touch");
    if touch.is_grabbed() {
        return;
    }
    let touched = touched.and_then(client_of);
    if touch_dismisses_popup_chain(app.popup_chain_owner().as_ref(), touched.as_ref()) {
        app.dismiss_popup_grab();
    }
}

/// Whether a touch that starts a new touch sequence on a surface of
/// `touched`, or on no surface, dismisses the popup-grab chain open for
/// `chain_owner`: it does unless it lands on one of that client's surfaces,
/// its popups or its windows, as Smithay's `PopupPointerGrab` treats a
/// click.
fn touch_dismisses_popup_chain<C: PartialEq>(chain_owner: Option<&C>, touched: Option<&C>) -> bool {
    chain_owner.is_some_and(|owner| touched != Some(owner))
}

/// Recomputes pointer focus where the pointer already is: a motion of zero
/// distance towards the surface now under it, with the same constraint
/// handling as real motion.
///
/// Pointer focus otherwise changes only when the pointer moves, so every
/// change to what lies under a stationary pointer (a window placed, raised,
/// unmapped or relocated, the output layout replaced, the session unlocked)
/// is followed by this. It does nothing while the session is locked, where
/// the lock path owns pointer focus, or while a pointer grab is active,
/// which decides focus itself. It must not be called from inside a pointer
/// grab's own callbacks, which run while Smithay holds the pointer.
pub fn refresh_pointer_focus(app: &mut App) {
    if app.session_locked() {
        return;
    }
    let pointer = pointer(app);
    if pointer.is_grabbed() {
        return;
    }
    let location = pointer.current_location();
    pointer_motion(app, &pointer, location, synthetic_event_time(), None);
}

/// The fail-closed path while the session is locked. Input reaches lock
/// surfaces only; the window store is never consulted. Keyboard input still
/// passes the VT-switch intercept and then goes to the focused surface,
/// which can only be a lock surface.
fn handle_locked<B: InputBackend>(app: &mut App, layout: &OutputLayout, event: InputEvent<B>) {
    let pointer = pointer(app);
    match event {
        InputEvent::Keyboard { event } => keyboard::<B>(app, event),
        InputEvent::PointerMotion { event } => {
            let target = layout.clamp(pointer.current_location() + event.delta());
            locked_pointer_motion(app, layout, &pointer, target, event.time_msec());
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let target = layout.clamp(absolute_position(layout, &event));
            locked_pointer_motion(app, layout, &pointer, target, event.time_msec());
        }
        InputEvent::PointerButton { event } => {
            // A click moves keyboard focus to the lock surface under the
            // pointer, which is how a multi-output locker is typed into.
            if event.state() == ButtonState::Pressed
                && let Some(surface) = pointer
                    .current_focus()
                    .filter(|surface| surface.alive() && app.protocols.is_lock_surface(surface))
            {
                app.set_keyboard_focus(Some(KeyboardFocusTarget::Wayland(surface)));
            }
            forward_button::<B>(app, &pointer, &event);
        }
        InputEvent::PointerAxis { event } => forward_axis::<B>(app, &pointer, &event),
        InputEvent::TouchDown { event } => {
            let location = event.position_transformed(layout.bounding_size());
            let focus = lock_surface_at(app, layout, location);
            touch_down::<B>(app, focus, location, &event);
        }
        InputEvent::TouchMotion { event } => {
            let location = event.position_transformed(layout.bounding_size());
            let focus = lock_surface_at(app, layout, location);
            touch_motion::<B>(app, focus, location, &event);
        }
        event => forward_common(app, &pointer, event),
    }
}

fn pointer(app: &App) -> PointerHandle<App> {
    app.seat
        .get_pointer()
        .expect("App::new always creates a pointer")
}

/// Absolute devices (touchscreens, tablets, the nested window) map across
/// the bounding box of the whole layout, not onto a single output.
fn absolute_position<B: InputBackend, E: AbsolutePositionEvent<B>>(
    layout: &OutputLayout,
    event: &E,
) -> Point<f64, Logical> {
    event.position_transformed(layout.bounding_size())
}

/// Every key passes through the VT-switch intercept, then reaches the
/// focused surface; the key event is recorded as the latest user action of
/// its kind with the client that received it, none if it was intercepted.
fn keyboard<B: InputBackend>(app: &mut App, event: B::KeyboardKeyEvent) {
    let keyboard = app
        .seat
        .get_keyboard()
        .expect("App::new always creates a keyboard");
    let pressed = event.state() == KeyState::Pressed;
    let serial = SERIAL_COUNTER.next_serial();
    let intercepted = keyboard.input::<(), _>(
        app,
        event.key_code(),
        event.state(),
        serial,
        event.time_msec(),
        move |app, modifiers, keysym| {
            if intercept_vt_switch(
                app,
                modifiers,
                keysym.modified_sym(),
                &keysym.raw_syms(),
                pressed,
            ) {
                FilterResult::Intercept(())
            } else {
                FilterResult::Forward
            }
        },
    );
    // Smithay's popup keyboard grab moves focus to the topmost popup before
    // it forwards the key, so the focus after `input` is the receiver.
    let receiver = intercepted
        .is_none()
        .then(|| keyboard.current_focus())
        .flatten()
        .and_then(|focus| focus.wl_surface().as_deref().and_then(client_of));
    app.user_actions.record_key(serial, event.state(), receiver);
}

/// Reserves the VT-switch keys when switching is enabled (TTY backend
/// only): the press records the request, and both press and release are
/// kept from applications. With switching disabled (the nested backend),
/// the keys reach applications like any others.
fn intercept_vt_switch(
    app: &mut App,
    modifiers: &ModifiersState,
    modified_sym: Keysym,
    raw_syms: &[Keysym],
    pressed: bool,
) -> bool {
    if !app.vt_switching_enabled() {
        return false;
    }
    let Some(vt) = vt_switch_target(modifiers, modified_sym, raw_syms) else {
        return false;
    };
    if pressed {
        app.request_vt_switch(vt);
    }
    true
}

/// The virtual terminal a key selects, if it is a VT-switch key.
///
/// XKB may resolve `Ctrl+Alt+Fn` directly to an `XF86Switch_VT_n` keysym, so
/// that keysym is accepted from the modified symbol, and `F1`…`F12` are
/// separately accepted from the raw, unmodified level-zero symbols while
/// Ctrl and Alt are held. Relying on the modified symbol alone made the
/// first physical test silently miss the chord.
fn vt_switch_target(
    modifiers: &ModifiersState,
    modified_sym: Keysym,
    raw_syms: &[Keysym],
) -> Option<i32> {
    let chord_vt = (modifiers.ctrl && modifiers.alt)
        .then(|| raw_syms.iter().find_map(|sym| function_key_vt_number(*sym)))
        .flatten();
    xf86_vt_number(modified_sym).or(chord_vt)
}

fn function_key_vt_number(sym: Keysym) -> Option<i32> {
    match sym {
        Keysym::F1 => Some(1),
        Keysym::F2 => Some(2),
        Keysym::F3 => Some(3),
        Keysym::F4 => Some(4),
        Keysym::F5 => Some(5),
        Keysym::F6 => Some(6),
        Keysym::F7 => Some(7),
        Keysym::F8 => Some(8),
        Keysym::F9 => Some(9),
        Keysym::F10 => Some(10),
        Keysym::F11 => Some(11),
        Keysym::F12 => Some(12),
        _ => None,
    }
}

fn xf86_vt_number(sym: Keysym) -> Option<i32> {
    match sym {
        Keysym::XF86_Switch_VT_1 => Some(1),
        Keysym::XF86_Switch_VT_2 => Some(2),
        Keysym::XF86_Switch_VT_3 => Some(3),
        Keysym::XF86_Switch_VT_4 => Some(4),
        Keysym::XF86_Switch_VT_5 => Some(5),
        Keysym::XF86_Switch_VT_6 => Some(6),
        Keysym::XF86_Switch_VT_7 => Some(7),
        Keysym::XF86_Switch_VT_8 => Some(8),
        Keysym::XF86_Switch_VT_9 => Some(9),
        Keysym::XF86_Switch_VT_10 => Some(10),
        Keysym::XF86_Switch_VT_11 => Some(11),
        Keysym::XF86_Switch_VT_12 => Some(12),
        _ => None,
    }
}

fn surface_under(app: &App, location: Point<f64, Logical>) -> Option<SurfaceFocus> {
    app.windows
        .surface_under(location)
        .map(|hit| (hit.surface, hit.origin))
}

/// The focus for `surface` at the current pointer position, if `surface` is
/// still the surface under the pointer.
fn focus_if_under_pointer(
    app: &App,
    pointer: &PointerHandle<App>,
    surface: &WlSurface,
) -> Option<SurfaceFocus> {
    surface_under(app, pointer.current_location()).filter(|(candidate, _)| candidate == surface)
}

enum ActiveConstraint {
    Locked,
    Confined(Option<RegionAttributes>),
}

fn active_constraint(
    surface: &WlSurface,
    pointer: &PointerHandle<App>,
) -> Option<ActiveConstraint> {
    with_pointer_constraint(surface, pointer, |constraint| {
        let constraint = constraint.filter(|constraint| constraint.is_active())?;
        Some(match &*constraint {
            PointerConstraint::Locked(_) => ActiveConstraint::Locked,
            PointerConstraint::Confined(confined) => {
                ActiveConstraint::Confined(confined.region().cloned())
            }
        })
    })
}

fn deactivate_constraint(surface: &WlSurface, pointer: &PointerHandle<App>) {
    with_pointer_constraint(surface, pointer, |constraint| {
        if let Some(constraint) = constraint
            && constraint.is_active()
        {
            constraint.deactivate();
        }
    });
}

fn region_contains(region: &RegionAttributes, local: Point<f64, Logical>) -> bool {
    region.contains((local.x.floor() as i32, local.y.floor() as i32))
}

/// Activates `surface`'s pointer constraint if `surface` holds pointer focus,
/// is the surface under the pointer and the pointer is inside the
/// constraint's region (a constraint without a region covers the whole
/// surface).
///
/// Requiring pointer focus keeps constraints inactive on surfaces a grab
/// moves the pointer across without giving them focus: a compositor move
/// or resize clears pointer focus, and Smithay's implicit click grab keeps
/// it on the pressed surface. Requiring the region keeps a confinement from
/// starting outside it, where every motion would be refused. While the
/// session is locked, pointer focus is a lock surface, which the window
/// store's hit test never finds, so nothing is activated; the lock path
/// does not honour constraints.
pub fn activate_pointer_constraint(app: &App, pointer: &PointerHandle<App>, surface: &WlSurface) {
    if pointer.current_focus().as_ref() != Some(surface) {
        return;
    }
    let Some((_, origin)) = focus_if_under_pointer(app, pointer, surface) else {
        return;
    };
    let local = pointer.current_location() - origin;
    with_pointer_constraint(surface, pointer, |constraint| {
        if let Some(constraint) = constraint
            && !constraint.is_active()
            && constraint
                .region()
                .is_none_or(|region| region_contains(region, local))
        {
            constraint.activate();
        }
    });
}

/// Pointer motion towards `target`, already clamped into the union of the
/// outputs. The surface under the target becomes pointer focus, subject to
/// pointer constraints:
///
/// - a constraint is activated only while its surface has pointer focus
///   and the pointer is inside its region ([`activate_pointer_constraint`],
///   after the motion), and deactivated when focus moves to another
///   surface;
/// - a locked pointer does not move; it only produces relative motion;
/// - a confined pointer refuses motion that would leave its surface or
///   region;
/// - either constraint lapses once its surface is no longer under the
///   pointer (it moved, unmapped or was covered).
///
/// Relative events also produce `relative_motion` for
/// `zwp_relative_pointer_v1`, including while the pointer is locked.
fn pointer_motion(
    app: &mut App,
    pointer: &PointerHandle<App>,
    target: Point<f64, Logical>,
    time: u32,
    relative: Option<RelativeMotionEvent>,
) {
    let current_surface = pointer.current_focus();
    let mut focus = surface_under(app, target);
    let mut location = target;

    if let Some(current_surface) = current_surface.as_ref() {
        match active_constraint(current_surface, pointer) {
            Some(ActiveConstraint::Locked) => {
                match focus_if_under_pointer(app, pointer, current_surface) {
                    Some(locked_focus) => {
                        if let Some(relative) = relative {
                            pointer.relative_motion(app, Some(locked_focus), &relative);
                            pointer.frame(app);
                        }
                        return;
                    }
                    None => deactivate_constraint(current_surface, pointer),
                }
            }
            Some(ActiveConstraint::Confined(region)) => {
                match focus_if_under_pointer(app, pointer, current_surface) {
                    Some(confined_focus) => {
                        let stays_on_surface = focus
                            .as_ref()
                            .is_some_and(|(surface, _)| surface == current_surface);
                        let inside_region = region.as_ref().is_none_or(|region| {
                            region_contains(region, location - confined_focus.1)
                        });
                        if !stays_on_surface || !inside_region {
                            location = pointer.current_location();
                            focus = Some(confined_focus);
                        }
                    }
                    None => deactivate_constraint(current_surface, pointer),
                }
            }
            None => {}
        }
    }

    if let Some(current_surface) = current_surface.as_ref()
        && focus.as_ref().map(|(surface, _)| surface) != Some(current_surface)
    {
        deactivate_constraint(current_surface, pointer);
    }

    if let Some(relative) = relative {
        pointer.relative_motion(app, focus.clone(), &relative);
    }
    pointer.motion(
        app,
        focus,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    if let Some(focused) = pointer.current_focus() {
        activate_pointer_constraint(app, pointer, &focused);
    }
    pointer.frame(app);
}

/// The lock surface of the output under `location`, positioned at that
/// output's global location.
fn lock_surface_at(
    app: &App,
    layout: &OutputLayout,
    location: Point<f64, Logical>,
) -> Option<SurfaceFocus> {
    let output = layout.output_at(location)?;
    Some((
        app.lock_surface_for_output(&output.name)?,
        output.global_location.to_f64(),
    ))
}

fn locked_pointer_motion(
    app: &mut App,
    layout: &OutputLayout,
    pointer: &PointerHandle<App>,
    location: Point<f64, Logical>,
    time: u32,
) {
    let focus = lock_surface_at(app, layout, location);
    pointer.motion(
        app,
        focus,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    pointer.frame(app);
}

/// The click-to-focus rule: a press on a placed, managed window focuses and
/// raises it; a press where no window is leaves focus alone. The router
/// skips it while the pointer has an active grab, so a click that dismisses
/// a popup (through Smithay's `PopupPointerGrab`) or ends a move does not
/// also switch windows.
fn click_to_focus(app: &mut App) {
    let Some(hit) = app.windows.surface_under(app.pointer_location()) else {
        return;
    };
    if app
        .windows
        .get(hit.window)
        .is_some_and(ManagedWindow::is_focusable)
    {
        app.focus_window(hit.window);
    }
}

/// Forwards a button event. A press is recorded as the latest button press
/// with the client of the surface that received it: the pointer focus once
/// Smithay has delivered it, which a press that dismisses a popup moves to
/// the surface under the pointer first, and which a move or resize clears.
fn forward_button<B: InputBackend>(
    app: &mut App,
    pointer: &PointerHandle<App>,
    event: &B::PointerButtonEvent,
) {
    let serial = SERIAL_COUNTER.next_serial();
    pointer.button(
        app,
        &ButtonEvent {
            serial,
            time: event.time_msec(),
            button: event.button_code(),
            state: event.state(),
        },
    );
    if event.state() == ButtonState::Pressed {
        let receiver = pointer
            .current_focus()
            .and_then(|surface| client_of(&surface));
        app.user_actions.record_button_press(serial, receiver);
    }
    pointer.frame(app);
}

/// Forwards every axis event, including zero-delta stop frames, which
/// kinetic scrolling in clients depends on.
fn forward_axis<B: InputBackend>(
    app: &mut App,
    pointer: &PointerHandle<App>,
    event: &B::PointerAxisEvent,
) {
    pointer.axis(app, axis_frame::<B>(event));
    pointer.frame(app);
}

/// Logical scroll distance of one wheel notch (120 high-resolution units),
/// matching libinput's convention for devices that report it.
const SCROLL_PER_NOTCH: f64 = 15.0;

/// One axis frame: the source, the relative direction, the continuous
/// value, the high-resolution discrete value and, for finger sources,
/// `stop` on a zero value. A device that reports only discrete steps (a
/// wheel through the nested backend) still gets a continuous value derived
/// from them, since `wl_pointer.axis_value120` must accompany an axis value.
fn axis_frame<B: InputBackend>(event: &B::PointerAxisEvent) -> AxisFrame {
    let source = event.source();
    let mut frame = AxisFrame::new(event.time_msec()).source(source);
    for axis in [Axis::Horizontal, Axis::Vertical] {
        frame = frame.relative_direction(axis, event.relative_direction(axis));
        let v120 = event.amount_v120(axis);
        match event.amount(axis) {
            Some(amount) if source == AxisSource::Finger && amount == 0.0 => {
                frame = frame.stop(axis);
            }
            Some(amount) => frame = frame.value(axis, amount),
            None => {
                if let Some(v120) = v120 {
                    frame = frame.value(axis, v120 * SCROLL_PER_NOTCH / 120.0);
                }
            }
        }
        if let Some(v120) = v120 {
            frame = frame.v120(axis, v120.round() as i32);
        }
    }
    frame
}

/// Forwards a touch-down and records it as the latest touch-down with the
/// client of the surface that received it.
///
/// That surface is the start focus of the touch grab once Smithay has
/// delivered the event: the first finger installs a `TouchDownGrab` on the
/// surface it went down on, and every further finger goes to that same
/// surface. A touch drag-and-drop swallows the event, and nothing receives
/// it.
fn touch_down<B: InputBackend>(
    app: &mut App,
    focus: Option<SurfaceFocus>,
    location: Point<f64, Logical>,
    event: &B::TouchDownEvent,
) {
    app.touch_points.0.insert(event.slot(), location);
    let touch = app.seat.get_touch().expect("App::new always creates touch");
    let serial = SERIAL_COUNTER.next_serial();
    touch.down(
        app,
        focus,
        &DownEvent {
            slot: event.slot(),
            location,
            serial,
            time: event.time_msec(),
        },
    );
    let receiver = touch
        .with_grab(|_, grab| {
            let grab = grab.downcast_ref::<TouchDownGrab<App>>()?;
            let (surface, _) = grab.start_data.focus.as_ref()?;
            client_of(surface)
        })
        .flatten();
    app.user_actions.record_touch_down(serial, receiver);
}

fn touch_motion<B: InputBackend>(
    app: &mut App,
    focus: Option<SurfaceFocus>,
    location: Point<f64, Logical>,
    event: &B::TouchMotionEvent,
) {
    app.touch_points.0.insert(event.slot(), location);
    let touch = app.seat.get_touch().expect("App::new always creates touch");
    touch.motion(
        app,
        focus,
        &TouchMotionEvent {
            slot: event.slot(),
            location,
            time: event.time_msec(),
        },
    );
}

/// The events both paths forward unchanged: gestures go to the pointer's
/// focus (the `zwp_pointer_gestures_v1` global delivers them to clients),
/// and the remaining touch events go to the surfaces their touch points
/// went down on. Touch never focuses or raises a window. Events
/// the compositor does not use (device changes, tablets, switches) are
/// dropped.
fn forward_common<B: InputBackend>(
    app: &mut App,
    pointer: &PointerHandle<App>,
    event: InputEvent<B>,
) {
    let touch = app.seat.get_touch().expect("App::new always creates touch");
    match event {
        InputEvent::GestureSwipeBegin { event } => pointer.gesture_swipe_begin(
            app,
            &GestureSwipeBeginEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: event.time_msec(),
                fingers: event.fingers(),
            },
        ),
        InputEvent::GestureSwipeUpdate { event } => pointer.gesture_swipe_update(
            app,
            &PointerSwipeUpdateEvent {
                time: event.time_msec(),
                delta: event.delta(),
            },
        ),
        InputEvent::GestureSwipeEnd { event } => pointer.gesture_swipe_end(
            app,
            &GestureSwipeEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: event.time_msec(),
                cancelled: event.cancelled(),
            },
        ),
        InputEvent::GesturePinchBegin { event } => pointer.gesture_pinch_begin(
            app,
            &GesturePinchBeginEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: event.time_msec(),
                fingers: event.fingers(),
            },
        ),
        InputEvent::GesturePinchUpdate { event } => pointer.gesture_pinch_update(
            app,
            &PointerPinchUpdateEvent {
                time: event.time_msec(),
                delta: event.delta(),
                scale: event.scale(),
                rotation: event.rotation(),
            },
        ),
        InputEvent::GesturePinchEnd { event } => pointer.gesture_pinch_end(
            app,
            &GesturePinchEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: event.time_msec(),
                cancelled: event.cancelled(),
            },
        ),
        InputEvent::GestureHoldBegin { event } => pointer.gesture_hold_begin(
            app,
            &GestureHoldBeginEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: event.time_msec(),
                fingers: event.fingers(),
            },
        ),
        InputEvent::GestureHoldEnd { event } => pointer.gesture_hold_end(
            app,
            &GestureHoldEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: event.time_msec(),
                cancelled: event.cancelled(),
            },
        ),
        InputEvent::TouchUp { event } => {
            app.touch_points.0.remove(&event.slot());
            touch.up(
                app,
                &UpEvent {
                    slot: event.slot(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchFrame { .. } => touch.frame(app),
        InputEvent::TouchCancel { .. } => {
            app.touch_points.0.clear();
            touch.cancel(app);
        }
        _ => {}
    }
}

/// Cancels every touch sequence in progress; the session lock calls it so
/// that no touch that began before the lock reaches a window during it.
///
/// A touch drag-and-drop first leaves its target, so that ending its grab
/// cancels the drag instead of dropping into a window. Then the touch grab
/// ends, which stops further fingers from following the first one to its
/// surface, and every surface with touch points receives `wl_touch.cancel`.
/// The points stay down but reach nothing until they are lifted; a new
/// touch starts a new sequence on whatever is under it. Smithay's cancel
/// only reaches points with an event newer than the last frame, so each
/// point first receives a motion to where it already is.
pub fn cancel_touch_sequences(app: &mut App) {
    let touch = app.seat.get_touch().expect("App::new always creates touch");
    let time = synthetic_event_time();
    if let Some(start) = touch.grab_start_data() {
        let location = app
            .touch_points
            .0
            .get(&start.slot)
            .copied()
            .unwrap_or(start.location);
        touch.motion(
            app,
            None,
            &TouchMotionEvent {
                slot: start.slot,
                location,
                time,
            },
        );
    }
    touch.unset_grab(app);
    let points = app
        .touch_points
        .0
        .iter()
        .map(|(slot, location)| (*slot, *location))
        .collect::<Vec<_>>();
    for (slot, location) in points {
        touch.motion(
            app,
            None,
            &TouchMotionEvent {
                slot,
                location,
                time,
            },
        );
    }
    touch.cancel(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_action_serials_keep_the_latest_action_of_each_kind() {
        let (editor, terminal) = ("editor", "terminal");
        let mut actions = UserActionSerials::default();
        assert!(!actions.delivered_to(Serial::from(1), &editor));

        actions.record_key(Serial::from(1), KeyState::Pressed, Some(editor));
        actions.record_key(Serial::from(2), KeyState::Released, Some(editor));
        actions.record_button_press(Serial::from(3), Some(editor));
        actions.record_touch_down(Serial::from(4), Some(terminal));
        assert!(actions.delivered_to(Serial::from(1), &editor));
        assert!(actions.delivered_to(Serial::from(2), &editor));
        assert!(actions.delivered_to(Serial::from(4), &terminal));

        // A button press stays valid after its release and after actions
        // of other kinds: a popover opened on release names the press.
        actions.record_key(Serial::from(5), KeyState::Pressed, Some(terminal));
        assert!(actions.delivered_to(Serial::from(3), &editor));

        // Only the client that received an action may name it.
        assert!(!actions.delivered_to(Serial::from(3), &terminal));
        assert!(!actions.delivered_to(Serial::from(4), &editor));

        // A later action of the same kind replaces the earlier one, even
        // one that reached no client.
        assert!(!actions.delivered_to(Serial::from(1), &editor));
        actions.record_button_press(Serial::from(6), None);
        assert!(!actions.delivered_to(Serial::from(3), &editor));
        assert!(!actions.delivered_to(Serial::from(6), &editor));
        actions.record_touch_down(Serial::from(7), Some(editor));
        assert!(!actions.delivered_to(Serial::from(4), &terminal));
        assert!(actions.delivered_to(Serial::from(7), &editor));

        // The latest key release stays valid until the next release.
        assert!(actions.delivered_to(Serial::from(2), &editor));
    }

    #[test]
    fn a_touch_outside_the_open_menu_client_dismisses_its_chain() {
        let (editor, terminal) = ("editor", "terminal");

        // Inside the client that holds the chain: its popups or windows.
        assert!(!touch_dismisses_popup_chain(Some(&editor), Some(&editor)));
        // Another client's surface, or no surface at all.
        assert!(touch_dismisses_popup_chain(Some(&editor), Some(&terminal)));
        assert!(touch_dismisses_popup_chain(Some(&editor), None));
        // With no chain open there is nothing to dismiss.
        assert!(!touch_dismisses_popup_chain(None, Some(&terminal)));
        assert!(!touch_dismisses_popup_chain::<&str>(None, None));
    }

    #[test]
    fn maps_function_keys_to_linux_vts() {
        for (sym, vt) in [
            (Keysym::F1, 1),
            (Keysym::F2, 2),
            (Keysym::F3, 3),
            (Keysym::F4, 4),
            (Keysym::F5, 5),
            (Keysym::F6, 6),
            (Keysym::F7, 7),
            (Keysym::F8, 8),
            (Keysym::F9, 9),
            (Keysym::F10, 10),
            (Keysym::F11, 11),
            (Keysym::F12, 12),
        ] {
            assert_eq!(function_key_vt_number(sym), Some(vt));
        }
        assert_eq!(function_key_vt_number(Keysym::Escape), None);
    }

    #[test]
    fn maps_dedicated_vt_keysyms_to_linux_vts() {
        for (sym, vt) in [
            (Keysym::XF86_Switch_VT_1, 1),
            (Keysym::XF86_Switch_VT_2, 2),
            (Keysym::XF86_Switch_VT_7, 7),
            (Keysym::XF86_Switch_VT_12, 12),
        ] {
            assert_eq!(xf86_vt_number(sym), Some(vt));
        }
        assert_eq!(xf86_vt_number(Keysym::F2), None);
    }

    #[test]
    fn only_ctrl_alt_function_keys_and_dedicated_keysyms_switch_vts() {
        let ctrl_alt = ModifiersState {
            ctrl: true,
            alt: true,
            ..ModifiersState::default()
        };
        let ctrl_only = ModifiersState {
            ctrl: true,
            ..ModifiersState::default()
        };
        let none = ModifiersState::default();

        // XKB resolved the chord to the dedicated keysym.
        assert_eq!(
            vt_switch_target(&ctrl_alt, Keysym::XF86_Switch_VT_3, &[Keysym::F3]),
            Some(3)
        );
        // XKB left the function key alone: the raw symbol decides.
        assert_eq!(
            vt_switch_target(&ctrl_alt, Keysym::F3, &[Keysym::F3]),
            Some(3)
        );
        // A layout that produces the dedicated keysym without the chord.
        assert_eq!(
            vt_switch_target(&none, Keysym::XF86_Switch_VT_12, &[Keysym::F12]),
            Some(12)
        );
        // Function keys without both Ctrl and Alt belong to applications.
        assert_eq!(vt_switch_target(&none, Keysym::F3, &[Keysym::F3]), None);
        assert_eq!(
            vt_switch_target(&ctrl_only, Keysym::F3, &[Keysym::F3]),
            None
        );
        // Other keys never switch, whatever the modifiers.
        assert_eq!(
            vt_switch_target(&ctrl_alt, Keysym::Delete, &[Keysym::Delete]),
            None
        );
    }
}

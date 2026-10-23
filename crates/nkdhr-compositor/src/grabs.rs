//! Client-initiated interactive move and resize.
//!
//! A client asks to be moved or resized from its own title bar or edges
//! (`xdg_toplevel.move`/`resize`, X11 `_NET_WM_MOVERESIZE`); the compositor
//! carries the request out as a Smithay `PointerGrab`. While a grab runs no
//! surface has pointer focus, every other pointer event passes through, and
//! releasing the last button ends it and gives pointer focus to whatever is
//! under the pointer then. Implementing compositor-owned pointer
//! interactions as grabs is what lets grab start, cancellation and the
//! session lock treat them uniformly.

use smithay::input::pointer::{
    AxisFrame, ButtonEvent, ClickGrab, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
    GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab,
    PointerInnerHandle, RelativeMotionEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle, Serial, Size};
use smithay::xwayland::xwm;

use crate::state::App;
use crate::windows::{ManagedWindow, WindowId};

/// The window edges a resize moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResizeEdges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl ResizeEdges {
    /// `None` for `ResizeEdge::None` and for values a newer protocol
    /// version might add.
    pub fn from_xdg(edge: xdg_toplevel::ResizeEdge) -> Option<Self> {
        use xdg_toplevel::ResizeEdge;
        let edges = |top, bottom, left, right| {
            Some(Self {
                top,
                bottom,
                left,
                right,
            })
        };
        match edge {
            ResizeEdge::Top => edges(true, false, false, false),
            ResizeEdge::Bottom => edges(false, true, false, false),
            ResizeEdge::Left => edges(false, false, true, false),
            ResizeEdge::Right => edges(false, false, false, true),
            ResizeEdge::TopLeft => edges(true, false, true, false),
            ResizeEdge::TopRight => edges(true, false, false, true),
            ResizeEdge::BottomLeft => edges(false, true, true, false),
            ResizeEdge::BottomRight => edges(false, true, false, true),
            _ => None,
        }
    }

    pub fn from_x11(edge: xwm::ResizeEdge) -> Self {
        use xwm::ResizeEdge;
        Self {
            top: matches!(
                edge,
                ResizeEdge::Top | ResizeEdge::TopLeft | ResizeEdge::TopRight
            ),
            bottom: matches!(
                edge,
                ResizeEdge::Bottom | ResizeEdge::BottomLeft | ResizeEdge::BottomRight
            ),
            left: matches!(
                edge,
                ResizeEdge::Left | ResizeEdge::TopLeft | ResizeEdge::BottomLeft
            ),
            right: matches!(
                edge,
                ResizeEdge::Right | ResizeEdge::TopRight | ResizeEdge::BottomRight
            ),
        }
    }
}

/// What a resize started from: the edges it moves and the window's location
/// and geometry size when the grab began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeData {
    edges: ResizeEdges,
    initial_location: Point<i32, Logical>,
    initial_size: Size<i32, Logical>,
}

impl ResizeData {
    /// The size the pointer's offset from the grab start asks for, clamped
    /// to the client's size hints (`0` meaning unconstrained) and to at
    /// least 1×1.
    fn size_for(
        &self,
        offset: Point<i32, Logical>,
        (min, max): (Size<i32, Logical>, Size<i32, Logical>),
    ) -> Size<i32, Logical> {
        let mut width = self.initial_size.w;
        let mut height = self.initial_size.h;
        if self.edges.left {
            width = width.saturating_sub(offset.x);
        } else if self.edges.right {
            width = width.saturating_add(offset.x);
        }
        if self.edges.top {
            height = height.saturating_sub(offset.y);
        } else if self.edges.bottom {
            height = height.saturating_add(offset.y);
        }
        (
            clamp_axis(width, min.w, max.w),
            clamp_axis(height, min.h, max.h),
        )
            .into()
    }

    /// The location that keeps the edge opposite a top or left resize where
    /// it was when the grab began, for a window that now has `size`. Axes
    /// the resize does not move from the start keep `current`.
    fn location_for(
        &self,
        current: Point<i32, Logical>,
        size: Size<i32, Logical>,
    ) -> Point<i32, Logical> {
        let mut location = current;
        if self.edges.left {
            location.x = self
                .initial_location
                .x
                .saturating_add(self.initial_size.w.saturating_sub(size.w));
        }
        if self.edges.top {
            location.y = self
                .initial_location
                .y
                .saturating_add(self.initial_size.h.saturating_sub(size.h));
        }
        location
    }

    fn moves_origin(&self) -> bool {
        self.edges.top || self.edges.left
    }
}

fn clamp_axis(value: i32, min: i32, max: i32) -> i32 {
    let mut value = value;
    if max > 0 {
        value = value.min(max);
    }
    if min > 0 {
        value = value.max(min);
    }
    value.max(1)
}

/// The resize bookkeeping an xdg toplevel carries between the grab and the
/// client's commits. The client answers each configure with a commit of its
/// own size, possibly later; the location is corrected on those commits, so
/// a top or left resize never shows the window jumping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeState {
    #[default]
    Idle,
    /// A resize grab is running.
    Resizing(ResizeData),
    /// The grab ended and the final configure, without the `Resizing`
    /// state, carries this serial; the client has not acknowledged it yet.
    WaitingForFinalAck(ResizeData, Serial),
    /// The client acknowledged the final configure; its next commit is the
    /// last one the resize adjusts.
    WaitingForCommit(ResizeData),
}

impl ResizeState {
    /// Advances the state for a buffer commit. Returns the resize whose
    /// fixed edge this commit must keep in place, if one is in progress.
    fn commit(&mut self) -> Option<ResizeData> {
        match *self {
            Self::Idle => None,
            Self::Resizing(data) | Self::WaitingForFinalAck(data, _) => Some(data),
            Self::WaitingForCommit(data) => {
                *self = Self::Idle;
                Some(data)
            }
        }
    }

    /// Advances the state for an acknowledged configure: once the final
    /// configure (or a later one) is acknowledged, the next commit is the
    /// last one the resize adjusts.
    fn acknowledge(&mut self, serial: Serial) {
        if let Self::WaitingForFinalAck(data, final_serial) = *self
            && serial >= final_serial
        {
            *self = Self::WaitingForCommit(data);
        }
    }

    /// The state once the resize grab for `data` has ended and its final
    /// configure, carrying `final_serial`, has been sent.
    ///
    /// The correction carries on to the client's answer only while the
    /// grab's own resize is still in progress. A move during the grab (the
    /// keep-reachable rule relocating the window, for example) has already
    /// cancelled it, and re-arming it would put the window back relative to
    /// where the resize began; any other state belongs to someone else and
    /// is kept.
    fn grab_ended(self, data: ResizeData, final_serial: Serial) -> Self {
        match self {
            Self::Resizing(current) if current == data => {
                Self::WaitingForFinalAck(data, final_serial)
            }
            other => other,
        }
    }

    /// Whether the window's location is still anchored to where the resize
    /// for `data` began.
    fn anchors(self, data: ResizeData) -> bool {
        self == Self::Resizing(data)
    }
}

/// Called for every buffer commit of a placed xdg toplevel. Returns the
/// location that keeps the opposite edge of a top or left resize in place,
/// if one is in progress, and advances the window's resize state.
pub fn commit_resize(window: &mut ManagedWindow) -> Option<Point<i32, Logical>> {
    let mut state = window.resize_state();
    let data = state.commit();
    window.set_resize_state(state);
    let data = data?;
    let current = window.location()?;
    data.moves_origin()
        .then(|| data.location_for(current, window.geometry().size))
}

/// Called when an xdg toplevel acknowledges a configure: once the final
/// configure of a resize is acknowledged, the next commit ends the resize.
pub fn ack_resize_configure(window: &mut ManagedWindow, serial: Serial) {
    let mut state = window.resize_state();
    state.acknowledge(serial);
    window.set_resize_state(state);
}

/// The start data of the pointer grab a move or resize builds on, if the
/// request may be honoured: the session is unlocked, the window is placed
/// and managed, and the pointer's active grab is Smithay's implicit
/// `ClickGrab` (a button held since a press) that started on this window
/// (its root surface, a subsurface or a popup).
///
/// Only the implicit grab carries the press a move or resize continues. A
/// request repeated while this module's own `MoveGrab` or `ResizeGrab` runs
/// shares that grab's serial but would pair the already-moved window with
/// the original press location and apply the offset twice; a popup grab
/// reports the global origin as its start location. Both are refused, and
/// so is a request started by touch, which has no pointer grab at all.
fn grab_start(app: &App, id: WindowId) -> Option<GrabStartData<App>> {
    if app.session_locked() {
        return None;
    }
    let window = app.windows.get(id).filter(|window| window.is_focusable())?;
    let pointer = app.seat.get_pointer()?;
    let start = pointer
        .with_grab(|_, grab| {
            grab.is::<ClickGrab<App>>()
                .then(|| grab.start_data().clone())
        })
        .flatten()?;
    let (focus, _) = start.focus.as_ref()?;
    window.matches_surface(focus).then_some(start)
}

/// Starts moving a window. The caller has validated that the request
/// belongs to the pointer grab `serial` identifies.
pub fn begin_move(app: &mut App, id: WindowId, serial: Serial) {
    let Some(start_data) = grab_start(app, id) else {
        return;
    };
    let Some(initial_location) = app.windows.get(id).and_then(ManagedWindow::location) else {
        return;
    };
    let Some(pointer) = app.seat.get_pointer() else {
        return;
    };
    let grab = MoveGrab {
        start_data,
        window: id,
        initial_location,
    };
    pointer.set_grab(app, grab, serial, Focus::Clear);
}

/// Starts resizing a window from `edges`. The caller has validated that
/// the request belongs to the pointer grab `serial` identifies.
pub fn begin_resize(app: &mut App, id: WindowId, edges: ResizeEdges, serial: Serial) {
    let Some(start_data) = grab_start(app, id) else {
        return;
    };
    let Some(window) = app.windows.get_mut(id) else {
        return;
    };
    let Some(initial_location) = window.location() else {
        return;
    };
    let data = ResizeData {
        edges,
        initial_location,
        initial_size: window.geometry().size,
    };
    if let Some(toplevel) = window.window().toplevel() {
        // Sent together with the first size request.
        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
        });
    }
    window.set_resize_state(ResizeState::Resizing(data));
    let Some(pointer) = app.seat.get_pointer() else {
        return;
    };
    let grab = ResizeGrab {
        start_data,
        window: id,
        data,
        last_size: data.initial_size,
    };
    pointer.set_grab(app, grab, serial, Focus::Clear);
}

/// The pointer offset since the grab started, in whole logical pixels.
fn offset(start: &GrabStartData<App>, location: Point<f64, Logical>) -> Point<i32, Logical> {
    let offset = location - start.location;
    (offset.x.round() as i32, offset.y.round() as i32).into()
}

/// The [`PointerGrab`] methods both grabs pass through unchanged. Pointer
/// focus is cleared while a grab runs, so these reach no client.
macro_rules! pass_through_pointer_events {
    () => {
        fn relative_motion(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            _focus: Option<(WlSurface, Point<f64, Logical>)>,
            event: &RelativeMotionEvent,
        ) {
            handle.relative_motion(app, None, event);
        }

        fn axis(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            details: AxisFrame,
        ) {
            handle.axis(app, details);
        }

        fn frame(&mut self, app: &mut App, handle: &mut PointerInnerHandle<'_, App>) {
            handle.frame(app);
        }

        fn gesture_swipe_begin(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GestureSwipeBeginEvent,
        ) {
            handle.gesture_swipe_begin(app, event);
        }

        fn gesture_swipe_update(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GestureSwipeUpdateEvent,
        ) {
            handle.gesture_swipe_update(app, event);
        }

        fn gesture_swipe_end(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GestureSwipeEndEvent,
        ) {
            handle.gesture_swipe_end(app, event);
        }

        fn gesture_pinch_begin(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GesturePinchBeginEvent,
        ) {
            handle.gesture_pinch_begin(app, event);
        }

        fn gesture_pinch_update(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GesturePinchUpdateEvent,
        ) {
            handle.gesture_pinch_update(app, event);
        }

        fn gesture_pinch_end(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GesturePinchEndEvent,
        ) {
            handle.gesture_pinch_end(app, event);
        }

        fn gesture_hold_begin(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GestureHoldBeginEvent,
        ) {
            handle.gesture_hold_begin(app, event);
        }

        fn gesture_hold_end(
            &mut self,
            app: &mut App,
            handle: &mut PointerInnerHandle<'_, App>,
            event: &GestureHoldEndEvent,
        ) {
            handle.gesture_hold_end(app, event);
        }

        fn start_data(&self) -> &GrabStartData<App> {
            &self.start_data
        }
    };
}

/// Ends a grab when the last pressed button is released.
fn end_on_release(
    grab: &mut dyn PointerGrab<App>,
    app: &mut App,
    handle: &mut PointerInnerHandle<'_, App>,
    event: &ButtonEvent,
) {
    handle.button(app, event);
    if handle.current_pressed().is_empty() {
        end_grab(grab, app, handle, event.serial, event.time);
    }
}

/// Ends a grab and gives pointer focus to the surface under the pointer.
///
/// Smithay's own focus restore would replay the focus the input router
/// computed for the last motion before the grab moved or resized the window
/// under it, which can be the surface that was behind the window's old
/// position.
fn end_grab(
    grab: &mut dyn PointerGrab<App>,
    app: &mut App,
    handle: &mut PointerInnerHandle<'_, App>,
    serial: Serial,
    time: u32,
) {
    handle.unset_grab(grab, app, serial, time, false);
    let location = handle.current_location();
    let focus = app
        .windows
        .surface_under(location)
        .map(|hit| (hit.surface, hit.origin));
    handle.motion(
        app,
        focus,
        &MotionEvent {
            location,
            serial,
            time,
        },
    );
}

/// Moves a window with the pointer: its location is the location it had
/// when the grab began plus the pointer's offset since then.
pub struct MoveGrab {
    start_data: GrabStartData<App>,
    window: WindowId,
    initial_location: Point<i32, Logical>,
}

impl PointerGrab<App> for MoveGrab {
    fn motion(
        &mut self,
        app: &mut App,
        handle: &mut PointerInnerHandle<'_, App>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(app, None, event);
        let location = self.initial_location + offset(&self.start_data, event.location);
        if !app.move_window(self.window, location) {
            end_grab(self, app, handle, event.serial, event.time);
        }
    }

    fn button(
        &mut self,
        app: &mut App,
        handle: &mut PointerInnerHandle<'_, App>,
        event: &ButtonEvent,
    ) {
        end_on_release(self, app, handle, event);
    }

    pass_through_pointer_events!();

    fn unset(&mut self, _app: &mut App) {}
}

/// Resizes a window with the pointer.
///
/// An xdg toplevel is asked for each new size with the `Resizing` state and
/// answers in its own time; the commit handler keeps the opposite edge in
/// place (see [`commit_resize`]). An X11 window is configured with the new
/// size and the adjusted position together, since X11 geometry is set by
/// the window manager.
pub struct ResizeGrab {
    start_data: GrabStartData<App>,
    window: WindowId,
    data: ResizeData,
    last_size: Size<i32, Logical>,
}

impl PointerGrab<App> for ResizeGrab {
    fn motion(
        &mut self,
        app: &mut App,
        handle: &mut PointerInnerHandle<'_, App>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(app, None, event);
        let Some((current, is_x11, limits, anchored)) = app
            .windows
            .get(self.window)
            .filter(|window| window.window().alive())
            .and_then(|window| {
                Some((
                    window.location()?,
                    window.x11_surface().is_some(),
                    window.size_limits(),
                    window.resize_state().anchors(self.data),
                ))
            })
        else {
            end_grab(self, app, handle, event.serial, event.time);
            return;
        };
        let size = self
            .data
            .size_for(offset(&self.start_data, event.location), limits);
        if size == self.last_size {
            return;
        }
        self.last_size = size;
        if is_x11 {
            // Once something else has moved the window, the fixed edge no
            // longer applies, exactly as for an xdg toplevel's commits.
            let location = if anchored {
                self.data.location_for(current, size)
            } else {
                current
            };
            app.set_x11_geometry(self.window, Rectangle::new(location, size));
        } else if let Some(window) = app.windows.get(self.window) {
            window.request_size(size);
        }
    }

    fn button(
        &mut self,
        app: &mut App,
        handle: &mut PointerInnerHandle<'_, App>,
        event: &ButtonEvent,
    ) {
        end_on_release(self, app, handle, event);
    }

    pass_through_pointer_events!();

    /// Runs however the grab ends: on release, when the window goes away,
    /// or when the session lock or another grab replaces it. The client
    /// always receives a final configure without `Resizing`; whether the
    /// fixed-edge correction carries on to its answer is
    /// [`ResizeState::grab_ended`]'s decision.
    fn unset(&mut self, app: &mut App) {
        let Some(window) = app.windows.get_mut(self.window) else {
            return;
        };
        let toplevel = window
            .window()
            .toplevel()
            .filter(|toplevel| toplevel.alive())
            .cloned();
        match toplevel {
            Some(toplevel) if window.is_placed() => {
                let size = self.last_size;
                toplevel.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Resizing);
                    state.size = Some(size);
                });
                let serial = toplevel.send_configure();
                let state = window.resize_state().grab_ended(self.data, serial);
                window.set_resize_state(state);
            }
            _ => window.set_resize_state(ResizeState::Idle),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(edges: ResizeEdges) -> ResizeData {
        ResizeData {
            edges,
            initial_location: (100, 50).into(),
            initial_size: (400, 300).into(),
        }
    }

    fn unconstrained() -> (Size<i32, Logical>, Size<i32, Logical>) {
        Default::default()
    }

    #[test]
    fn bottom_right_resize_grows_with_the_pointer_and_keeps_the_origin() {
        let resize = data(ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::BottomRight).unwrap());

        let size = resize.size_for((30, -20).into(), unconstrained());

        assert_eq!(size, (430, 280).into());
        assert!(!resize.moves_origin());
    }

    #[test]
    fn top_left_resize_keeps_the_opposite_edges_in_place() {
        let resize = data(ResizeEdges::from_x11(xwm::ResizeEdge::TopLeft));

        let size = resize.size_for((-30, 20).into(), unconstrained());
        let location = resize.location_for((100, 50).into(), size);

        assert_eq!(size, (430, 280).into());
        assert_eq!(location, (70, 70).into());
        assert_eq!(location.x + size.w, 100 + 400);
        assert_eq!(location.y + size.h, 50 + 300);
    }

    #[test]
    fn single_edge_resizes_leave_the_other_axis_alone() {
        let resize = data(ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::Left).unwrap());

        let size = resize.size_for((10, 500).into(), unconstrained());

        assert_eq!(size, (390, 300).into());
        assert_eq!(
            resize.location_for((100, 80).into(), size),
            (110, 80).into()
        );
    }

    #[test]
    fn sizes_respect_size_hints_and_never_collapse() {
        let resize = data(ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::BottomRight).unwrap());

        let hinted = (Size::from((200, 150)), Size::from((500, 0)));
        assert_eq!(
            resize.size_for((-300, -280).into(), hinted),
            (200, 150).into()
        );
        assert_eq!(
            resize.size_for((300, 900).into(), hinted),
            (500, 1200).into()
        );
        assert_eq!(
            resize.size_for((-900, -900).into(), unconstrained()),
            (1, 1).into()
        );
    }

    #[test]
    fn xdg_none_edge_is_not_a_resize() {
        assert_eq!(ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::None), None);
    }

    #[test]
    fn the_fixed_edge_holds_until_the_final_configure_is_committed() {
        let resize = data(ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::TopLeft).unwrap());
        let final_serial = Serial::from(10);

        // Every commit while the grab runs is corrected.
        let mut state = ResizeState::Resizing(resize);
        assert_eq!(state.commit(), Some(resize));
        assert_eq!(state, ResizeState::Resizing(resize));

        // The grab ended. Commits answering earlier configures are still
        // corrected, and acknowledging one of those changes nothing.
        state = ResizeState::WaitingForFinalAck(resize, final_serial);
        state.acknowledge(Serial::from(9));
        assert_eq!(state.commit(), Some(resize));
        assert_eq!(state, ResizeState::WaitingForFinalAck(resize, final_serial));

        // The commit answering the final configure is the last corrected.
        state.acknowledge(final_serial);
        assert_eq!(state, ResizeState::WaitingForCommit(resize));
        assert_eq!(state.commit(), Some(resize));
        assert_eq!(state, ResizeState::Idle);
        assert_eq!(state.commit(), None);
    }

    #[test]
    fn ending_the_grab_rearms_the_fixed_edge_only_for_its_own_resize() {
        let resize = data(ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::TopLeft).unwrap());
        let final_serial = Serial::from(10);

        // The grab's resize is still in progress: the correction carries on
        // until the client answers the final configure.
        assert!(ResizeState::Resizing(resize).anchors(resize));
        assert_eq!(
            ResizeState::Resizing(resize).grab_ended(resize, final_serial),
            ResizeState::WaitingForFinalAck(resize, final_serial)
        );

        // Something moved the window during the grab, which reset the state:
        // the correction stays cancelled.
        assert!(!ResizeState::Idle.anchors(resize));
        assert_eq!(
            ResizeState::Idle.grab_ended(resize, final_serial),
            ResizeState::Idle
        );

        // A state that belongs to another resize is left alone.
        let other = ResizeData {
            initial_location: (0, 0).into(),
            ..resize
        };
        assert!(!ResizeState::Resizing(other).anchors(resize));
        assert_eq!(
            ResizeState::Resizing(other).grab_ended(resize, final_serial),
            ResizeState::Resizing(other)
        );
    }

    #[test]
    fn resize_arithmetic_saturates_at_the_end_of_the_coordinate_range() {
        let resize = ResizeData {
            edges: ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::TopLeft).unwrap(),
            initial_location: (i32::MAX - 10, i32::MAX - 10).into(),
            initial_size: (400, 300).into(),
        };

        let size = resize.size_for((i32::MIN, i32::MIN).into(), unconstrained());
        assert_eq!(size, (i32::MAX, i32::MAX).into());
        assert_eq!(
            resize.location_for((0, 0).into(), (1, 1).into()),
            (i32::MAX, i32::MAX).into()
        );
    }

    #[test]
    fn a_later_acknowledgement_also_completes_the_resize() {
        let resize = data(ResizeEdges::from_xdg(xdg_toplevel::ResizeEdge::Left).unwrap());
        let mut state = ResizeState::WaitingForFinalAck(resize, Serial::from(10));

        state.acknowledge(Serial::from(12));

        assert_eq!(state, ResizeState::WaitingForCommit(resize));
    }
}

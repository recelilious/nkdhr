use std::cell::Cell;
use std::time::{Duration, Instant};

use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::solid::SolidColorBuffer;
use smithay::desktop::{Window, WindowSurfaceType};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle, Size};
use smithay::wayland::seat::WaylandFocus;
use smithay::xwayland::X11Surface;

use crate::settings::GridSettings;
use crate::widget_host::{InputHandled, PinnedLayer, PinnedNode, PinnedPointerEvent};

/// Type-level marker for nkdhr's own canvas world coordinate space
/// (ROADMAP.md §2.3) — distinct from Smithay's own `Logical`/`Physical`,
/// since a canvas is unbounded and output-independent, unlike either of
/// those. `f64` throughout: the canvas is conceptually infinite, and
/// COMP-4's zoom makes non-integer positions and sizes routine.
#[derive(Debug)]
pub struct World;

pub struct ManagedWindow {
    pub window: Window,
    pub position: Point<f64, World>,
    position_animation: Option<PositionAnimation>,
    decoration: SolidColorBuffer,
}

const DECORATION_BORDER: f64 = 2.0;
const DECORATION_TITLEBAR: f64 = 28.0;
const DECORATION_COLOR: Color32F = Color32F::new(0.20, 0.22, 0.29, 1.0);

impl ManagedWindow {
    pub fn wl_surface(&self) -> Option<WlSurface> {
        self.window.wl_surface().map(|surface| surface.into_owned())
    }

    pub fn matches_surface(&self, surface: &WlSurface) -> bool {
        let found = Cell::new(false);
        self.window.with_surfaces(|candidate, _| {
            if candidate == surface {
                found.set(true);
            }
        });
        found.get()
    }

    pub fn matches_x11(&self, surface: &X11Surface) -> bool {
        self.window.x11_surface() == Some(surface)
    }

    /// The window's current committed bounding size. Smithay's unified
    /// desktop window updates this for both xdg-shell and X11 surfaces.
    pub fn size(&self) -> Size<f64, World> {
        let logical = self.window.bbox().size;
        (f64::from(logical.w), f64::from(logical.h)).into()
    }

    pub fn close(&self) {
        if let Some(toplevel) = self.window.toplevel() {
            toplevel.send_close();
        } else if let Some(surface) = self.window.x11_surface() {
            let _ = surface.close();
        }
    }

    pub fn request_size(&self, size: Size<i32, Logical>) {
        if let Some(toplevel) = self.window.toplevel() {
            toplevel.with_pending_state(|state| state.size = Some(size));
            toplevel.send_configure();
        } else if let Some(surface) = self.window.x11_surface() {
            let mut geometry = surface.geometry();
            geometry.size = size;
            let _ = surface.configure(geometry);
        }
    }

    pub fn rect(&self) -> Rectangle<f64, World> {
        self.decoration_rect()
            .unwrap_or_else(|| self.content_rect())
    }

    pub fn content_rect(&self) -> Rectangle<f64, World> {
        Rectangle::new(self.position, self.size())
    }

    /// The visual frame promised by server-side decoration negotiation.
    /// The client surface remains rooted at `position`; the titlebar and
    /// border extend outward so client content is never covered.
    pub fn decoration(&self) -> Option<(Rectangle<f64, World>, SolidColorBuffer)> {
        self.decoration_rect()
            .map(|rect| (rect, self.decoration.clone()))
    }

    fn decoration_rect(&self) -> Option<Rectangle<f64, World>> {
        let server_side = self.window.toplevel().is_some_and(|toplevel| {
            toplevel.current_state().decoration_mode == Some(Mode::ServerSide)
        }) || self
            .window
            .x11_surface()
            .is_some_and(|surface| !surface.is_override_redirect() && !surface.is_decorated());
        server_side.then(|| {
            Rectangle::new(
                (
                    self.position.x - DECORATION_BORDER,
                    self.position.y - DECORATION_TITLEBAR - DECORATION_BORDER,
                )
                    .into(),
                (
                    self.size().w + DECORATION_BORDER * 2.0,
                    self.size().h + DECORATION_TITLEBAR + DECORATION_BORDER * 2.0,
                )
                    .into(),
            )
        })
    }

    /// The window's world-space center — where overview mode's
    /// click-to-zoom animates the viewport to.
    pub fn center(&self) -> Point<f64, World> {
        let rect = self.rect();
        (
            rect.loc.x + rect.size.w / 2.0,
            rect.loc.y + rect.size.h / 2.0,
        )
            .into()
    }
}

/// Every window mapped on the canvas, in stacking order (last = topmost,
/// both for rendering and for hit-testing). COMP-3 gave this a fixed
/// implicit viewport; COMP-4's [`Viewport`] makes panning/zooming over it
/// real. COMP-5's `App::canvases` owns one instance per first-class canvas.
#[derive(Default)]
pub struct Canvas {
    windows: Vec<ManagedWindow>,
    pinned_nodes: Vec<Box<dyn PinnedNode>>,
}

impl Canvas {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a newly mapped window at a cascaded default position — COMP-3's
    /// free-placement model means nothing *constrains* where a window can
    /// go, not that a sensible starting point doesn't exist. Each new
    /// window offsets from the last so opening several in a row produces
    /// visibly distinct, not perfectly overlapping, windows.
    pub fn map(&mut self, window: Window, grid: GridSettings) -> Point<f64, World> {
        let coordinate = grid.cascade_coordinate(self.windows.len());
        let position = (coordinate, coordinate).into();
        self.windows.push(ManagedWindow {
            window,
            position,
            position_animation: None,
            decoration: SolidColorBuffer::new((0, 0), DECORATION_COLOR),
        });
        position
    }

    pub fn unmap(&mut self, surface: &WlSurface) {
        self.windows
            .retain(|window| !window.matches_surface(surface));
    }

    pub fn unmap_x11(&mut self, surface: &X11Surface) {
        self.windows.retain(|window| !window.matches_x11(surface));
    }

    pub fn windows(&self) -> &[ManagedWindow] {
        &self.windows
    }

    /// Remove surfaces whose client disappeared without completing its
    /// protocol-level destruction sequence. Returns the number reclaimed.
    pub fn remove_dead_windows(&mut self) -> usize {
        let before = self.windows.len();
        self.windows.retain(|window| window.window.alive());
        before - self.windows.len()
    }

    pub fn add_pinned(&mut self, node: Box<dyn PinnedNode>) {
        let id = node.id().to_owned();
        assert!(
            self.pinned_nodes
                .iter()
                .all(|candidate| candidate.id() != id),
            "pinned node IDs must be unique within a canvas: {id:?}"
        );
        self.pinned_nodes.push(node);
    }

    pub fn pinned_nodes(&self) -> impl DoubleEndedIterator<Item = &dyn PinnedNode> {
        self.pinned_nodes.iter().map(Box::as_ref)
    }

    /// Dispatch to the topmost node in `layer` at `point`. Ignored events
    /// continue through lower nodes in that layer before falling through to
    /// the normal window/canvas input path.
    pub fn dispatch_pinned_pointer(
        &mut self,
        point: Point<f64, World>,
        layer: PinnedLayer,
        event: impl Fn(Point<f64, crate::widget_host::PinnedLocal>) -> PinnedPointerEvent,
    ) -> InputHandled {
        for node in self.pinned_nodes.iter_mut().rev() {
            let rect = node.world_rect();
            if node.layer() != layer || !rect.contains(point) {
                continue;
            }
            let local = (point.x - rect.loc.x, point.y - rect.loc.y).into();
            if node.pointer_event(event(local)) == InputHandled::Captured {
                return InputHandled::Captured;
            }
        }
        InputHandled::Ignored
    }

    /// The topmost window whose rect contains `point`, for click-to-focus
    /// and the move/resize grab's hit test.
    pub fn window_at(&self, point: Point<f64, World>) -> Option<&ManagedWindow> {
        self.windows
            .iter()
            .rev()
            .find(|window| window.rect().contains(point))
    }

    /// Returns the exact toplevel, subsurface, or popup under a world-space
    /// point together with that surface's offset from the window root.
    pub fn surface_at(
        &self,
        point: Point<f64, World>,
    ) -> Option<(&ManagedWindow, WlSurface, Point<i32, Logical>)> {
        self.windows.iter().rev().find_map(|window| {
            let local = (point.x - window.position.x, point.y - window.position.y);
            window
                .window
                .surface_under(local, WindowSurfaceType::ALL)
                .map(|(surface, offset)| (window, surface, offset))
        })
    }

    pub fn set_position(&mut self, surface: &WlSurface, position: Point<f64, World>) {
        if let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.matches_surface(surface))
        {
            window.position = position;
            window.position_animation = None;
        }
    }

    pub fn set_x11_position(&mut self, surface: &X11Surface, position: Point<f64, World>) {
        if let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.matches_x11(surface))
        {
            window.position = position;
            window.position_animation = None;
        }
    }

    pub fn animate_position(
        &mut self,
        surface: &WlSurface,
        target: Point<f64, World>,
        duration: Duration,
    ) {
        let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.matches_surface(surface))
        else {
            return;
        };
        if window.position == target {
            window.position_animation = None;
        } else {
            window.position_animation =
                Some(PositionAnimation::new(window.position, target, duration));
        }
    }

    pub fn animations_running(&self) -> bool {
        self.windows
            .iter()
            .any(|window| window.position_animation.is_some())
    }

    pub fn advance_animations(&mut self, now: Instant) {
        for window in &mut self.windows {
            let Some(animation) = &window.position_animation else {
                continue;
            };
            match animation.advance(now) {
                Some(position) => window.position = position,
                None => {
                    window.position = animation.target;
                    window.position_animation = None;
                }
            }
        }
    }

    /// Moves `surface` to the top of the stack (rendered last, hit-tested
    /// first) — the window-raising half of click-to-focus.
    pub fn raise(&mut self, surface: &WlSurface) {
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.matches_surface(surface))
        {
            let window = self.windows.remove(index);
            self.windows.push(window);
        }
    }

    /// The window after `current` in stacking order, cycling back to the
    /// first when `current` is the last (or `None`/not found) — the
    /// alt-tab-equivalent `cycle_focus` keybinding's whole implementation.
    pub fn next_after(&self, current: Option<&WlSurface>) -> Option<&ManagedWindow> {
        if self.windows.is_empty() {
            return None;
        }
        let start = current
            .and_then(|surface| {
                self.windows
                    .iter()
                    .position(|window| window.matches_surface(surface))
            })
            .map_or(0, |index| (index + 1) % self.windows.len());
        self.windows.get(start)
    }

    /// The smallest world-space rect containing every mapped window, for
    /// overview mode to fit its zoomed-out view to. `None` with nothing
    /// mapped — the caller decides what "overview of an empty canvas"
    /// means (COMP-4: just don't zoom at all).
    pub fn bounding_rect(&self) -> Option<Rectangle<f64, World>> {
        self.windows
            .iter()
            .map(ManagedWindow::rect)
            .chain(self.pinned_nodes.iter().map(|node| node.world_rect()))
            .reduce(|a, b| a.merge(b))
    }
}

/// A camera onto the canvas: the world-space point shown at the output
/// group's canvas anchor, plus a zoom factor. The anchor defaults to the
/// primary physical output's center. COMP-5 owns one viewport per output
/// group, shared by every output in that group's rigid arrangement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub center: Point<f64, World>,
    pub zoom: f64,
}

impl Viewport {
    pub const WORK: Viewport = Viewport {
        center: Point::new(0.0, 0.0),
        zoom: 1.0,
    };

    /// World point to logical coordinates in an output group's rigid
    /// virtual display area. Each physical output subtracts its own group
    /// offset and applies its scale after this shared transform.
    pub fn to_group_logical(
        self,
        point: Point<f64, World>,
        canvas_anchor: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        let x = (point.x - self.center.x) * self.zoom + canvas_anchor.x;
        let y = (point.y - self.center.y) * self.zoom + canvas_anchor.y;
        (x, y).into()
    }

    /// A point relative to an output group's rigid logical rectangle into
    /// the world viewed by that group. The inverse of
    /// [`Viewport::to_group_logical`].
    pub fn group_logical_to_world(
        self,
        point: Point<f64, Logical>,
        canvas_anchor: Point<f64, Logical>,
    ) -> Point<f64, World> {
        let x = (point.x - canvas_anchor.x) / self.zoom + self.center.x;
        let y = (point.y - canvas_anchor.y) / self.zoom + self.center.y;
        (x, y).into()
    }

    /// A `Logical` pointer-motion delta (e.g. from a drag) -> the
    /// world-space distance it represents at this viewport's current zoom
    /// — screen pixels shrink to fewer world units as zoom increases.
    pub fn to_world_delta(self, delta: Point<f64, Logical>) -> Point<f64, World> {
        (delta.x / self.zoom, delta.y / self.zoom).into()
    }

    /// The viewport that fits `rect` (typically [`Canvas::bounding_rect`])
    /// inside the group's logical size with margin to spare, for overview.
    /// Never zooms *in* past 1:1 — overview only ever zooms out or stays
    /// put, per the sharpness policy (ROADMAP §2.4: 1:1 in work state,
    /// scaling blur only accepted in the transient overview state).
    pub fn fit_group(
        rect: Rectangle<f64, World>,
        group_size: Size<i32, Logical>,
        canvas_anchor: Point<f64, Logical>,
    ) -> Viewport {
        const MARGIN: f64 = 1.25;
        let rect_center: Point<f64, World> = (
            rect.loc.x + rect.size.w / 2.0,
            rect.loc.y + rect.size.h / 2.0,
        )
            .into();
        let content_w = rect.size.w.max(1.0) * MARGIN;
        let content_h = rect.size.h.max(1.0) * MARGIN;
        let zoom = (f64::from(group_size.w) / content_w)
            .min(f64::from(group_size.h) / content_h)
            .min(1.0);
        let group_center: Point<f64, Logical> =
            (f64::from(group_size.w) / 2.0, f64::from(group_size.h) / 2.0).into();
        let center = (
            rect_center.x - (group_center.x - canvas_anchor.x) / zoom,
            rect_center.y - (group_center.y - canvas_anchor.y) / zoom,
        )
            .into();
        Viewport { center, zoom }
    }
}

/// An in-progress eased transition between two [`Viewport`]s — COMP-4's
/// "animated transitions" (overview enter/exit, jumping to a mark), driven
/// by the render loop's own frame timing rather than a separate animation
/// engine (there isn't one yet; that's a Phase 3 UI concern once there are
/// more things than viewport moves to animate).
pub struct Animation {
    from: Viewport,
    to: Viewport,
    start: Instant,
    duration: Duration,
}

impl Animation {
    pub fn new(from: Viewport, to: Viewport, duration: Duration) -> Self {
        Self {
            from,
            to,
            start: Instant::now(),
            duration,
        }
    }

    /// Where this animation is headed — what the caller should snap the
    /// viewport to once [`Animation::advance`] returns `None`.
    pub fn target(&self) -> Viewport {
        self.to
    }

    /// The viewport at `now`, eased — or `None` once the animation has run
    /// its full duration, at which point the caller should snap to `to`
    /// directly (not keep calling this) and drop the `Animation`.
    pub fn advance(&self, now: Instant) -> Option<Viewport> {
        let eased = eased_progress(self.start, self.duration, now)?;
        Some(Viewport {
            center: (
                self.from.center.x + (self.to.center.x - self.from.center.x) * eased,
                self.from.center.y + (self.to.center.y - self.from.center.y) * eased,
            )
                .into(),
            zoom: self.from.zoom + (self.to.zoom - self.from.zoom) * eased,
        })
    }
}

struct PositionAnimation {
    from: Point<f64, World>,
    target: Point<f64, World>,
    start: Instant,
    duration: Duration,
}

impl PositionAnimation {
    fn new(from: Point<f64, World>, target: Point<f64, World>, duration: Duration) -> Self {
        Self {
            from,
            target,
            start: Instant::now(),
            duration,
        }
    }

    fn advance(&self, now: Instant) -> Option<Point<f64, World>> {
        let eased = eased_progress(self.start, self.duration, now)?;
        Some(
            (
                self.from.x + (self.target.x - self.from.x) * eased,
                self.from.y + (self.target.y - self.from.y) * eased,
            )
                .into(),
        )
    }
}

fn eased_progress(start: Instant, duration: Duration, now: Instant) -> Option<f64> {
    let elapsed = now.saturating_duration_since(start).as_secs_f64();
    let progress = elapsed / duration.as_secs_f64();
    if progress >= 1.0 {
        None
    } else {
        Some(1.0 - (1.0 - progress).powi(3))
    }
}

/// An in-progress compositor-driven interaction, started by a
/// modifier-held pointer drag (`super+drag` to move a window,
/// `super+right-drag` to resize one, a plain drag on empty canvas to pan
/// — see `docs-staging/canvas/USAGE.md`). Not a Smithay `PointerGrab`:
/// those exist for protocol-visible grabs (popup dismissal,
/// client-requested interactive move/resize), but a WM-level modifier
/// gesture is purely the compositor's own business, observed and handled
/// entirely within `main.rs`'s own input dispatch before events are ever
/// forwarded to the seat — there is nothing here a client needs to know
/// about via the Wayland protocol.
#[derive(Debug, Clone)]
pub enum Drag {
    Move {
        surface: WlSurface,
        window_start: Point<f64, World>,
        pointer_start: Point<f64, Logical>,
    },
    Resize {
        surface: WlSurface,
        size_start: Size<i32, Logical>,
        window_start: Point<f64, World>,
        pointer_start: Point<f64, Logical>,
        edge: ResizeEdge,
    },
    Pan {
        viewport_start: Point<f64, World>,
        pointer_start: Point<f64, Logical>,
        zoom: f64,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum ResizeEdge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeEdge {
    pub(crate) fn left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    pub(crate) fn right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    pub(crate) fn top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    pub(crate) fn bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::BottomLeft | Self::BottomRight)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
    use smithay::utils::{Logical, Transform};

    use super::*;
    use crate::widget_host::{PinnedLocal, PinnedRenderData};

    #[test]
    fn viewport_maps_its_world_center_to_the_canvas_anchor() {
        let viewport = Viewport {
            center: (64.0, -32.0).into(),
            zoom: 2.0,
        };
        let anchor = (300.0, 200.0).into();

        assert_eq!(viewport.to_group_logical(viewport.center, anchor), anchor);
        assert_eq!(
            viewport.group_logical_to_world(anchor, anchor),
            viewport.center
        );
    }

    #[test]
    fn overview_centers_content_in_the_group_with_an_off_center_anchor() {
        let rect = Rectangle::new((100.0, 200.0).into(), (1000.0, 500.0).into());
        let group_size = (2000, 1000).into();
        let anchor = (400.0, 300.0).into();
        let viewport = Viewport::fit_group(rect, group_size, anchor);
        let rect_center = (600.0, 450.0).into();

        assert_eq!(
            viewport.to_group_logical(rect_center, anchor),
            (1000.0, 500.0).into()
        );
    }

    #[test]
    fn position_animation_eases_and_finishes_at_target() {
        let start = Instant::now();
        let animation = PositionAnimation {
            from: (0.0, 0.0).into(),
            target: (32.0, -64.0).into(),
            start,
            duration: Duration::from_millis(100),
        };

        assert_eq!(animation.advance(start), Some((0.0, 0.0).into()));
        let midpoint = animation
            .advance(start + Duration::from_millis(50))
            .unwrap();
        assert_eq!(midpoint, (28.0, -56.0).into());
        assert_eq!(animation.advance(start + Duration::from_millis(100)), None);
    }

    struct TestNode {
        id: &'static str,
        rect: Rectangle<f64, World>,
        layer: PinnedLayer,
        result: InputHandled,
        buffer: MemoryRenderBuffer,
        events: Arc<Mutex<Vec<Point<f64, PinnedLocal>>>>,
    }

    impl TestNode {
        fn new(
            id: &'static str,
            rect: Rectangle<f64, World>,
            layer: PinnedLayer,
            result: InputHandled,
            events: Arc<Mutex<Vec<Point<f64, PinnedLocal>>>>,
        ) -> Self {
            Self {
                id,
                rect,
                layer,
                result,
                buffer: MemoryRenderBuffer::from_slice(
                    &[255, 255, 255, 255],
                    Fourcc::Abgr8888,
                    (1, 1),
                    1,
                    Transform::Normal,
                    None,
                ),
                events,
            }
        }
    }

    impl PinnedNode for TestNode {
        fn id(&self) -> &str {
            self.id
        }

        fn world_rect(&self) -> Rectangle<f64, World> {
            self.rect
        }

        fn layer(&self) -> PinnedLayer {
            self.layer
        }

        fn render_data(&self) -> PinnedRenderData<'_> {
            PinnedRenderData::Memory {
                buffer: &self.buffer,
                source_size: Size::<i32, Logical>::from((1, 1)),
            }
        }

        fn pointer_event(&mut self, event: PinnedPointerEvent) -> InputHandled {
            let position = match event {
                PinnedPointerEvent::Motion { position, .. }
                | PinnedPointerEvent::Button { position, .. }
                | PinnedPointerEvent::Axis { position, .. } => position,
            };
            self.events.lock().unwrap().push(position);
            self.result
        }
    }

    #[test]
    fn pinned_hit_testing_is_reverse_registration_order_and_node_local() {
        let lower_events = Arc::new(Mutex::new(Vec::new()));
        let upper_events = Arc::new(Mutex::new(Vec::new()));
        let mut canvas = Canvas::new();
        canvas.add_pinned(Box::new(TestNode::new(
            "lower",
            Rectangle::new((10.0, 20.0).into(), (100.0, 100.0).into()),
            PinnedLayer::AboveWindows,
            InputHandled::Captured,
            lower_events.clone(),
        )));
        canvas.add_pinned(Box::new(TestNode::new(
            "upper",
            Rectangle::new((20.0, 30.0).into(), (100.0, 100.0).into()),
            PinnedLayer::AboveWindows,
            InputHandled::Captured,
            upper_events.clone(),
        )));

        let handled = canvas.dispatch_pinned_pointer(
            (25.0, 35.0).into(),
            PinnedLayer::AboveWindows,
            |position| PinnedPointerEvent::Motion { position, time: 1 },
        );

        assert_eq!(handled, InputHandled::Captured);
        assert!(lower_events.lock().unwrap().is_empty());
        assert_eq!(
            upper_events.lock().unwrap().as_slice(),
            &[(5.0, 5.0).into()]
        );
    }

    #[test]
    fn ignored_pinned_input_falls_through_and_bounds_include_nodes() {
        let upper_events = Arc::new(Mutex::new(Vec::new()));
        let lower_events = Arc::new(Mutex::new(Vec::new()));
        let mut canvas = Canvas::new();
        canvas.add_pinned(Box::new(TestNode::new(
            "lower",
            Rectangle::new((-50.0, -40.0).into(), (20.0, 10.0).into()),
            PinnedLayer::BehindWindows,
            InputHandled::Captured,
            lower_events.clone(),
        )));
        canvas.add_pinned(Box::new(TestNode::new(
            "upper",
            Rectangle::new((-50.0, -40.0).into(), (20.0, 10.0).into()),
            PinnedLayer::BehindWindows,
            InputHandled::Ignored,
            upper_events.clone(),
        )));

        let handled = canvas.dispatch_pinned_pointer(
            (-45.0, -35.0).into(),
            PinnedLayer::BehindWindows,
            |position| PinnedPointerEvent::Motion { position, time: 1 },
        );

        assert_eq!(handled, InputHandled::Captured);
        assert_eq!(upper_events.lock().unwrap().len(), 1);
        assert_eq!(lower_events.lock().unwrap().len(), 1);
        assert_eq!(
            canvas.bounding_rect(),
            Some(Rectangle::new((-50.0, -40.0).into(), (20.0, 10.0).into()))
        );
    }
}

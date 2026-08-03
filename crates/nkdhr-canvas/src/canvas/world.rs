use std::time::{Duration, Instant};

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;

/// Type-level marker for nkdhr's own canvas world coordinate space
/// (ROADMAP.md §2.3) — distinct from Smithay's own `Logical`/`Physical`,
/// since a canvas is unbounded and output-independent, unlike either of
/// those. `f64` throughout: the canvas is conceptually infinite, and
/// COMP-4's zoom makes non-integer positions and sizes routine.
#[derive(Debug)]
pub struct World;

pub struct ManagedWindow {
    pub surface: ToplevelSurface,
    pub position: Point<f64, World>,
}

impl ManagedWindow {
    /// The window's current on-screen size. Reads the *committed* buffer
    /// size via Smithay's renderer-surface-state tracking (populated by
    /// `on_commit_buffer_handler`) rather than whatever size was last
    /// requested — a client may commit a different size than asked for,
    /// and hit-testing against a stale requested size would be wrong.
    /// `(0, 0)` before the client's first commit.
    pub fn size(&self) -> Size<f64, World> {
        let logical =
            with_renderer_surface_state(self.surface.wl_surface(), |state| state.surface_size())
                .flatten()
                .unwrap_or_default();
        (f64::from(logical.w), f64::from(logical.h)).into()
    }

    pub fn rect(&self) -> Rectangle<f64, World> {
        Rectangle::new(self.position, self.size())
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
    pub fn map(&mut self, surface: ToplevelSurface) -> Point<f64, World> {
        let n = self.windows.len() as f64;
        let position = (100.0 + 40.0 * (n % 10.0), 100.0 + 40.0 * (n % 10.0)).into();
        self.windows.push(ManagedWindow { surface, position });
        position
    }

    pub fn unmap(&mut self, surface: &WlSurface) {
        self.windows
            .retain(|window| window.surface.wl_surface() != surface);
    }

    pub fn windows(&self) -> &[ManagedWindow] {
        &self.windows
    }

    /// The topmost window whose rect contains `point`, for click-to-focus
    /// and the move/resize grab's hit test.
    pub fn window_at(&self, point: Point<f64, World>) -> Option<&ManagedWindow> {
        self.windows
            .iter()
            .rev()
            .find(|window| window.rect().contains(point))
    }

    pub fn set_position(&mut self, surface: &WlSurface, position: Point<f64, World>) {
        if let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.surface.wl_surface() == surface)
        {
            window.position = position;
        }
    }

    /// Moves `surface` to the top of the stack (rendered last, hit-tested
    /// first) — the window-raising half of click-to-focus.
    pub fn raise(&mut self, surface: &WlSurface) {
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.surface.wl_surface() == surface)
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
                    .position(|window| window.surface.wl_surface() == surface)
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
            .reduce(|a, b| a.merge(b))
    }
}

/// A camera onto the canvas: a world-space point at the center of the
/// view, plus a zoom factor. COMP-5 owns one per output group, shared by
/// every physical output in that group's rigid arrangement.
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
        group_size: Size<i32, Logical>,
    ) -> Point<f64, Logical> {
        let x = (point.x - self.center.x) * self.zoom + f64::from(group_size.w) / 2.0;
        let y = (point.y - self.center.y) * self.zoom + f64::from(group_size.h) / 2.0;
        (x, y).into()
    }

    /// A point relative to an output group's rigid logical rectangle into
    /// the world viewed by that group. The inverse of
    /// [`Viewport::to_group_logical`].
    pub fn group_logical_to_world(
        self,
        point: Point<f64, Logical>,
        group_size: Size<i32, Logical>,
    ) -> Point<f64, World> {
        let x = (point.x - f64::from(group_size.w) / 2.0) / self.zoom + self.center.x;
        let y = (point.y - f64::from(group_size.h) / 2.0) / self.zoom + self.center.y;
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
    pub fn fit_group(rect: Rectangle<f64, World>, group_size: Size<i32, Logical>) -> Viewport {
        const MARGIN: f64 = 1.25;
        let center = (
            rect.loc.x + rect.size.w / 2.0,
            rect.loc.y + rect.size.h / 2.0,
        )
            .into();
        let content_w = rect.size.w.max(1.0) * MARGIN;
        let content_h = rect.size.h.max(1.0) * MARGIN;
        let zoom = (f64::from(group_size.w) / content_w)
            .min(f64::from(group_size.h) / content_h)
            .min(1.0);
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
        let elapsed = now.saturating_duration_since(self.start).as_secs_f64();
        let t = elapsed / self.duration.as_secs_f64();
        if t >= 1.0 {
            return None;
        }
        // Ease-out cubic: fast start, gentle settle — a normal choice for
        // "camera" moves, not something the project has a stronger opinion
        // on yet (that's a Phase 3 theming/motion concern).
        let eased = 1.0 - (1.0 - t).powi(3);
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
        pointer_start: Point<f64, Logical>,
    },
    Pan {
        viewport_start: Point<f64, World>,
        pointer_start: Point<f64, Logical>,
        zoom: f64,
    },
}

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;

/// Type-level marker for nkdhr's own canvas world coordinate space
/// (ROADMAP.md §2.3) — distinct from Smithay's own `Logical`/`Physical`,
/// since a canvas is unbounded and output-independent, unlike either of
/// those. `f64` throughout: the canvas is conceptually infinite, and
/// COMP-4's zoom will make non-integer positions and sizes routine.
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
}

/// Every window mapped on the canvas, in stacking order (last = topmost,
/// both for rendering and for hit-testing) — COMP-3 has exactly one
/// canvas and one implicit viewport; COMP-4 is what makes either of those
/// plural or introduces real pan/zoom.
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
}

/// COMP-3 has no real viewport yet (no pan, no zoom — that's COMP-4): the
/// nested window looks directly at world-space origin at 1:1 scale, so
/// converting a pointer's on-screen `Logical` position into a `World`
/// position is a straight coordinate-copy today. Kept as a named
/// conversion rather than inlined `.into()` at each call site so COMP-4
/// only has to change *this* function (to subtract a real viewport origin
/// and divide by zoom) — every caller stays the same.
pub fn logical_to_world(point: Point<f64, Logical>) -> Point<f64, World> {
    (point.x, point.y).into()
}

/// The world-space equivalent of a `Logical` pointer-motion delta — same
/// "nothing to convert yet, COMP-4 changes only this" reasoning as
/// [`logical_to_world`].
pub fn logical_delta_to_world(delta: Point<f64, Logical>) -> Point<f64, World> {
    (delta.x, delta.y).into()
}

/// An in-progress compositor-driven window interaction, started by a
/// modifier-held pointer drag (`super+drag` to move, `super+right-drag`
/// to resize — see `docs-staging/canvas/USAGE.md`). Not a Smithay
/// `PointerGrab`: those exist for protocol-visible grabs (popup dismissal,
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
}

//! The window store: the one list of client windows, their locations and
//! their stacking order, plus the rules that decide where a window goes.
//!
//! Rendering, hit testing, output membership, frame callbacks and focus all
//! read this store, so what clients are told always matches what is shown.

use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use smithay::desktop::{Window, WindowSurfaceType};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use smithay::xwayland::X11Surface;

use crate::grabs::ResizeState;
use crate::outputs::OutputLayout;
use crate::state::KeyboardFocusTarget;

/// A process-unique window identity.
///
/// Ids increase monotonically and are never reused, so an id kept by a
/// grab or by a future interaction layer can never alias a newer window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(u64);

impl WindowId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One client window: an xdg toplevel or an X11 surface, with its place in
/// the compositor-global logical space.
///
/// The location is the global position of the root surface's origin,
/// point `(0, 0)` of the window in Smithay's terms. The visible frame is
/// [`geometry`](Self::geometry) offset by the location; for a client that
/// draws shadows outside its xdg window geometry, `geometry().loc` is the
/// shadow inset. For X11 windows it is always `(0, 0)`, so the location
/// equals the X11 position.
pub struct ManagedWindow {
    id: WindowId,
    window: Window,
    location: Option<Point<i32, Logical>>,
    resize: ResizeState,
}

impl ManagedWindow {
    pub fn id(&self) -> WindowId {
        self.id
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// `None` while the window is unplaced: an xdg toplevel before its first
    /// buffer, or again after it unmapped itself with a null buffer.
    pub fn location(&self) -> Option<Point<i32, Logical>> {
        self.location
    }

    pub fn is_placed(&self) -> bool {
        self.location.is_some()
    }

    /// The root `wl_surface`. An X11 window has one only once XWayland has
    /// associated it, which happens before it enters the store.
    pub fn wl_surface(&self) -> Option<WlSurface> {
        self.window.wl_surface().map(|surface| surface.into_owned())
    }

    pub fn x11_surface(&self) -> Option<&X11Surface> {
        self.window.x11_surface()
    }

    /// Menus, tooltips and other X11 windows the window manager does not
    /// manage: they keep their own position and never take focus.
    pub fn is_override_redirect(&self) -> bool {
        self.x11_surface()
            .is_some_and(X11Surface::is_override_redirect)
    }

    /// Whether the focus policy may give this window keyboard focus.
    pub fn is_focusable(&self) -> bool {
        self.is_placed() && !self.is_override_redirect()
    }

    /// Whether `surface` is this window's root surface, one of its
    /// subsurfaces or one of its popups (and their subsurfaces).
    pub fn matches_surface(&self, surface: &WlSurface) -> bool {
        let found = Cell::new(false);
        self.window.with_surfaces(|candidate, _| {
            if candidate == surface {
                found.set(true);
            }
        });
        found.get()
    }

    /// The window geometry in window-local coordinates: the visible frame,
    /// excluding client-side shadows.
    pub fn geometry(&self) -> Rectangle<i32, Logical> {
        self.window.geometry()
    }

    /// Everything the window draws in window-local coordinates, popups
    /// included.
    pub fn bbox(&self) -> Rectangle<i32, Logical> {
        self.window.bbox_with_popups()
    }

    /// The bounding box in the global space, or `None` while unplaced.
    pub fn global_bbox(&self) -> Option<Rectangle<i32, Logical>> {
        let location = self.location?;
        let mut bbox = self.bbox();
        bbox.loc += location;
        Some(bbox)
    }

    /// The keyboard focus target that focusing this window sets.
    pub fn keyboard_target(&self) -> Option<KeyboardFocusTarget> {
        match self.x11_surface() {
            Some(surface) => Some(KeyboardFocusTarget::X11(surface.clone())),
            None => self.wl_surface().map(KeyboardFocusTarget::Wayland),
        }
    }

    /// Whether `target` belongs to this window: its X11 surface, its root
    /// surface, or one of its subsurfaces or popups (a popup holds the
    /// keyboard while it has an explicit grab).
    pub fn owns_focus(&self, target: &KeyboardFocusTarget) -> bool {
        match target {
            KeyboardFocusTarget::X11(surface) => self.x11_surface() == Some(surface),
            KeyboardFocusTarget::Wayland(surface) => self.matches_surface(surface),
        }
    }

    /// Asks the client for a new size: an xdg configure (sent only when the
    /// pending state actually changed), or an X11 configure that keeps the
    /// window's position.
    pub fn request_size(&self, size: Size<i32, Logical>) {
        if let Some(toplevel) = self.window.toplevel() {
            toplevel.with_pending_state(|state| state.size = Some(size));
            toplevel.send_pending_configure();
        } else if let Some(surface) = self.x11_surface() {
            let mut geometry = surface.geometry();
            geometry.size = size;
            if let Err(error) = surface.configure(geometry) {
                eprintln!(
                    "nkdhr-compositor: failed to resize X11 window {}: {error}",
                    surface.window_id()
                );
            }
        }
    }

    /// The client's minimum and maximum size hints, where `0` on an axis
    /// means that axis is unconstrained.
    pub fn size_limits(&self) -> (Size<i32, Logical>, Size<i32, Logical>) {
        if let Some(toplevel) = self.window.toplevel() {
            with_states(toplevel.wl_surface(), |states| {
                let mut cached = states.cached_state.get::<SurfaceCachedState>();
                let current = cached.current();
                (current.min_size, current.max_size)
            })
        } else if let Some(surface) = self.x11_surface() {
            (
                surface.min_size().unwrap_or_default(),
                surface.max_size().unwrap_or_default(),
            )
        } else {
            Default::default()
        }
    }

    pub fn resize_state(&self) -> ResizeState {
        self.resize
    }

    pub fn set_resize_state(&mut self, state: ResizeState) {
        self.resize = state;
    }
}

/// A surface found under a global point.
pub struct SurfaceHit {
    pub window: WindowId,
    /// The exact surface under the point: the root, a subsurface or a popup.
    pub surface: WlSurface,
    /// That surface's origin in the global space, which is what pointer and
    /// touch focus need.
    pub origin: Point<f64, Logical>,
}

/// Every client window, ordered bottom to top: the last element is topmost.
///
/// Unplaced windows are only ever found by the surface lookups protocol
/// handlers use; hit testing, focus, rendering and frame callbacks skip
/// them.
#[derive(Default)]
pub struct WindowStore {
    windows: Vec<ManagedWindow>,
}

impl WindowStore {
    /// Adds an xdg toplevel that has not drawn anything yet. It stays
    /// unplaced until its first buffer commit.
    pub fn insert_unplaced(&mut self, window: Window) -> WindowId {
        self.insert(window, None)
    }

    /// Adds a window that is placed from the start (X11 windows, whose size
    /// is known when they map), on top of the stack.
    pub fn insert_placed(&mut self, window: Window, location: Point<i32, Logical>) -> WindowId {
        self.insert(window, Some(location))
    }

    fn insert(&mut self, window: Window, location: Option<Point<i32, Logical>>) -> WindowId {
        let id = WindowId::next();
        self.windows.push(ManagedWindow {
            id,
            window,
            location,
            resize: ResizeState::Idle,
        });
        id
    }

    /// Places a window at `location` and raises it.
    pub fn place(&mut self, id: WindowId, location: Point<i32, Logical>) {
        if self.set_location(id, location) {
            self.raise(id);
        }
    }

    /// Makes a window unplaced again, forgetting any resize in progress.
    pub fn unplace(&mut self, id: WindowId) {
        if let Some(window) = self.get_mut(id) {
            window.location = None;
            window.resize = ResizeState::Idle;
        }
    }

    /// Moves a window. Returns whether the window exists.
    pub fn set_location(&mut self, id: WindowId, location: Point<i32, Logical>) -> bool {
        match self.get_mut(id) {
            Some(window) => {
                window.location = Some(location);
                true
            }
            None => false,
        }
    }

    /// Moves a window to the top of the stack.
    pub fn raise(&mut self, id: WindowId) {
        if let Some(index) = self.index(id) {
            let window = self.windows.remove(index);
            self.windows.push(window);
        }
    }

    pub fn remove(&mut self, id: WindowId) -> Option<ManagedWindow> {
        let index = self.index(id)?;
        Some(self.windows.remove(index))
    }

    pub fn get(&self, id: WindowId) -> Option<&ManagedWindow> {
        self.windows.iter().find(|window| window.id == id)
    }

    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut ManagedWindow> {
        self.windows.iter_mut().find(|window| window.id == id)
    }

    /// The window whose root surface is `surface`, placed or not.
    pub fn find_root(&self, surface: &WlSurface) -> Option<&ManagedWindow> {
        self.windows
            .iter()
            .find(|window| window.wl_surface().as_ref() == Some(surface))
    }

    pub fn find_x11(&self, surface: &X11Surface) -> Option<&ManagedWindow> {
        self.windows
            .iter()
            .find(|window| window.x11_surface() == Some(surface))
    }

    /// The topmost window the focus policy may focus.
    pub fn topmost_focusable(&self) -> Option<&ManagedWindow> {
        self.windows
            .iter()
            .rev()
            .find(|window| window.is_focusable())
    }

    /// Every window, placed or not, bottom to top.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ManagedWindow> {
        self.windows.iter()
    }

    /// Every placed window, bottom to top.
    pub fn placed(&self) -> impl DoubleEndedIterator<Item = &ManagedWindow> {
        self.windows.iter().filter(|window| window.is_placed())
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// The topmost surface under a global point, respecting input regions,
    /// with popups and subsurfaces resolved to the exact surface hit.
    pub fn surface_under(&self, point: Point<f64, Logical>) -> Option<SurfaceHit> {
        self.windows.iter().rev().find_map(|window| {
            let location = window.location?;
            let (surface, offset) = window
                .window
                .surface_under(point - location.to_f64(), WindowSurfaceType::ALL)?;
            Some(SurfaceHit {
                window: window.id,
                surface,
                origin: (location + offset).to_f64(),
            })
        })
    }

    /// Removes windows whose client is gone. Graceful destruction removes a
    /// window immediately through its protocol callback; this catches abrupt
    /// client death, after which those callbacks alone left dead windows
    /// behind.
    pub fn reap_dead(&mut self) -> Vec<ManagedWindow> {
        let (dead, alive) = std::mem::take(&mut self.windows)
            .into_iter()
            .partition(|window| !window.window.alive());
        self.windows = alive;
        dead
    }

    fn index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|window| window.id == id)
    }
}

/// The initial-placement rule: where a new window goes. It is the only place
/// that decides this, and it is called exactly once per mapping.
///
/// `geometry` is the window geometry in window-local coordinates (its size,
/// and the inset of its visible frame inside the root surface). The target
/// output is the one containing the pointer, or the first output in layout
/// order when the pointer is on none. On each axis independently, a frame
/// that fits is centred on that output; a frame larger than the output
/// starts at the output's edge instead, so its title bar and left edge stay
/// reachable. With no output at all, the frame starts at the origin.
///
/// The result is the window's location (the root surface origin), so the
/// visible frame rather than the shadow is what gets centred. The rule never
/// moves any other window.
pub fn initial_location(
    geometry: Rectangle<i32, Logical>,
    layout: &OutputLayout,
    pointer: Point<f64, Logical>,
) -> Point<i32, Logical> {
    let frame_origin = layout
        .output_at(pointer)
        .or_else(|| layout.outputs().first())
        .map_or_else(
            || Point::from((0, 0)),
            |output| {
                let area = output.rect();
                (
                    centred_start(area.loc.x, area.size.w, geometry.size.w),
                    centred_start(area.loc.y, area.size.h, geometry.size.h),
                )
                    .into()
            },
        );
    frame_origin - geometry.loc
}

/// Saturates like the layout it reads: an output may sit at the far end of
/// the coordinate range.
fn centred_start(start: i32, extent: i32, size: i32) -> i32 {
    if size <= extent {
        start.saturating_add(extent.saturating_sub(size) / 2)
    } else {
        start
    }
}

/// The keep-reachable rule, applied to every placed, managed window after
/// each output layout change: a window whose visible frame no longer
/// intersects any output is placed again by [`initial_location`], so a
/// window can never be stranded where no display shows it. Returns the new
/// location, or `None` when the window keeps its location.
///
/// Windows that still intersect an output stay exactly where they are, even
/// if mostly off-screen: rearranging displays moves nothing that is still
/// reachable. A layout without any output moves nothing either; the rule
/// runs again when outputs return.
pub fn location_to_keep_reachable(
    location: Point<i32, Logical>,
    geometry: Rectangle<i32, Logical>,
    layout: &OutputLayout,
    pointer: Point<f64, Logical>,
) -> Option<Point<i32, Logical>> {
    if layout.is_empty() {
        return None;
    }
    // A window that has not drawn its frame yet still occupies its location.
    let frame = Rectangle::new(
        location + geometry.loc,
        (geometry.size.w.max(1), geometry.size.h.max(1)).into(),
    );
    let reachable = layout
        .outputs()
        .iter()
        .any(|output| output.rect().overlaps(frame));
    (!reachable).then(|| initial_location(geometry, layout, pointer))
}

#[cfg(test)]
mod tests {
    use nkdhr_ipc::{OutputPlacement, OutputPlacements};

    use super::*;
    use crate::outputs::ConnectedOutput;

    fn layout(outputs: &[(&str, i32, i32, i32, i32)]) -> OutputLayout {
        let config = outputs
            .iter()
            .map(|(name, x, y, _, _)| {
                (
                    (*name).to_owned(),
                    OutputPlacement {
                        x: *x,
                        y: *y,
                        scale: 1.0,
                    },
                )
            })
            .collect::<OutputPlacements>();
        let connected = outputs
            .iter()
            .map(|(name, _, _, width, height)| ConnectedOutput {
                name: (*name).to_owned(),
                physical_size: (*width, *height).into(),
            })
            .collect::<Vec<_>>();
        OutputLayout::resolve(&config, &connected)
    }

    fn frame(width: i32, height: i32) -> Rectangle<i32, Logical> {
        Rectangle::from_size((width, height).into())
    }

    #[test]
    fn a_window_is_centred_on_the_output_under_the_pointer() {
        let layout = layout(&[("A", 0, 0, 1920, 1080), ("B", 1920, 0, 2560, 1440)]);

        assert_eq!(
            initial_location(frame(800, 600), &layout, (100.0, 100.0).into()),
            (560, 240).into()
        );
        assert_eq!(
            initial_location(frame(801, 601), &layout, (2000.0, 100.0).into()),
            (1920 + 879, 419).into()
        );
    }

    #[test]
    fn an_oversized_axis_starts_at_the_output_edge() {
        let layout = layout(&[("A", 0, 0, 1920, 1080), ("B", 1920, 0, 1280, 1024)]);

        // Too tall only: centred horizontally, top-aligned.
        assert_eq!(
            initial_location(frame(800, 1200), &layout, (2000.0, 10.0).into()),
            (1920 + 240, 0).into()
        );
        // Too wide only: left-aligned, centred vertically.
        assert_eq!(
            initial_location(frame(1500, 24), &layout, (2000.0, 10.0).into()),
            (1920, 500).into()
        );
        // Too large in both directions: the output's top-left corner.
        assert_eq!(
            initial_location(frame(3000, 2000), &layout, (2000.0, 10.0).into()),
            (1920, 0).into()
        );
    }

    #[test]
    fn the_visible_frame_is_centred_not_the_shadow() {
        let layout = layout(&[("A", 0, 0, 1000, 1000)]);
        let geometry = Rectangle::new((20, 12).into(), (400, 300).into());

        let location = initial_location(geometry, &layout, (10.0, 10.0).into());

        assert_eq!(location, (280, 338).into());
        assert_eq!(location + geometry.loc, (300, 350).into());
    }

    #[test]
    fn a_pointer_outside_every_output_falls_back_to_the_first_output() {
        let layout = layout(&[("B", 1000, 0, 1000, 1000), ("A", 0, 500, 1000, 500)]);

        // The pointer sits in the gap above output A; A is first in layout
        // order because configured outputs are ordered by name.
        assert_eq!(layout.outputs()[0].name, "A");
        assert_eq!(
            initial_location(frame(200, 100), &layout, (500.0, 100.0).into()),
            (400, 700).into()
        );
    }

    #[test]
    fn placement_saturates_on_an_output_at_the_end_of_the_coordinate_range() {
        // Any `i32` position is valid configuration; centring on an output
        // that reaches past `i32::MAX` must not overflow.
        let layout = layout(&[
            ("A", 0, 0, 1920, 1080),
            ("B", i32::MAX - 100, 0, 1920, 1080),
        ]);
        let start = layout.output("B").unwrap().global_location.x;

        let location = initial_location(frame(800, 600), &layout, (f64::from(start), 10.0).into());

        assert_eq!(location, (i32::MAX, 240).into());
    }

    #[test]
    fn without_outputs_the_frame_starts_at_the_origin() {
        let layout = OutputLayout::default();
        let geometry = Rectangle::new((8, 8).into(), (640, 480).into());

        assert_eq!(
            initial_location(geometry, &layout, (300.0, 300.0).into()),
            (-8, -8).into()
        );
    }

    #[test]
    fn windows_that_still_intersect_an_output_keep_their_location() {
        let layout = layout(&[("A", 0, 0, 1920, 1080)]);

        // Fully visible.
        assert_eq!(
            location_to_keep_reachable(
                (100, 100).into(),
                frame(800, 600),
                &layout,
                (0.0, 0.0).into()
            ),
            None
        );
        // Mostly off-screen, but one column of its frame is still visible.
        assert_eq!(
            location_to_keep_reachable(
                (1919, -500).into(),
                frame(800, 600),
                &layout,
                (0.0, 0.0).into()
            ),
            None
        );
    }

    #[test]
    fn stranded_windows_are_placed_again_by_the_initial_rule() {
        // The window was on an output to the right that has been unplugged.
        let layout = layout(&[("A", 0, 0, 1920, 1080)]);
        let geometry = Rectangle::new((10, 10).into(), (800, 600).into());

        assert_eq!(
            location_to_keep_reachable((2500, 100).into(), geometry, &layout, (50.0, 50.0).into()),
            Some((550, 230).into())
        );
        // Touching the output's far edge is not intersecting it.
        assert_eq!(
            location_to_keep_reachable((1910, 0).into(), geometry, &layout, (50.0, 50.0).into()),
            Some((550, 230).into())
        );
    }

    #[test]
    fn a_layout_without_outputs_moves_nothing() {
        assert_eq!(
            location_to_keep_reachable(
                (5000, 5000).into(),
                frame(800, 600),
                &OutputLayout::default(),
                (0.0, 0.0).into()
            ),
            None
        );
    }
}

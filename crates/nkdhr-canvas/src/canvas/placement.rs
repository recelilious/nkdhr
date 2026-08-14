//! Modal eight-way window placement shared by launcher and workspace moves.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::workspace::WorkspaceId;
use super::world::{Viewport, World};

pub const RELEASE_SETTLE: Duration = Duration::from_millis(110);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlacementDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementVector {
    pub x: i8,
    pub y: i8,
}

#[derive(Debug, Clone, Default)]
pub struct HeldDirections {
    keys: BTreeSet<PlacementDirection>,
}

impl HeldDirections {
    pub fn set(&mut self, direction: PlacementDirection, pressed: bool) {
        if pressed {
            self.keys.insert(direction);
        } else {
            self.keys.remove(&direction);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn vector(&self) -> PlacementVector {
        PlacementVector {
            x: i8::from(self.keys.contains(&PlacementDirection::Right))
                - i8::from(self.keys.contains(&PlacementDirection::Left)),
            y: i8::from(self.keys.contains(&PlacementDirection::Down))
                - i8::from(self.keys.contains(&PlacementDirection::Up)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlacementGeometry {
    pub viewport: Viewport,
    pub canvas_anchor: Point<f64, Logical>,
    /// The physical display chosen at session start, in output-group logical
    /// coordinates. Center placement never clamps to this rectangle.
    pub display_rect: Rectangle<i32, Logical>,
    pub reference: Option<Rectangle<f64, World>>,
    pub gap: f64,
}

impl PlacementGeometry {
    pub fn display_center_world(&self) -> Point<f64, World> {
        let center: Point<f64, Logical> = (
            f64::from(self.display_rect.loc.x) + f64::from(self.display_rect.size.w) / 2.0,
            f64::from(self.display_rect.loc.y) + f64::from(self.display_rect.size.h) / 2.0,
        )
            .into();
        self.viewport
            .group_logical_to_world(center, self.canvas_anchor)
    }

    pub fn position_for(
        &self,
        vector: PlacementVector,
        window_size: Size<f64, World>,
    ) -> Point<f64, World> {
        let center = self.display_center_world();
        let reference = self
            .reference
            .unwrap_or_else(|| Rectangle::new(center, Size::<f64, World>::from((0.0, 0.0))));
        let reference_center: Point<f64, World> = (
            reference.loc.x + reference.size.w / 2.0,
            reference.loc.y + reference.size.h / 2.0,
        )
            .into();
        let x = match vector.x {
            -1 => reference.loc.x - self.gap - window_size.w,
            1 => reference.loc.x + reference.size.w + self.gap,
            _ => reference_center.x - window_size.w / 2.0,
        };
        let y = match vector.y {
            -1 => reference.loc.y - self.gap - window_size.h,
            1 => reference.loc.y + reference.size.h + self.gap,
            _ => reference_center.y - window_size.h / 2.0,
        };
        (x, y).into()
    }

    pub fn position_at_pointer(
        &self,
        group_logical: Point<f64, Logical>,
        window_size: Size<f64, World>,
    ) -> Point<f64, World> {
        let center = self
            .viewport
            .group_logical_to_world(group_logical, self.canvas_anchor);
        (
            center.x - window_size.w / 2.0,
            center.y - window_size.h / 2.0,
        )
            .into()
    }

    pub fn edge_vector(&self, point: Point<f64, Logical>) -> Option<PlacementVector> {
        let width = f64::from(self.display_rect.size.w.max(1));
        let height = f64::from(self.display_rect.size.h.max(1));
        let local_x = point.x - f64::from(self.display_rect.loc.x);
        let local_y = point.y - f64::from(self.display_rect.loc.y);
        let edge_x = (width * 0.12).clamp(24.0, 96.0);
        let edge_y = (height * 0.12).clamp(24.0, 96.0);
        let x = if local_x <= edge_x {
            -1
        } else if local_x >= width - edge_x {
            1
        } else {
            0
        };
        let y = if local_y <= edge_y {
            -1
        } else if local_y >= height - edge_y {
            1
        } else {
            0
        };
        (x != 0 || y != 0).then_some(PlacementVector { x, y })
    }
}

#[derive(Debug, Clone)]
pub struct PlacementSession {
    pub surface: WlSurface,
    pub source_workspace: WorkspaceId,
    pub source_canvas: String,
    pub source_position: Point<f64, World>,
    pub target_workspace: WorkspaceId,
    pub target_canvas: String,
    pub geometry: PlacementGeometry,
    held: HeldDirections,
    used_directions: bool,
    settle_at: Option<Instant>,
}

impl PlacementSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        surface: WlSurface,
        source_workspace: WorkspaceId,
        source_canvas: String,
        source_position: Point<f64, World>,
        target_workspace: WorkspaceId,
        target_canvas: String,
        geometry: PlacementGeometry,
    ) -> Self {
        Self {
            surface,
            source_workspace,
            source_canvas,
            source_position,
            target_workspace,
            target_canvas,
            geometry,
            held: HeldDirections::default(),
            used_directions: false,
            settle_at: None,
        }
    }

    pub fn direction(&mut self, direction: PlacementDirection, pressed: bool, now: Instant) {
        self.held.set(direction, pressed);
        self.used_directions = true;
        self.settle_at = if self.held.is_empty() {
            Some(now + RELEASE_SETTLE)
        } else {
            None
        };
    }

    pub fn vector(&self) -> PlacementVector {
        self.held.vector()
    }

    pub fn should_settle(&self, now: Instant) -> bool {
        self.used_directions && self.settle_at.is_some_and(|deadline| now >= deadline)
    }

    pub fn settle_pending(&self) -> bool {
        self.settle_at.is_some()
    }

    pub fn cancel_settle(&mut self) {
        self.settle_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> PlacementGeometry {
        PlacementGeometry {
            viewport: Viewport::WORK,
            canvas_anchor: (500.0, 400.0).into(),
            display_rect: Rectangle::new((0, 0).into(), (1000, 800).into()),
            reference: Some(Rectangle::new((100.0, 200.0).into(), (300.0, 200.0).into())),
            gap: 32.0,
        }
    }

    #[test]
    fn opposing_keys_cancel_then_release_reveals_the_remaining_direction() {
        let mut held = HeldDirections::default();
        held.set(PlacementDirection::Left, true);
        held.set(PlacementDirection::Up, true);
        assert_eq!(held.vector(), PlacementVector { x: -1, y: -1 });
        held.set(PlacementDirection::Right, true);
        assert_eq!(held.vector(), PlacementVector { x: 0, y: -1 });
        held.set(PlacementDirection::Left, false);
        assert_eq!(held.vector(), PlacementVector { x: 1, y: -1 });
    }

    #[test]
    fn relative_placement_aligns_centers_and_keeps_edges_unclamped() {
        let geometry = geometry();
        let size = Size::<f64, World>::from((200.0, 100.0));
        assert_eq!(
            geometry.position_for(PlacementVector { x: -1, y: 0 }, size),
            (-132.0, 250.0).into()
        );
        assert_eq!(
            geometry.position_for(PlacementVector { x: 1, y: -1 }, size),
            (432.0, 68.0).into()
        );
    }

    #[test]
    fn edge_zones_cover_all_eight_directions_but_not_the_center() {
        let geometry = geometry();
        assert_eq!(
            geometry.edge_vector((2.0, 2.0).into()),
            Some(PlacementVector { x: -1, y: -1 })
        );
        assert_eq!(
            geometry.edge_vector((500.0, 2.0).into()),
            Some(PlacementVector { x: 0, y: -1 })
        );
        assert_eq!(geometry.edge_vector((500.0, 400.0).into()), None);
    }
}

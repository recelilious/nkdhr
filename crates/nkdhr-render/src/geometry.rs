use std::fmt;

/// A point in logical UI coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A rectangle in logical UI coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.right().is_finite()
            && self.bottom().is_finite()
    }

    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.y >= self.y && point.x < self.right() && point.y < self.bottom()
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > x && bottom > y).then(|| Self::new(x, y, right - x, bottom - y))
    }

    pub fn inset(self, amount: f32) -> Self {
        Self::new(
            self.x + amount,
            self.y + amount,
            (self.width - amount * 2.0).max(0.0),
            (self.height - amount * 2.0).max(0.0),
        )
    }

    pub fn expand(self, amount: f32) -> Self {
        Self::new(
            self.x - amount,
            self.y - amount,
            self.width + amount * 2.0,
            self.height + amount * 2.0,
        )
    }
}

/// Independent corner radii in top-left, top-right, bottom-right,
/// bottom-left order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CornerRadii {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

impl CornerRadii {
    pub const ZERO: Self = Self::all(0.0);

    pub const fn all(radius: f32) -> Self {
        Self {
            top_left: radius,
            top_right: radius,
            bottom_right: radius,
            bottom_left: radius,
        }
    }

    pub const fn new(top_left: f32, top_right: f32, bottom_right: f32, bottom_left: f32) -> Self {
        Self {
            top_left,
            top_right,
            bottom_right,
            bottom_left,
        }
    }

    pub fn is_valid(self) -> bool {
        [
            self.top_left,
            self.top_right,
            self.bottom_right,
            self.bottom_left,
        ]
        .into_iter()
        .all(|radius| radius.is_finite() && radius >= 0.0)
    }

    /// Apply the CSS corner-overlap rule so adjacent radii always fit.
    pub fn normalized(self, rect: Rect) -> Self {
        let ratios = [
            ratio(rect.width, self.top_left + self.top_right),
            ratio(rect.width, self.bottom_left + self.bottom_right),
            ratio(rect.height, self.top_left + self.bottom_left),
            ratio(rect.height, self.top_right + self.bottom_right),
        ];
        let scale = ratios.into_iter().fold(1.0_f32, f32::min).min(1.0);
        Self::new(
            self.top_left * scale,
            self.top_right * scale,
            self.bottom_right * scale,
            self.bottom_left * scale,
        )
    }

    pub(crate) fn inset(self, amount: f32) -> Self {
        Self::new(
            (self.top_left - amount).max(0.0),
            (self.top_right - amount).max(0.0),
            (self.bottom_right - amount).max(0.0),
            (self.bottom_left - amount).max(0.0),
        )
    }

    pub(crate) fn expand(self, amount: f32) -> Self {
        Self::new(
            (self.top_left + amount).max(0.0),
            (self.top_right + amount).max(0.0),
            (self.bottom_right + amount).max(0.0),
            (self.bottom_left + amount).max(0.0),
        )
    }

    pub(crate) fn as_array(self) -> [f32; 4] {
        [
            self.top_left,
            self.top_right,
            self.bottom_right,
            self.bottom_left,
        ]
    }
}

fn ratio(limit: f32, sum: f32) -> f32 {
    if sum > 0.0 { limit / sum } else { 1.0 }
}

/// A two-dimensional affine transform.
///
/// Points map as `x' = a*x + c*y + tx`, `y' = b*x + d*y + ty`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub const fn translation(x: f32, y: f32) -> Self {
        Self {
            tx: x,
            ty: y,
            ..Self::IDENTITY
        }
    }

    pub const fn scale(x: f32, y: f32) -> Self {
        Self {
            a: x,
            d: y,
            ..Self::IDENTITY
        }
    }

    pub fn rotation(radians: f32) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            tx: 0.0,
            ty: 0.0,
        }
    }

    pub fn is_finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.tx, self.ty]
            .into_iter()
            .all(f32::is_finite)
    }

    pub fn is_axis_aligned(self) -> bool {
        self.b.abs() <= f32::EPSILON && self.c.abs() <= f32::EPSILON
    }

    /// Compose transforms so `self.concat(child)` applies `child` first.
    pub fn concat(self, child: Self) -> Self {
        Self {
            a: self.a * child.a + self.c * child.b,
            b: self.b * child.a + self.d * child.b,
            c: self.a * child.c + self.c * child.d,
            d: self.b * child.c + self.d * child.d,
            tx: self.a * child.tx + self.c * child.ty + self.tx,
            ty: self.b * child.tx + self.d * child.ty + self.ty,
        }
    }

    pub fn map_point(self, point: Point) -> Point {
        Point::new(
            self.a * point.x + self.c * point.y + self.tx,
            self.b * point.x + self.d * point.y + self.ty,
        )
    }

    pub fn map_rect_bounds(self, rect: Rect) -> Rect {
        let points = [
            self.map_point(Point::new(rect.x, rect.y)),
            self.map_point(Point::new(rect.right(), rect.y)),
            self.map_point(Point::new(rect.right(), rect.bottom())),
            self.map_point(Point::new(rect.x, rect.bottom())),
        ];
        let min_x = points
            .iter()
            .map(|point| point.x)
            .fold(f32::INFINITY, f32::min);
        let min_y = points
            .iter()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
        let max_x = points
            .iter()
            .map(|point| point.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let max_y = points
            .iter()
            .map(|point| point.y)
            .fold(f32::NEG_INFINITY, f32::max);
        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    pub fn inverse(self) -> Option<Self> {
        let determinant = self.a * self.d - self.b * self.c;
        if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
            return None;
        }
        let inverse = 1.0 / determinant;
        let a = self.d * inverse;
        let b = -self.b * inverse;
        let c = -self.c * inverse;
        let d = self.a * inverse;
        let result = Self {
            a,
            b,
            c,
            d,
            tx: -(a * self.tx + c * self.ty),
            ty: -(b * self.tx + d * self.ty),
        };
        result.is_finite().then_some(result)
    }

    pub(crate) fn minimum_scale(self) -> f32 {
        let x = self.a.hypot(self.b);
        let y = self.c.hypot(self.d);
        x.min(y)
    }
}

/// An RGBA color with normalized, straight-alpha components.
#[derive(Clone, Copy, PartialEq)]
pub struct Color([f32; 4]);

impl fmt::Debug for Color {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Color").field(&self.0).finish()
    }
}

impl Color {
    pub const TRANSPARENT: Self = Self([0.0; 4]);
    pub const WHITE: Self = Self([1.0; 4]);

    pub fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Option<Self> {
        let components = [red, green, blue, alpha];
        components
            .into_iter()
            .all(|component| component.is_finite() && (0.0..=1.0).contains(&component))
            .then_some(Self(components))
    }

    pub const fn from_srgba8(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self([
            red as f32 / 255.0,
            green as f32 / 255.0,
            blue as f32 / 255.0,
            alpha as f32 / 255.0,
        ])
    }

    pub const fn components(self) -> [f32; 4] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_overlapping_corner_radii() {
        let radii =
            CornerRadii::new(80.0, 40.0, 20.0, 60.0).normalized(Rect::new(0.0, 0.0, 100.0, 50.0));
        assert!((radii.top_left - 28.571_43).abs() < 0.0001);
        assert!((radii.top_right - 14.285_715).abs() < 0.0001);
        assert!((radii.bottom_right - 7.142_857).abs() < 0.0001);
        assert!((radii.bottom_left - 21.428_572).abs() < 0.0001);
    }

    #[test]
    fn affine_composition_and_inverse_round_trip() {
        let transform = Transform::translation(20.0, -8.0)
            .concat(Transform::rotation(0.4))
            .concat(Transform::scale(2.0, 3.0));
        let point = Point::new(5.0, 7.0);
        let mapped = transform.map_point(point);
        let restored = transform.inverse().unwrap().map_point(mapped);
        assert!((restored.x - point.x).abs() < 0.0001);
        assert!((restored.y - point.y).abs() < 0.0001);
    }

    #[test]
    fn derived_non_finite_geometry_is_rejected() {
        assert!(!Rect::new(f32::MAX, 0.0, f32::MAX, 1.0).is_finite());
        assert!(
            Transform {
                tx: f32::MAX,
                ..Transform::scale(f32::MIN_POSITIVE, 1.0)
            }
            .inverse()
            .is_none()
        );
    }
}

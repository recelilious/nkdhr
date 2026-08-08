use std::fmt;

use crate::{Color, CornerRadii, Rect, Sampling, TextureId, Transform};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread: f32,
    pub color: Color,
}

impl Shadow {
    pub const fn new(
        offset_x: f32,
        offset_y: f32,
        blur_radius: f32,
        spread: f32,
        color: Color,
    ) -> Self {
        Self {
            offset_x,
            offset_y,
            blur_radius,
            spread,
            color,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShapeStyle {
    Fill(Color),
    Border { width: f32, color: Color },
    Shadow(Shadow),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapePrimitive {
    pub rect: Rect,
    pub radii: CornerRadii,
    pub style: ShapeStyle,
    pub transform: Transform,
    pub clip: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TexturePrimitive {
    pub rect: Rect,
    pub texture: TextureId,
    pub source: Option<Rect>,
    pub opacity: f32,
    pub sampling: Sampling,
    pub transform: Transform,
    pub clip: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Primitive {
    Shape(ShapePrimitive),
    Texture(TexturePrimitive),
}

/// Immutable renderer-independent commands in painter's order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayList {
    primitives: Vec<Primitive>,
}

impl DisplayList {
    pub fn primitives(&self) -> &[Primitive] {
        &self.primitives
    }

    pub fn is_empty(&self) -> bool {
        self.primitives.is_empty()
    }

    pub fn len(&self) -> usize {
        self.primitives.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    NonFiniteGeometry,
    NegativeSize,
    InvalidRadius,
    InvalidBorderWidth,
    InvalidShadow,
    InvalidOpacity,
    SingularTransform,
    NonAxisAlignedClip,
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NonFiniteGeometry => "geometry and transforms must be finite",
            Self::NegativeSize => "rectangle sizes must not be negative",
            Self::InvalidRadius => "corner radii must be finite and non-negative",
            Self::InvalidBorderWidth => "border width must be finite and non-negative",
            Self::InvalidShadow => {
                "shadow offset, blur and spread must be finite; blur must be non-negative"
            }
            Self::InvalidOpacity => "opacity must be finite and between zero and one",
            Self::SingularTransform => "transform must be invertible",
            Self::NonAxisAlignedClip => "clips may only be translated or scaled",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for BuildError {}

/// Validating recorder for a [`DisplayList`].
#[derive(Debug)]
pub struct DisplayListBuilder {
    primitives: Vec<Primitive>,
    transforms: Vec<Transform>,
    clips: Vec<Option<Rect>>,
}

impl Default for DisplayListBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayListBuilder {
    pub fn new() -> Self {
        Self {
            primitives: Vec::new(),
            transforms: vec![Transform::IDENTITY],
            clips: vec![None],
        }
    }

    pub fn rect(&mut self, rect: Rect, color: Color) -> Result<(), BuildError> {
        self.shape(rect, CornerRadii::ZERO, ShapeStyle::Fill(color))
    }

    pub fn rounded_rect(
        &mut self,
        rect: Rect,
        radii: CornerRadii,
        color: Color,
    ) -> Result<(), BuildError> {
        self.shape(rect, radii, ShapeStyle::Fill(color))
    }

    pub fn border(
        &mut self,
        rect: Rect,
        radii: CornerRadii,
        width: f32,
        color: Color,
    ) -> Result<(), BuildError> {
        if !width.is_finite() || width < 0.0 {
            return Err(BuildError::InvalidBorderWidth);
        }
        self.shape(rect, radii, ShapeStyle::Border { width, color })
    }

    pub fn shadow(
        &mut self,
        rect: Rect,
        radii: CornerRadii,
        shadow: Shadow,
    ) -> Result<(), BuildError> {
        if ![
            shadow.offset_x,
            shadow.offset_y,
            shadow.blur_radius,
            shadow.spread,
        ]
        .into_iter()
        .all(f32::is_finite)
            || shadow.blur_radius < 0.0
        {
            return Err(BuildError::InvalidShadow);
        }
        let shadow_rect = Rect::new(
            rect.x + shadow.offset_x,
            rect.y + shadow.offset_y,
            rect.width,
            rect.height,
        )
        .expand(shadow.spread);
        if !shadow_rect.is_finite() {
            return Err(BuildError::InvalidShadow);
        }
        self.shape(rect, radii, ShapeStyle::Shadow(shadow))
    }

    pub fn texture(
        &mut self,
        rect: Rect,
        texture: TextureId,
        source: Option<Rect>,
        opacity: f32,
        sampling: Sampling,
    ) -> Result<(), BuildError> {
        validate_rect(rect)?;
        if let Some(source) = source {
            validate_rect(source)?;
        }
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(BuildError::InvalidOpacity);
        }
        if rect.is_empty() || opacity == 0.0 {
            return Ok(());
        }
        self.primitives.push(Primitive::Texture(TexturePrimitive {
            rect,
            texture,
            source,
            opacity,
            sampling,
            transform: self.current_transform(),
            clip: self.current_clip(),
        }));
        Ok(())
    }

    pub fn with_transform<T>(
        &mut self,
        transform: Transform,
        record: impl FnOnce(&mut Self) -> Result<T, BuildError>,
    ) -> Result<T, BuildError> {
        if !transform.is_finite() {
            return Err(BuildError::NonFiniteGeometry);
        }
        let combined = self.current_transform().concat(transform);
        if !combined.is_finite() {
            return Err(BuildError::NonFiniteGeometry);
        }
        if combined.inverse().is_none() {
            return Err(BuildError::SingularTransform);
        }
        self.transforms.push(combined);
        let result = record(self);
        self.transforms.pop();
        result
    }

    pub fn with_clip<T>(
        &mut self,
        clip: Rect,
        record: impl FnOnce(&mut Self) -> Result<T, BuildError>,
    ) -> Result<T, BuildError> {
        validate_rect(clip)?;
        let transform = self.current_transform();
        if !transform.is_axis_aligned() {
            return Err(BuildError::NonAxisAlignedClip);
        }
        let mapped = transform.map_rect_bounds(clip);
        let combined = match self.current_clip() {
            Some(parent) => parent.intersect(mapped),
            None => (!mapped.is_empty()).then_some(mapped),
        };
        self.clips.push(combined);
        let before = self.primitives.len();
        let result = record(self);
        if combined.is_none() {
            self.primitives.truncate(before);
        }
        self.clips.pop();
        result
    }

    pub fn finish(self) -> DisplayList {
        debug_assert_eq!(self.transforms.len(), 1);
        debug_assert_eq!(self.clips.len(), 1);
        DisplayList {
            primitives: self.primitives,
        }
    }

    fn shape(
        &mut self,
        rect: Rect,
        radii: CornerRadii,
        style: ShapeStyle,
    ) -> Result<(), BuildError> {
        validate_rect(rect)?;
        if !radii.is_valid() {
            return Err(BuildError::InvalidRadius);
        }
        if rect.is_empty() {
            return Ok(());
        }
        self.primitives.push(Primitive::Shape(ShapePrimitive {
            rect,
            radii: radii.normalized(rect),
            style,
            transform: self.current_transform(),
            clip: self.current_clip(),
        }));
        Ok(())
    }

    fn current_transform(&self) -> Transform {
        *self
            .transforms
            .last()
            .expect("builder always has a transform")
    }

    fn current_clip(&self) -> Option<Rect> {
        *self.clips.last().expect("builder always has a clip entry")
    }
}

fn validate_rect(rect: Rect) -> Result<(), BuildError> {
    if !rect.is_finite() {
        Err(BuildError::NonFiniteGeometry)
    } else if rect.width < 0.0 || rect.height < 0.0 {
        Err(BuildError::NegativeSize)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white() -> Color {
        Color::from_srgba8(255, 255, 255, 255)
    }

    #[test]
    fn state_scopes_restore_after_errors() {
        let mut builder = DisplayListBuilder::new();
        let result = builder.with_transform(Transform::translation(10.0, 20.0), |builder| {
            builder.rect(Rect::new(0.0, 0.0, -1.0, 2.0), white())
        });
        assert_eq!(result, Err(BuildError::NegativeSize));
        builder
            .rect(Rect::new(0.0, 0.0, 2.0, 2.0), white())
            .unwrap();
        let Primitive::Shape(shape) = builder.finish().primitives()[0] else {
            panic!("expected shape");
        };
        assert_eq!(shape.transform, Transform::IDENTITY);
    }

    #[test]
    fn nested_clips_intersect_in_target_space() {
        let mut builder = DisplayListBuilder::new();
        builder
            .with_clip(Rect::new(0.0, 0.0, 20.0, 20.0), |builder| {
                builder.with_transform(Transform::translation(10.0, 10.0), |builder| {
                    builder.with_clip(Rect::new(0.0, 0.0, 20.0, 20.0), |builder| {
                        builder.rect(Rect::new(0.0, 0.0, 30.0, 30.0), white())
                    })
                })
            })
            .unwrap();
        let Primitive::Shape(shape) = builder.finish().primitives()[0] else {
            panic!("expected shape");
        };
        assert_eq!(shape.clip, Some(Rect::new(10.0, 10.0, 10.0, 10.0)));
    }

    #[test]
    fn rotated_clips_are_explicitly_rejected() {
        let mut builder = DisplayListBuilder::new();
        let result = builder.with_transform(Transform::rotation(0.2), |builder| {
            builder.with_clip(Rect::new(0.0, 0.0, 10.0, 10.0), |_| Ok(()))
        });
        assert_eq!(result, Err(BuildError::NonAxisAlignedClip));
    }

    #[test]
    fn arithmetic_overflow_is_rejected_during_recording() {
        let mut builder = DisplayListBuilder::new();
        assert_eq!(
            builder.rect(Rect::new(f32::MAX, 0.0, f32::MAX, 1.0), white()),
            Err(BuildError::NonFiniteGeometry)
        );
        assert_eq!(
            builder.with_transform(Transform::translation(f32::MAX, 0.0), |builder| {
                builder.with_transform(Transform::translation(f32::MAX, 0.0), |_| Ok(()))
            }),
            Err(BuildError::NonFiniteGeometry)
        );
        assert_eq!(
            builder.shadow(
                Rect::new(f32::MAX, 0.0, 1.0, 1.0),
                CornerRadii::ZERO,
                Shadow::new(f32::MAX, 0.0, 0.0, 0.0, white()),
            ),
            Err(BuildError::InvalidShadow)
        );
    }
}

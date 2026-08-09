//! Deterministic scalar renderer used as the golden-image oracle.
//!
//! This module intentionally favors readable, stable math over speed. Product
//! UI frames use the GLES backend.

use std::fmt;

use crate::{
    AlphaMode, BackdropBlurPrimitive, Color, CornerRadii, DisplayList, Point, Primitive, Rect,
    Sampling, ShapePrimitive, ShapeStyle, TextureAsset, TextureError, TextureFormat, TextureId,
    TexturePrimitive, TextureStore,
};

const BLUR_WEIGHTS: [f32; 5] = [
    0.227_027_03,
    0.194_594_59,
    0.121_621_62,
    0.054_054_055,
    0.016_216_217,
];

#[derive(Debug)]
pub enum SoftwareRenderError {
    InvalidScale,
    SizeOverflow,
    Texture(TextureError),
    InvalidTextureSource(TextureId),
}

impl fmt::Display for SoftwareRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScale => write!(formatter, "render scale must be finite and positive"),
            Self::SizeOverflow => write!(formatter, "software target size overflows usize"),
            Self::Texture(error) => error.fmt(formatter),
            Self::InvalidTextureSource(id) => {
                write!(formatter, "source rectangle is outside texture {id:?}")
            }
        }
    }
}

impl std::error::Error for SoftwareRenderError {}

/// Premultiplied floating-point software target.
#[derive(Debug, Clone)]
pub struct SoftwareRenderer {
    width: u32,
    height: u32,
    pixels: Vec<[f32; 4]>,
}

impl SoftwareRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self, SoftwareRenderError> {
        let length = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(SoftwareRenderError::SizeOverflow)?;
        Ok(Self {
            width,
            height,
            pixels: vec![[0.0; 4]; length],
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn clear(&mut self, color: Color) {
        let [red, green, blue, alpha] = color.components();
        self.pixels
            .fill([red * alpha, green * alpha, blue * alpha, alpha]);
    }

    pub fn render(
        &mut self,
        display_list: &DisplayList,
        textures: &TextureStore,
        scale: f32,
    ) -> Result<(), SoftwareRenderError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(SoftwareRenderError::InvalidScale);
        }
        for primitive in display_list.primitives() {
            match primitive {
                Primitive::Shape(shape) => self.draw_shape(*shape, scale),
                Primitive::Texture(texture) => self.draw_texture(*texture, textures, scale)?,
                Primitive::BackdropBlur(blur) => self.draw_backdrop_blur(*blur, scale),
            }
        }
        Ok(())
    }

    /// Return straight-alpha RGBA8 pixels.
    pub fn rgba8(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.pixels.len() * 4);
        for [red, green, blue, alpha] in &self.pixels {
            let inverse_alpha = if *alpha > 0.0 { 1.0 / alpha } else { 0.0 };
            output.extend([
                to_u8(red * inverse_alpha),
                to_u8(green * inverse_alpha),
                to_u8(blue * inverse_alpha),
                to_u8(*alpha),
            ]);
        }
        output
    }

    /// Encode the target as a binary PPM after compositing over its existing
    /// background. Golden fixtures use opaque backgrounds.
    pub fn ppm(&self) -> Vec<u8> {
        let mut output = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for pixel in self.rgba8().chunks_exact(4) {
            output.extend_from_slice(&pixel[..3]);
        }
        output
    }

    fn draw_shape(&mut self, primitive: ShapePrimitive, scale: f32) {
        let (shape_rect, radii, draw_rect) = match primitive.style {
            ShapeStyle::Shadow(shadow) => {
                let shape_rect = Rect::new(
                    primitive.rect.x + shadow.offset_x,
                    primitive.rect.y + shadow.offset_y,
                    primitive.rect.width,
                    primitive.rect.height,
                )
                .expand(shadow.spread);
                if shape_rect.is_empty() {
                    return;
                }
                let radii = primitive.radii.expand(shadow.spread).normalized(shape_rect);
                let draw_rect = shape_rect.expand(shadow.blur_radius * 3.0 + 1.0 / scale);
                (shape_rect, radii, draw_rect)
            }
            _ => (
                primitive.rect,
                primitive.radii,
                primitive.rect.expand(1.0 / scale),
            ),
        };
        let inverse = primitive
            .transform
            .inverse()
            .expect("display-list transforms are validated");
        let effective_scale = (scale * primitive.transform.minimum_scale()).max(f32::EPSILON);
        let bounds = primitive.transform.map_rect_bounds(draw_rect);
        self.for_each_pixel(bounds, primitive.clip, scale, |target| {
            let local = inverse.map_point(target);
            let distance = rounded_distance(local, shape_rect, radii);
            match primitive.style {
                ShapeStyle::Fill(color) => (color, edge_coverage(distance, effective_scale)),
                ShapeStyle::Border { width, color } => {
                    let outer = edge_coverage(distance, effective_scale);
                    let inner_rect = shape_rect.inset(width);
                    let inner = if inner_rect.is_empty() {
                        0.0
                    } else {
                        edge_coverage(
                            rounded_distance(local, inner_rect, radii.inset(width)),
                            effective_scale,
                        )
                    };
                    (color, (outer - inner).clamp(0.0, 1.0))
                }
                ShapeStyle::Shadow(shadow) => {
                    let coverage = if shadow.blur_radius <= 0.0 {
                        edge_coverage(distance, effective_scale)
                    } else if distance <= 0.0 {
                        1.0
                    } else {
                        (-0.5 * (distance / shadow.blur_radius).powi(2)).exp()
                    };
                    (shadow.color, coverage)
                }
            }
        });
    }

    fn draw_texture(
        &mut self,
        primitive: TexturePrimitive,
        textures: &TextureStore,
        scale: f32,
    ) -> Result<(), SoftwareRenderError> {
        let asset = textures.get(primitive.texture).ok_or({
            SoftwareRenderError::Texture(TextureError::UnknownTexture(primitive.texture))
        })?;
        let source = validate_source(primitive.texture, primitive.source, asset)?;
        let inverse = primitive
            .transform
            .inverse()
            .expect("display-list transforms are validated");
        let bounds = primitive.transform.map_rect_bounds(primitive.rect);
        self.for_each_pixel(bounds, primitive.clip, scale, |target| {
            let local = inverse.map_point(target);
            if !primitive.rect.contains(local) {
                return (Color::TRANSPARENT, 0.0);
            }
            let u = (local.x - primitive.rect.x) / primitive.rect.width;
            let v = (local.y - primitive.rect.y) / primitive.rect.height;
            let sample = sample_texture(asset, source, u, v, primitive.sampling);
            if asset.format() == TextureFormat::Alpha8 {
                return (primitive.tint, sample[3] * primitive.opacity);
            }
            let alpha = match asset.alpha_mode() {
                AlphaMode::Opaque => 1.0,
                AlphaMode::Straight | AlphaMode::Premultiplied => sample[3],
            };
            let straight = match asset.alpha_mode() {
                AlphaMode::Premultiplied if alpha > 0.0 => [
                    sample[0] / alpha,
                    sample[1] / alpha,
                    sample[2] / alpha,
                    alpha,
                ],
                AlphaMode::Premultiplied => [0.0; 4],
                AlphaMode::Straight | AlphaMode::Opaque => [sample[0], sample[1], sample[2], alpha],
            };
            let tint = primitive.tint.components();
            let color = Color::new(
                straight[0] * tint[0],
                straight[1] * tint[1],
                straight[2] * tint[2],
                straight[3] * tint[3],
            )
            .unwrap_or(Color::TRANSPARENT);
            (color, primitive.opacity)
        });
        Ok(())
    }

    fn draw_backdrop_blur(&mut self, primitive: BackdropBlurPrimitive, scale: f32) {
        let radius = primitive.radius * scale * primitive.transform.minimum_scale();
        if radius <= f32::EPSILON {
            return;
        }

        // Filtering always reads the painter-order snapshot from immediately
        // before this primitive. Keeping the intermediate separate prevents a
        // blur from feeding its own output back into later samples.
        let source = self.pixels.clone();
        let horizontal = blur_horizontal(&source, self.width, self.height, radius);
        let inverse = primitive
            .transform
            .inverse()
            .expect("display-list transforms are validated");
        let bounds = primitive.transform.map_rect_bounds(primitive.rect);
        let Some((left, top, right, bottom)) =
            pixel_bounds(bounds, primitive.clip, scale, self.width, self.height)
        else {
            return;
        };
        let effective_scale = (scale * primitive.transform.minimum_scale()).max(f32::EPSILON);
        for y in top..bottom {
            for x in left..right {
                let target = Point::new((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
                if primitive.clip.is_some_and(|clip| !clip.contains(target)) {
                    continue;
                }
                let local = inverse.map_point(target);
                let coverage = edge_coverage(
                    rounded_distance(local, primitive.rect, primitive.radii),
                    effective_scale,
                );
                if coverage <= 0.0 {
                    continue;
                }
                let blurred = blur_sample(
                    &horizontal,
                    self.width,
                    self.height,
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    radius,
                    false,
                );
                let index = y as usize * self.width as usize + x as usize;
                self.pixels[index] = mix(self.pixels[index], blurred, coverage.clamp(0.0, 1.0));
            }
        }
    }

    fn for_each_pixel(
        &mut self,
        bounds: Rect,
        clip: Option<Rect>,
        scale: f32,
        mut shade: impl FnMut(Point) -> (Color, f32),
    ) {
        let Some((left, top, right, bottom)) =
            pixel_bounds(bounds, clip, scale, self.width, self.height)
        else {
            return;
        };
        for y in top..bottom {
            for x in left..right {
                let target = Point::new((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
                if clip.is_some_and(|clip| !clip.contains(target)) {
                    continue;
                }
                let (color, coverage) = shade(target);
                if coverage <= 0.0 {
                    continue;
                }
                let index = y as usize * self.width as usize + x as usize;
                blend(&mut self.pixels[index], color, coverage.clamp(0.0, 1.0));
            }
        }
    }
}

fn pixel_bounds(
    bounds: Rect,
    clip: Option<Rect>,
    scale: f32,
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let bounds = match clip {
        Some(clip) => bounds.intersect(clip),
        None => Some(bounds),
    }?;
    let left = (bounds.x * scale).floor().max(0.0) as u32;
    let top = (bounds.y * scale).floor().max(0.0) as u32;
    let right = (bounds.right() * scale).ceil().clamp(0.0, width as f32) as u32;
    let bottom = (bounds.bottom() * scale).ceil().clamp(0.0, height as f32) as u32;
    (right > left && bottom > top).then_some((left, top, right, bottom))
}

fn blur_horizontal(source: &[[f32; 4]], width: u32, height: u32, radius: f32) -> Vec<[f32; 4]> {
    let mut output = vec![[0.0; 4]; source.len()];
    for y in 0..height {
        for x in 0..width {
            output[y as usize * width as usize + x as usize] = blur_sample(
                source,
                width,
                height,
                x as f32 + 0.5,
                y as f32 + 0.5,
                radius,
                true,
            );
        }
    }
    output
}

fn blur_sample(
    pixels: &[[f32; 4]],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    radius: f32,
    horizontal: bool,
) -> [f32; 4] {
    let mut result = multiply(sample_pixels(pixels, width, height, x, y), BLUR_WEIGHTS[0]);
    let step = radius / 4.0;
    for (index, weight) in BLUR_WEIGHTS.iter().copied().enumerate().skip(1) {
        let offset = step * index as f32;
        let (negative_x, negative_y, positive_x, positive_y) = if horizontal {
            (x - offset, y, x + offset, y)
        } else {
            (x, y - offset, x, y + offset)
        };
        add_scaled(
            &mut result,
            sample_pixels(pixels, width, height, negative_x, negative_y),
            weight,
        );
        add_scaled(
            &mut result,
            sample_pixels(pixels, width, height, positive_x, positive_y),
            weight,
        );
    }
    result
}

fn sample_pixels(pixels: &[[f32; 4]], width: u32, height: u32, x: f32, y: f32) -> [f32; 4] {
    let pixel_x = x - 0.5;
    let pixel_y = y - 0.5;
    let x0_float = pixel_x.floor();
    let y0_float = pixel_y.floor();
    let tx = pixel_x - x0_float;
    let ty = pixel_y - y0_float;
    let clamp_x = |value: f32| value.clamp(0.0, width.saturating_sub(1) as f32) as u32;
    let clamp_y = |value: f32| value.clamp(0.0, height.saturating_sub(1) as f32) as u32;
    let x0 = clamp_x(x0_float);
    let y0 = clamp_y(y0_float);
    let x1 = clamp_x(x0_float + 1.0);
    let y1 = clamp_y(y0_float + 1.0);
    let at = |x: u32, y: u32| pixels[y as usize * width as usize + x as usize];
    mix(
        mix(at(x0, y0), at(x1, y0), tx),
        mix(at(x0, y1), at(x1, y1), tx),
        ty,
    )
}

fn multiply(mut value: [f32; 4], factor: f32) -> [f32; 4] {
    for channel in &mut value {
        *channel *= factor;
    }
    value
}

fn add_scaled(target: &mut [f32; 4], value: [f32; 4], factor: f32) {
    for (target, value) in target.iter_mut().zip(value) {
        *target += value * factor;
    }
}

pub(crate) fn validate_source(
    id: TextureId,
    source: Option<Rect>,
    asset: &TextureAsset,
) -> Result<Rect, SoftwareRenderError> {
    let source = source.unwrap_or(Rect::new(
        0.0,
        0.0,
        asset.width() as f32,
        asset.height() as f32,
    ));
    if source.is_empty()
        || source.x < 0.0
        || source.y < 0.0
        || source.right() > asset.width() as f32
        || source.bottom() > asset.height() as f32
    {
        Err(SoftwareRenderError::InvalidTextureSource(id))
    } else {
        Ok(source)
    }
}

pub(crate) fn rounded_distance(point: Point, rect: Rect, radii: CornerRadii) -> f32 {
    let center_x = rect.x + rect.width * 0.5;
    let center_y = rect.y + rect.height * 0.5;
    let radius = if point.x < center_x {
        if point.y < center_y {
            radii.top_left
        } else {
            radii.bottom_left
        }
    } else if point.y < center_y {
        radii.top_right
    } else {
        radii.bottom_right
    };
    let qx = (point.x - center_x).abs() - rect.width * 0.5 + radius;
    let qy = (point.y - center_y).abs() - rect.height * 0.5 + radius;
    qx.max(qy).min(0.0) + qx.max(0.0).hypot(qy.max(0.0)) - radius
}

fn edge_coverage(distance: f32, effective_scale: f32) -> f32 {
    let half_width = 0.5 / effective_scale;
    1.0 - smoothstep(-half_width, half_width, distance)
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let value = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn sample_texture(
    asset: &TextureAsset,
    source: Rect,
    u: f32,
    v: f32,
    sampling: Sampling,
) -> [f32; 4] {
    match sampling {
        Sampling::Nearest => {
            let x = (source.x + u * source.width)
                .floor()
                .clamp(source.x, source.right() - 1.0) as u32;
            let y = (source.y + v * source.height)
                .floor()
                .clamp(source.y, source.bottom() - 1.0) as u32;
            texel(asset, x, y)
        }
        Sampling::Linear => {
            let x = source.x + u * source.width - 0.5;
            let y = source.y + v * source.height - 0.5;
            let x0 = x.floor();
            let y0 = y.floor();
            let tx = x - x0;
            let ty = y - y0;
            let x0 = x0.clamp(source.x, source.right() - 1.0) as u32;
            let y0 = y0.clamp(source.y, source.bottom() - 1.0) as u32;
            let x1 = (x0 + 1).min(source.right() as u32 - 1);
            let y1 = (y0 + 1).min(source.bottom() as u32 - 1);
            let top = mix(texel(asset, x0, y0), texel(asset, x1, y0), tx);
            let bottom = mix(texel(asset, x0, y1), texel(asset, x1, y1), tx);
            mix(top, bottom, ty)
        }
    }
}

fn texel(asset: &TextureAsset, x: u32, y: u32) -> [f32; 4] {
    let offset =
        (y as usize * asset.width() as usize + x as usize) * asset.format().bytes_per_pixel();
    let pixels = asset.pixels();
    match asset.format() {
        TextureFormat::Rgba8 => [
            pixels[offset] as f32 / 255.0,
            pixels[offset + 1] as f32 / 255.0,
            pixels[offset + 2] as f32 / 255.0,
            pixels[offset + 3] as f32 / 255.0,
        ],
        TextureFormat::Alpha8 => [1.0, 1.0, 1.0, pixels[offset] as f32 / 255.0],
    }
}

fn mix(left: [f32; 4], right: [f32; 4], amount: f32) -> [f32; 4] {
    std::array::from_fn(|index| left[index] + (right[index] - left[index]) * amount)
}

fn blend(destination: &mut [f32; 4], color: Color, coverage: f32) {
    let [red, green, blue, alpha] = color.components();
    let alpha = alpha * coverage;
    let inverse = 1.0 - alpha;
    destination[0] = red * alpha + destination[0] * inverse;
    destination[1] = green * alpha + destination[1] * inverse;
    destination[2] = blue * alpha + destination[2] * inverse;
    destination[3] = alpha + destination[3] * inverse;
}

fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DisplayListBuilder, Shadow, Transform};

    #[test]
    fn premultiplied_blending_matches_source_over() {
        let mut renderer = SoftwareRenderer::new(1, 1).unwrap();
        renderer.clear(Color::from_srgba8(0, 0, 255, 255));
        let mut builder = DisplayListBuilder::new();
        builder
            .rect(
                Rect::new(0.0, 0.0, 1.0, 1.0),
                Color::from_srgba8(255, 0, 0, 128),
            )
            .unwrap();
        renderer
            .render(&builder.finish(), &TextureStore::new(), 1.0)
            .unwrap();
        let pixel = renderer.rgba8();
        assert_eq!(pixel, vec![128, 0, 127, 255]);
    }

    #[test]
    fn shadows_extend_beyond_the_source_shape() {
        let mut renderer = SoftwareRenderer::new(16, 16).unwrap();
        let mut builder = DisplayListBuilder::new();
        builder
            .shadow(
                Rect::new(6.0, 6.0, 4.0, 4.0),
                CornerRadii::all(1.0),
                Shadow::new(0.0, 0.0, 2.0, 0.0, Color::from_srgba8(255, 255, 255, 255)),
            )
            .unwrap();
        renderer
            .render(&builder.finish(), &TextureStore::new(), 1.0)
            .unwrap();
        assert!(renderer.rgba8()[(4 * 16 + 4) * 4 + 3] > 0);
    }

    #[test]
    fn negative_shadow_spread_may_collapse_the_shape() {
        let mut renderer = SoftwareRenderer::new(4, 4).unwrap();
        let mut builder = DisplayListBuilder::new();
        builder
            .shadow(
                Rect::new(1.0, 1.0, 2.0, 2.0),
                CornerRadii::all(1.0),
                Shadow::new(0.0, 0.0, 0.0, -2.0, Color::from_srgba8(255, 255, 255, 255)),
            )
            .unwrap();
        renderer
            .render(&builder.finish(), &TextureStore::new(), 1.0)
            .unwrap();
        assert!(renderer.rgba8().iter().all(|channel| *channel == 0));
    }

    #[test]
    fn transformed_texture_uses_the_same_local_coordinates() {
        let mut textures = TextureStore::new();
        let texture = textures
            .insert(
                2,
                1,
                vec![255, 0, 0, 255, 0, 255, 0, 255],
                AlphaMode::Straight,
            )
            .unwrap();
        let mut builder = DisplayListBuilder::new();
        builder
            .with_transform(Transform::translation(2.0, 0.0), |builder| {
                builder.texture(
                    Rect::new(0.0, 0.0, 2.0, 1.0),
                    texture,
                    None,
                    1.0,
                    Sampling::Nearest,
                )
            })
            .unwrap();
        let mut renderer = SoftwareRenderer::new(4, 1).unwrap();
        renderer.render(&builder.finish(), &textures, 1.0).unwrap();
        assert_eq!(renderer.rgba8()[8..16], [255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn one_mask_texture_can_be_drawn_with_different_tints() {
        let mut textures = TextureStore::new();
        let mask = textures.insert_mask(1, 1, vec![128]).unwrap();
        let mut builder = DisplayListBuilder::new();
        builder
            .tinted_texture(
                Rect::new(0.0, 0.0, 1.0, 1.0),
                mask,
                None,
                Color::from_srgba8(255, 0, 0, 255),
                1.0,
                Sampling::Nearest,
            )
            .unwrap();
        builder
            .tinted_texture(
                Rect::new(1.0, 0.0, 1.0, 1.0),
                mask,
                None,
                Color::from_srgba8(0, 255, 0, 255),
                1.0,
                Sampling::Nearest,
            )
            .unwrap();
        let mut renderer = SoftwareRenderer::new(2, 1).unwrap();
        renderer.render(&builder.finish(), &textures, 1.0).unwrap();
        assert_eq!(renderer.rgba8(), [255, 0, 0, 128, 0, 255, 0, 128]);
    }

    #[test]
    fn backdrop_blur_filters_only_its_rounded_painter_order_region() {
        let mut builder = DisplayListBuilder::new();
        for x in 0..12 {
            let color = if x % 2 == 0 {
                Color::from_srgba8(255, 255, 255, 255)
            } else {
                Color::from_srgba8(0, 0, 0, 255)
            };
            builder
                .rect(Rect::new(x as f32, 0.0, 1.0, 8.0), color)
                .unwrap();
        }
        builder
            .backdrop_blur(Rect::new(2.0, 1.0, 8.0, 6.0), CornerRadii::all(2.0), 2.0)
            .unwrap();
        builder
            .rect(
                Rect::new(5.0, 3.0, 2.0, 2.0),
                Color::from_srgba8(255, 0, 0, 255),
            )
            .unwrap();

        let mut renderer = SoftwareRenderer::new(12, 8).unwrap();
        renderer
            .render(&builder.finish(), &TextureStore::new(), 1.0)
            .unwrap();
        let pixels = renderer.rgba8();
        let pixel = |x: usize, y: usize| &pixels[(y * 12 + x) * 4..(y * 12 + x + 1) * 4];
        assert_eq!(pixel(0, 4), [255, 255, 255, 255]);
        assert_eq!(pixel(1, 4), [0, 0, 0, 255]);
        assert!(pixel(4, 4)[0] > 40 && pixel(4, 4)[0] < 215);
        assert_eq!(pixel(5, 3), [255, 0, 0, 255]);
    }
}

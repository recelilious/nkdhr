//! Batched OpenGL ES backend embedded in Smithay's active GLES frame.

use std::{
    collections::{HashMap, HashSet},
    ffi::{CString, c_void},
    fmt,
    mem::{offset_of, size_of},
};

use smithay::{
    backend::renderer::gles::{
        Capability, GlesFrame, GlesRenderer,
        ffi::{self, Gles2},
        link_program,
    },
    utils::{Physical, Rectangle, Size},
};

use crate::{
    AlphaMode, BackdropBlurPrimitive, CornerRadii, DisplayList, Point, Primitive, Rect, Sampling,
    ShapePrimitive, ShapeStyle, TextureAsset, TextureError, TextureFormat, TextureId,
    TexturePrimitive, TextureStore,
};

const SHAPE_VERTEX_SHADER: &str = include_str!("shaders/shape.vert");
const SHAPE_FRAGMENT_SHADER: &str = include_str!("shaders/shape.frag");
const TEXTURE_VERTEX_SHADER: &str = include_str!("shaders/texture.vert");
const TEXTURE_FRAGMENT_SHADER: &str = include_str!("shaders/texture.frag");
const BLUR_VERTEX_SHADER: &str = include_str!("shaders/blur.vert");
const BLUR_FRAGMENT_SHADER: &str = include_str!("shaders/blur.frag");
const BACKDROP_VERTEX_SHADER: &str = include_str!("shaders/backdrop.vert");
const BACKDROP_FRAGMENT_SHADER: &str = include_str!("shaders/backdrop.frag");

#[derive(Debug)]
pub enum GlesBackendError {
    InvalidScale,
    InvalidTarget,
    Destroyed,
    StalePreparedList,
    Texture(TextureError),
    InvalidTextureSource(TextureId),
    TextureTooLarge(TextureId),
    VertexDataTooLarge,
    ShaderInterface(&'static str),
    Smithay(String),
    GlOperation(u32),
}

impl fmt::Display for GlesBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScale => write!(formatter, "render scale must be finite and positive"),
            Self::InvalidTarget => write!(formatter, "render target dimensions must be positive"),
            Self::Destroyed => write!(formatter, "GLES backend resources were already destroyed"),
            Self::StalePreparedList => write!(
                formatter,
                "a newer display list was prepared for this GLES backend"
            ),
            Self::Texture(error) => error.fmt(formatter),
            Self::InvalidTextureSource(id) => {
                write!(formatter, "source rectangle is outside texture {id:?}")
            }
            Self::TextureTooLarge(id) => {
                write!(formatter, "texture {id:?} exceeds GLES dimensions")
            }
            Self::VertexDataTooLarge => {
                write!(formatter, "prepared vertex data exceeds GLES limits")
            }
            Self::ShaderInterface(name) => {
                write!(formatter, "GLSL compiler removed required interface {name}")
            }
            Self::Smithay(error) => write!(formatter, "Smithay GLES error: {error}"),
            Self::GlOperation(code) => {
                write!(formatter, "OpenGL ES operation failed with 0x{code:04x}")
            }
        }
    }
}

impl std::error::Error for GlesBackendError {}

impl From<TextureError> for GlesBackendError {
    fn from(value: TextureError) -> Self {
        Self::Texture(value)
    }
}

impl From<smithay::backend::renderer::gles::GlesError> for GlesBackendError {
    fn from(value: smithay::backend::renderer::gles::GlesError) -> Self {
        Self::Smithay(value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scissor {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Scissor {
    fn full(size: Size<i32, Physical>) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size.w,
            height: size.h,
        }
    }

    fn from_logical(rect: Rect, scale: f32, target: Size<i32, Physical>) -> Option<Self> {
        let left = (rect.x * scale).floor().max(0.0) as i32;
        let top = (rect.y * scale).floor().max(0.0) as i32;
        let right = (rect.right() * scale).ceil().min(target.w as f32) as i32;
        let bottom = (rect.bottom() * scale).ceil().min(target.h as f32) as i32;
        (right > left && bottom > top).then_some(Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    fn from_damage(rect: Rectangle<i32, Physical>, target: Size<i32, Physical>) -> Option<Self> {
        let left = rect.loc.x.max(0);
        let top = rect.loc.y.max(0);
        let right = (rect.loc.x + rect.size.w).min(target.w);
        let bottom = (rect.loc.y + rect.size.h).min(target.h);
        (right > left && bottom > top).then_some(Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    fn intersect(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > left && bottom > top).then_some(Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    fn expand(self, amount: i32, target: Size<i32, Physical>) -> Self {
        let left = (self.x - amount).max(0);
        let top = (self.y - amount).max(0);
        let right = (self.x + self.width + amount).min(target.w);
        let bottom = (self.y + self.height + amount).min(target.h);
        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    fn intersects(self, other: Self) -> bool {
        self.intersect(other).is_some()
    }

    fn rectangle(self) -> Rectangle<i32, Physical> {
        Rectangle::new((self.x, self.y).into(), (self.width, self.height).into())
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ShapeVertex {
    position: [f32; 2],
    local: [f32; 2],
    rect: [f32; 4],
    radii: [f32; 4],
    color: [f32; 4],
    parameters: [f32; 4],
    effect: [f32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct TextureVertex {
    position: [f32; 2],
    uv: [f32; 2],
    tint: [f32; 4],
    opacity: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct BlurVertex {
    position: [f32; 2],
    local: [f32; 2],
}

#[derive(Debug)]
enum Batch {
    Shapes {
        clip: Scissor,
        buffer_offset: usize,
        vertices: Vec<ShapeVertex>,
    },
    Texture {
        clip: Scissor,
        texture: TextureId,
        sampling: Sampling,
        alpha_mode: AlphaMode,
        format: TextureFormat,
        buffer_offset: usize,
        vertices: Vec<TextureVertex>,
    },
    BackdropBlur {
        clip: Scissor,
        output: Scissor,
        dependency: Scissor,
        buffer_offset: usize,
        vertices: Box<[BlurVertex; 12]>,
        rect: [f32; 4],
        radii: [f32; 4],
        radius: f32,
        effective_scale: f32,
    },
}

impl Batch {
    fn clip(&self) -> Scissor {
        match self {
            Self::Shapes { clip, .. }
            | Self::Texture { clip, .. }
            | Self::BackdropBlur { clip, .. } => *clip,
        }
    }
}

/// A validated, scaled display list ready for one GLES target.
#[derive(Debug)]
pub struct PreparedDisplayList {
    target: Size<i32, Physical>,
    batches: Vec<Batch>,
    primitive_count: usize,
    generation: u64,
}

impl PreparedDisplayList {
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    pub fn primitive_count(&self) -> usize {
        self.primitive_count
    }

    /// Whether drawing this list reads pixels already present in the target.
    pub fn has_backdrop_blur(&self) -> bool {
        self.batches
            .iter()
            .any(|batch| matches!(batch, Batch::BackdropBlur { .. }))
    }

    /// Expand incremental damage to include backdrop-filter dependencies.
    ///
    /// The returned rectangles must be used when repainting compositor layers
    /// below this display list and then passed to [`GlesBackend::draw`]. This
    /// prevents a partial update from sampling this list's pixels left over
    /// from the previous frame instead of a freshly painted backdrop.
    pub fn expand_damage(
        &self,
        damage: &[Rectangle<i32, Physical>],
    ) -> Vec<Rectangle<i32, Physical>> {
        let mut expanded: Vec<Scissor> = damage
            .iter()
            .filter_map(|damage| Scissor::from_damage(*damage, self.target))
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for batch in &self.batches {
                let Batch::BackdropBlur { dependency, .. } = batch else {
                    continue;
                };
                if expanded.iter().any(|damage| damage.intersects(*dependency))
                    && !expanded.contains(dependency)
                {
                    expanded.push(*dependency);
                    changed = true;
                }
            }
        }
        expanded.into_iter().map(Scissor::rectangle).collect()
    }
}

#[derive(Debug)]
struct ShapeProgram {
    id: u32,
    projection: i32,
    position: u32,
    local: u32,
    rect: u32,
    radii: u32,
    color: u32,
    parameters: u32,
    effect: u32,
}

#[derive(Debug)]
struct TextureProgram {
    id: u32,
    projection: i32,
    sampler: i32,
    alpha_mode: i32,
    format: i32,
    position: u32,
    uv: u32,
    tint: u32,
    opacity: u32,
}

#[derive(Debug)]
struct BlurProgram {
    id: u32,
    projection: i32,
    source: i32,
    target_size: i32,
    radius: i32,
    position: u32,
}

#[derive(Debug)]
struct BackdropProgram {
    id: u32,
    projection: i32,
    original: i32,
    horizontal: i32,
    target_size: i32,
    radius: i32,
    effective_scale: i32,
    rect: i32,
    radii: i32,
    position: u32,
    local: u32,
}

#[derive(Debug)]
struct BlurTargets {
    snapshot: u32,
    horizontal: u32,
    framebuffer: u32,
    size: Size<i32, Physical>,
}

#[derive(Debug)]
struct Resources {
    shape: ShapeProgram,
    texture: TextureProgram,
    blur: BlurProgram,
    backdrop: BackdropProgram,
    blur_targets: Option<BlurTargets>,
    vertex_buffer: u32,
    index_buffer: u32,
    reset_attribute_divisors: bool,
}

#[derive(Debug)]
struct UploadedTexture {
    id: u32,
    revision: u64,
}

/// Context-bound production renderer.
#[derive(Debug)]
pub struct GlesBackend {
    resources: Option<Resources>,
    textures: HashMap<TextureId, UploadedTexture>,
    active_generation: u64,
    next_generation: u64,
}

impl GlesBackend {
    pub fn new(renderer: &mut GlesRenderer) -> Result<Self, GlesBackendError> {
        let reset_attribute_divisors = renderer.capabilities().contains(&Capability::Instancing);
        let resources =
            renderer.with_context(|gl| create_resources(gl, reset_attribute_divisors))??;
        Ok(Self {
            resources: Some(resources),
            textures: HashMap::new(),
            active_generation: 0,
            next_generation: 1,
        })
    }

    /// Upload texture revisions and compile commands before borrowing the
    /// renderer through `GlesFrame`.
    pub fn prepare(
        &mut self,
        renderer: &mut GlesRenderer,
        display_list: &DisplayList,
        textures: &TextureStore,
        target: Size<i32, Physical>,
        scale: f32,
    ) -> Result<PreparedDisplayList, GlesBackendError> {
        self.resources.as_ref().ok_or(GlesBackendError::Destroyed)?;
        if target.w <= 0 || target.h <= 0 {
            return Err(GlesBackendError::InvalidTarget);
        }
        if !scale.is_finite() || scale <= 0.0 {
            return Err(GlesBackendError::InvalidScale);
        }
        renderer.with_context(|gl| self.synchronize_textures(gl, display_list, textures))??;
        let mut prepared = compile_batches(display_list, textures, target, scale)?;
        prepared.generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let resources = self.resources.as_ref().ok_or(GlesBackendError::Destroyed)?;
        renderer.with_context(|gl| upload_prepared(gl, resources, &mut prepared))??;
        self.active_generation = prepared.generation;
        Ok(prepared)
    }

    /// Draw the prepared list into Smithay's currently active target.
    pub fn draw(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        prepared: &PreparedDisplayList,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesBackendError> {
        let resources = self.resources.as_mut().ok_or(GlesBackendError::Destroyed)?;
        if damage.is_empty() {
            return Ok(());
        }
        if prepared.generation != self.active_generation {
            return Err(GlesBackendError::StalePreparedList);
        }
        let projection = *frame.projection();
        frame.with_context(|gl| {
            draw_batches(gl, resources, &self.textures, prepared, damage, &projection)
        })??;
        Ok(())
    }

    /// Delete every context-bound GL object. Hosts call this before destroying
    /// the matching Smithay renderer.
    pub fn destroy(&mut self, renderer: &mut GlesRenderer) -> Result<(), GlesBackendError> {
        let Some(resources) = self.resources.as_ref() else {
            return Ok(());
        };
        let texture_ids: Vec<u32> = self.textures.values().map(|texture| texture.id).collect();
        renderer.with_context(|gl| {
            destroy_resources(gl, resources, &texture_ids);
        })?;
        self.resources.take();
        self.textures.clear();
        Ok(())
    }

    fn synchronize_textures(
        &mut self,
        gl: &Gles2,
        display_list: &DisplayList,
        textures: &TextureStore,
    ) -> Result<(), GlesBackendError> {
        let needed: HashSet<TextureId> = display_list
            .primitives()
            .iter()
            .filter_map(|primitive| match primitive {
                Primitive::Texture(texture) => Some(texture.texture),
                Primitive::Shape(_) | Primitive::BackdropBlur(_) => None,
            })
            .collect();
        for id in &needed {
            let asset = textures.get(*id).ok_or(TextureError::UnknownTexture(*id))?;
            let current = self.textures.get(id).map(|texture| texture.revision);
            if current == Some(asset.revision()) {
                continue;
            }
            if let Some(old) = self.textures.remove(id) {
                delete_texture(gl, old.id);
            }
            let uploaded = upload_texture(gl, *id, asset)?;
            self.textures.insert(*id, uploaded);
        }

        let live: HashSet<TextureId> = textures.ids().collect();
        let removed: Vec<TextureId> = self
            .textures
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        for id in removed {
            if let Some(texture) = self.textures.remove(&id) {
                delete_texture(gl, texture.id);
            }
        }
        Ok(())
    }
}

fn compile_batches(
    display_list: &DisplayList,
    textures: &TextureStore,
    target: Size<i32, Physical>,
    scale: f32,
) -> Result<PreparedDisplayList, GlesBackendError> {
    let mut batches = Vec::new();
    for primitive in display_list.primitives() {
        match primitive {
            Primitive::Shape(shape) => {
                let clip = match shape.clip {
                    Some(clip) => {
                        let Some(clip) = Scissor::from_logical(clip, scale, target) else {
                            continue;
                        };
                        clip
                    }
                    None => Scissor::full(target),
                };
                let vertices = shape_vertices(*shape, scale);
                if vertices.is_empty() {
                    continue;
                }
                match batches.last_mut() {
                    Some(Batch::Shapes {
                        clip: batch_clip,
                        buffer_offset: _,
                        vertices: batch_vertices,
                    }) if *batch_clip == clip => batch_vertices.extend(vertices),
                    _ => batches.push(Batch::Shapes {
                        clip,
                        buffer_offset: 0,
                        vertices,
                    }),
                }
            }
            Primitive::Texture(texture) => {
                let asset = textures
                    .get(texture.texture)
                    .ok_or(TextureError::UnknownTexture(texture.texture))?;
                let source = texture_source(*texture, asset)?;
                let clip = match texture.clip {
                    Some(clip) => {
                        let Some(clip) = Scissor::from_logical(clip, scale, target) else {
                            continue;
                        };
                        clip
                    }
                    None => Scissor::full(target),
                };
                let vertices = texture_vertices(*texture, source, asset, scale);
                match batches.last_mut() {
                    Some(Batch::Texture {
                        clip: batch_clip,
                        texture: batch_texture,
                        sampling: batch_sampling,
                        alpha_mode: batch_alpha,
                        format: batch_format,
                        buffer_offset: _,
                        vertices: batch_vertices,
                    }) if *batch_clip == clip
                        && *batch_texture == texture.texture
                        && *batch_sampling == texture.sampling
                        && *batch_alpha == asset.alpha_mode()
                        && *batch_format == asset.format() =>
                    {
                        batch_vertices.extend(vertices);
                    }
                    _ => batches.push(Batch::Texture {
                        clip,
                        texture: texture.texture,
                        sampling: texture.sampling,
                        alpha_mode: asset.alpha_mode(),
                        format: asset.format(),
                        buffer_offset: 0,
                        vertices: vertices.to_vec(),
                    }),
                }
            }
            Primitive::BackdropBlur(blur) => {
                let clip = match blur.clip {
                    Some(clip) => {
                        let Some(clip) = Scissor::from_logical(clip, scale, target) else {
                            continue;
                        };
                        clip
                    }
                    None => Scissor::full(target),
                };
                let bounds = blur.transform.map_rect_bounds(blur.rect);
                let Some(output) = Scissor::from_logical(bounds, scale, target)
                    .and_then(|bounds| bounds.intersect(clip))
                else {
                    continue;
                };
                let effective_scale = (scale * blur.transform.minimum_scale()).max(f32::EPSILON);
                let radius = blur.radius * effective_scale;
                let dependency = output.expand(radius.ceil() as i32, target);
                batches.push(Batch::BackdropBlur {
                    clip,
                    output,
                    dependency,
                    buffer_offset: 0,
                    vertices: Box::new(backdrop_vertices(*blur, target, scale)),
                    rect: [blur.rect.x, blur.rect.y, blur.rect.width, blur.rect.height],
                    radii: blur.radii.as_array(),
                    radius,
                    effective_scale,
                });
            }
        }
    }
    Ok(PreparedDisplayList {
        target,
        batches,
        primitive_count: display_list.len(),
        generation: 0,
    })
}

fn backdrop_vertices(
    primitive: BackdropBlurPrimitive,
    target: Size<i32, Physical>,
    scale: f32,
) -> [BlurVertex; 12] {
    let pass = quad_points(Rect::new(0.0, 0.0, target.w as f32, target.h as f32)).map(|point| {
        BlurVertex {
            position: [point.x, point.y],
            local: [0.0, 0.0],
        }
    });
    let composite = quad_points(primitive.rect).map(|local| {
        let target = primitive.transform.map_point(local);
        BlurVertex {
            position: [target.x * scale, target.y * scale],
            local: [local.x, local.y],
        }
    });
    std::array::from_fn(|index| {
        if index < 6 {
            pass[index]
        } else {
            composite[index - 6]
        }
    })
}

fn shape_vertices(primitive: ShapePrimitive, target_scale: f32) -> Vec<ShapeVertex> {
    let scale = effective_scale(primitive, target_scale);
    let (shape_rect, radii, draw_rects, color, parameters, effect) = match primitive.style {
        ShapeStyle::Fill(color) => {
            let kind = if primitive.radii == CornerRadii::ZERO {
                3.0
            } else {
                0.0
            };
            (
                primitive.rect,
                primitive.radii,
                vec![primitive.rect],
                color,
                [kind, 0.0, 0.0, scale],
                [0.0; 4],
            )
        }
        ShapeStyle::Border { width, color } => (
            primitive.rect,
            primitive.radii,
            border_draw_rects(primitive.rect, primitive.radii, width, scale),
            color,
            [1.0, width, 0.0, scale],
            [0.0; 4],
        ),
        ShapeStyle::Shadow(shadow) => {
            let rect = Rect::new(
                primitive.rect.x + shadow.offset_x,
                primitive.rect.y + shadow.offset_y,
                primitive.rect.width,
                primitive.rect.height,
            )
            .expand(shadow.spread);
            if rect.is_empty() {
                return Vec::new();
            }
            let radii = primitive.radii.expand(shadow.spread).normalized(rect);
            (
                rect,
                radii,
                vec![rect.expand(shadow.blur_radius * 3.0 + 1.0 / target_scale)],
                shadow.color,
                [2.0, 0.0, shadow.blur_radius, scale],
                [0.0; 4],
            )
        }
        ShapeStyle::InsetShadow(shadow) => (
            primitive.rect,
            primitive.radii,
            vec![primitive.rect],
            shadow.color,
            [4.0, 0.0, shadow.blur_radius, scale],
            [shadow.offset_x, shadow.offset_y, shadow.spread, 0.0],
        ),
    };
    let rect = [
        shape_rect.x,
        shape_rect.y,
        shape_rect.width,
        shape_rect.height,
    ];
    let radii = radii.as_array();
    let color = color.components();
    draw_rects
        .into_iter()
        .flat_map(|draw_rect| {
            quad_points(draw_rect).map(|local| {
                let target = primitive.transform.map_point(local);
                ShapeVertex {
                    position: [target.x * target_scale, target.y * target_scale],
                    local: [local.x, local.y],
                    rect,
                    radii,
                    color,
                    parameters,
                    effect,
                }
            })
        })
        .collect()
}

fn border_draw_rects(
    rect: Rect,
    radii: CornerRadii,
    width: f32,
    effective_scale: f32,
) -> Vec<Rect> {
    let antialias = 1.0 / effective_scale;
    let top = radii
        .top_left
        .max(radii.top_right)
        .max(width + antialias)
        .min(rect.height);
    let bottom = radii
        .bottom_left
        .max(radii.bottom_right)
        .max(width + antialias)
        .min(rect.height);
    let left = radii
        .top_left
        .max(radii.bottom_left)
        .max(width + antialias)
        .min(rect.width);
    let right = radii
        .top_right
        .max(radii.bottom_right)
        .max(width + antialias)
        .min(rect.width);
    if top + bottom >= rect.height || left + right >= rect.width {
        return vec![rect];
    }
    vec![
        Rect::new(rect.x, rect.y, rect.width, top),
        Rect::new(rect.x, rect.bottom() - bottom, rect.width, bottom),
        Rect::new(rect.x, rect.y + top, left, rect.height - top - bottom),
        Rect::new(
            rect.right() - right,
            rect.y + top,
            right,
            rect.height - top - bottom,
        ),
    ]
}

fn texture_vertices(
    primitive: TexturePrimitive,
    source: Rect,
    asset: &TextureAsset,
    scale: f32,
) -> [TextureVertex; 4] {
    let points = [
        Point::new(primitive.rect.x, primitive.rect.y),
        Point::new(primitive.rect.right(), primitive.rect.y),
        Point::new(primitive.rect.x, primitive.rect.bottom()),
        Point::new(primitive.rect.right(), primitive.rect.bottom()),
    ];
    let u0 = source.x / asset.width() as f32;
    let v0 = source.y / asset.height() as f32;
    let u1 = source.right() / asset.width() as f32;
    let v1 = source.bottom() / asset.height() as f32;
    let uv = [[u0, v0], [u1, v0], [u0, v1], [u1, v1]];
    let tint = primitive.tint.components();
    std::array::from_fn(|index| {
        let target = primitive.transform.map_point(points[index]);
        TextureVertex {
            position: [target.x * scale, target.y * scale],
            uv: uv[index],
            tint,
            opacity: primitive.opacity,
        }
    })
}

fn quad_points(rect: Rect) -> [Point; 6] {
    [
        Point::new(rect.x, rect.y),
        Point::new(rect.right(), rect.y),
        Point::new(rect.x, rect.bottom()),
        Point::new(rect.x, rect.bottom()),
        Point::new(rect.right(), rect.y),
        Point::new(rect.right(), rect.bottom()),
    ]
}

fn effective_scale(primitive: ShapePrimitive, target_scale: f32) -> f32 {
    (primitive.transform.minimum_scale() * target_scale).max(f32::EPSILON)
}

fn texture_source(
    primitive: TexturePrimitive,
    asset: &TextureAsset,
) -> Result<Rect, GlesBackendError> {
    let source = primitive.source.unwrap_or(Rect::new(
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
        Err(GlesBackendError::InvalidTextureSource(primitive.texture))
    } else {
        Ok(source)
    }
}

fn create_resources(
    gl: &Gles2,
    reset_attribute_divisors: bool,
) -> Result<Resources, GlesBackendError> {
    let mut program_ids = Vec::new();
    let linked = (|| -> Result<[u32; 4], GlesBackendError> {
        let shape = unsafe { link_program(gl, SHAPE_VERTEX_SHADER, SHAPE_FRAGMENT_SHADER) }?;
        program_ids.push(shape);
        let texture = unsafe { link_program(gl, TEXTURE_VERTEX_SHADER, TEXTURE_FRAGMENT_SHADER) }?;
        program_ids.push(texture);
        let blur = unsafe { link_program(gl, BLUR_VERTEX_SHADER, BLUR_FRAGMENT_SHADER) }?;
        program_ids.push(blur);
        let backdrop =
            unsafe { link_program(gl, BACKDROP_VERTEX_SHADER, BACKDROP_FRAGMENT_SHADER) }?;
        program_ids.push(backdrop);
        Ok([shape, texture, blur, backdrop])
    })();
    let [shape_id, texture_id, blur_id, backdrop_id] = match linked {
        Ok(ids) => ids,
        Err(error) => {
            unsafe {
                for program in program_ids {
                    gl.DeleteProgram(program);
                }
            }
            return Err(error);
        }
    };
    let resources = (|| {
        let shape = ShapeProgram {
            id: shape_id,
            projection: uniform(gl, shape_id, "u_projection")?,
            position: attribute(gl, shape_id, "a_position")?,
            local: attribute(gl, shape_id, "a_local")?,
            rect: attribute(gl, shape_id, "a_rect")?,
            radii: attribute(gl, shape_id, "a_radii")?,
            color: attribute(gl, shape_id, "a_color")?,
            parameters: attribute(gl, shape_id, "a_parameters")?,
            effect: attribute(gl, shape_id, "a_effect")?,
        };
        let texture = TextureProgram {
            id: texture_id,
            projection: uniform(gl, texture_id, "u_projection")?,
            sampler: uniform(gl, texture_id, "u_texture")?,
            alpha_mode: uniform(gl, texture_id, "u_alpha_mode")?,
            format: uniform(gl, texture_id, "u_texture_format")?,
            position: attribute(gl, texture_id, "a_position")?,
            uv: attribute(gl, texture_id, "a_uv")?,
            tint: attribute(gl, texture_id, "a_tint")?,
            opacity: attribute(gl, texture_id, "a_opacity")?,
        };
        let blur = BlurProgram {
            id: blur_id,
            projection: uniform(gl, blur_id, "u_projection")?,
            source: uniform(gl, blur_id, "u_source")?,
            target_size: uniform(gl, blur_id, "u_target_size")?,
            radius: uniform(gl, blur_id, "u_radius")?,
            position: attribute(gl, blur_id, "a_position")?,
        };
        let backdrop = BackdropProgram {
            id: backdrop_id,
            projection: uniform(gl, backdrop_id, "u_projection")?,
            original: uniform(gl, backdrop_id, "u_original")?,
            horizontal: uniform(gl, backdrop_id, "u_horizontal")?,
            target_size: uniform(gl, backdrop_id, "u_target_size")?,
            radius: uniform(gl, backdrop_id, "u_radius")?,
            effective_scale: uniform(gl, backdrop_id, "u_effective_scale")?,
            rect: uniform(gl, backdrop_id, "u_rect")?,
            radii: uniform(gl, backdrop_id, "u_radii")?,
            position: attribute(gl, backdrop_id, "a_position")?,
            local: attribute(gl, backdrop_id, "a_local")?,
        };
        let mut buffers = [0; 2];
        unsafe { gl.GenBuffers(buffers.len() as i32, buffers.as_mut_ptr()) };
        if buffers.contains(&0) {
            unsafe { gl.DeleteBuffers(buffers.len() as i32, buffers.as_ptr()) };
            return Err(GlesBackendError::GlOperation(ffi::OUT_OF_MEMORY));
        }
        Ok(Resources {
            shape,
            texture,
            blur,
            backdrop,
            blur_targets: None,
            vertex_buffer: buffers[0],
            index_buffer: buffers[1],
            reset_attribute_divisors,
        })
    })();
    if resources.is_err() {
        unsafe {
            for program in [shape_id, texture_id, blur_id, backdrop_id] {
                gl.DeleteProgram(program);
            }
        }
    }
    resources
}

fn attribute(gl: &Gles2, program: u32, name: &'static str) -> Result<u32, GlesBackendError> {
    let name_c = CString::new(name).expect("static shader names contain no NUL");
    let location = unsafe { gl.GetAttribLocation(program, name_c.as_ptr()) };
    u32::try_from(location).map_err(|_| GlesBackendError::ShaderInterface(name))
}

fn uniform(gl: &Gles2, program: u32, name: &'static str) -> Result<i32, GlesBackendError> {
    let name_c = CString::new(name).expect("static shader names contain no NUL");
    let location = unsafe { gl.GetUniformLocation(program, name_c.as_ptr()) };
    (location >= 0)
        .then_some(location)
        .ok_or(GlesBackendError::ShaderInterface(name))
}

fn upload_texture(
    gl: &Gles2,
    id: TextureId,
    asset: &TextureAsset,
) -> Result<UploadedTexture, GlesBackendError> {
    let width = i32::try_from(asset.width()).map_err(|_| GlesBackendError::TextureTooLarge(id))?;
    let height =
        i32::try_from(asset.height()).map_err(|_| GlesBackendError::TextureTooLarge(id))?;
    let mut texture = 0;
    unsafe {
        while gl.GetError() != ffi::NO_ERROR {}
        gl.GenTextures(1, &mut texture);
        gl.BindTexture(ffi::TEXTURE_2D, texture);
        gl.PixelStorei(ffi::UNPACK_ALIGNMENT, 1);
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_S,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_T,
            ffi::CLAMP_TO_EDGE as i32,
        );
        let format = match asset.format() {
            TextureFormat::Rgba8 => ffi::RGBA,
            TextureFormat::Alpha8 => ffi::ALPHA,
        };
        gl.TexImage2D(
            ffi::TEXTURE_2D,
            0,
            format as i32,
            width,
            height,
            0,
            format,
            ffi::UNSIGNED_BYTE,
            asset.pixels().as_ptr().cast(),
        );
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.PixelStorei(ffi::UNPACK_ALIGNMENT, 4);
    }
    let error = unsafe { gl.GetError() };
    if error != ffi::NO_ERROR {
        delete_texture(gl, texture);
        return Err(GlesBackendError::GlOperation(error));
    }
    Ok(UploadedTexture {
        id: texture,
        revision: asset.revision(),
    })
}

fn delete_texture(gl: &Gles2, texture: u32) {
    unsafe { gl.DeleteTextures(1, &texture) };
}

fn upload_prepared(
    gl: &Gles2,
    resources: &Resources,
    prepared: &mut PreparedDisplayList,
) -> Result<(), GlesBackendError> {
    let mut bytes = Vec::new();
    let mut maximum_texture_quads = 0;
    for batch in &mut prepared.batches {
        match batch {
            Batch::Shapes {
                buffer_offset,
                vertices,
                ..
            } => {
                *buffer_offset = bytes.len();
                bytes.extend_from_slice(vertex_bytes(vertices.as_ref()));
            }
            Batch::Texture {
                buffer_offset,
                vertices,
                ..
            } => {
                *buffer_offset = bytes.len();
                bytes.extend_from_slice(vertex_bytes(vertices));
                maximum_texture_quads = maximum_texture_quads.max(vertices.len() / 4);
            }
            Batch::BackdropBlur {
                buffer_offset,
                vertices,
                ..
            } => {
                *buffer_offset = bytes.len();
                bytes.extend_from_slice(vertex_bytes(vertices.as_ref()));
            }
        }
    }
    let byte_length =
        isize::try_from(bytes.len()).map_err(|_| GlesBackendError::VertexDataTooLarge)?;
    unsafe {
        while gl.GetError() != ffi::NO_ERROR {}
        gl.BindBuffer(ffi::ARRAY_BUFFER, resources.vertex_buffer);
        gl.BufferData(
            ffi::ARRAY_BUFFER,
            byte_length,
            bytes.as_ptr().cast(),
            ffi::DYNAMIC_DRAW,
        );
        let indices = texture_indices(maximum_texture_quads)?;
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, resources.index_buffer);
        gl.BufferData(
            ffi::ELEMENT_ARRAY_BUFFER,
            isize::try_from(std::mem::size_of_val(indices.as_slice()))
                .map_err(|_| GlesBackendError::VertexDataTooLarge)?,
            indices.as_ptr().cast(),
            ffi::DYNAMIC_DRAW,
        );
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
    }
    let error = unsafe { gl.GetError() };
    if error == ffi::NO_ERROR {
        Ok(())
    } else {
        Err(GlesBackendError::GlOperation(error))
    }
}

fn texture_indices(quad_count: usize) -> Result<Vec<u16>, GlesBackendError> {
    let Some(vertex_count) = quad_count.checked_mul(4) else {
        return Err(GlesBackendError::VertexDataTooLarge);
    };
    if vertex_count > u16::MAX as usize {
        return Err(GlesBackendError::VertexDataTooLarge);
    }
    let mut indices = Vec::with_capacity(quad_count * 6);
    for quad in 0..quad_count {
        let base = u16::try_from(quad * 4).map_err(|_| GlesBackendError::VertexDataTooLarge)?;
        indices.extend_from_slice(&[base, base + 1, base + 3, base, base + 3, base + 2]);
    }
    Ok(indices)
}

fn vertex_bytes<T>(vertices: &[T]) -> &[u8] {
    let length = std::mem::size_of_val(vertices);
    unsafe { std::slice::from_raw_parts(vertices.as_ptr().cast(), length) }
}

fn ensure_blur_targets(
    gl: &Gles2,
    resources: &mut Resources,
    target: Size<i32, Physical>,
) -> Result<(), GlesBackendError> {
    if resources
        .blur_targets
        .as_ref()
        .is_some_and(|targets| targets.size == target)
    {
        return Ok(());
    }
    if let Some(old) = resources.blur_targets.take() {
        destroy_blur_targets(gl, &old);
    }

    let mut textures = [0_u32; 2];
    let mut framebuffer = 0_u32;
    let mut previous_framebuffer = 0_i32;
    unsafe {
        while gl.GetError() != ffi::NO_ERROR {}
        gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut previous_framebuffer);
        gl.GenTextures(2, textures.as_mut_ptr());
        for texture in textures {
            gl.BindTexture(ffi::TEXTURE_2D, texture);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexImage2D(
                ffi::TEXTURE_2D,
                0,
                ffi::RGBA as i32,
                target.w,
                target.h,
                0,
                ffi::RGBA,
                ffi::UNSIGNED_BYTE,
                std::ptr::null(),
            );
        }
        gl.GenFramebuffers(1, &mut framebuffer);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffer);
        gl.FramebufferTexture2D(
            ffi::FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            textures[1],
            0,
        );
    }
    let framebuffer_status = unsafe { gl.CheckFramebufferStatus(ffi::FRAMEBUFFER) };
    unsafe {
        gl.BindFramebuffer(ffi::FRAMEBUFFER, previous_framebuffer as u32);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
    }
    let error = unsafe { gl.GetError() };
    if textures.contains(&0)
        || framebuffer == 0
        || framebuffer_status != ffi::FRAMEBUFFER_COMPLETE
        || error != ffi::NO_ERROR
    {
        let failed = BlurTargets {
            snapshot: textures[0],
            horizontal: textures[1],
            framebuffer,
            size: target,
        };
        destroy_blur_targets(gl, &failed);
        return Err(GlesBackendError::GlOperation(if error != ffi::NO_ERROR {
            error
        } else if framebuffer_status != ffi::FRAMEBUFFER_COMPLETE {
            framebuffer_status
        } else {
            ffi::OUT_OF_MEMORY
        }));
    }
    resources.blur_targets = Some(BlurTargets {
        snapshot: textures[0],
        horizontal: textures[1],
        framebuffer,
        size: target,
    });
    Ok(())
}

fn destroy_blur_targets(gl: &Gles2, targets: &BlurTargets) {
    unsafe {
        if targets.framebuffer != 0 {
            gl.DeleteFramebuffers(1, &targets.framebuffer);
        }
        let textures = [targets.snapshot, targets.horizontal];
        gl.DeleteTextures(textures.len() as i32, textures.as_ptr());
    }
}

fn draw_batches(
    gl: &Gles2,
    resources: &mut Resources,
    textures: &HashMap<TextureId, UploadedTexture>,
    prepared: &PreparedDisplayList,
    damage: &[Rectangle<i32, Physical>],
    projection: &[f32; 9],
) -> Result<(), GlesBackendError> {
    unsafe {
        while gl.GetError() != ffi::NO_ERROR {}
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.Enable(ffi::SCISSOR_TEST);
        gl.BindBuffer(ffi::ARRAY_BUFFER, resources.vertex_buffer);
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, resources.index_buffer);
    }

    let damage: Vec<Scissor> = damage
        .iter()
        .filter_map(|damage| Scissor::from_damage(*damage, prepared.target))
        .collect();
    for batch in &prepared.batches {
        match batch {
            Batch::BackdropBlur { output, .. } => {
                let scissors: Vec<Scissor> = damage
                    .iter()
                    .filter_map(|damage| output.intersect(*damage))
                    .collect();
                if !scissors.is_empty() {
                    draw_backdrop_batch(
                        gl,
                        resources,
                        batch,
                        &scissors,
                        prepared.target,
                        projection,
                    )?;
                }
            }
            Batch::Shapes { .. } | Batch::Texture { .. } => {
                for scissor in damage
                    .iter()
                    .filter_map(|damage| batch.clip().intersect(*damage))
                {
                    set_scissor(gl, scissor);
                    match batch {
                        Batch::Shapes {
                            buffer_offset,
                            vertices,
                            ..
                        } => draw_shape_batch(
                            gl,
                            resources,
                            *buffer_offset,
                            vertices.len(),
                            projection,
                        ),
                        Batch::Texture {
                            texture,
                            sampling,
                            alpha_mode,
                            format,
                            buffer_offset,
                            vertices,
                            ..
                        } => {
                            let texture = textures
                                .get(texture)
                                .ok_or(TextureError::UnknownTexture(*texture))?;
                            draw_texture_batch(
                                gl,
                                resources,
                                texture.id,
                                *sampling,
                                *alpha_mode,
                                *format,
                                *buffer_offset,
                                vertices.len(),
                                projection,
                            );
                        }
                        Batch::BackdropBlur { .. } => unreachable!(),
                    }
                }
            }
        }
    }

    reset_state(gl, prepared.target);
    let error = unsafe { gl.GetError() };
    if error == ffi::NO_ERROR {
        Ok(())
    } else {
        Err(GlesBackendError::GlOperation(error))
    }
}

fn draw_backdrop_batch(
    gl: &Gles2,
    resources: &mut Resources,
    batch: &Batch,
    damage: &[Scissor],
    target: Size<i32, Physical>,
    projection: &[f32; 9],
) -> Result<(), GlesBackendError> {
    let Batch::BackdropBlur {
        dependency,
        buffer_offset,
        rect,
        radii,
        radius,
        effective_scale,
        ..
    } = batch
    else {
        unreachable!()
    };
    ensure_blur_targets(gl, resources, target)?;
    let targets = resources
        .blur_targets
        .as_ref()
        .expect("blur targets were just allocated");

    let mut original_framebuffer = 0_i32;
    unsafe {
        gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut original_framebuffer);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, targets.snapshot);
        gl.CopyTexSubImage2D(
            ffi::TEXTURE_2D,
            0,
            dependency.x,
            dependency.y,
            dependency.x,
            dependency.y,
            dependency.width,
            dependency.height,
        );

        gl.BindFramebuffer(ffi::FRAMEBUFFER, targets.framebuffer);
        gl.Viewport(0, 0, target.w, target.h);
        gl.Disable(ffi::BLEND);
    }
    set_scissor(gl, *dependency);
    draw_horizontal_blur(
        gl,
        resources,
        targets.snapshot,
        *radius,
        *buffer_offset,
        target,
        projection,
    );

    unsafe {
        gl.BindFramebuffer(ffi::FRAMEBUFFER, original_framebuffer as u32);
        gl.Viewport(0, 0, target.w, target.h);
    }
    for scissor in damage {
        set_scissor(gl, *scissor);
        draw_backdrop_composite(
            gl,
            resources,
            targets.snapshot,
            targets.horizontal,
            *radius,
            *effective_scale,
            *rect,
            *radii,
            *buffer_offset + 6 * size_of::<BlurVertex>(),
            target,
            projection,
        );
    }
    unsafe {
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
    }
    Ok(())
}

fn draw_horizontal_blur(
    gl: &Gles2,
    resources: &Resources,
    source: u32,
    radius: f32,
    buffer_offset: usize,
    target: Size<i32, Physical>,
    projection: &[f32; 9],
) {
    let program = &resources.blur;
    unsafe {
        gl.UseProgram(program.id);
        gl.UniformMatrix3fv(program.projection, 1, ffi::FALSE, projection.as_ptr());
        gl.Uniform1i(program.source, 0);
        gl.Uniform2f(program.target_size, target.w as f32, target.h as f32);
        gl.Uniform1f(program.radius, radius);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, source);
        enable_attribute::<BlurVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.position,
            2,
            buffer_offset + offset_of!(BlurVertex, position),
        );
        gl.DrawArrays(ffi::TRIANGLES, 0, 6);
        gl.DisableVertexAttribArray(program.position);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_backdrop_composite(
    gl: &Gles2,
    resources: &Resources,
    original: u32,
    horizontal: u32,
    radius: f32,
    effective_scale: f32,
    rect: [f32; 4],
    radii: [f32; 4],
    buffer_offset: usize,
    target: Size<i32, Physical>,
    projection: &[f32; 9],
) {
    let program = &resources.backdrop;
    unsafe {
        gl.Disable(ffi::BLEND);
        gl.UseProgram(program.id);
        gl.UniformMatrix3fv(program.projection, 1, ffi::FALSE, projection.as_ptr());
        gl.Uniform1i(program.original, 0);
        gl.Uniform1i(program.horizontal, 1);
        gl.Uniform2f(program.target_size, target.w as f32, target.h as f32);
        gl.Uniform1f(program.radius, radius);
        gl.Uniform1f(program.effective_scale, effective_scale);
        gl.Uniform4f(program.rect, rect[0], rect[1], rect[2], rect[3]);
        gl.Uniform4f(program.radii, radii[0], radii[1], radii[2], radii[3]);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, original);
        gl.ActiveTexture(ffi::TEXTURE1);
        gl.BindTexture(ffi::TEXTURE_2D, horizontal);
        enable_attribute::<BlurVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.position,
            2,
            buffer_offset + offset_of!(BlurVertex, position),
        );
        enable_attribute::<BlurVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.local,
            2,
            buffer_offset + offset_of!(BlurVertex, local),
        );
        gl.DrawArrays(ffi::TRIANGLES, 0, 6);
        gl.DisableVertexAttribArray(program.position);
        gl.DisableVertexAttribArray(program.local);
        gl.ActiveTexture(ffi::TEXTURE0);
    }
}

fn set_scissor(gl: &Gles2, scissor: Scissor) {
    unsafe {
        gl.Scissor(scissor.x, scissor.y, scissor.width, scissor.height);
    }
}

fn draw_shape_batch(
    gl: &Gles2,
    resources: &Resources,
    buffer_offset: usize,
    vertex_count: usize,
    projection: &[f32; 9],
) {
    let program = &resources.shape;
    unsafe {
        gl.UseProgram(program.id);
        gl.UniformMatrix3fv(program.projection, 1, ffi::FALSE, projection.as_ptr());
        enable_attribute::<ShapeVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.position,
            2,
            buffer_offset + offset_of!(ShapeVertex, position),
        );
        enable_attribute::<ShapeVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.local,
            2,
            buffer_offset + offset_of!(ShapeVertex, local),
        );
        enable_attribute::<ShapeVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.rect,
            4,
            buffer_offset + offset_of!(ShapeVertex, rect),
        );
        enable_attribute::<ShapeVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.radii,
            4,
            buffer_offset + offset_of!(ShapeVertex, radii),
        );
        enable_attribute::<ShapeVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.color,
            4,
            buffer_offset + offset_of!(ShapeVertex, color),
        );
        enable_attribute::<ShapeVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.parameters,
            4,
            buffer_offset + offset_of!(ShapeVertex, parameters),
        );
        enable_attribute::<ShapeVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.effect,
            4,
            buffer_offset + offset_of!(ShapeVertex, effect),
        );
        gl.DrawArrays(ffi::TRIANGLES, 0, vertex_count as i32);
        for attribute in [
            program.position,
            program.local,
            program.rect,
            program.radii,
            program.color,
            program.parameters,
            program.effect,
        ] {
            gl.DisableVertexAttribArray(attribute);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_texture_batch(
    gl: &Gles2,
    resources: &Resources,
    texture: u32,
    sampling: Sampling,
    alpha_mode: AlphaMode,
    format: TextureFormat,
    buffer_offset: usize,
    vertex_count: usize,
    projection: &[f32; 9],
) {
    let program = &resources.texture;
    let filter = match sampling {
        Sampling::Nearest => ffi::NEAREST,
        Sampling::Linear => ffi::LINEAR,
    };
    let alpha_mode = match alpha_mode {
        AlphaMode::Straight => 0.0,
        AlphaMode::Premultiplied => 1.0,
        AlphaMode::Opaque => 2.0,
    };
    let format = match format {
        TextureFormat::Rgba8 => 0.0,
        TextureFormat::Alpha8 => 1.0,
    };
    unsafe {
        gl.UseProgram(program.id);
        gl.UniformMatrix3fv(program.projection, 1, ffi::FALSE, projection.as_ptr());
        gl.Uniform1i(program.sampler, 0);
        gl.Uniform1f(program.alpha_mode, alpha_mode);
        gl.Uniform1f(program.format, format);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, texture);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, filter as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, filter as i32);
        enable_attribute::<TextureVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.position,
            2,
            buffer_offset + offset_of!(TextureVertex, position),
        );
        enable_attribute::<TextureVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.uv,
            2,
            buffer_offset + offset_of!(TextureVertex, uv),
        );
        enable_attribute::<TextureVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.tint,
            4,
            buffer_offset + offset_of!(TextureVertex, tint),
        );
        enable_attribute::<TextureVertex>(
            gl,
            resources.reset_attribute_divisors,
            program.opacity,
            1,
            buffer_offset + offset_of!(TextureVertex, opacity),
        );
        gl.DrawElements(
            ffi::TRIANGLES,
            (vertex_count / 4 * 6) as i32,
            ffi::UNSIGNED_SHORT,
            std::ptr::null(),
        );
        for attribute in [program.position, program.uv, program.tint, program.opacity] {
            gl.DisableVertexAttribArray(attribute);
        }
    }
}

unsafe fn enable_attribute<T>(
    gl: &Gles2,
    reset_divisor: bool,
    location: u32,
    components: i32,
    offset: usize,
) {
    unsafe {
        if reset_divisor {
            gl.VertexAttribDivisor(location, 0);
        }
        gl.EnableVertexAttribArray(location);
        gl.VertexAttribPointer(
            location,
            components,
            ffi::FLOAT,
            ffi::FALSE,
            size_of::<T>() as i32,
            offset as *const c_void,
        );
    }
}

fn reset_state(gl: &Gles2, target: Size<i32, Physical>) {
    unsafe {
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
        gl.ActiveTexture(ffi::TEXTURE1);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.UseProgram(0);
        gl.Enable(ffi::SCISSOR_TEST);
        gl.Scissor(0, 0, target.w, target.h);
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
    }
}

fn destroy_resources(gl: &Gles2, resources: &Resources, textures: &[u32]) {
    unsafe {
        gl.DeleteProgram(resources.shape.id);
        gl.DeleteProgram(resources.texture.id);
        gl.DeleteProgram(resources.blur.id);
        gl.DeleteProgram(resources.backdrop.id);
        gl.DeleteBuffers(1, &resources.vertex_buffer);
        gl.DeleteBuffers(1, &resources.index_buffer);
        if !textures.is_empty() {
            gl.DeleteTextures(textures.len() as i32, textures.as_ptr());
        }
    }
    if let Some(targets) = &resources.blur_targets {
        destroy_blur_targets(gl, targets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Color, CornerRadii, DisplayListBuilder, Transform};

    fn target() -> Size<i32, Physical> {
        (100, 100).into()
    }

    #[test]
    fn consecutive_shapes_share_one_batch_until_the_clip_changes() {
        let mut builder = DisplayListBuilder::new();
        builder
            .rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                Color::from_srgba8(1, 2, 3, 255),
            )
            .unwrap();
        builder
            .rounded_rect(
                Rect::new(10.0, 0.0, 10.0, 10.0),
                CornerRadii::all(2.0),
                Color::from_srgba8(4, 5, 6, 255),
            )
            .unwrap();
        builder
            .with_clip(Rect::new(0.0, 0.0, 10.0, 10.0), |builder| {
                builder.rect(
                    Rect::new(0.0, 0.0, 10.0, 10.0),
                    Color::from_srgba8(7, 8, 9, 255),
                )
            })
            .unwrap();
        let prepared =
            compile_batches(&builder.finish(), &TextureStore::new(), target(), 1.0).unwrap();
        assert_eq!(prepared.primitive_count(), 3);
        assert_eq!(prepared.batch_count(), 2);
    }

    #[test]
    fn same_texture_batches_but_painters_order_is_not_reordered() {
        let mut textures = TextureStore::new();
        let texture = textures
            .insert(1, 1, vec![255; 4], AlphaMode::Straight)
            .unwrap();
        let mut builder = DisplayListBuilder::new();
        for x in [0.0, 10.0] {
            builder
                .texture(
                    Rect::new(x, 0.0, 10.0, 10.0),
                    texture,
                    None,
                    1.0,
                    Sampling::Linear,
                )
                .unwrap();
        }
        builder
            .with_transform(Transform::translation(0.0, 10.0), |builder| {
                builder.rect(
                    Rect::new(0.0, 0.0, 10.0, 10.0),
                    Color::from_srgba8(1, 2, 3, 255),
                )
            })
            .unwrap();
        builder
            .texture(
                Rect::new(20.0, 0.0, 10.0, 10.0),
                texture,
                None,
                1.0,
                Sampling::Linear,
            )
            .unwrap();
        let prepared = compile_batches(&builder.finish(), &textures, target(), 1.0).unwrap();
        assert_eq!(prepared.batch_count(), 3);
    }

    #[test]
    fn backdrop_blur_is_a_batch_barrier_and_expands_damage_dependencies() {
        let mut builder = DisplayListBuilder::new();
        builder
            .rect(
                Rect::new(0.0, 0.0, 100.0, 100.0),
                Color::from_srgba8(1, 2, 3, 255),
            )
            .unwrap();
        builder
            .backdrop_blur(
                Rect::new(40.0, 40.0, 20.0, 20.0),
                CornerRadii::all(4.0),
                8.0,
            )
            .unwrap();
        builder
            .rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                Color::from_srgba8(4, 5, 6, 255),
            )
            .unwrap();
        let prepared =
            compile_batches(&builder.finish(), &TextureStore::new(), target(), 1.0).unwrap();
        assert_eq!(prepared.batch_count(), 3);
        assert!(prepared.has_backdrop_blur());

        let damage = [Rectangle::new((50, 50).into(), (1, 1).into())];
        let expanded = prepared.expand_damage(&damage);
        assert!(expanded.contains(&Rectangle::new((32, 32).into(), (36, 36).into())));

        let unrelated = [Rectangle::new((0, 0).into(), (1, 1).into())];
        assert_eq!(prepared.expand_damage(&unrelated), unrelated);
    }
}

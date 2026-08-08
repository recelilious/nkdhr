//! Unicode shaping, cached layout and bounded glyph atlases.

mod atlas;
mod layout;

use std::{fmt, sync::Arc};

use cosmic_text::{FontSystem, SwashCache};
use nkdhr_render::{
    BuildError, Color, DisplayListBuilder, Point, Rect, Sampling, TextureError, TextureStore,
};

pub use atlas::{AtlasConfig, AtlasStats};
pub use layout::{FontSlant, TextAlign, TextLayout, TextStyle, TextWrap};

use self::{atlas::GlyphAtlas, layout::LayoutCache};

/// Text-system resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextConfig {
    pub layout_cache_capacity: usize,
    pub atlas: AtlasConfig,
}

impl Default for TextConfig {
    fn default() -> Self {
        Self {
            layout_cache_capacity: 256,
            atlas: AtlasConfig::default(),
        }
    }
}

#[derive(Debug)]
pub enum TextError {
    InvalidConfig(&'static str),
    InvalidStyle(&'static str),
    InvalidBounds,
    InvalidScale,
    InvalidOrigin,
    AtlasSizeOverflow,
    AtlasFull,
    GlyphTooLarge {
        width: u32,
        height: u32,
        page_size: u32,
    },
    InvalidGlyphData {
        expected: usize,
        actual: usize,
    },
    UnsupportedSubpixelMask,
    Texture(TextureError),
    DisplayList(BuildError),
}

impl fmt::Display for TextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) | Self::InvalidStyle(message) => {
                formatter.write_str(message)
            }
            Self::InvalidBounds => write!(formatter, "text bounds must be finite and non-negative"),
            Self::InvalidScale => write!(formatter, "text scale must be finite and positive"),
            Self::InvalidOrigin => {
                write!(formatter, "text origin must remain finite after scaling")
            }
            Self::AtlasSizeOverflow => write!(formatter, "glyph atlas allocation size overflowed"),
            Self::AtlasFull => write!(
                formatter,
                "all bounded glyph atlas pages are pinned by the current frame"
            ),
            Self::GlyphTooLarge {
                width,
                height,
                page_size,
            } => write!(
                formatter,
                "glyph {width}x{height} does not fit a {page_size}x{page_size} atlas page"
            ),
            Self::InvalidGlyphData { expected, actual } => write!(
                formatter,
                "rasterizer returned {actual} glyph bytes, expected {expected}"
            ),
            Self::UnsupportedSubpixelMask => {
                write!(formatter, "RGB subpixel glyph masks are not supported")
            }
            Self::Texture(error) => error.fmt(formatter),
            Self::DisplayList(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TextError {}

impl From<TextureError> for TextError {
    fn from(value: TextureError) -> Self {
        Self::Texture(value)
    }
}

impl From<BuildError> for TextError {
    fn from(value: BuildError) -> Self {
        Self::DisplayList(value)
    }
}

/// Per-draw cache activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextDrawStats {
    pub visible_glyphs: usize,
    pub recorded_glyphs: usize,
    pub cache_hits: u64,
    pub rasterized_glyphs: u64,
    pub atlas_uploads: u64,
    pub atlas_evictions: u64,
}

/// Shared application text state. One instance owns one font database,
/// shaping cache and render-context atlas policy.
#[derive(Debug)]
pub struct TextSystem {
    font_system: FontSystem,
    rasterizer: SwashCache,
    layouts: LayoutCache,
    atlas: GlyphAtlas,
    frame: u64,
}

impl TextSystem {
    /// Load installed fonts once and create bounded caches.
    pub fn new(config: TextConfig) -> Result<Self, TextError> {
        Self::with_font_system(FontSystem::new(), config)
    }

    /// Construct with an already configured cosmic-text font database. This
    /// is useful for deterministic tests and embedded-font environments.
    pub fn with_font_system(
        font_system: FontSystem,
        config: TextConfig,
    ) -> Result<Self, TextError> {
        Ok(Self {
            font_system,
            rasterizer: SwashCache::new(),
            layouts: LayoutCache::new(config.layout_cache_capacity)?,
            atlas: GlyphAtlas::new(config.atlas)?,
            frame: 0,
        })
    }

    /// Shape or retrieve a color-independent cached paragraph.
    pub fn layout(
        &mut self,
        text: &str,
        style: &TextStyle,
        width: Option<f32>,
        scale: f32,
    ) -> Result<Arc<TextLayout>, TextError> {
        self.layouts
            .get_or_shape(&mut self.font_system, text, style, width, scale)
    }

    /// Begin one submission frame. Pages referenced through the returned
    /// guard cannot be evicted until the guard is dropped.
    pub fn begin_frame(&mut self) -> TextFrame<'_> {
        self.frame = self.frame.wrapping_add(1).max(1);
        TextFrame { system: self }
    }

    pub fn atlas_stats(&self) -> AtlasStats {
        self.atlas.stats()
    }

    /// Changes when eviction invalidates previously recorded glyph texture
    /// coordinates. Retained paint caches compare this value before reuse.
    pub fn atlas_generation(&self) -> u64 {
        self.atlas.generation()
    }

    pub fn layout_cache_len(&self) -> usize {
        self.layouts.len()
    }

    /// Resolve the actual font family names used by a shaped layout.
    pub fn resolved_families(&self, layout: &TextLayout) -> Vec<String> {
        let mut families = layout
            .font_ids
            .iter()
            .filter_map(|id| self.font_system.db().face(*id))
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect::<Vec<_>>();
        families.sort();
        families.dedup();
        families
    }
}

/// Frame-scoped atlas pinning guard.
#[derive(Debug)]
pub struct TextFrame<'a> {
    system: &'a mut TextSystem,
}

impl TextFrame<'_> {
    /// Record visible glyphs at `origin`. An optional target-space clip avoids
    /// rasterizing offscreen lines and is also recorded in the display list.
    pub fn draw(
        &mut self,
        builder: &mut DisplayListBuilder,
        textures: &mut TextureStore,
        layout: &TextLayout,
        origin: Point,
        color: Color,
        clip: Option<Rect>,
    ) -> Result<TextDrawStats, TextError> {
        let scale = layout.scale();
        if !origin.x.is_finite()
            || !origin.y.is_finite()
            || !(origin.x * scale).is_finite()
            || !(origin.y * scale).is_finite()
        {
            return Err(TextError::InvalidOrigin);
        }
        if clip.is_some_and(|clip| !clip.is_finite()) {
            return Err(TextError::InvalidBounds);
        }

        let before = self.system.atlas.stats();
        let mut commands = Vec::new();
        let mut visible_glyphs = 0;
        let glyph_range = clip.map_or(0..layout.glyphs.len(), |clip| {
            layout.visible_glyph_range(origin.y, clip.y, clip.bottom())
        });
        for positioned in &layout.glyphs[glyph_range] {
            if clip.is_some_and(|clip| !estimated_visible(positioned, origin, clip)) {
                continue;
            }
            visible_glyphs += 1;
            let physical = positioned.glyph.physical(
                (origin.x * scale, (origin.y + positioned.line_y) * scale),
                scale,
            );
            let Some(resident) = self.system.atlas.resolve(
                &mut self.system.font_system,
                &mut self.system.rasterizer,
                textures,
                physical.cache_key,
                self.system.frame,
            )?
            else {
                continue;
            };
            let destination = Rect::new(
                (physical.x + resident.left) as f32 / scale,
                (physical.y - resident.top) as f32 / scale,
                resident.width as f32 / scale,
                resident.height as f32 / scale,
            );
            commands.push((
                destination,
                resident.texture,
                resident.source,
                if resident.color { Color::WHITE } else { color },
            ));
        }
        self.system.atlas.flush(textures)?;

        let record = |builder: &mut DisplayListBuilder| -> Result<(), BuildError> {
            for (destination, texture, source, tint) in &commands {
                builder.tinted_texture(
                    *destination,
                    *texture,
                    Some(*source),
                    *tint,
                    1.0,
                    Sampling::Nearest,
                )?;
            }
            Ok(())
        };
        match clip {
            Some(clip) => builder.with_clip(clip, record)?,
            None => record(builder)?,
        }

        let after = self.system.atlas.stats();
        Ok(TextDrawStats {
            visible_glyphs,
            recorded_glyphs: commands.len(),
            cache_hits: after.cache_hits - before.cache_hits,
            rasterized_glyphs: after.rasterized_glyphs - before.rasterized_glyphs,
            atlas_uploads: after.uploads - before.uploads,
            atlas_evictions: after.evictions - before.evictions,
        })
    }
}

fn estimated_visible(positioned: &layout::LayoutGlyph, origin: Point, clip: Rect) -> bool {
    let margin = positioned.glyph.font_size;
    let bounds = Rect::new(
        origin.x + positioned.glyph.x - margin,
        origin.y + positioned.line_top - margin,
        positioned.glyph.w + margin * 2.0,
        positioned.line_height + margin * 2.0,
    );
    bounds.intersect(clip).is_some()
}

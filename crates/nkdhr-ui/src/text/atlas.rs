use std::collections::{HashMap, HashSet};

use cosmic_text::{CacheKey, FontSystem, SwashCache, SwashContent, SwashImage};
use nkdhr_render::{AlphaMode, Rect, TextureFormat, TextureId, TextureStore};

use super::TextError;

const GLYPH_PADDING: u32 = 1;

/// Hard limits for render-context glyph storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasConfig {
    pub page_size: u32,
    pub max_mask_pages: usize,
    pub max_color_pages: usize,
    pub max_empty_entries: usize,
}

impl Default for AtlasConfig {
    fn default() -> Self {
        Self {
            page_size: 1024,
            max_mask_pages: 4,
            max_color_pages: 2,
            max_empty_entries: 2048,
        }
    }
}

impl AtlasConfig {
    pub(crate) fn validate(self) -> Result<Self, TextError> {
        if !(16..=8192).contains(&self.page_size) {
            return Err(TextError::InvalidConfig(
                "atlas page size must be between 16 and 8192 pixels",
            ));
        }
        if self.max_mask_pages == 0 || self.max_color_pages == 0 {
            return Err(TextError::InvalidConfig(
                "mask and color atlas page limits must be positive",
            ));
        }
        if self.max_empty_entries == 0 {
            return Err(TextError::InvalidConfig(
                "empty glyph cache capacity must be positive",
            ));
        }
        Ok(self)
    }
}

/// Current and cumulative atlas counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AtlasStats {
    pub mask_pages: usize,
    pub color_pages: usize,
    pub resident_glyphs: usize,
    pub evictions: u64,
    pub uploads: u64,
    pub rasterized_glyphs: u64,
    pub cache_hits: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AtlasKind {
    Mask,
    Color,
}

#[derive(Debug)]
struct GlyphImage {
    kind: AtlasKind,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl GlyphImage {
    fn from_swash(image: SwashImage) -> Result<Self, TextError> {
        let kind = match image.content {
            SwashContent::Mask => AtlasKind::Mask,
            SwashContent::Color => AtlasKind::Color,
            SwashContent::SubpixelMask => return Err(TextError::UnsupportedSubpixelMask),
        };
        Ok(Self {
            kind,
            left: image.placement.left,
            top: image.placement.top,
            width: image.placement.width,
            height: image.placement.height,
            data: image.data,
        })
    }
}

impl AtlasKind {
    fn format(self) -> TextureFormat {
        match self {
            Self::Mask => TextureFormat::Alpha8,
            Self::Color => TextureFormat::Rgba8,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResidentGlyph {
    pub texture: TextureId,
    pub source: Rect,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub color: bool,
}

#[derive(Debug, Clone, Copy)]
struct GlyphEntry {
    kind: AtlasKind,
    page: usize,
    source: Rect,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

#[derive(Debug)]
struct AtlasPage {
    texture: TextureId,
    pixels: Vec<u8>,
    allocator: SkylineAllocator,
    keys: HashSet<CacheKey>,
    last_used: u64,
    pinned_frame: u64,
    dirty: bool,
}

impl AtlasPage {
    fn new(
        kind: AtlasKind,
        page_size: u32,
        textures: &mut TextureStore,
    ) -> Result<Self, TextError> {
        let pixel_count = usize::try_from(page_size)
            .ok()
            .and_then(|size| size.checked_mul(size))
            .and_then(|pixels| pixels.checked_mul(kind.format().bytes_per_pixel()))
            .ok_or(TextError::AtlasSizeOverflow)?;
        let pixels = vec![0; pixel_count];
        let texture = match kind {
            AtlasKind::Mask => textures.insert_mask(page_size, page_size, pixels.clone())?,
            AtlasKind::Color => {
                textures.insert(page_size, page_size, pixels.clone(), AlphaMode::Straight)?
            }
        };
        Ok(Self {
            texture,
            pixels,
            allocator: SkylineAllocator::new(page_size),
            keys: HashSet::new(),
            last_used: 0,
            pinned_frame: 0,
            dirty: false,
        })
    }

    fn reset(&mut self, page_size: u32) {
        self.pixels.fill(0);
        self.allocator = SkylineAllocator::new(page_size);
        self.keys.clear();
        self.dirty = true;
    }
}

#[derive(Debug)]
pub(crate) struct GlyphAtlas {
    config: AtlasConfig,
    mask_pages: Vec<AtlasPage>,
    color_pages: Vec<AtlasPage>,
    entries: HashMap<CacheKey, GlyphEntry>,
    empty: HashSet<CacheKey>,
    clock: u64,
    generation: u64,
    stats: AtlasStats,
}

impl GlyphAtlas {
    pub fn new(config: AtlasConfig) -> Result<Self, TextError> {
        Ok(Self {
            config: config.validate()?,
            mask_pages: Vec::new(),
            color_pages: Vec::new(),
            entries: HashMap::new(),
            empty: HashSet::new(),
            clock: 0,
            generation: 1,
            stats: AtlasStats::default(),
        })
    }

    pub fn stats(&self) -> AtlasStats {
        AtlasStats {
            mask_pages: self.mask_pages.len(),
            color_pages: self.color_pages.len(),
            resident_glyphs: self.entries.len(),
            ..self.stats
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn resolve(
        &mut self,
        font_system: &mut FontSystem,
        rasterizer: &mut SwashCache,
        textures: &mut TextureStore,
        key: CacheKey,
        frame: u64,
    ) -> Result<Option<ResidentGlyph>, TextError> {
        self.clock = self.clock.wrapping_add(1).max(1);
        if let Some(entry) = self.entries.get(&key).copied() {
            let clock = self.clock;
            let texture = {
                let page = self.page_mut(entry.kind, entry.page);
                page.last_used = clock;
                page.pinned_frame = frame;
                page.texture
            };
            self.stats.cache_hits += 1;
            return Ok(Some(resident(texture, entry)));
        }
        if self.empty.contains(&key) {
            self.stats.cache_hits += 1;
            return Ok(None);
        }

        let image = match rasterize_colr(font_system, key) {
            Some(image) => Some(image),
            None => rasterizer
                .get_image_uncached(font_system, key)
                .map(GlyphImage::from_swash)
                .transpose()?,
        };
        let Some(image) = image else {
            self.remember_empty(key);
            return Ok(None);
        };
        if image.width == 0 || image.height == 0 {
            self.remember_empty(key);
            return Ok(None);
        }
        let kind = image.kind;
        validate_image(&image)?;
        let padded_width = image
            .width
            .checked_add(GLYPH_PADDING * 2)
            .ok_or(TextError::AtlasSizeOverflow)?;
        let padded_height = image
            .height
            .checked_add(GLYPH_PADDING * 2)
            .ok_or(TextError::AtlasSizeOverflow)?;
        if padded_width > self.config.page_size || padded_height > self.config.page_size {
            return Err(TextError::GlyphTooLarge {
                width: image.width,
                height: image.height,
                page_size: self.config.page_size,
            });
        }

        let (page_index, x, y) =
            self.allocate(kind, padded_width, padded_height, textures, frame)?;
        let source_x = x + GLYPH_PADDING;
        let source_y = y + GLYPH_PADDING;
        let page_size = self.config.page_size;
        let clock = self.clock;
        let texture = {
            let page = self.page_mut(kind, page_index);
            blit(page, page_size, kind, source_x, source_y, &image)?;
            page.keys.insert(key);
            page.last_used = clock;
            page.pinned_frame = frame;
            page.dirty = true;
            page.texture
        };
        let entry = GlyphEntry {
            kind,
            page: page_index,
            source: Rect::new(
                source_x as f32,
                source_y as f32,
                image.width as f32,
                image.height as f32,
            ),
            left: image.left,
            top: image.top,
            width: image.width,
            height: image.height,
        };
        self.entries.insert(key, entry);
        self.stats.rasterized_glyphs += 1;
        Ok(Some(resident(texture, entry)))
    }

    pub fn flush(&mut self, textures: &mut TextureStore) -> Result<(), TextError> {
        for (kind, pages) in [
            (AtlasKind::Mask, &mut self.mask_pages),
            (AtlasKind::Color, &mut self.color_pages),
        ] {
            for page in pages {
                if !page.dirty {
                    continue;
                }
                match kind {
                    AtlasKind::Mask => textures.update_mask(
                        page.texture,
                        self.config.page_size,
                        self.config.page_size,
                        page.pixels.clone(),
                    )?,
                    AtlasKind::Color => textures.update(
                        page.texture,
                        self.config.page_size,
                        self.config.page_size,
                        page.pixels.clone(),
                        AlphaMode::Straight,
                    )?,
                }
                page.dirty = false;
                self.stats.uploads += 1;
            }
        }
        Ok(())
    }

    fn allocate(
        &mut self,
        kind: AtlasKind,
        width: u32,
        height: u32,
        textures: &mut TextureStore,
        frame: u64,
    ) -> Result<(usize, u32, u32), TextError> {
        let page_count = self.pages(kind).len();
        for index in 0..page_count {
            if let Some((x, y)) = self.page_mut(kind, index).allocator.allocate(width, height) {
                return Ok((index, x, y));
            }
        }

        let maximum = match kind {
            AtlasKind::Mask => self.config.max_mask_pages,
            AtlasKind::Color => self.config.max_color_pages,
        };
        if page_count < maximum {
            let mut page = AtlasPage::new(kind, self.config.page_size, textures)?;
            let (x, y) = page
                .allocator
                .allocate(width, height)
                .expect("a validated glyph fits an empty page");
            self.pages_mut(kind).push(page);
            return Ok((page_count, x, y));
        }

        let victim = self
            .pages(kind)
            .iter()
            .enumerate()
            .filter(|(_, page)| page.pinned_frame != frame)
            .min_by_key(|(_, page)| page.last_used)
            .map(|(index, _)| index)
            .ok_or(TextError::AtlasFull)?;
        let removed = {
            let page_size = self.config.page_size;
            let page = self.page_mut(kind, victim);
            let removed = page.keys.iter().copied().collect::<Vec<_>>();
            page.reset(page_size);
            removed
        };
        for key in removed {
            self.entries.remove(&key);
        }
        self.stats.evictions += 1;
        self.generation = self.generation.wrapping_add(1).max(1);
        let (x, y) = self
            .page_mut(kind, victim)
            .allocator
            .allocate(width, height)
            .expect("a validated glyph fits a reset page");
        Ok((victim, x, y))
    }

    fn remember_empty(&mut self, key: CacheKey) {
        if self.empty.len() == self.config.max_empty_entries {
            self.empty.clear();
        }
        self.empty.insert(key);
    }

    fn pages(&self, kind: AtlasKind) -> &[AtlasPage] {
        match kind {
            AtlasKind::Mask => &self.mask_pages,
            AtlasKind::Color => &self.color_pages,
        }
    }

    fn pages_mut(&mut self, kind: AtlasKind) -> &mut Vec<AtlasPage> {
        match kind {
            AtlasKind::Mask => &mut self.mask_pages,
            AtlasKind::Color => &mut self.color_pages,
        }
    }

    fn page_mut(&mut self, kind: AtlasKind, index: usize) -> &mut AtlasPage {
        &mut self.pages_mut(kind)[index]
    }
}

fn resident(texture: TextureId, entry: GlyphEntry) -> ResidentGlyph {
    ResidentGlyph {
        texture,
        source: entry.source,
        left: entry.left,
        top: entry.top,
        width: entry.width,
        height: entry.height,
        color: entry.kind == AtlasKind::Color,
    }
}

fn rasterize_colr(font_system: &mut FontSystem, key: CacheKey) -> Option<GlyphImage> {
    let font = font_system.get_font(key.font_id, key.font_weight)?;
    let image = oxitext_raster::render_colr_glyph_sized(
        font.data(),
        key.glyph_id,
        f32::from_bits(key.font_size_bits),
        0,
    )?;
    Some(GlyphImage {
        kind: AtlasKind::Color,
        left: image.bearing_x,
        top: image.bearing_y,
        width: image.width,
        height: image.height,
        data: image.rgba,
    })
}

fn validate_image(image: &GlyphImage) -> Result<(), TextError> {
    let expected = usize::try_from(image.width)
        .ok()
        .and_then(|width| {
            usize::try_from(image.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(image.kind.format().bytes_per_pixel()))
        .ok_or(TextError::AtlasSizeOverflow)?;
    if image.data.len() == expected {
        Ok(())
    } else {
        Err(TextError::InvalidGlyphData {
            expected,
            actual: image.data.len(),
        })
    }
}

fn blit(
    page: &mut AtlasPage,
    page_size: u32,
    kind: AtlasKind,
    x: u32,
    y: u32,
    image: &GlyphImage,
) -> Result<(), TextError> {
    let bytes_per_pixel = kind.format().bytes_per_pixel();
    let page_stride = usize::try_from(page_size)
        .ok()
        .and_then(|size| size.checked_mul(bytes_per_pixel))
        .ok_or(TextError::AtlasSizeOverflow)?;
    let source_stride = usize::try_from(image.width)
        .ok()
        .and_then(|width| width.checked_mul(bytes_per_pixel))
        .ok_or(TextError::AtlasSizeOverflow)?;
    for row in 0..image.height as usize {
        let destination = (y as usize + row)
            .checked_mul(page_stride)
            .and_then(|offset| offset.checked_add(x as usize * bytes_per_pixel))
            .ok_or(TextError::AtlasSizeOverflow)?;
        let source = row
            .checked_mul(source_stride)
            .ok_or(TextError::AtlasSizeOverflow)?;
        page.pixels[destination..destination + source_stride]
            .copy_from_slice(&image.data[source..source + source_stride]);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct SkylineNode {
    x: u32,
    y: u32,
    width: u32,
}

#[derive(Debug)]
struct SkylineAllocator {
    size: u32,
    nodes: Vec<SkylineNode>,
}

impl SkylineAllocator {
    fn new(size: u32) -> Self {
        Self {
            size,
            nodes: vec![SkylineNode {
                x: 0,
                y: 0,
                width: size,
            }],
        }
    }

    fn allocate(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let mut best: Option<(usize, u32, u32)> = None;
        for index in 0..self.nodes.len() {
            let Some(y) = self.fit(index, width, height) else {
                continue;
            };
            let x = self.nodes[index].x;
            if best.is_none_or(|(_, best_y, best_x)| (y, x) < (best_y, best_x)) {
                best = Some((index, y, x));
            }
        }
        let (index, y, x) = best?;
        self.nodes.insert(
            index,
            SkylineNode {
                x,
                y: y + height,
                width,
            },
        );

        let cursor = index + 1;
        while cursor < self.nodes.len() {
            let previous_right = self.nodes[cursor - 1].x + self.nodes[cursor - 1].width;
            if self.nodes[cursor].x >= previous_right {
                break;
            }
            let shrink = previous_right - self.nodes[cursor].x;
            if self.nodes[cursor].width <= shrink {
                self.nodes.remove(cursor);
            } else {
                self.nodes[cursor].x += shrink;
                self.nodes[cursor].width -= shrink;
                break;
            }
        }
        self.merge();
        Some((x, y))
    }

    fn fit(&self, index: usize, width: u32, height: u32) -> Option<u32> {
        let x = self.nodes[index].x;
        if x.checked_add(width)? > self.size {
            return None;
        }
        let mut y = self.nodes[index].y;
        let mut remaining = width;
        let mut cursor = index;
        while remaining > 0 {
            let node = *self.nodes.get(cursor)?;
            y = y.max(node.y);
            if y.checked_add(height)? > self.size {
                return None;
            }
            remaining = remaining.saturating_sub(node.width);
            cursor += 1;
        }
        Some(y)
    }

    fn merge(&mut self) {
        let mut index = 0;
        while index + 1 < self.nodes.len() {
            if self.nodes[index].y == self.nodes[index + 1].y {
                let width = self.nodes[index + 1].width;
                self.nodes[index].width += width;
                self.nodes.remove(index + 1);
            } else {
                index += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skyline_reuses_vertical_space_without_overlap() {
        let mut allocator = SkylineAllocator::new(16);
        assert_eq!(allocator.allocate(8, 8), Some((0, 0)));
        assert_eq!(allocator.allocate(8, 4), Some((8, 0)));
        assert_eq!(allocator.allocate(8, 4), Some((8, 4)));
        assert_eq!(allocator.allocate(16, 8), Some((0, 8)));
        assert_eq!(allocator.allocate(1, 1), None);
    }
}

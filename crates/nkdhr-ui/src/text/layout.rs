use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};

use cosmic_text::{
    Align, Attrs, Buffer, CacheKeyFlags, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap,
};

use super::TextError;

/// CSS-like font slant used by nkdhr text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FontSlant {
    #[default]
    Normal,
    Italic,
    Oblique,
}

/// Line wrapping policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TextWrap {
    None,
    Glyph,
    Word,
    #[default]
    WordOrGlyph,
}

/// Horizontal line alignment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    #[default]
    Start,
    End,
    Center,
    Justified,
}

/// Font and paragraph attributes that affect shaping or layout.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// Preferred families in priority order. The first installed family is
    /// selected before cosmic-text performs per-script fallback.
    pub families: Vec<String>,
    /// OpenType weight in the inclusive range 1..=1000.
    pub weight: u16,
    pub slant: FontSlant,
    pub font_size: f32,
    pub line_height: f32,
    pub wrap: TextWrap,
    pub align: TextAlign,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            families: vec!["Noto Sans".to_owned()],
            weight: 400,
            slant: FontSlant::Normal,
            font_size: 16.0,
            line_height: 22.0,
            wrap: TextWrap::WordOrGlyph,
            align: TextAlign::Start,
        }
    }
}

impl TextStyle {
    pub(crate) fn validate(&self) -> Result<(), TextError> {
        if self.families.iter().any(|family| family.trim().is_empty()) {
            return Err(TextError::InvalidStyle("font families must not be empty"));
        }
        if !(1..=1000).contains(&self.weight) {
            return Err(TextError::InvalidStyle(
                "font weight must be between 1 and 1000",
            ));
        }
        if !self.font_size.is_finite() || self.font_size <= 0.0 {
            return Err(TextError::InvalidStyle(
                "font size must be finite and positive",
            ));
        }
        if !self.line_height.is_finite() || self.line_height <= 0.0 {
            return Err(TextError::InvalidStyle(
                "line height must be finite and positive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LayoutGlyph {
    pub glyph: cosmic_text::LayoutGlyph,
    pub line_y: f32,
    pub line_top: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone)]
struct LayoutLineRange {
    glyphs: Range<usize>,
    top: f32,
    bottom: f32,
}

/// Immutable shaped text, independent from glyph-atlas residency and color.
#[derive(Debug)]
pub struct TextLayout {
    pub(crate) glyphs: Vec<LayoutGlyph>,
    pub(crate) font_ids: HashSet<cosmic_text::fontdb::ID>,
    lines: Vec<LayoutLineRange>,
    line_margin: f32,
    width: f32,
    height: f32,
    line_count: usize,
    scale: f32,
}

impl TextLayout {
    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    pub fn glyph_count(&self) -> usize {
        self.glyphs.len()
    }

    pub fn distinct_font_count(&self) -> usize {
        self.font_ids.len()
    }

    pub fn has_right_to_left_glyphs(&self) -> bool {
        self.glyphs.iter().any(|glyph| glyph.glyph.level.is_rtl())
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub(crate) fn visible_glyph_range(
        &self,
        origin_y: f32,
        clip_top: f32,
        clip_bottom: f32,
    ) -> Range<usize> {
        let local_top = clip_top - origin_y;
        let local_bottom = clip_bottom - origin_y;
        let start = self
            .lines
            .partition_point(|line| line.bottom + self.line_margin <= local_top);
        let end = self.lines[start..]
            .partition_point(|line| line.top - self.line_margin < local_bottom)
            + start;
        match self.lines.get(start..end) {
            Some(lines) if !lines.is_empty() => {
                lines.first().expect("non-empty slice").glyphs.start
                    ..lines.last().expect("non-empty slice").glyphs.end
            }
            _ => 0..0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LayoutKey {
    text: Arc<str>,
    families: Arc<[String]>,
    weight: u16,
    slant: FontSlant,
    font_size_bits: u32,
    line_height_bits: u32,
    wrap: TextWrap,
    align: TextAlign,
    width_bits: Option<u32>,
    scale_bits: u32,
    locale: Arc<str>,
}

#[derive(Debug)]
struct CachedLayout {
    layout: Arc<TextLayout>,
    last_used: u64,
}

/// Bounded LRU cache of shaped paragraphs.
#[derive(Debug)]
pub(crate) struct LayoutCache {
    capacity: usize,
    clock: u64,
    entries: HashMap<LayoutKey, CachedLayout>,
}

impl LayoutCache {
    pub fn new(capacity: usize) -> Result<Self, TextError> {
        if capacity == 0 {
            return Err(TextError::InvalidConfig(
                "layout cache capacity must be positive",
            ));
        }
        Ok(Self {
            capacity,
            clock: 0,
            entries: HashMap::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get_or_shape(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        style: &TextStyle,
        width: Option<f32>,
        scale: f32,
    ) -> Result<Arc<TextLayout>, TextError> {
        style.validate()?;
        if width.is_some_and(|width| !width.is_finite() || width < 0.0) {
            return Err(TextError::InvalidBounds);
        }
        if !scale.is_finite() || scale <= 0.0 {
            return Err(TextError::InvalidScale);
        }
        self.clock = self.clock.wrapping_add(1).max(1);
        let key = LayoutKey {
            text: Arc::from(text),
            families: Arc::from(style.families.clone()),
            weight: style.weight,
            slant: style.slant,
            font_size_bits: style.font_size.to_bits(),
            line_height_bits: style.line_height.to_bits(),
            wrap: style.wrap,
            align: style.align,
            width_bits: width.map(f32::to_bits),
            scale_bits: scale.to_bits(),
            locale: Arc::from(font_system.locale()),
        };
        if let Some(cached) = self.entries.get_mut(&key) {
            cached.last_used = self.clock;
            return Ok(Arc::clone(&cached.layout));
        }

        let layout = Arc::new(shape(font_system, text, style, width, scale)?);
        if self.entries.len() == self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key.clone())
                .expect("a full positive-capacity cache has an entry");
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            key,
            CachedLayout {
                layout: Arc::clone(&layout),
                last_used: self.clock,
            },
        );
        Ok(layout)
    }
}

fn shape(
    font_system: &mut FontSystem,
    text: &str,
    style: &TextStyle,
    width: Option<f32>,
    scale: f32,
) -> Result<TextLayout, TextError> {
    let family = select_family(font_system, &style.families);
    let attrs = Attrs::new()
        .family(family)
        .weight(Weight(style.weight))
        .style(match style.slant {
            FontSlant::Normal => Style::Normal,
            FontSlant::Italic => Style::Italic,
            FontSlant::Oblique => Style::Oblique,
        })
        .cache_key_flags(CacheKeyFlags::empty());
    let mut buffer = Buffer::new(
        font_system,
        Metrics::new(style.font_size, style.line_height),
    );
    {
        let mut buffer = buffer.borrow_with(font_system);
        buffer.set_size(width, None);
        buffer.set_wrap(match style.wrap {
            TextWrap::None => Wrap::None,
            TextWrap::Glyph => Wrap::Glyph,
            TextWrap::Word => Wrap::Word,
            TextWrap::WordOrGlyph => Wrap::WordOrGlyph,
        });
        let alignment = match style.align {
            TextAlign::Start => None,
            TextAlign::End => Some(Align::End),
            TextAlign::Center => Some(Align::Center),
            TextAlign::Justified => Some(Align::Justified),
        };
        buffer.set_text(text, &attrs, Shaping::Advanced, alignment);
        buffer.shape_until_scroll(false);
    }

    let mut glyphs = Vec::new();
    let mut lines = Vec::new();
    let mut font_ids = HashSet::new();
    let mut measured_width = 0.0_f32;
    let mut measured_height = 0.0_f32;
    let mut line_count = 0;
    for run in buffer.layout_runs() {
        line_count += 1;
        measured_width = measured_width.max(run.line_w);
        measured_height = measured_height.max(run.line_top + run.line_height);
        let start = glyphs.len();
        for glyph in run.glyphs {
            font_ids.insert(glyph.font_id);
            glyphs.push(LayoutGlyph {
                glyph: glyph.clone(),
                line_y: run.line_y,
                line_top: run.line_top,
                line_height: run.line_height,
            });
        }
        lines.push(LayoutLineRange {
            glyphs: start..glyphs.len(),
            top: run.line_top,
            bottom: run.line_top + run.line_height,
        });
    }
    Ok(TextLayout {
        glyphs,
        font_ids,
        lines,
        line_margin: style.font_size,
        width: measured_width,
        height: measured_height,
        line_count,
        scale,
    })
}

fn select_family<'a>(font_system: &FontSystem, families: &'a [String]) -> Family<'a> {
    families
        .iter()
        .find(|requested| {
            font_system.db().faces().any(|face| {
                face.families
                    .iter()
                    .any(|(available, _)| available.eq_ignore_ascii_case(requested))
            })
        })
        .map(|family| Family::Name(family))
        .unwrap_or(Family::SansSerif)
}

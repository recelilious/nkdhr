use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};

use cosmic_text::{
    Align, Attrs, Buffer, CacheKeyFlags, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap,
};
use nkdhr_render::{Point, Rect};
use unicode_segmentation::UnicodeSegmentation;

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
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_y: f32,
    pub line_top: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone)]
struct LayoutLineRange {
    glyphs: Range<usize>,
    bytes: Range<usize>,
    top: f32,
    bottom: f32,
}

/// Nearest editable byte boundary returned by text hit testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextHit {
    pub byte_index: usize,
    pub line_index: usize,
}

/// Logical caret geometry relative to the text layout origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextCaret {
    pub byte_index: usize,
    pub line_index: usize,
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

/// One visual fragment of a logical text selection.
///
/// A single logical range can produce several fragments on one line when it
/// crosses bidirectional runs, and one fragment per visual line when wrapped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextSelectionRect {
    pub line_index: usize,
    pub rect: Rect,
}

/// Immutable shaped text, independent from glyph-atlas residency and color.
#[derive(Debug)]
pub struct TextLayout {
    text: Arc<str>,
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

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Find the nearest grapheme-safe byte boundary for a local point.
    pub fn hit_test(&self, point: Point) -> TextHit {
        let line_index = self.line_for_y(point.y);
        let Some(line) = self.lines.get(line_index) else {
            return TextHit {
                byte_index: 0,
                line_index: 0,
            };
        };
        let glyphs = &self.glyphs[line.glyphs.clone()];
        if glyphs.is_empty() {
            return TextHit {
                byte_index: line.bytes.start.min(self.text.len()),
                line_index,
            };
        }

        for positioned in glyphs {
            let glyph = &positioned.glyph;
            if point.x >= glyph.x && point.x <= glyph.x + glyph.w {
                return TextHit {
                    byte_index: hit_glyph(&self.text, positioned, point.x),
                    line_index,
                };
            }
        }

        let mut nearest = (f32::INFINITY, line.bytes.end.min(self.text.len()));
        for positioned in glyphs {
            let glyph = &positioned.glyph;
            let edges = if glyph.level.is_rtl() {
                [
                    (glyph.x, positioned.byte_end),
                    (glyph.x + glyph.w, positioned.byte_start),
                ]
            } else {
                [
                    (glyph.x, positioned.byte_start),
                    (glyph.x + glyph.w, positioned.byte_end),
                ]
            };
            for (x, byte_index) in edges {
                let distance = (point.x - x).abs();
                if distance < nearest.0 {
                    nearest = (distance, byte_index);
                }
            }
        }
        TextHit {
            byte_index: nearest.1,
            line_index,
        }
    }

    /// Resolve a grapheme-safe source byte boundary to local caret geometry.
    pub fn caret(&self, byte_index: usize) -> TextCaret {
        let byte_index = floor_grapheme_boundary(&self.text, byte_index.min(self.text.len()));
        let mut fallback = TextCaret {
            byte_index,
            line_index: self.lines.len().saturating_sub(1),
            x: self.width,
            y: self.lines.last().map_or(0.0, |line| line.top),
            height: self.lines.last().map_or(0.0, |line| line.bottom - line.top),
        };

        // Prefer a cluster start. At wrapped or explicit line boundaries this
        // places the caret on the following visual line.
        for (line_index, line) in self.lines.iter().enumerate() {
            for positioned in &self.glyphs[line.glyphs.clone()] {
                if byte_index == positioned.byte_start {
                    return caret_at_edge(positioned, byte_index, line_index, true);
                }
            }
        }
        for (line_index, line) in self.lines.iter().enumerate() {
            for positioned in &self.glyphs[line.glyphs.clone()] {
                if byte_index > positioned.byte_start && byte_index < positioned.byte_end {
                    let cluster = &self.text[positioned.byte_start..positioned.byte_end];
                    let before = cluster
                        .grapheme_indices(true)
                        .filter(|(index, _)| positioned.byte_start + index < byte_index)
                        .count();
                    let total = cluster.graphemes(true).count().max(1);
                    let fraction = before as f32 / total as f32;
                    let x = if positioned.glyph.level.is_rtl() {
                        positioned.glyph.x + positioned.glyph.w * (1.0 - fraction)
                    } else {
                        positioned.glyph.x + positioned.glyph.w * fraction
                    };
                    return TextCaret {
                        byte_index,
                        line_index,
                        x,
                        y: positioned.line_top,
                        height: positioned.line_height,
                    };
                }
                if byte_index == positioned.byte_end {
                    fallback = caret_at_edge(positioned, byte_index, line_index, false);
                }
            }
            if line.glyphs.is_empty() && line.bytes.contains(&byte_index) {
                return TextCaret {
                    byte_index,
                    line_index,
                    x: 0.0,
                    y: line.top,
                    height: line.bottom - line.top,
                };
            }
        }
        fallback
    }

    /// Resolve a grapheme-aligned logical range into exact visual fragments.
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<TextSelectionRect> {
        let start = floor_grapheme_boundary(&self.text, range.start.min(self.text.len()));
        let end = ceil_grapheme_boundary(&self.text, range.end.min(self.text.len()));
        if start >= end {
            return Vec::new();
        }

        let mut fragments = Vec::new();
        for (line_index, line) in self.lines.iter().enumerate() {
            let mut cells = Vec::new();
            for positioned in &self.glyphs[line.glyphs.clone()] {
                if positioned.byte_end <= start || positioned.byte_start >= end {
                    continue;
                }
                let graphemes = self.text[positioned.byte_start..positioned.byte_end]
                    .grapheme_indices(true)
                    .collect::<Vec<_>>();
                if graphemes.is_empty() || positioned.glyph.w <= 0.0 {
                    continue;
                }
                let cell_width = positioned.glyph.w / graphemes.len() as f32;
                for (logical_index, (offset, grapheme)) in graphemes.iter().enumerate() {
                    let byte_start = positioned.byte_start + offset;
                    let byte_end = byte_start + grapheme.len();
                    if byte_end <= start || byte_start >= end {
                        continue;
                    }
                    let visual_index = if positioned.glyph.level.is_rtl() {
                        graphemes.len() - 1 - logical_index
                    } else {
                        logical_index
                    };
                    cells.push(Rect::new(
                        positioned.glyph.x + visual_index as f32 * cell_width,
                        positioned.line_top,
                        cell_width.max(1.0),
                        positioned.line_height,
                    ));
                }
            }

            cells.sort_by(|left, right| left.x.total_cmp(&right.x));
            let mut merged: Vec<Rect> = Vec::new();
            for cell in cells {
                if let Some(previous) = merged.last_mut()
                    && cell.x <= previous.right() + 0.5
                    && (cell.y - previous.y).abs() <= 0.5
                {
                    let right = previous.right().max(cell.right());
                    previous.width = right - previous.x;
                    previous.height = previous.height.max(cell.height);
                } else {
                    merged.push(cell);
                }
            }

            if merged.is_empty()
                && line.bytes.is_empty()
                && start <= line.bytes.start
                && end > line.bytes.start
            {
                merged.push(Rect::new(0.0, line.top, 1.0, line.bottom - line.top));
            }
            fragments.extend(
                merged
                    .into_iter()
                    .map(|rect| TextSelectionRect { line_index, rect }),
            );
        }
        fragments
    }

    /// Bounds for a visual line, used by vertical caret movement.
    pub fn line_bounds(&self, line_index: usize) -> Option<Rect> {
        self.lines
            .get(line_index)
            .map(|line| Rect::new(0.0, line.top, self.width, line.bottom - line.top))
    }

    /// Move to the nearest grapheme boundary at `x` on a visual line.
    pub fn hit_line(&self, line_index: usize, x: f32) -> TextHit {
        let line_index = line_index.min(self.lines.len().saturating_sub(1));
        let y = self
            .lines
            .get(line_index)
            .map_or(0.0, |line| (line.top + line.bottom) * 0.5);
        let x = if x == f32::NEG_INFINITY {
            -1.0
        } else if x == f32::INFINITY {
            self.width + 1.0
        } else {
            x
        };
        self.hit_test(Point::new(x, y))
    }

    /// Move one grapheme boundary in visual order, crossing visual lines at
    /// their nearest edge. Logical word movement remains a TextInput policy.
    pub fn visual_neighbor(&self, byte_index: usize, forward: bool) -> usize {
        let current = self.caret(byte_index);
        let mut carets = self
            .text
            .grapheme_indices(true)
            .map(|(boundary, _)| boundary)
            .chain(std::iter::once(self.text.len()))
            .map(|boundary| self.caret(boundary))
            .filter(|caret| caret.line_index == current.line_index)
            .collect::<Vec<_>>();
        carets.sort_by(|left, right| {
            left.x
                .total_cmp(&right.x)
                .then_with(|| left.byte_index.cmp(&right.byte_index))
        });
        carets.dedup_by_key(|caret| caret.byte_index);
        if let Some(index) = carets
            .iter()
            .position(|caret| caret.byte_index == current.byte_index)
        {
            if forward && index + 1 < carets.len() {
                return carets[index + 1].byte_index;
            }
            if !forward && index > 0 {
                return carets[index - 1].byte_index;
            }
        }

        let adjacent = if forward {
            current.line_index.checked_add(1)
        } else {
            current.line_index.checked_sub(1)
        };
        adjacent
            .filter(|line| *line < self.lines.len())
            .map(|line| {
                self.hit_line(
                    line,
                    if forward {
                        f32::NEG_INFINITY
                    } else {
                        f32::INFINITY
                    },
                )
                .byte_index
            })
            .unwrap_or(current.byte_index)
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

    fn line_for_y(&self, y: f32) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        if y < self.lines[0].top {
            return 0;
        }
        self.lines
            .iter()
            .position(|line| y >= line.top && y < line.bottom)
            .unwrap_or(self.lines.len() - 1)
    }
}

fn hit_glyph(text: &str, positioned: &LayoutGlyph, x: f32) -> usize {
    let glyph = &positioned.glyph;
    let cluster = &text[positioned.byte_start..positioned.byte_end];
    let graphemes = cluster.grapheme_indices(true).collect::<Vec<_>>();
    if graphemes.is_empty() || glyph.w <= 0.0 {
        return positioned.byte_start;
    }
    let cell_width = glyph.w / graphemes.len() as f32;
    let visual = ((x - glyph.x) / cell_width)
        .floor()
        .clamp(0.0, (graphemes.len() - 1) as f32) as usize;
    let logical = if glyph.level.is_rtl() {
        graphemes.len() - 1 - visual
    } else {
        visual
    };
    let (offset, grapheme) = graphemes[logical];
    let cell_x = glyph.x + visual as f32 * cell_width;
    let after_visual_half = x >= cell_x + cell_width * 0.5;
    let after_logically = after_visual_half != glyph.level.is_rtl();
    positioned.byte_start + offset + usize::from(after_logically) * grapheme.len()
}

fn caret_at_edge(
    positioned: &LayoutGlyph,
    byte_index: usize,
    line_index: usize,
    start: bool,
) -> TextCaret {
    let rtl = positioned.glyph.level.is_rtl();
    let x = if start == rtl {
        positioned.glyph.x + positioned.glyph.w
    } else {
        positioned.glyph.x
    };
    TextCaret {
        byte_index,
        line_index,
        x,
        y: positioned.line_top,
        height: positioned.line_height,
    }
}

fn floor_grapheme_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(boundary, _)| boundary)
        .take_while(|boundary| *boundary <= index)
        .last()
        .unwrap_or(0)
}

fn ceil_grapheme_boundary(text: &str, index: usize) -> usize {
    if index == 0 || index >= text.len() {
        return index.min(text.len());
    }
    text.grapheme_indices(true)
        .map(|(boundary, _)| boundary)
        .chain(std::iter::once(text.len()))
        .find(|boundary| *boundary >= index)
        .unwrap_or(text.len())
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
    let line_starts = source_line_starts(text);
    for run in buffer.layout_runs() {
        line_count += 1;
        measured_width = measured_width.max(run.line_w);
        measured_height = measured_height.max(run.line_top + run.line_height);
        let start = glyphs.len();
        let source_start = line_starts.get(run.line_i).copied().unwrap_or(0);
        for glyph in run.glyphs {
            font_ids.insert(glyph.font_id);
            glyphs.push(LayoutGlyph {
                byte_start: source_start + glyph.start,
                byte_end: source_start + glyph.end,
                glyph: glyph.clone(),
                line_y: run.line_y,
                line_top: run.line_top,
                line_height: run.line_height,
            });
        }
        let bytes = glyphs[start..]
            .iter()
            .map(|glyph| glyph.byte_start..glyph.byte_end)
            .reduce(|first, next| first.start.min(next.start)..first.end.max(next.end))
            .unwrap_or(source_start..source_start + run.text.len());
        lines.push(LayoutLineRange {
            glyphs: start..glyphs.len(),
            bytes,
            top: run.line_top,
            bottom: run.line_top + run.line_height,
        });
    }
    Ok(TextLayout {
        text: Arc::from(text),
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

fn source_line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        text.char_indices()
            .filter_map(|(index, character)| (character == '\n').then_some(index + 1)),
    );
    starts
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

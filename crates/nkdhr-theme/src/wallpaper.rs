//! Deterministic, resource-bounded wallpaper palette generation.
//!
//! Image decoding belongs to the host. This module accepts a borrowed RGBA8
//! view, samples it with a fixed upper bound, and returns a complete portable
//! semantic palette. The generated palette is data only and never retains the
//! source pixels.

use std::{f32::consts::TAU, fmt};

use crate::{PaletteData, ThemeBase, ThemeProfile, ThemeProfileError};

pub const MAX_WALLPAPER_PALETTE_SAMPLES: usize = 262_144;
const HISTOGRAM_AXIS_BITS: usize = 5;
const HISTOGRAM_AXIS_SIZE: usize = 1 << HISTOGRAM_AXIS_BITS;
const HISTOGRAM_SIZE: usize = HISTOGRAM_AXIS_SIZE * HISTOGRAM_AXIS_SIZE * HISTOGRAM_AXIS_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperAppearance {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallpaperPaletteOptions {
    pub appearance: WallpaperAppearance,
    /// Scales generated chroma without changing semantic-role hue identity.
    pub colorfulness: f32,
    /// Scales foreground/background lightness separation.
    pub contrast: f32,
}

impl Default for WallpaperPaletteOptions {
    fn default() -> Self {
        Self {
            appearance: WallpaperAppearance::Auto,
            colorfulness: 1.0,
            contrast: 1.0,
        }
    }
}

impl WallpaperPaletteOptions {
    pub fn validate(self) -> Result<Self, WallpaperPaletteError> {
        if !self.colorfulness.is_finite()
            || !(0.0..=2.0).contains(&self.colorfulness)
            || !self.contrast.is_finite()
            || !(0.5..=2.0).contains(&self.contrast)
        {
            return Err(WallpaperPaletteError::InvalidOptions);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WallpaperImage<'a> {
    width: usize,
    height: usize,
    row_stride: usize,
    rgba8: &'a [u8],
}

impl<'a> WallpaperImage<'a> {
    pub fn new(
        width: usize,
        height: usize,
        row_stride: usize,
        rgba8: &'a [u8],
    ) -> Result<Self, WallpaperPaletteError> {
        if width == 0 || height == 0 {
            return Err(WallpaperPaletteError::EmptyImage);
        }
        let minimum_stride = width
            .checked_mul(4)
            .ok_or(WallpaperPaletteError::InvalidDimensions)?;
        if row_stride < minimum_stride {
            return Err(WallpaperPaletteError::InvalidStride);
        }
        let required = height
            .checked_sub(1)
            .and_then(|rows| rows.checked_mul(row_stride))
            .and_then(|offset| offset.checked_add(minimum_stride))
            .ok_or(WallpaperPaletteError::InvalidDimensions)?;
        if rgba8.len() < required {
            return Err(WallpaperPaletteError::BufferTooSmall);
        }
        Ok(Self {
            width,
            height,
            row_stride,
            rgba8,
        })
    }

    pub const fn width(self) -> usize {
        self.width
    }

    pub const fn height(self) -> usize {
        self.height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedWallpaperPalette {
    pub palette: PaletteData,
    pub appearance: WallpaperAppearance,
    pub sampled_pixels: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WallpaperPaletteGenerator;

impl WallpaperPaletteGenerator {
    pub fn generate(
        self,
        image: WallpaperImage<'_>,
        options: WallpaperPaletteOptions,
    ) -> Result<GeneratedWallpaperPalette, WallpaperPaletteError> {
        let options = options.validate()?;
        let analysis = analyze(image)?;
        let appearance = match options.appearance {
            WallpaperAppearance::Auto if analysis.median_lightness >= 0.68 => {
                WallpaperAppearance::Light
            }
            WallpaperAppearance::Auto => WallpaperAppearance::Dark,
            appearance => appearance,
        };
        let palette = build_palette(&analysis, appearance, options);
        palette.validate().map_err(WallpaperPaletteError::Profile)?;
        Ok(GeneratedWallpaperPalette {
            palette,
            appearance,
            sampled_pixels: analysis.sampled_pixels,
        })
    }

    pub fn generate_live_profile(
        self,
        profile: &ThemeProfile,
        wallpaper_id: impl Into<String>,
        image: WallpaperImage<'_>,
        options: WallpaperPaletteOptions,
    ) -> Result<(ThemeProfile, GeneratedWallpaperPalette), WallpaperPaletteError> {
        let generated = self.generate(image, options)?;
        let profile =
            regenerate_live_wallpaper_profile(profile, wallpaper_id, generated.palette.clone())?;
        Ok((profile, generated))
    }

    pub fn generate_profile(
        self,
        id: impl Into<String>,
        name: impl Into<String>,
        wallpaper_id: impl Into<String>,
        live: bool,
        image: WallpaperImage<'_>,
        options: WallpaperPaletteOptions,
    ) -> Result<(ThemeProfile, GeneratedWallpaperPalette), WallpaperPaletteError> {
        let generated = self.generate(image, options)?;
        let wallpaper_id = wallpaper_id.into();
        if (live && wallpaper_id.trim().is_empty())
            || wallpaper_id.len() > 1024
            || wallpaper_id.chars().any(char::is_control)
        {
            return Err(WallpaperPaletteError::InvalidWallpaperId);
        }
        let profile = ThemeProfile {
            id: id.into(),
            name: name.into(),
            base: ThemeBase::Wallpaper {
                live,
                wallpaper_id,
                frozen_palette: Box::new(generated.palette.clone()),
            },
            ..ThemeProfile::default()
        };
        profile.resolve().map_err(WallpaperPaletteError::Profile)?;
        Ok((profile, generated))
    }
}

/// Replace only a live wallpaper profile's source and frozen base palette.
/// Identity, display name and the complete explicit override tree are kept.
pub fn regenerate_live_wallpaper_profile(
    profile: &ThemeProfile,
    wallpaper_id: impl Into<String>,
    frozen_palette: PaletteData,
) -> Result<ThemeProfile, WallpaperPaletteError> {
    frozen_palette
        .validate()
        .map_err(WallpaperPaletteError::Profile)?;
    let wallpaper_id = wallpaper_id.into();
    if wallpaper_id.trim().is_empty()
        || wallpaper_id.len() > 1024
        || wallpaper_id.chars().any(char::is_control)
    {
        return Err(WallpaperPaletteError::InvalidWallpaperId);
    }
    match &profile.base {
        ThemeBase::BuiltIn { .. } => return Err(WallpaperPaletteError::NotWallpaperProfile),
        ThemeBase::Wallpaper { live: false, .. } => {
            return Err(WallpaperPaletteError::NotLiveLinked);
        }
        ThemeBase::Wallpaper { live: true, .. } => {}
    }
    let mut regenerated = profile.clone();
    regenerated.base = ThemeBase::Wallpaper {
        live: true,
        wallpaper_id,
        frozen_palette: Box::new(frozen_palette),
    };
    regenerated
        .resolve()
        .map_err(WallpaperPaletteError::Profile)?;
    Ok(regenerated)
}

#[derive(Debug, Clone, Copy, Default)]
struct HistogramBin {
    weight: u64,
    red: u64,
    green: u64,
    blue: u64,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    weight: u64,
    color: Oklch,
}

struct WallpaperAnalysis {
    candidates: Vec<Candidate>,
    average: Oklab,
    median_lightness: f32,
    sampled_pixels: usize,
}

fn analyze(image: WallpaperImage<'_>) -> Result<WallpaperAnalysis, WallpaperPaletteError> {
    let pixel_count = image
        .width
        .checked_mul(image.height)
        .ok_or(WallpaperPaletteError::InvalidDimensions)?;
    let sample_count = pixel_count.min(MAX_WALLPAPER_PALETTE_SAMPLES);
    let mut histogram = vec![HistogramBin::default(); HISTOGRAM_SIZE];
    let mut sampled_pixels = 0;
    for sample in 0..sample_count {
        let linear =
            ((sample as u128 * 2 + 1) * pixel_count as u128 / (sample_count as u128 * 2)) as usize;
        let x = linear % image.width;
        let y = linear / image.width;
        let offset = y * image.row_stride + x * 4;
        let pixel = &image.rgba8[offset..offset + 4];
        let alpha = pixel[3] as u64;
        if alpha < 16 {
            continue;
        }
        let index = ((pixel[0] as usize >> 3) << 10)
            | ((pixel[1] as usize >> 3) << 5)
            | (pixel[2] as usize >> 3);
        let bin = &mut histogram[index];
        bin.weight += alpha;
        bin.red += pixel[0] as u64 * alpha;
        bin.green += pixel[1] as u64 * alpha;
        bin.blue += pixel[2] as u64 * alpha;
        sampled_pixels += 1;
    }
    if sampled_pixels == 0 {
        return Err(WallpaperPaletteError::NoVisiblePixels);
    }

    let mut candidates = Vec::new();
    let mut total_weight = 0_u64;
    let mut average = Oklab::default();
    for bin in histogram.into_iter().filter(|bin| bin.weight > 0) {
        let red = bin.red as f32 / bin.weight as f32 / 255.0;
        let green = bin.green as f32 / bin.weight as f32 / 255.0;
        let blue = bin.blue as f32 / bin.weight as f32 / 255.0;
        let lab = srgb_to_oklab([red, green, blue]);
        average.lightness += lab.lightness * bin.weight as f32;
        average.a += lab.a * bin.weight as f32;
        average.b += lab.b * bin.weight as f32;
        total_weight += bin.weight;
        candidates.push(Candidate {
            weight: bin.weight,
            color: lab.to_oklch(),
        });
    }
    let total = total_weight as f32;
    average.lightness /= total;
    average.a /= total;
    average.b /= total;

    candidates.sort_by(|left, right| left.color.lightness.total_cmp(&right.color.lightness));
    let midpoint = total_weight / 2;
    let mut accumulated = 0_u64;
    let median_lightness = candidates
        .iter()
        .find_map(|candidate| {
            accumulated += candidate.weight;
            (accumulated >= midpoint).then_some(candidate.color.lightness)
        })
        .unwrap_or(average.lightness);
    Ok(WallpaperAnalysis {
        candidates,
        average,
        median_lightness,
        sampled_pixels,
    })
}

fn build_palette(
    analysis: &WallpaperAnalysis,
    appearance: WallpaperAppearance,
    options: WallpaperPaletteOptions,
) -> PaletteData {
    let average = analysis.average.to_oklch();
    let fallback_hue = if average.chroma >= 0.01 {
        average.hue
    } else {
        4.36
    };
    let primary = select_accent(&analysis.candidates, fallback_hue, None);
    let secondary = select_accent(
        &analysis.candidates,
        normalize_hue(primary.hue + 1.35),
        Some(primary.hue),
    );
    let colorfulness = options.colorfulness;
    let contrast = options.contrast;
    let base_chroma = (average.chroma * 0.32 * colorfulness).min(0.055);
    let accent_chroma = (primary.chroma.max(0.075) * 1.18 * colorfulness).min(0.24);
    let secondary_chroma = (secondary.chroma.max(0.065) * colorfulness).min(0.20);
    let contrast_delta = (contrast - 1.0) * 0.035;

    let (backdrop_l, surface_l, raised_l, primary_text_l, secondary_text_l, muted_text_l) =
        match appearance {
            WallpaperAppearance::Dark => (
                (0.17 - contrast_delta).clamp(0.09, 0.23),
                (0.25 - contrast_delta).clamp(0.16, 0.32),
                (0.34 - contrast_delta * 0.5).clamp(0.24, 0.42),
                (0.95 + contrast_delta).clamp(0.90, 0.99),
                (0.82 + contrast_delta).clamp(0.74, 0.92),
                (0.70 + contrast_delta).clamp(0.60, 0.82),
            ),
            WallpaperAppearance::Light => (
                (0.93 + contrast_delta).clamp(0.86, 0.98),
                (0.98 + contrast_delta * 0.4).clamp(0.92, 1.0),
                (0.88 + contrast_delta).clamp(0.79, 0.95),
                (0.18 - contrast_delta).clamp(0.08, 0.25),
                (0.32 - contrast_delta).clamp(0.20, 0.43),
                (0.45 - contrast_delta).clamp(0.33, 0.56),
            ),
            WallpaperAppearance::Auto => unreachable!("auto appearance is resolved first"),
        };
    let accent_l = match appearance {
        WallpaperAppearance::Dark => 0.72,
        WallpaperAppearance::Light => 0.48,
        WallpaperAppearance::Auto => unreachable!(),
    };
    let secondary_l = match appearance {
        WallpaperAppearance::Dark => 0.76,
        WallpaperAppearance::Light => 0.43,
        WallpaperAppearance::Auto => unreachable!(),
    };

    let backdrop = Oklch::new(backdrop_l, base_chroma, fallback_hue);
    let surface = Oklch::new(surface_l, base_chroma * 0.82, fallback_hue);
    let surface_raised = Oklch::new(raised_l, base_chroma, fallback_hue);
    let accent = Oklch::new(accent_l, accent_chroma, primary.hue);
    let accent_secondary = Oklch::new(secondary_l, secondary_chroma, secondary.hue);
    let semantic_chroma = (0.15 * colorfulness).min(0.18);
    let semantic_l = if appearance == WallpaperAppearance::Dark {
        0.72
    } else {
        0.48
    };
    let edge = if appearance == WallpaperAppearance::Dark {
        Oklch::new(0.91, base_chroma * 0.45, fallback_hue)
    } else {
        Oklch::new(0.22, base_chroma * 0.45, fallback_hue)
    };
    let inverse_edge = if appearance == WallpaperAppearance::Dark {
        Oklch::new(0.08, base_chroma * 0.25, fallback_hue)
    } else {
        Oklch::new(0.98, base_chroma * 0.25, fallback_hue)
    };
    let shadow = if appearance == WallpaperAppearance::Dark {
        Oklch::new(0.035, base_chroma * 0.25, fallback_hue)
    } else {
        Oklch::new(0.11, base_chroma * 0.20, fallback_hue)
    };
    let on_accent = highest_contrast_foreground(accent);
    let prefer_light_text = appearance == WallpaperAppearance::Dark;
    let text_primary = ensure_contrast(
        Oklch::new(primary_text_l, base_chroma * 0.25, fallback_hue),
        surface,
        7.0,
        prefer_light_text,
    );
    let text_secondary = ensure_contrast(
        Oklch::new(secondary_text_l, base_chroma * 0.22, fallback_hue),
        surface,
        4.5,
        prefer_light_text,
    );
    let text_muted = ensure_contrast(
        Oklch::new(muted_text_l, base_chroma * 0.18, fallback_hue),
        surface,
        3.0,
        prefer_light_text,
    );

    PaletteData {
        backdrop: backdrop.to_hex(),
        surface: surface.to_hex(),
        surface_raised: surface_raised.to_hex(),
        text_primary: text_primary.to_hex(),
        text_secondary: text_secondary.to_hex(),
        text_muted: text_muted.to_hex(),
        accent: accent.to_hex(),
        accent_secondary: accent_secondary.to_hex(),
        on_accent: on_accent.to_hex(),
        success: Oklch::new(semantic_l, semantic_chroma, 2.50).to_hex(),
        warning: Oklch::new(semantic_l, semantic_chroma, 1.42).to_hex(),
        error: Oklch::new(semantic_l, semantic_chroma, 0.43).to_hex(),
        edge: edge.to_hex(),
        inverse_edge: inverse_edge.to_hex(),
        shadow: shadow.to_hex(),
    }
}

fn select_accent(candidates: &[Candidate], fallback_hue: f32, avoid_hue: Option<f32>) -> Oklch {
    candidates
        .iter()
        .filter(|candidate| {
            candidate.color.lightness >= 0.18
                && candidate.color.lightness <= 0.92
                && avoid_hue.is_none_or(|hue| hue_distance(candidate.color.hue, hue) >= 0.65)
        })
        .max_by(|left, right| {
            accent_score(**left)
                .total_cmp(&accent_score(**right))
                .then_with(|| left.weight.cmp(&right.weight))
        })
        .map(|candidate| candidate.color)
        .unwrap_or_else(|| Oklch::new(0.65, 0.08, fallback_hue))
}

fn accent_score(candidate: Candidate) -> f32 {
    (candidate.weight as f32).sqrt() * (candidate.color.chroma + 0.025)
}

fn highest_contrast_foreground(background: Oklch) -> Oklch {
    let black = Oklch::new(0.0, 0.0, 0.0);
    let white = Oklch::new(1.0, 0.0, 0.0);
    if contrast_ratio(black.to_srgb(), background.to_srgb())
        >= contrast_ratio(white.to_srgb(), background.to_srgb())
    {
        black
    } else {
        white
    }
}

fn ensure_contrast(
    foreground: Oklch,
    background: Oklch,
    minimum: f32,
    prefer_light: bool,
) -> Oklch {
    let background = background.to_srgb();
    if contrast_ratio(foreground.to_srgb(), background) >= minimum {
        return foreground;
    }
    if prefer_light {
        let mut failing = foreground.lightness;
        let mut passing = 1.0;
        for _ in 0..16 {
            let lightness = (failing + passing) * 0.5;
            let candidate = Oklch::new(lightness, foreground.chroma, foreground.hue);
            if contrast_ratio(candidate.to_srgb(), background) >= minimum {
                passing = lightness;
            } else {
                failing = lightness;
            }
        }
        Oklch::new(passing, foreground.chroma, foreground.hue)
    } else {
        let mut passing = 0.0;
        let mut failing = foreground.lightness;
        for _ in 0..16 {
            let lightness = (passing + failing) * 0.5;
            let candidate = Oklch::new(lightness, foreground.chroma, foreground.hue);
            if contrast_ratio(candidate.to_srgb(), background) >= minimum {
                passing = lightness;
            } else {
                failing = lightness;
            }
        }
        Oklch::new(passing, foreground.chroma, foreground.hue)
    }
}

fn contrast_ratio(left: [f32; 3], right: [f32; 3]) -> f32 {
    let left = relative_luminance(left);
    let right = relative_luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

fn relative_luminance(srgb: [f32; 3]) -> f32 {
    let linear = srgb.map(srgb_to_linear);
    linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
}

#[derive(Debug, Clone, Copy, Default)]
struct Oklab {
    lightness: f32,
    a: f32,
    b: f32,
}

impl Oklab {
    fn to_oklch(self) -> Oklch {
        Oklch {
            lightness: self.lightness,
            chroma: self.a.hypot(self.b),
            hue: normalize_hue(self.b.atan2(self.a)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Oklch {
    lightness: f32,
    chroma: f32,
    hue: f32,
}

impl Oklch {
    fn new(lightness: f32, chroma: f32, hue: f32) -> Self {
        Self {
            lightness,
            chroma,
            hue: normalize_hue(hue),
        }
    }

    fn to_srgb(self) -> [f32; 3] {
        let mut low = 0.0;
        let mut high = self.chroma.max(0.0);
        let mut best = oklab_to_srgb(self.with_chroma(0.0).to_oklab());
        for _ in 0..14 {
            let chroma = (low + high) * 0.5;
            let candidate = oklab_to_srgb(self.with_chroma(chroma).to_oklab());
            if candidate
                .iter()
                .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
            {
                low = chroma;
                best = candidate;
            } else {
                high = chroma;
            }
        }
        best.map(|component| component.clamp(0.0, 1.0))
    }

    fn to_hex(self) -> String {
        let [red, green, blue] = self
            .to_srgb()
            .map(|component| (component.mul_add(255.0, 0.5).floor() as i32).clamp(0, 255) as u8);
        format!("#{red:02x}{green:02x}{blue:02x}ff")
    }

    fn to_oklab(self) -> Oklab {
        Oklab {
            lightness: self.lightness,
            a: self.chroma * self.hue.cos(),
            b: self.chroma * self.hue.sin(),
        }
    }

    fn with_chroma(self, chroma: f32) -> Self {
        Self { chroma, ..self }
    }
}

fn srgb_to_oklab(srgb: [f32; 3]) -> Oklab {
    let [red, green, blue] = srgb.map(srgb_to_linear);
    let l = 0.412_221_46 * red + 0.536_332_55 * green + 0.051_445_995 * blue;
    let m = 0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue;
    let s = 0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue;
    let l = l.cbrt();
    let m = m.cbrt();
    let s = s.cbrt();
    Oklab {
        lightness: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

fn oklab_to_srgb(lab: Oklab) -> [f32; 3] {
    let l = (lab.lightness + 0.396_337_78 * lab.a + 0.215_803_76 * lab.b).powi(3);
    let m = (lab.lightness - 0.105_561_346 * lab.a - 0.063_854_17 * lab.b).powi(3);
    let s = (lab.lightness - 0.089_484_18 * lab.a - 1.291_485_5 * lab.b).powi(3);
    [
        linear_to_srgb(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
        linear_to_srgb(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
        linear_to_srgb(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
    ]
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn normalize_hue(hue: f32) -> f32 {
    hue.rem_euclid(TAU)
}

fn hue_distance(left: f32, right: f32) -> f32 {
    let distance = (normalize_hue(left) - normalize_hue(right)).abs();
    distance.min(TAU - distance)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperPaletteError {
    EmptyImage,
    InvalidDimensions,
    InvalidStride,
    BufferTooSmall,
    NoVisiblePixels,
    InvalidOptions,
    InvalidWallpaperId,
    NotWallpaperProfile,
    NotLiveLinked,
    Profile(ThemeProfileError),
}

impl fmt::Display for WallpaperPaletteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyImage => formatter.write_str("wallpaper image is empty"),
            Self::InvalidDimensions => formatter.write_str("wallpaper dimensions overflow"),
            Self::InvalidStride => formatter.write_str("wallpaper RGBA row stride is too small"),
            Self::BufferTooSmall => formatter.write_str("wallpaper RGBA buffer is too small"),
            Self::NoVisiblePixels => formatter.write_str("wallpaper contains no visible pixels"),
            Self::InvalidOptions => formatter.write_str("invalid wallpaper palette options"),
            Self::InvalidWallpaperId => formatter.write_str("invalid wallpaper identity"),
            Self::NotWallpaperProfile => formatter.write_str("profile is not wallpaper-based"),
            Self::NotLiveLinked => formatter.write_str("wallpaper profile is frozen"),
            Self::Profile(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for WallpaperPaletteError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn image(width: usize, height: usize, pixels: &[[u8; 4]]) -> WallpaperImage<'_> {
        let bytes = pixels.as_flattened();
        WallpaperImage::new(width, height, width * 4, bytes).unwrap()
    }

    fn parse_srgb(value: &str) -> [f32; 3] {
        let rgba = crate::parse_color(value).unwrap();
        [
            rgba[0] as f32 / 255.0,
            rgba[1] as f32 / 255.0,
            rgba[2] as f32 / 255.0,
        ]
    }

    #[test]
    fn palette_is_complete_deterministic_and_readable_for_dark_and_light_sources() {
        for (pixels, expected_appearance) in [
            (
                vec![[12, 18, 40, 255], [88, 40, 140, 255], [20, 100, 130, 255]],
                WallpaperAppearance::Dark,
            ),
            (
                vec![
                    [245, 240, 230, 255],
                    [220, 235, 250, 255],
                    [250, 215, 225, 255],
                ],
                WallpaperAppearance::Light,
            ),
        ] {
            let source = image(3, 1, &pixels);
            let first = WallpaperPaletteGenerator
                .generate(source, WallpaperPaletteOptions::default())
                .unwrap();
            let second = WallpaperPaletteGenerator
                .generate(source, WallpaperPaletteOptions::default())
                .unwrap();
            assert_eq!(first, second);
            assert_eq!(first.appearance, expected_appearance);
            first.palette.validate().unwrap();
            assert!(
                contrast_ratio(
                    parse_srgb(&first.palette.text_primary),
                    parse_srgb(&first.palette.surface)
                ) >= 7.0
            );
            assert!(
                contrast_ratio(
                    parse_srgb(&first.palette.on_accent),
                    parse_srgb(&first.palette.accent)
                ) >= 4.5
            );
        }
    }

    #[test]
    fn transparent_input_and_invalid_views_are_rejected() {
        assert_eq!(
            WallpaperImage::new(2, 1, 7, &[0; 8]).unwrap_err(),
            WallpaperPaletteError::InvalidStride
        );
        let transparent = [[1, 2, 3, 0]];
        assert_eq!(
            WallpaperPaletteGenerator
                .generate(
                    image(1, 1, &transparent),
                    WallpaperPaletteOptions::default()
                )
                .unwrap_err(),
            WallpaperPaletteError::NoVisiblePixels
        );
    }

    #[test]
    fn large_images_use_the_fixed_sample_bound() {
        let width = 1024;
        let height = 1024;
        let pixels = vec![[20, 80, 140, 255]; width * height];
        let generated = WallpaperPaletteGenerator
            .generate(
                image(width, height, &pixels),
                WallpaperPaletteOptions::default(),
            )
            .unwrap();
        assert_eq!(generated.sampled_pixels, MAX_WALLPAPER_PALETTE_SAMPLES);
    }

    #[test]
    fn option_extremes_keep_semantic_foregrounds_readable() {
        let mut pixels = Vec::new();
        for red in [0, 51, 102, 153, 204, 255] {
            for green in [0, 51, 102, 153, 204, 255] {
                for blue in [0, 51, 102, 153, 204, 255] {
                    pixels.push([red, green, blue, 255]);
                }
            }
        }
        let source = image(pixels.len(), 1, &pixels);
        for appearance in [WallpaperAppearance::Dark, WallpaperAppearance::Light] {
            for (colorfulness, contrast) in [(0.0, 0.5), (2.0, 2.0)] {
                let generated = WallpaperPaletteGenerator
                    .generate(
                        source,
                        WallpaperPaletteOptions {
                            appearance,
                            colorfulness,
                            contrast,
                        },
                    )
                    .unwrap();
                assert!(
                    contrast_ratio(
                        parse_srgb(&generated.palette.text_primary),
                        parse_srgb(&generated.palette.surface)
                    ) >= 7.0
                );
                assert!(
                    contrast_ratio(
                        parse_srgb(&generated.palette.on_accent),
                        parse_srgb(&generated.palette.accent)
                    ) >= 4.5
                );
                assert!(
                    contrast_ratio(
                        parse_srgb(&generated.palette.text_secondary),
                        parse_srgb(&generated.palette.surface)
                    ) >= 4.5
                );
                assert!(
                    contrast_ratio(
                        parse_srgb(&generated.palette.text_muted),
                        parse_srgb(&generated.palette.surface)
                    ) >= 3.0
                );
            }
        }
    }

    #[test]
    fn generator_creates_live_and_frozen_portable_profiles() {
        let pixels = [[20, 80, 140, 255], [180, 70, 120, 255]];
        let source = image(2, 1, &pixels);
        let (live, _) = WallpaperPaletteGenerator
            .generate_profile(
                "live",
                "Live",
                "wallpaper:one",
                true,
                source,
                WallpaperPaletteOptions::default(),
            )
            .unwrap();
        let (frozen, _) = WallpaperPaletteGenerator
            .generate_profile(
                "frozen",
                "Frozen",
                "",
                false,
                source,
                WallpaperPaletteOptions::default(),
            )
            .unwrap();
        live.resolve().unwrap();
        frozen.resolve().unwrap();
        assert!(matches!(live.base, ThemeBase::Wallpaper { live: true, .. }));
        assert!(matches!(
            frozen.base,
            ThemeBase::Wallpaper { live: false, .. }
        ));
    }

    #[test]
    fn live_regeneration_changes_only_base_and_preserves_every_override() {
        let overrides = json!({
            "palette": {"accent": "#010203ff"},
            "materials": {"content_surface": {"opacity": 0.91}}
        });
        let profile = ThemeProfile {
            id: "wallpaper".into(),
            name: "Wallpaper".into(),
            base: ThemeBase::Wallpaper {
                live: true,
                wallpaper_id: "old".into(),
                frozen_palette: Box::new(PaletteData::tokyo_night()),
            },
            overrides: overrides.clone(),
            ..ThemeProfile::default()
        };
        let regenerated =
            regenerate_live_wallpaper_profile(&profile, "new", PaletteData::nord()).unwrap();
        assert_eq!(regenerated.id, profile.id);
        assert_eq!(regenerated.name, profile.name);
        assert_eq!(regenerated.overrides, overrides);
        let resolved = regenerated.resolve().unwrap();
        assert_eq!(resolved.data.palette.accent, "#010203ff");
        assert_eq!(resolved.data.palette.surface, PaletteData::nord().surface);
    }

    #[test]
    fn frozen_and_non_wallpaper_profiles_cannot_regenerate_implicitly() {
        assert_eq!(
            regenerate_live_wallpaper_profile(
                &ThemeProfile::default(),
                "next",
                PaletteData::nord()
            )
            .unwrap_err(),
            WallpaperPaletteError::NotWallpaperProfile
        );
        let frozen = ThemeProfile {
            base: ThemeBase::Wallpaper {
                live: false,
                wallpaper_id: String::new(),
                frozen_palette: Box::new(PaletteData::tokyo_night()),
            },
            ..ThemeProfile::default()
        };
        assert_eq!(
            regenerate_live_wallpaper_profile(&frozen, "next", PaletteData::nord()).unwrap_err(),
            WallpaperPaletteError::NotLiveLinked
        );
    }
}

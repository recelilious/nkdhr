//! Typed owner-approved design tokens used by UI-3 components.

use std::fmt;

use nkdhr_render::Color;

use crate::MotionProfile;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Density {
    Compact,
    #[default]
    Standard,
    Relaxed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DensityMetrics {
    pub control_height: f32,
    pub row_height: f32,
    pub navigation_height: f32,
    pub toggle_width: f32,
    pub toggle_height: f32,
    pub slider_node: f32,
    pub slider_track: f32,
    pub scrollbar: f32,
}

impl DensityMetrics {
    pub const COMPACT: Self = Self {
        control_height: 28.0,
        row_height: 36.0,
        navigation_height: 40.0,
        toggle_width: 32.0,
        toggle_height: 18.0,
        slider_node: 14.0,
        slider_track: 3.0,
        scrollbar: 4.0,
    };
    pub const STANDARD: Self = Self {
        control_height: 36.0,
        row_height: 48.0,
        navigation_height: 48.0,
        toggle_width: 36.0,
        toggle_height: 20.0,
        slider_node: 16.0,
        slider_track: 4.0,
        scrollbar: 6.0,
    };
    pub const RELAXED: Self = Self {
        control_height: 44.0,
        row_height: 60.0,
        navigation_height: 56.0,
        toggle_width: 44.0,
        toggle_height: 24.0,
        slider_node: 20.0,
        slider_track: 5.0,
        scrollbar: 8.0,
    };

    pub const fn for_density(density: Density) -> Self {
        match density {
            Density::Compact => Self::COMPACT,
            Density::Standard => Self::STANDARD,
            Density::Relaxed => Self::RELAXED,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacing {
    pub xxs: f32,
    pub xs: f32,
    pub small: f32,
    pub medium: f32,
    pub large: f32,
    pub xl: f32,
    pub xxl: f32,
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            xxs: 4.0,
            xs: 8.0,
            small: 12.0,
            medium: 16.0,
            large: 24.0,
            xl: 32.0,
            xxl: 48.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Radii {
    pub small: f32,
    pub control: f32,
    pub relaxed: f32,
    pub group: f32,
    pub popover: f32,
    pub major: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontStacks {
    pub ui: Vec<String>,
    pub mono: Vec<String>,
}

impl Default for FontStacks {
    fn default() -> Self {
        Self {
            ui: vec![
                "Noto Sans".to_owned(),
                "Noto Sans CJK SC".to_owned(),
                "system-ui".to_owned(),
                "Noto Color Emoji".to_owned(),
            ],
            mono: vec![
                "Maple Mono".to_owned(),
                "Noto Sans Mono".to_owned(),
                "monospace".to_owned(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeToken {
    pub font_size: f32,
    pub line_height: f32,
    pub weight: u16,
}

impl TypeToken {
    pub const fn new(font_size: f32, line_height: f32, weight: u16) -> Self {
        Self {
            font_size,
            line_height,
            weight,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextRole {
    Caption,
    BodySmall,
    Body,
    Label,
    Section,
    Page,
    Display,
    Mono,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Typography {
    pub families: FontStacks,
    pub caption: TypeToken,
    pub body_small: TypeToken,
    pub body: TypeToken,
    pub label: TypeToken,
    pub section: TypeToken,
    pub page: TypeToken,
    pub display: TypeToken,
    pub mono: TypeToken,
    /// Independent from component density.
    pub scale: f32,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            families: FontStacks::default(),
            caption: TypeToken::new(12.0, 16.0, 500),
            body_small: TypeToken::new(13.0, 18.0, 400),
            body: TypeToken::new(14.0, 20.0, 400),
            label: TypeToken::new(14.0, 20.0, 500),
            section: TypeToken::new(16.0, 24.0, 600),
            page: TypeToken::new(24.0, 32.0, 600),
            display: TypeToken::new(32.0, 40.0, 600),
            mono: TypeToken::new(13.0, 20.0, 400),
            scale: 1.0,
        }
    }
}

impl Typography {
    pub fn token(&self, role: TextRole) -> TypeToken {
        let token = match role {
            TextRole::Caption => self.caption,
            TextRole::BodySmall => self.body_small,
            TextRole::Body => self.body,
            TextRole::Label => self.label,
            TextRole::Section => self.section,
            TextRole::Page => self.page,
            TextRole::Display => self.display,
            TextRole::Mono => self.mono,
        };
        TypeToken::new(
            token.font_size * self.scale,
            token.line_height * self.scale,
            token.weight,
        )
    }
}

impl Default for Radii {
    fn default() -> Self {
        Self {
            small: 8.0,
            control: 10.0,
            relaxed: 12.0,
            group: 18.0,
            popover: 20.0,
            major: 28.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialTier {
    Ghost,
    CompactNode,
    HoverTransient,
    Popover,
    ExpandedPanel,
    ContentSurface,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassMaterial {
    pub opacity: f32,
    pub backdrop_blur: f32,
    pub wallpaper_tint: f32,
}

impl GlassMaterial {
    pub const fn new(opacity: f32, backdrop_blur: f32, wallpaper_tint: f32) -> Self {
        Self {
            opacity,
            backdrop_blur,
            wallpaper_tint,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowToken {
    pub offset_y: f32,
    pub blur: f32,
    pub alpha: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub backdrop: Color,
    pub surface: Color,
    pub surface_raised: Color,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_secondary: Color,
    pub on_accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub edge: Color,
    pub inverse_edge: Color,
    pub shadow: Color,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            backdrop: Color::from_srgba8(22, 22, 30, 255),
            surface: Color::from_srgba8(36, 40, 59, 255),
            surface_raised: Color::from_srgba8(65, 72, 104, 255),
            text_primary: Color::from_srgba8(192, 202, 245, 255),
            text_secondary: Color::from_srgba8(169, 177, 214, 255),
            text_muted: Color::from_srgba8(121, 130, 171, 255),
            accent: Color::from_srgba8(122, 162, 247, 255),
            accent_secondary: Color::from_srgba8(187, 154, 247, 255),
            on_accent: Color::from_srgba8(22, 22, 30, 255),
            success: Color::from_srgba8(158, 206, 106, 255),
            warning: Color::from_srgba8(224, 175, 104, 255),
            error: Color::from_srgba8(247, 118, 142, 255),
            edge: Color::from_srgba8(224, 228, 255, 255),
            inverse_edge: Color::from_srgba8(8, 10, 18, 255),
            shadow: Color::from_srgba8(5, 7, 17, 255),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaterialCapabilities {
    pub backdrop_blur: bool,
    pub reduced_transparency: bool,
    pub high_contrast: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedMaterial {
    pub fill: Color,
    /// Requested logical blur. Zero means the current backend or accessibility
    /// mode must use the compensated fill instead.
    pub backdrop_blur: f32,
    pub wallpaper_tint: f32,
    pub edge: Color,
    pub inverse_edge: Color,
    pub inner_highlight: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub density: Density,
    pub spacing: Spacing,
    pub radii: Radii,
    pub typography: Typography,
    pub palette: Palette,
    pub motion: MotionProfile,
    pub ghost: GlassMaterial,
    pub compact_node: GlassMaterial,
    pub hover_transient: GlassMaterial,
    pub popover: GlassMaterial,
    pub expanded_panel: GlassMaterial,
    pub content_surface: GlassMaterial,
    pub terminal: GlassMaterial,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            density: Density::Standard,
            spacing: Spacing::default(),
            radii: Radii::default(),
            typography: Typography::default(),
            palette: Palette::default(),
            motion: MotionProfile::default(),
            ghost: GlassMaterial::new(0.08, 0.0, 0.0),
            compact_node: GlassMaterial::new(0.34, 16.0, 0.12),
            hover_transient: GlassMaterial::new(0.46, 20.0, 0.10),
            popover: GlassMaterial::new(0.68, 24.0, 0.08),
            expanded_panel: GlassMaterial::new(0.80, 32.0, 0.06),
            content_surface: GlassMaterial::new(0.86, 36.0, 0.04),
            terminal: GlassMaterial::new(0.95, 12.0, 0.0),
        }
    }
}

impl Theme {
    pub fn density_metrics(&self) -> DensityMetrics {
        DensityMetrics::for_density(self.density)
    }

    pub fn material(&self, tier: MaterialTier) -> GlassMaterial {
        match tier {
            MaterialTier::Ghost => self.ghost,
            MaterialTier::CompactNode => self.compact_node,
            MaterialTier::HoverTransient => self.hover_transient,
            MaterialTier::Popover => self.popover,
            MaterialTier::ExpandedPanel => self.expanded_panel,
            MaterialTier::ContentSurface => self.content_surface,
            MaterialTier::Terminal => self.terminal,
        }
    }

    pub fn resolve_material(
        &self,
        tier: MaterialTier,
        capabilities: MaterialCapabilities,
    ) -> ResolvedMaterial {
        let token = self.material(tier);
        let (opacity, blur, wallpaper_tint) = if capabilities.reduced_transparency {
            (0.97, 0.0, 0.0)
        } else if capabilities.backdrop_blur {
            (token.opacity, token.backdrop_blur, token.wallpaper_tint)
        } else {
            ((token.opacity + 0.11).min(0.98), 0.0, token.wallpaper_tint)
        };
        let edge_alpha = if capabilities.high_contrast {
            0.86
        } else {
            0.28
        };
        let inverse_alpha = if capabilities.high_contrast {
            0.56
        } else {
            0.12
        };
        let inner_alpha = if capabilities.high_contrast {
            0.32
        } else {
            0.10
        };
        ResolvedMaterial {
            fill: with_alpha(self.palette.surface, opacity),
            backdrop_blur: blur,
            wallpaper_tint,
            edge: with_alpha(self.palette.edge, edge_alpha),
            inverse_edge: with_alpha(self.palette.inverse_edge, inverse_alpha),
            inner_highlight: with_alpha(self.palette.edge, inner_alpha),
        }
    }

    pub fn validate(&self) -> Result<(), ThemeError> {
        validate_positive_sequence(
            "spacing",
            [
                self.spacing.xxs,
                self.spacing.xs,
                self.spacing.small,
                self.spacing.medium,
                self.spacing.large,
                self.spacing.xl,
                self.spacing.xxl,
            ],
        )?;
        validate_positive_sequence(
            "radii",
            [
                self.radii.small,
                self.radii.control,
                self.radii.relaxed,
                self.radii.group,
                self.radii.popover,
                self.radii.major,
            ],
        )?;
        if !self.typography.scale.is_finite() || self.typography.scale <= 0.0 {
            return Err(ThemeError::InvalidTypography);
        }
        if self
            .typography
            .families
            .ui
            .iter()
            .chain(&self.typography.families.mono)
            .any(|family| family.trim().is_empty())
        {
            return Err(ThemeError::InvalidTypography);
        }
        for role in [
            TextRole::Caption,
            TextRole::BodySmall,
            TextRole::Body,
            TextRole::Label,
            TextRole::Section,
            TextRole::Page,
            TextRole::Display,
            TextRole::Mono,
        ] {
            let token = self.typography.token(role);
            if !token.font_size.is_finite()
                || token.font_size <= 0.0
                || !token.line_height.is_finite()
                || token.line_height <= 0.0
                || !(1..=1000).contains(&token.weight)
            {
                return Err(ThemeError::InvalidTypography);
            }
        }
        for tier in [
            MaterialTier::Ghost,
            MaterialTier::CompactNode,
            MaterialTier::HoverTransient,
            MaterialTier::Popover,
            MaterialTier::ExpandedPanel,
            MaterialTier::ContentSurface,
            MaterialTier::Terminal,
        ] {
            let material = self.material(tier);
            if !material.opacity.is_finite()
                || !(0.0..=1.0).contains(&material.opacity)
                || !material.backdrop_blur.is_finite()
                || material.backdrop_blur < 0.0
                || !material.wallpaper_tint.is_finite()
                || !(0.0..=1.0).contains(&material.wallpaper_tint)
            {
                return Err(ThemeError::InvalidMaterial(tier));
            }
        }
        self.motion
            .validate()
            .map_err(|_| ThemeError::InvalidMotion)?;
        Ok(())
    }
}

fn validate_positive_sequence<const N: usize>(
    name: &'static str,
    values: [f32; N],
) -> Result<(), ThemeError> {
    if values
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
        && values.windows(2).all(|pair| pair[0] <= pair[1])
    {
        Ok(())
    } else {
        Err(ThemeError::InvalidScale(name))
    }
}

pub(crate) fn with_alpha(color: Color, alpha: f32) -> Color {
    let [red, green, blue, _] = color.components();
    Color::new(red, green, blue, alpha.clamp(0.0, 1.0))
        .expect("validated palette color and clamped alpha always form a color")
}

pub(crate) fn mix(first: Color, second: Color, amount: f32) -> Color {
    let first = first.components();
    let second = second.components();
    let amount = amount.clamp(0.0, 1.0);
    Color::new(
        first[0] + (second[0] - first[0]) * amount,
        first[1] + (second[1] - first[1]) * amount,
        first[2] + (second[2] - first[2]) * amount,
        first[3] + (second[3] - first[3]) * amount,
    )
    .expect("mixing valid colors remains valid")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeError {
    InvalidScale(&'static str),
    InvalidMaterial(MaterialTier),
    InvalidTypography,
    InvalidMotion,
}

impl fmt::Display for ThemeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScale(name) => write!(formatter, "{name} must be finite and ordered"),
            Self::InvalidMaterial(tier) => write!(formatter, "invalid {tier:?} material token"),
            Self::InvalidTypography => formatter.write_str("invalid typography token"),
            Self::InvalidMotion => formatter.write_str("invalid motion token"),
        }
    }
}

impl std::error::Error for ThemeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_defaults_validate_and_match_density_contract() {
        let theme = Theme::default();
        theme.validate().unwrap();
        assert_eq!(theme.density_metrics().control_height, 36.0);
        assert_eq!(theme.density_metrics().toggle_width, 36.0);
        assert_eq!(theme.content_surface.opacity, 0.86);
        assert_eq!(theme.content_surface.backdrop_blur, 36.0);
        assert_eq!(theme.typography.token(TextRole::Body).font_size, 14.0);
    }

    #[test]
    fn blur_capability_and_accessibility_resolve_explicitly() {
        let theme = Theme::default();
        let blurred = theme.resolve_material(
            MaterialTier::ContentSurface,
            MaterialCapabilities {
                backdrop_blur: true,
                ..MaterialCapabilities::default()
            },
        );
        assert_eq!(blurred.backdrop_blur, 36.0);
        assert_eq!(blurred.fill.components()[3], 0.86);

        let fallback = theme.resolve_material(
            MaterialTier::ContentSurface,
            MaterialCapabilities::default(),
        );
        assert_eq!(fallback.backdrop_blur, 0.0);
        assert!((fallback.fill.components()[3] - 0.97).abs() < 0.0001);

        let reduced = theme.resolve_material(
            MaterialTier::CompactNode,
            MaterialCapabilities {
                backdrop_blur: true,
                reduced_transparency: true,
                high_contrast: true,
            },
        );
        assert_eq!(reduced.backdrop_blur, 0.0);
        assert_eq!(reduced.wallpaper_tint, 0.0);
        assert!((reduced.fill.components()[3] - 0.97).abs() < 0.0001);
    }

    #[test]
    fn invalid_professional_motion_override_rejects_the_snapshot() {
        let mut theme = Theme::default();
        theme.motion.fluid.neck_variation = 0.75;
        assert_eq!(theme.validate(), Err(ThemeError::InvalidMotion));
    }
}

//! Portable, non-executable UI-4 theme profiles.
//!
//! A profile selects an immutable built-in base or a wallpaper palette and
//! overlays a sparse JSON object. Resolution always produces one complete,
//! validated [`ThemeData`]. Keeping the sparse overlay separate is what lets a
//! live wallpaper regenerate non-overridden roles without losing explicit user
//! choices.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as Json};

pub const THEME_SCHEMA_VERSION: u32 = 1;
pub const THEME_LIBRARY_SCHEMA_VERSION: u32 = 1;
pub const MAX_THEME_LIBRARY_PROFILES: usize = 256;
const MAX_PROFILE_TEXT_BYTES: usize = 1024 * 1024;
const MAX_THEME_LIBRARY_TEXT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltInTheme {
    TokyoNight,
    Nord,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DensityData {
    Compact,
    #[default]
    Standard,
    Relaxed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MotionModeData {
    Off,
    Reduced,
    Standard,
    Expressive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteData {
    pub backdrop: String,
    pub surface: String,
    pub surface_raised: String,
    pub text_primary: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub accent: String,
    pub accent_secondary: String,
    pub on_accent: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub edge: String,
    pub inverse_edge: String,
    pub shadow: String,
}

impl PaletteData {
    pub fn tokyo_night() -> Self {
        Self {
            backdrop: "#16161eff".into(),
            surface: "#24283bff".into(),
            surface_raised: "#414868ff".into(),
            text_primary: "#c0caf5ff".into(),
            text_secondary: "#a9b1d6ff".into(),
            text_muted: "#7982abff".into(),
            accent: "#7aa2f7ff".into(),
            accent_secondary: "#bb9af7ff".into(),
            on_accent: "#16161eff".into(),
            success: "#9ece6aff".into(),
            warning: "#e0af68ff".into(),
            error: "#f7768eff".into(),
            edge: "#e0e4ffff".into(),
            inverse_edge: "#080a12ff".into(),
            shadow: "#050711ff".into(),
        }
    }

    pub fn nord() -> Self {
        Self {
            backdrop: "#2e3440ff".into(),
            surface: "#3b4252ff".into(),
            surface_raised: "#4c566aff".into(),
            text_primary: "#eceff4ff".into(),
            text_secondary: "#d8dee9ff".into(),
            text_muted: "#81a1c1ff".into(),
            accent: "#88c0d0ff".into(),
            accent_secondary: "#b48eadff".into(),
            on_accent: "#2e3440ff".into(),
            success: "#a3be8cff".into(),
            warning: "#ebcb8bff".into(),
            error: "#bf616aff".into(),
            edge: "#e5e9f0ff".into(),
            inverse_edge: "#2e3440ff".into(),
            shadow: "#1f2430ff".into(),
        }
    }

    pub fn validate(&self) -> Result<(), ThemeProfileError> {
        for (path, value) in [
            ("palette.backdrop", &self.backdrop),
            ("palette.surface", &self.surface),
            ("palette.surface_raised", &self.surface_raised),
            ("palette.text_primary", &self.text_primary),
            ("palette.text_secondary", &self.text_secondary),
            ("palette.text_muted", &self.text_muted),
            ("palette.accent", &self.accent),
            ("palette.accent_secondary", &self.accent_secondary),
            ("palette.on_accent", &self.on_accent),
            ("palette.success", &self.success),
            ("palette.warning", &self.warning),
            ("palette.error", &self.error),
            ("palette.edge", &self.edge),
            ("palette.inverse_edge", &self.inverse_edge),
            ("palette.shadow", &self.shadow),
        ] {
            parse_color(value).map_err(|_| ThemeProfileError::InvalidToken(path.into()))?;
        }
        Ok(())
    }

    fn normalize(&mut self) -> Result<(), ThemeProfileError> {
        for value in [
            &mut self.backdrop,
            &mut self.surface,
            &mut self.surface_raised,
            &mut self.text_primary,
            &mut self.text_secondary,
            &mut self.text_muted,
            &mut self.accent,
            &mut self.accent_secondary,
            &mut self.on_accent,
            &mut self.success,
            &mut self.warning,
            &mut self.error,
            &mut self.edge,
            &mut self.inverse_edge,
            &mut self.shadow,
        ] {
            let [red, green, blue, alpha] = parse_color(value)?;
            *value = format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}");
        }
        Ok(())
    }
}

impl Default for PaletteData {
    fn default() -> Self {
        Self::tokyo_night()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpacingData {
    pub xxs: f32,
    pub xs: f32,
    pub small: f32,
    pub medium: f32,
    pub large: f32,
    pub xl: f32,
    pub xxl: f32,
}

impl Default for SpacingData {
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadiiData {
    pub small: f32,
    pub control: f32,
    pub relaxed: f32,
    pub group: f32,
    pub popover: f32,
    pub major: f32,
}

impl Default for RadiiData {
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeData {
    pub font_size: f32,
    pub line_height: f32,
    pub weight: u16,
}

impl TypeData {
    const fn new(font_size: f32, line_height: f32, weight: u16) -> Self {
        Self {
            font_size,
            line_height,
            weight,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypographyData {
    pub ui_families: Vec<String>,
    pub mono_families: Vec<String>,
    pub scale: f32,
    pub caption: TypeData,
    pub body_small: TypeData,
    pub body: TypeData,
    pub label: TypeData,
    pub section: TypeData,
    pub page: TypeData,
    pub display: TypeData,
    pub mono: TypeData,
}

impl Default for TypographyData {
    fn default() -> Self {
        Self {
            ui_families: vec![
                "Noto Sans".into(),
                "Noto Sans CJK SC".into(),
                "system-ui".into(),
                "Noto Color Emoji".into(),
            ],
            mono_families: vec![
                "Maple Mono".into(),
                "Noto Sans Mono".into(),
                "monospace".into(),
            ],
            scale: 1.0,
            caption: TypeData::new(12.0, 16.0, 500),
            body_small: TypeData::new(13.0, 18.0, 400),
            body: TypeData::new(14.0, 20.0, 400),
            label: TypeData::new(14.0, 20.0, 500),
            section: TypeData::new(16.0, 24.0, 600),
            page: TypeData::new(24.0, 32.0, 600),
            display: TypeData::new(32.0, 40.0, 600),
            mono: TypeData::new(13.0, 20.0, 400),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlassMaterialData {
    pub opacity: f32,
    pub backdrop_blur: f32,
    pub wallpaper_tint: f32,
}

impl GlassMaterialData {
    const fn new(opacity: f32, backdrop_blur: f32, wallpaper_tint: f32) -> Self {
        Self {
            opacity,
            backdrop_blur,
            wallpaper_tint,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialsData {
    pub ghost: GlassMaterialData,
    pub compact_node: GlassMaterialData,
    pub hover_transient: GlassMaterialData,
    pub popover: GlassMaterialData,
    pub expanded_panel: GlassMaterialData,
    pub content_surface: GlassMaterialData,
    pub terminal: GlassMaterialData,
}

impl Default for MaterialsData {
    fn default() -> Self {
        Self {
            ghost: GlassMaterialData::new(0.08, 0.0, 0.0),
            compact_node: GlassMaterialData::new(0.34, 16.0, 0.12),
            hover_transient: GlassMaterialData::new(0.46, 20.0, 0.10),
            popover: GlassMaterialData::new(0.68, 24.0, 0.08),
            expanded_panel: GlassMaterialData::new(0.80, 32.0, 0.06),
            content_surface: GlassMaterialData::new(0.86, 36.0, 0.04),
            terminal: GlassMaterialData::new(0.95, 12.0, 0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionDurationsData {
    pub reduced_transition: u64,
    pub hover_in: u64,
    pub hover_out: u64,
    pub press: u64,
    pub release: u64,
    pub focus: u64,
    pub toggle: u64,
    pub slider_trail: u64,
    pub list_transfer: u64,
    pub text_input_focus: u64,
    pub validation: u64,
    pub scrollbar_show: u64,
    pub scrollbar_hide: u64,
    pub overscroll: u64,
    pub tooltip_enter: u64,
    pub tooltip_exit: u64,
    pub popover_enter: u64,
    pub popover_exit: u64,
    pub panel_enter: u64,
    pub panel_exit: u64,
    pub drawer_enter: u64,
    pub drawer_exit: u64,
    pub workspace: u64,
    pub wallpaper: u64,
}

impl Default for MotionDurationsData {
    fn default() -> Self {
        Self {
            reduced_transition: 100,
            hover_in: 120,
            hover_out: 160,
            press: 70,
            release: 140,
            focus: 140,
            toggle: 220,
            slider_trail: 90,
            list_transfer: 180,
            text_input_focus: 160,
            validation: 220,
            scrollbar_show: 100,
            scrollbar_hide: 220,
            overscroll: 260,
            tooltip_enter: 140,
            tooltip_exit: 110,
            popover_enter: 180,
            popover_exit: 150,
            panel_enter: 280,
            panel_exit: 220,
            drawer_enter: 320,
            drawer_exit: 240,
            workspace: 300,
            wallpaper: 800,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FluidData {
    pub neck_variation: f32,
    pub trail_variation: f32,
    pub phase_variation: f32,
    pub maximum_path_offset: f32,
    pub toggle_stretch: f32,
    pub slider_trail: f32,
    pub transfer_base: u64,
    pub transfer_per_unit_ms: f32,
    pub transfer_maximum: u64,
    pub bud_duration: u64,
    pub bud_stagger: u64,
    pub group_maximum: u64,
}

impl Default for FluidData {
    fn default() -> Self {
        Self {
            neck_variation: 0.06,
            trail_variation: 0.08,
            phase_variation: 0.10,
            maximum_path_offset: 3.0,
            toggle_stretch: 6.0,
            slider_trail: 6.0,
            transfer_base: 280,
            transfer_per_unit_ms: 0.18,
            transfer_maximum: 650,
            bud_duration: 180,
            bud_stagger: 60,
            group_maximum: 700,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionData {
    pub mode: MotionModeData,
    pub speed_multiplier: f32,
    pub standard: [f32; 4],
    pub settle: [f32; 4],
    pub exit: [f32; 4],
    pub soft: [f32; 4],
    pub durations: MotionDurationsData,
    pub fluid: FluidData,
}

impl Default for MotionData {
    fn default() -> Self {
        Self {
            mode: MotionModeData::Standard,
            speed_multiplier: 1.0,
            standard: [0.2, 0.0, 0.0, 1.0],
            settle: [0.16, 1.0, 0.3, 1.0],
            exit: [0.4, 0.0, 1.0, 1.0],
            soft: [0.33, 1.0, 0.68, 1.0],
            durations: MotionDurationsData::default(),
            fluid: FluidData::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeData {
    pub density: DensityData,
    pub spacing: SpacingData,
    pub radii: RadiiData,
    pub typography: TypographyData,
    pub palette: PaletteData,
    pub motion: MotionData,
    pub materials: MaterialsData,
}

impl ThemeData {
    pub fn built_in(preset: BuiltInTheme) -> Self {
        Self {
            palette: match preset {
                BuiltInTheme::TokyoNight => PaletteData::tokyo_night(),
                BuiltInTheme::Nord => PaletteData::nord(),
            },
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), ThemeProfileError> {
        validate_ordered(
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
        validate_ordered(
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
        self.palette.validate()?;
        if !self.typography.scale.is_finite()
            || self.typography.scale <= 0.0
            || self.typography.ui_families.is_empty()
            || self.typography.mono_families.is_empty()
            || self
                .typography
                .ui_families
                .iter()
                .chain(&self.typography.mono_families)
                .any(|family| family.trim().is_empty())
        {
            return Err(ThemeProfileError::InvalidToken("typography".into()));
        }
        for (name, token) in [
            ("caption", self.typography.caption),
            ("body_small", self.typography.body_small),
            ("body", self.typography.body),
            ("label", self.typography.label),
            ("section", self.typography.section),
            ("page", self.typography.page),
            ("display", self.typography.display),
            ("mono", self.typography.mono),
        ] {
            if !token.font_size.is_finite()
                || token.font_size <= 0.0
                || !token.line_height.is_finite()
                || token.line_height <= 0.0
                || !(1..=1000).contains(&token.weight)
            {
                return Err(ThemeProfileError::InvalidToken(format!(
                    "typography.{name}"
                )));
            }
        }
        for (name, material) in [
            ("ghost", self.materials.ghost),
            ("compact_node", self.materials.compact_node),
            ("hover_transient", self.materials.hover_transient),
            ("popover", self.materials.popover),
            ("expanded_panel", self.materials.expanded_panel),
            ("content_surface", self.materials.content_surface),
            ("terminal", self.materials.terminal),
        ] {
            if !material.opacity.is_finite()
                || !(0.0..=1.0).contains(&material.opacity)
                || !material.backdrop_blur.is_finite()
                || material.backdrop_blur < 0.0
                || !material.wallpaper_tint.is_finite()
                || !(0.0..=1.0).contains(&material.wallpaper_tint)
            {
                return Err(ThemeProfileError::InvalidToken(format!("materials.{name}")));
            }
        }
        validate_motion(&self.motion)
    }
}

impl Default for ThemeData {
    fn default() -> Self {
        Self {
            density: DensityData::Standard,
            spacing: SpacingData::default(),
            radii: RadiiData::default(),
            typography: TypographyData::default(),
            palette: PaletteData::default(),
            motion: MotionData::default(),
            materials: MaterialsData::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ThemeBase {
    BuiltIn {
        preset: BuiltInTheme,
    },
    Wallpaper {
        live: bool,
        wallpaper_id: String,
        frozen_palette: Box<PaletteData>,
    },
}

impl Default for ThemeBase {
    fn default() -> Self {
        Self::BuiltIn {
            preset: BuiltInTheme::TokyoNight,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeProfile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub base: ThemeBase,
    #[serde(default = "empty_json_object")]
    pub overrides: Json,
}

impl Default for ThemeProfile {
    fn default() -> Self {
        Self {
            schema_version: THEME_SCHEMA_VERSION,
            id: "tokyo-night".into(),
            name: "Tokyo Night".into(),
            base: ThemeBase::default(),
            overrides: empty_json_object(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTheme {
    pub profile: ThemeProfile,
    pub data: ThemeData,
    pub explicit_overrides: BTreeSet<String>,
}

impl ThemeProfile {
    pub fn from_json(text: &str) -> Result<Self, ThemeProfileError> {
        if text.len() > MAX_PROFILE_TEXT_BYTES {
            return Err(ThemeProfileError::TooLarge);
        }
        serde_json::from_str(text).map_err(|error| ThemeProfileError::Syntax(error.to_string()))
    }

    pub fn to_json_pretty(&self) -> Result<String, ThemeProfileError> {
        serde_json::to_string_pretty(self)
            .map_err(|error| ThemeProfileError::Syntax(error.to_string()))
    }

    pub fn resolve(&self) -> Result<ResolvedTheme, ThemeProfileError> {
        if serde_json::to_vec(self)
            .map_err(|error| ThemeProfileError::Syntax(error.to_string()))?
            .len()
            > MAX_PROFILE_TEXT_BYTES
        {
            return Err(ThemeProfileError::TooLarge);
        }
        if self.schema_version != THEME_SCHEMA_VERSION {
            return Err(ThemeProfileError::UnsupportedVersion(self.schema_version));
        }
        validate_profile_name("id", &self.id)?;
        validate_profile_name("name", &self.name)?;
        let mut data = match &self.base {
            ThemeBase::BuiltIn { preset } => ThemeData::built_in(*preset),
            ThemeBase::Wallpaper {
                live,
                wallpaper_id,
                frozen_palette,
            } => {
                if *live && wallpaper_id.trim().is_empty() {
                    return Err(ThemeProfileError::InvalidMetadata(
                        "a live wallpaper profile requires wallpaper_id".into(),
                    ));
                }
                frozen_palette.validate()?;
                ThemeData {
                    palette: frozen_palette.as_ref().clone(),
                    ..ThemeData::default()
                }
            }
        };
        let overrides = self
            .overrides
            .as_object()
            .ok_or(ThemeProfileError::OverridesMustBeObject)?;
        let mut materialized = serde_json::to_value(&data)
            .map_err(|error| ThemeProfileError::Syntax(error.to_string()))?;
        merge_json(&mut materialized, &Json::Object(overrides.clone()));
        data = serde_json::from_value(materialized)
            .map_err(|error| ThemeProfileError::InvalidOverride(error.to_string()))?;
        data.palette.normalize()?;
        data.validate()?;
        let mut explicit_overrides = BTreeSet::new();
        flatten_paths(
            "",
            &Json::Object(overrides.clone()),
            &mut explicit_overrides,
        );
        Ok(ResolvedTheme {
            profile: self.clone(),
            data,
            explicit_overrides,
        })
    }
}

/// Versioned collection of user-saved profiles.
///
/// Built-in profiles are resolved by the application and do not need to be
/// copied into this collection. Library mutations validate a complete
/// candidate before replacing the current value, which gives callers the same
/// all-or-nothing behavior as the `theme.profile` CTRL-5 leaf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeProfileLibrary {
    pub schema_version: u32,
    pub profiles: Vec<ThemeProfile>,
}

impl Default for ThemeProfileLibrary {
    fn default() -> Self {
        Self {
            schema_version: THEME_LIBRARY_SCHEMA_VERSION,
            profiles: Vec::new(),
        }
    }
}

impl ThemeProfileLibrary {
    pub fn from_json(text: &str) -> Result<Self, ThemeLibraryError> {
        if text.len() > MAX_THEME_LIBRARY_TEXT_BYTES {
            return Err(ThemeLibraryError::TooLarge);
        }
        let library: Self = serde_json::from_str(text)
            .map_err(|error| ThemeLibraryError::Syntax(error.to_string()))?;
        library.validate()?;
        Ok(library)
    }

    pub fn to_json(&self) -> Result<String, ThemeLibraryError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| ThemeLibraryError::Syntax(error.to_string()))
    }

    pub fn to_json_pretty(&self) -> Result<String, ThemeLibraryError> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|error| ThemeLibraryError::Syntax(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), ThemeLibraryError> {
        if self.schema_version != THEME_LIBRARY_SCHEMA_VERSION {
            return Err(ThemeLibraryError::UnsupportedVersion(self.schema_version));
        }
        if self.profiles.len() > MAX_THEME_LIBRARY_PROFILES {
            return Err(ThemeLibraryError::TooManyProfiles);
        }
        let mut ids = BTreeSet::new();
        for profile in &self.profiles {
            profile.resolve().map_err(ThemeLibraryError::Profile)?;
            if !ids.insert(profile.id.clone()) {
                return Err(ThemeLibraryError::DuplicateId(profile.id.clone()));
            }
        }
        let size = serde_json::to_vec(self)
            .map_err(|error| ThemeLibraryError::Syntax(error.to_string()))?
            .len();
        if size > MAX_THEME_LIBRARY_TEXT_BYTES {
            return Err(ThemeLibraryError::TooLarge);
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ThemeProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// Insert a profile or replace the saved profile with the same identity.
    /// The collection is unchanged if the resulting library is invalid.
    pub fn save(&mut self, profile: ThemeProfile) -> Result<bool, ThemeLibraryError> {
        profile.resolve().map_err(ThemeLibraryError::Profile)?;
        let mut candidate = self.clone();
        let replaced = if let Some(saved) = candidate
            .profiles
            .iter_mut()
            .find(|saved| saved.id == profile.id)
        {
            *saved = profile;
            true
        } else {
            candidate.profiles.push(profile);
            false
        };
        candidate.validate()?;
        *self = candidate;
        Ok(replaced)
    }

    /// Copy a saved profile under an explicit new identity and display name.
    pub fn copy(
        &mut self,
        source_id: &str,
        new_id: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Result<ThemeProfile, ThemeLibraryError> {
        let mut profile = self
            .get(source_id)
            .cloned()
            .ok_or_else(|| ThemeLibraryError::MissingProfile(source_id.into()))?;
        profile.id = new_id.into();
        profile.name = new_name.into();
        if self.get(&profile.id).is_some() {
            return Err(ThemeLibraryError::DuplicateId(profile.id));
        }
        self.save(profile.clone())?;
        Ok(profile)
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let previous_len = self.profiles.len();
        self.profiles.retain(|profile| profile.id != id);
        self.profiles.len() != previous_len
    }

    pub fn import_profile_json(&mut self, text: &str) -> Result<bool, ThemeLibraryError> {
        let profile = ThemeProfile::from_json(text).map_err(ThemeLibraryError::Profile)?;
        self.save(profile)
    }

    pub fn export_profile_json(&self, id: &str) -> Result<String, ThemeLibraryError> {
        self.get(id)
            .ok_or_else(|| ThemeLibraryError::MissingProfile(id.into()))?
            .to_json_pretty()
            .map_err(ThemeLibraryError::Profile)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenImpact {
    Paint,
    Layout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeTokenChange {
    pub path: String,
    pub impact: TokenImpact,
}

pub fn diff(previous: &ThemeData, current: &ThemeData) -> Vec<ThemeTokenChange> {
    let previous = serde_json::to_value(previous).expect("ThemeData always serializes");
    let current = serde_json::to_value(current).expect("ThemeData always serializes");
    let mut old = BTreeMap::new();
    let mut new = BTreeMap::new();
    flatten_values("", &previous, &mut old);
    flatten_values("", &current, &mut new);
    new.into_iter()
        .filter_map(|(path, value)| {
            (old.get(&path) != Some(&value)).then(|| ThemeTokenChange {
                impact: token_impact(&path),
                path,
            })
        })
        .collect()
}

pub fn token_impact(path: &str) -> TokenImpact {
    if path == "density"
        || path.starts_with("spacing.")
        || path.starts_with("typography.")
        || path.starts_with("motion.")
    {
        TokenImpact::Layout
    } else {
        TokenImpact::Paint
    }
}

pub fn parse_color(value: &str) -> Result<[u8; 4], ThemeProfileError> {
    let digits = value
        .strip_prefix('#')
        .ok_or_else(|| ThemeProfileError::InvalidColor(value.into()))?;
    if digits.len() != 6 && digits.len() != 8 {
        return Err(ThemeProfileError::InvalidColor(value.into()));
    }
    let byte = |offset| {
        u8::from_str_radix(&digits[offset..offset + 2], 16)
            .map_err(|_| ThemeProfileError::InvalidColor(value.into()))
    };
    Ok([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if digits.len() == 8 { byte(6)? } else { 255 },
    ])
}

fn validate_motion(motion: &MotionData) -> Result<(), ThemeProfileError> {
    if !motion.speed_multiplier.is_finite() || !(0.05..=20.0).contains(&motion.speed_multiplier) {
        return Err(ThemeProfileError::InvalidToken(
            "motion.speed_multiplier".into(),
        ));
    }
    for (name, curve) in [
        ("standard", motion.standard),
        ("settle", motion.settle),
        ("exit", motion.exit),
        ("soft", motion.soft),
    ] {
        if !curve.into_iter().all(f32::is_finite)
            || !(0.0..=1.0).contains(&curve[0])
            || !(0.0..=1.0).contains(&curve[2])
        {
            return Err(ThemeProfileError::InvalidToken(format!("motion.{name}")));
        }
    }
    let durations = serde_json::to_value(motion.durations).expect("durations serialize");
    if durations.as_object().is_none_or(|values| {
        values
            .values()
            .any(|value| value.as_u64().is_none_or(|value| value > 60_000))
    }) {
        return Err(ThemeProfileError::InvalidToken("motion.durations".into()));
    }
    let fluid = motion.fluid;
    if ![
        fluid.neck_variation,
        fluid.trail_variation,
        fluid.phase_variation,
    ]
    .into_iter()
    .all(|value| value.is_finite() && (0.0..=0.5).contains(&value))
        || ![
            fluid.maximum_path_offset,
            fluid.toggle_stretch,
            fluid.slider_trail,
            fluid.transfer_per_unit_ms,
        ]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
        || fluid.transfer_base > fluid.transfer_maximum
        || fluid.bud_duration > fluid.group_maximum
        || fluid.bud_stagger > fluid.group_maximum
    {
        return Err(ThemeProfileError::InvalidToken("motion.fluid".into()));
    }
    Ok(())
}

fn validate_ordered<const N: usize>(name: &str, values: [f32; N]) -> Result<(), ThemeProfileError> {
    if values
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
        && values.windows(2).all(|pair| pair[0] <= pair[1])
    {
        Ok(())
    } else {
        Err(ThemeProfileError::InvalidToken(name.into()))
    }
}

fn validate_profile_name(field: &str, value: &str) -> Result<(), ThemeProfileError> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err(ThemeProfileError::InvalidMetadata(field.into()))
    } else {
        Ok(())
    }
}

fn empty_json_object() -> Json {
    Json::Object(JsonMap::new())
}

fn merge_json(target: &mut Json, patch: &Json) {
    match (target, patch) {
        (Json::Object(target), Json::Object(patch)) => {
            for (key, value) in patch {
                if let Some(target) = target.get_mut(key) {
                    merge_json(target, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

fn flatten_paths(prefix: &str, value: &Json, out: &mut BTreeSet<String>) {
    match value {
        Json::Object(object) => {
            for (key, value) in object {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_paths(&path, value, out);
            }
        }
        _ if !prefix.is_empty() => {
            out.insert(prefix.into());
        }
        _ => {}
    }
}

fn flatten_values(prefix: &str, value: &Json, out: &mut BTreeMap<String, Json>) {
    match value {
        Json::Object(object) => {
            for (key, value) in object {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_values(&path, value, out);
            }
        }
        _ => {
            out.insert(prefix.into(), value.clone());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeProfileError {
    TooLarge,
    Syntax(String),
    UnsupportedVersion(u32),
    InvalidMetadata(String),
    OverridesMustBeObject,
    InvalidOverride(String),
    InvalidToken(String),
    InvalidColor(String),
}

impl fmt::Display for ThemeProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("theme profile exceeds 1 MiB"),
            Self::Syntax(error) => write!(formatter, "invalid theme profile JSON: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported theme schema version {version}")
            }
            Self::InvalidMetadata(field) => {
                write!(formatter, "invalid theme profile metadata: {field}")
            }
            Self::OverridesMustBeObject => {
                formatter.write_str("theme overrides must be a JSON object")
            }
            Self::InvalidOverride(error) => write!(formatter, "invalid theme override: {error}"),
            Self::InvalidToken(path) => write!(formatter, "invalid theme token: {path}"),
            Self::InvalidColor(color) => {
                write!(formatter, "invalid #RRGGBB or #RRGGBBAA color: {color}")
            }
        }
    }
}

impl std::error::Error for ThemeProfileError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeLibraryError {
    TooLarge,
    TooManyProfiles,
    Syntax(String),
    UnsupportedVersion(u32),
    DuplicateId(String),
    MissingProfile(String),
    Profile(ThemeProfileError),
}

impl fmt::Display for ThemeLibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("theme library exceeds 4 MiB"),
            Self::TooManyProfiles => write!(
                formatter,
                "theme library exceeds {MAX_THEME_LIBRARY_PROFILES} profiles"
            ),
            Self::Syntax(error) => write!(formatter, "invalid theme library JSON: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported theme library schema version {version}"
                )
            }
            Self::DuplicateId(id) => write!(formatter, "duplicate theme profile id: {id}"),
            Self::MissingProfile(id) => write!(formatter, "theme profile not found: {id}"),
            Self::Profile(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ThemeLibraryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn built_ins_are_complete_and_valid() {
        for preset in [BuiltInTheme::TokyoNight, BuiltInTheme::Nord] {
            ThemeData::built_in(preset).validate().unwrap();
        }
    }

    #[test]
    fn sparse_override_inherits_and_is_tracked() {
        let profile = ThemeProfile {
            overrides: json!({"palette": {"accent": "#112233"}, "spacing": {"medium": 18.0}}),
            ..ThemeProfile::default()
        };
        let resolved = profile.resolve().unwrap();
        assert_eq!(resolved.data.palette.accent, "#112233ff");
        assert_eq!(
            resolved.data.palette.surface,
            PaletteData::tokyo_night().surface
        );
        assert!(resolved.explicit_overrides.contains("palette.accent"));
        assert!(resolved.explicit_overrides.contains("spacing.medium"));
    }

    #[test]
    fn wallpaper_regeneration_preserves_explicit_roles() {
        let overrides = json!({"palette": {"accent": "#010203ff"}});
        let profile = ThemeProfile {
            id: "wallpaper".into(),
            name: "Wallpaper".into(),
            base: ThemeBase::Wallpaper {
                live: true,
                wallpaper_id: "sha256:old".into(),
                frozen_palette: Box::new(PaletteData::tokyo_night()),
            },
            overrides: overrides.clone(),
            ..ThemeProfile::default()
        };
        let mut regenerated = profile.clone();
        regenerated.base = ThemeBase::Wallpaper {
            live: true,
            wallpaper_id: "sha256:new".into(),
            frozen_palette: Box::new(PaletteData::nord()),
        };
        regenerated.overrides = overrides;
        assert_eq!(profile.resolve().unwrap().data.palette.accent, "#010203ff");
        let resolved = regenerated.resolve().unwrap();
        assert_eq!(resolved.data.palette.accent, "#010203ff");
        assert_eq!(resolved.data.palette.surface, PaletteData::nord().surface);
    }

    #[test]
    fn unknown_or_invalid_override_rejects_entire_profile() {
        let unknown = ThemeProfile {
            overrides: json!({"palette": {"accnet": "#ffffff"}}),
            ..ThemeProfile::default()
        };
        assert!(matches!(
            unknown.resolve(),
            Err(ThemeProfileError::InvalidOverride(_))
        ));
        let invalid = ThemeProfile {
            overrides: json!({"spacing": {"small": 99.0}}),
            ..ThemeProfile::default()
        };
        assert_eq!(
            invalid.resolve().unwrap_err(),
            ThemeProfileError::InvalidToken("spacing".into())
        );
    }

    #[test]
    fn diff_is_token_exact_and_classifies_impact() {
        let old = ThemeData::default();
        let mut new = old.clone();
        new.palette.accent = "#010203ff".into();
        let changes = diff(&old, &new);
        assert_eq!(
            changes,
            vec![ThemeTokenChange {
                path: "palette.accent".into(),
                impact: TokenImpact::Paint
            }]
        );
        new.spacing.medium = 18.0;
        assert!(diff(&old, &new).contains(&ThemeTokenChange {
            path: "spacing.medium".into(),
            impact: TokenImpact::Layout
        }));
    }

    #[test]
    fn equivalent_color_spellings_resolve_to_one_semantic_token() {
        let short = ThemeProfile {
            overrides: json!({"palette": {"accent": "#AABBCC"}}),
            ..ThemeProfile::default()
        }
        .resolve()
        .unwrap();
        let explicit_alpha = ThemeProfile {
            overrides: json!({"palette": {"accent": "#aabbccff"}}),
            ..ThemeProfile::default()
        }
        .resolve()
        .unwrap();
        assert!(diff(&short.data, &explicit_alpha.data).is_empty());
    }

    #[test]
    fn library_save_copy_and_profile_round_trip_are_atomic() {
        let mut library = ThemeProfileLibrary::default();
        let profile = ThemeProfile {
            id: "night-work".into(),
            name: "Night Work".into(),
            overrides: json!({"palette": {"accent": "#123456ff"}}),
            ..ThemeProfile::default()
        };
        assert!(!library.save(profile.clone()).unwrap());
        assert!(library.save(profile).unwrap());
        let copied = library
            .copy("night-work", "night-work-copy", "Night Work Copy")
            .unwrap();
        assert_eq!(copied.id, "night-work-copy");

        let exported = library.export_profile_json("night-work-copy").unwrap();
        let mut imported = ThemeProfileLibrary::default();
        assert!(!imported.import_profile_json(&exported).unwrap());
        assert_eq!(imported.get("night-work-copy"), Some(&copied));

        let whole_library =
            ThemeProfileLibrary::from_json(&library.to_json_pretty().unwrap()).unwrap();
        assert_eq!(whole_library, library);
    }

    #[test]
    fn invalid_or_duplicate_library_candidate_preserves_last_good_value() {
        let valid = ThemeProfile {
            id: "valid".into(),
            name: "Valid".into(),
            ..ThemeProfile::default()
        };
        let mut library = ThemeProfileLibrary::default();
        library.save(valid.clone()).unwrap();
        let before = library.clone();
        let invalid = ThemeProfile {
            id: "broken".into(),
            name: "Broken".into(),
            overrides: json!({"materials": {"content_surface": {"opacity": 9.0}}}),
            ..ThemeProfile::default()
        };
        assert!(library.save(invalid).is_err());
        assert_eq!(library, before);

        let duplicate = ThemeProfileLibrary {
            schema_version: THEME_LIBRARY_SCHEMA_VERSION,
            profiles: vec![valid.clone(), valid],
        };
        assert_eq!(
            duplicate.validate().unwrap_err(),
            ThemeLibraryError::DuplicateId("valid".into())
        );
    }
}

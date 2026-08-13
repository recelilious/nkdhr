//! Versioned, non-executable motion-style profiles and preset snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{MotionCurveData, MotionCurveDataError, MotionData};

pub const MOTION_STYLE_SCHEMA_VERSION: u32 = 1;
pub const MOTION_PRESET_LIBRARY_SCHEMA_VERSION: u32 = 1;
pub const BALANCED_MOTION_STYLE_REVISION: u32 = 1;
pub const MAX_MOTION_STYLE_NODES: usize = 4096;
pub const MAX_MOTION_PRESET_LIBRARY_PRESETS: usize = 256;
const MAX_MOTION_STYLE_TEXT_BYTES: usize = 1024 * 1024;
const MAX_MOTION_PRESET_LIBRARY_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_MOTION_DURATION_MS: u64 = 60_000;

/// Stable identifiers are deliberately data rather than display strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltInMotionStyle {
    Balanced,
    Lively,
    Calm,
    Direct,
}

/// The semantic family vocabulary matches the runtime's existing public
/// families but remains portable and independent from `nkdhr-ui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MotionSemanticFamilyData {
    HoverIn,
    HoverOut,
    Press,
    Release,
    Focus,
    Toggle,
    SliderTrail,
    ListTransfer,
    TextInputFocus,
    Validation,
    ScrollbarShow,
    ScrollbarHide,
    Overscroll,
    TooltipEnter,
    TooltipExit,
    PopoverEnter,
    PopoverExit,
    PanelEnter,
    PanelExit,
    DrawerEnter,
    DrawerExit,
    Workspace,
    Wallpaper,
}

impl MotionSemanticFamilyData {
    pub const ALL: [Self; 23] = [
        Self::HoverIn,
        Self::HoverOut,
        Self::Press,
        Self::Release,
        Self::Focus,
        Self::Toggle,
        Self::SliderTrail,
        Self::ListTransfer,
        Self::TextInputFocus,
        Self::Validation,
        Self::ScrollbarShow,
        Self::ScrollbarHide,
        Self::Overscroll,
        Self::TooltipEnter,
        Self::TooltipExit,
        Self::PopoverEnter,
        Self::PopoverExit,
        Self::PanelEnter,
        Self::PanelExit,
        Self::DrawerEnter,
        Self::DrawerExit,
        Self::Workspace,
        Self::Wallpaper,
    ];
}

/// One inheritance unit. A curve is always replaced atomically; duration is a
/// separate field and can therefore inherit from a different scope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionValuesData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<MotionCurveData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl MotionValuesData {
    pub fn is_empty(&self) -> bool {
        self.curve.is_none() && self.duration_ms.is_none()
    }

    fn validate(&self) -> Result<(), MotionStyleError> {
        if let Some(curve) = &self.curve {
            curve.validate().map_err(MotionStyleError::Curve)?;
        }
        if self
            .duration_ms
            .is_some_and(|value| value > MAX_MOTION_DURATION_MS)
        {
            return Err(MotionStyleError::InvalidDuration);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionComponentNodeData {
    #[serde(default)]
    pub values: MotionValuesData,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub transitions: BTreeMap<String, MotionValuesData>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionFamilyNodeData {
    #[serde(default)]
    pub values: MotionValuesData,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, MotionComponentNodeData>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionStyleTreeData {
    #[serde(default)]
    pub values: MotionValuesData,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub families: BTreeMap<MotionSemanticFamilyData, MotionFamilyNodeData>,
}

impl MotionStyleTreeData {
    fn validate(&self, require_complete_root: bool) -> Result<(), MotionStyleError> {
        self.values.validate()?;
        if require_complete_root
            && (self.values.curve.is_none() || self.values.duration_ms.is_none())
        {
            return Err(MotionStyleError::IncompletePresetRoot);
        }
        let mut nodes = 1_usize;
        for family in self.families.values() {
            nodes = nodes.saturating_add(1);
            family.values.validate()?;
            for (component_id, component) in &family.components {
                validate_stable_id("component", component_id)?;
                nodes = nodes.saturating_add(1);
                component.values.validate()?;
                for (transition_id, values) in &component.transitions {
                    validate_stable_id("transition", transition_id)?;
                    if values.is_empty() {
                        return Err(MotionStyleError::EmptyTransition);
                    }
                    values.validate()?;
                    nodes = nodes.saturating_add(1);
                }
            }
        }
        if nodes > MAX_MOTION_STYLE_NODES {
            return Err(MotionStyleError::TooManyNodes);
        }
        Ok(())
    }
}

/// Immutable preset payload. `(id, revision)` is its permanent identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionStylePresetData {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub revision: u32,
    pub style: MotionStyleTreeData,
}

impl MotionStylePresetData {
    pub fn built_in(style: BuiltInMotionStyle, revision: u32) -> Result<Self, MotionStyleError> {
        match (style, revision) {
            (BuiltInMotionStyle::Balanced, BALANCED_MOTION_STYLE_REVISION) => {
                legacy_preset(&MotionData::default(), "balanced", "Balanced", revision)
            }
            _ => Err(MotionStyleError::UnavailableBuiltInRevision { style, revision }),
        }
    }

    pub fn from_legacy_motion(motion: &MotionData) -> Result<Self, MotionStyleError> {
        let preset = legacy_preset(motion, "legacy-motion", "Legacy Motion", 1)?;
        preset.validate()?;
        Ok(preset)
    }

    pub fn from_json(text: &str) -> Result<Self, MotionStyleError> {
        if text.len() > MAX_MOTION_STYLE_TEXT_BYTES {
            return Err(MotionStyleError::TooLarge);
        }
        let preset: Self = serde_json::from_str(text)
            .map_err(|error| MotionStyleError::Syntax(error.to_string()))?;
        preset.validate()?;
        Ok(preset)
    }

    pub fn to_json_pretty(&self) -> Result<String, MotionStyleError> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|error| MotionStyleError::Syntax(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), MotionStyleError> {
        validate_schema(self.schema_version)?;
        validate_metadata("id", &self.id)?;
        validate_metadata("name", &self.name)?;
        if self.revision == 0 {
            return Err(MotionStyleError::InvalidRevision);
        }
        self.style.validate(true)?;
        validate_serialized_size(self, MAX_MOTION_STYLE_TEXT_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MotionStyleBaseData {
    BuiltIn {
        style: BuiltInMotionStyle,
        revision: u32,
    },
    Embedded {
        preset: Box<MotionStylePresetData>,
    },
}

impl Default for MotionStyleBaseData {
    fn default() -> Self {
        Self::BuiltIn {
            style: BuiltInMotionStyle::Balanced,
            revision: BALANCED_MOTION_STYLE_REVISION,
        }
    }
}

/// Active style document. The base is an immutable pinned snapshot and the
/// sparse override tree contains only authored differences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionStyleProfileData {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub base: MotionStyleBaseData,
    #[serde(default)]
    pub overrides: MotionStyleTreeData,
}

impl Default for MotionStyleProfileData {
    fn default() -> Self {
        Self {
            schema_version: MOTION_STYLE_SCHEMA_VERSION,
            id: "balanced".into(),
            name: "Balanced".into(),
            base: MotionStyleBaseData::default(),
            overrides: MotionStyleTreeData::default(),
        }
    }
}

impl MotionStyleProfileData {
    pub fn from_legacy_motion(motion: &MotionData) -> Result<Self, MotionStyleError> {
        Ok(Self {
            schema_version: MOTION_STYLE_SCHEMA_VERSION,
            id: "legacy-motion".into(),
            name: "Legacy Motion".into(),
            base: MotionStyleBaseData::Embedded {
                preset: Box::new(MotionStylePresetData::from_legacy_motion(motion)?),
            },
            overrides: MotionStyleTreeData::default(),
        })
    }

    pub fn from_embedded_preset(
        id: impl Into<String>,
        name: impl Into<String>,
        preset: MotionStylePresetData,
    ) -> Result<Self, MotionStyleError> {
        preset.validate()?;
        let profile = Self {
            schema_version: MOTION_STYLE_SCHEMA_VERSION,
            id: id.into(),
            name: name.into(),
            base: MotionStyleBaseData::Embedded {
                preset: Box::new(preset),
            },
            overrides: MotionStyleTreeData::default(),
        };
        profile.resolve()?;
        Ok(profile)
    }

    pub fn from_json(text: &str) -> Result<Self, MotionStyleError> {
        if text.len() > MAX_MOTION_STYLE_TEXT_BYTES {
            return Err(MotionStyleError::TooLarge);
        }
        let profile: Self = serde_json::from_str(text)
            .map_err(|error| MotionStyleError::Syntax(error.to_string()))?;
        profile.resolve()?;
        Ok(profile)
    }

    pub fn to_json_pretty(&self) -> Result<String, MotionStyleError> {
        self.resolve()?;
        serde_json::to_string_pretty(self)
            .map_err(|error| MotionStyleError::Syntax(error.to_string()))
    }

    pub fn resolve(&self) -> Result<ResolvedMotionStyleData, MotionStyleError> {
        validate_schema(self.schema_version)?;
        validate_metadata("id", &self.id)?;
        validate_metadata("name", &self.name)?;
        self.overrides.validate(false)?;
        let (preset, origin) = match &self.base {
            MotionStyleBaseData::BuiltIn { style, revision } => (
                MotionStylePresetData::built_in(*style, *revision)?,
                MotionValueOriginData::BuiltIn {
                    style: *style,
                    revision: *revision,
                },
            ),
            MotionStyleBaseData::Embedded { preset } => {
                preset.validate()?;
                (
                    preset.as_ref().clone(),
                    MotionValueOriginData::EmbeddedPreset {
                        preset_id: preset.id.clone(),
                        revision: preset.revision,
                    },
                )
            }
        };
        validate_serialized_size(self, MAX_MOTION_STYLE_TEXT_BYTES)?;
        Ok(ResolvedMotionStyleData {
            profile: self.clone(),
            preset,
            base_origin: origin,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionScopeData {
    pub family: Option<MotionSemanticFamilyData>,
    pub component: Option<String>,
    pub transition: Option<String>,
}

impl MotionScopeData {
    pub fn profile() -> Self {
        Self {
            family: None,
            component: None,
            transition: None,
        }
    }

    pub fn family(family: MotionSemanticFamilyData) -> Self {
        Self {
            family: Some(family),
            component: None,
            transition: None,
        }
    }

    pub fn component(family: MotionSemanticFamilyData, component: impl Into<String>) -> Self {
        Self {
            family: Some(family),
            component: Some(component.into()),
            transition: None,
        }
    }

    pub fn transition(
        family: MotionSemanticFamilyData,
        component: impl Into<String>,
        transition: impl Into<String>,
    ) -> Self {
        Self {
            family: Some(family),
            component: Some(component.into()),
            transition: Some(transition.into()),
        }
    }

    pub fn validate(&self) -> Result<(), MotionStyleError> {
        if self.family.is_none() && (self.component.is_some() || self.transition.is_some())
            || self.component.is_none() && self.transition.is_some()
        {
            return Err(MotionStyleError::InvalidScope);
        }
        if let Some(component) = &self.component {
            validate_stable_id("component", component)?;
        }
        if let Some(transition) = &self.transition {
            validate_stable_id("transition", transition)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionScopeLevelData {
    Profile,
    Family,
    Component,
    Transition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionValueOriginData {
    BuiltIn {
        style: BuiltInMotionStyle,
        revision: u32,
    },
    EmbeddedPreset {
        preset_id: String,
        revision: u32,
    },
    ProfileOverride {
        profile_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionValueProvenanceData {
    pub origin: MotionValueOriginData,
    pub level: MotionScopeLevelData,
    pub family: Option<MotionSemanticFamilyData>,
    pub component: Option<String>,
    pub transition: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMotionValuesData {
    pub curve: MotionCurveData,
    pub duration_ms: u64,
    pub curve_provenance: MotionValueProvenanceData,
    pub duration_provenance: MotionValueProvenanceData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMotionStyleData {
    pub profile: MotionStyleProfileData,
    pub preset: MotionStylePresetData,
    base_origin: MotionValueOriginData,
}

impl ResolvedMotionStyleData {
    pub fn base_origin(&self) -> &MotionValueOriginData {
        &self.base_origin
    }

    /// Freeze the base plus every same-scope profile override into one new
    /// immutable user preset revision. Parent inheritance remains sparse; the
    /// result no longer depends on the source built-in revision.
    pub fn snapshot_as_preset(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        revision: u32,
    ) -> Result<MotionStylePresetData, MotionStyleError> {
        let mut style = self.preset.style.clone();
        overlay_tree(&mut style, &self.profile.overrides);
        let preset = MotionStylePresetData {
            schema_version: MOTION_STYLE_SCHEMA_VERSION,
            id: id.into(),
            name: name.into(),
            revision,
            style,
        };
        preset.validate()?;
        Ok(preset)
    }

    pub fn resolve_scope(
        &self,
        scope: &MotionScopeData,
    ) -> Result<ResolvedMotionValuesData, MotionStyleError> {
        scope.validate()?;
        let mut curve = None;
        let mut duration_ms = None;
        let mut curve_provenance = None;
        let mut duration_provenance = None;
        let override_origin = MotionValueOriginData::ProfileOverride {
            profile_id: self.profile.id.clone(),
        };
        for level in [
            MotionScopeLevelData::Profile,
            MotionScopeLevelData::Family,
            MotionScopeLevelData::Component,
            MotionScopeLevelData::Transition,
        ] {
            for (tree, origin) in [
                (&self.preset.style, &self.base_origin),
                (&self.profile.overrides, &override_origin),
            ] {
                if let Some(values) = values_at_level(tree, scope, level) {
                    apply_values(
                        values,
                        provenance(origin, level, scope),
                        &mut curve,
                        &mut duration_ms,
                        &mut curve_provenance,
                        &mut duration_provenance,
                    );
                }
            }
        }
        Ok(ResolvedMotionValuesData {
            curve: curve.ok_or(MotionStyleError::IncompletePresetRoot)?,
            duration_ms: duration_ms.ok_or(MotionStyleError::IncompletePresetRoot)?,
            curve_provenance: curve_provenance.ok_or(MotionStyleError::IncompletePresetRoot)?,
            duration_provenance: duration_provenance
                .ok_or(MotionStyleError::IncompletePresetRoot)?,
        })
    }
}

fn overlay_tree(base: &mut MotionStyleTreeData, overlay: &MotionStyleTreeData) {
    overlay_values(&mut base.values, &overlay.values);
    for (family_id, overlay_family) in &overlay.families {
        let family = base.families.entry(*family_id).or_default();
        overlay_values(&mut family.values, &overlay_family.values);
        for (component_id, overlay_component) in &overlay_family.components {
            let component = family.components.entry(component_id.clone()).or_default();
            overlay_values(&mut component.values, &overlay_component.values);
            for (transition_id, overlay_values_data) in &overlay_component.transitions {
                let values = component
                    .transitions
                    .entry(transition_id.clone())
                    .or_default();
                overlay_values(values, overlay_values_data);
            }
        }
    }
}

fn overlay_values(base: &mut MotionValuesData, overlay: &MotionValuesData) {
    if let Some(curve) = &overlay.curve {
        base.curve = Some(curve.clone());
    }
    if let Some(duration_ms) = overlay.duration_ms {
        base.duration_ms = Some(duration_ms);
    }
}

fn values_at_level<'a>(
    tree: &'a MotionStyleTreeData,
    scope: &MotionScopeData,
    level: MotionScopeLevelData,
) -> Option<&'a MotionValuesData> {
    match level {
        MotionScopeLevelData::Profile => Some(&tree.values),
        MotionScopeLevelData::Family => tree
            .families
            .get(&scope.family?)
            .map(|family| &family.values),
        MotionScopeLevelData::Component => tree
            .families
            .get(&scope.family?)?
            .components
            .get(scope.component.as_deref()?)
            .map(|component| &component.values),
        MotionScopeLevelData::Transition => tree
            .families
            .get(&scope.family?)?
            .components
            .get(scope.component.as_deref()?)?
            .transitions
            .get(scope.transition.as_deref()?),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_values(
    values: &MotionValuesData,
    provenance: MotionValueProvenanceData,
    curve: &mut Option<MotionCurveData>,
    duration_ms: &mut Option<u64>,
    curve_provenance: &mut Option<MotionValueProvenanceData>,
    duration_provenance: &mut Option<MotionValueProvenanceData>,
) {
    if let Some(value) = &values.curve {
        *curve = Some(value.clone());
        *curve_provenance = Some(provenance.clone());
    }
    if let Some(value) = values.duration_ms {
        *duration_ms = Some(value);
        *duration_provenance = Some(provenance);
    }
}

fn provenance(
    origin: &MotionValueOriginData,
    level: MotionScopeLevelData,
    scope: &MotionScopeData,
) -> MotionValueProvenanceData {
    MotionValueProvenanceData {
        origin: origin.clone(),
        level,
        family: (level != MotionScopeLevelData::Profile)
            .then_some(scope.family)
            .flatten(),
        component: matches!(
            level,
            MotionScopeLevelData::Component | MotionScopeLevelData::Transition
        )
        .then(|| scope.component.clone())
        .flatten(),
        transition: (level == MotionScopeLevelData::Transition)
            .then(|| scope.transition.clone())
            .flatten(),
    }
}

/// Saved user presets are immutable snapshots. The same `(id, revision)` can
/// be imported twice only if its complete payload is identical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionPresetLibraryData {
    pub schema_version: u32,
    pub presets: Vec<MotionStylePresetData>,
}

impl Default for MotionPresetLibraryData {
    fn default() -> Self {
        Self {
            schema_version: MOTION_PRESET_LIBRARY_SCHEMA_VERSION,
            presets: Vec::new(),
        }
    }
}

impl MotionPresetLibraryData {
    pub fn from_json(text: &str) -> Result<Self, MotionPresetLibraryError> {
        if text.len() > MAX_MOTION_PRESET_LIBRARY_TEXT_BYTES {
            return Err(MotionPresetLibraryError::TooLarge);
        }
        let library: Self = serde_json::from_str(text)
            .map_err(|error| MotionPresetLibraryError::Syntax(error.to_string()))?;
        library.validate()?;
        Ok(library)
    }

    pub fn to_json(&self) -> Result<String, MotionPresetLibraryError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| MotionPresetLibraryError::Syntax(error.to_string()))
    }

    pub fn to_json_pretty(&self) -> Result<String, MotionPresetLibraryError> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|error| MotionPresetLibraryError::Syntax(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), MotionPresetLibraryError> {
        if self.schema_version != MOTION_PRESET_LIBRARY_SCHEMA_VERSION {
            return Err(MotionPresetLibraryError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        if self.presets.len() > MAX_MOTION_PRESET_LIBRARY_PRESETS {
            return Err(MotionPresetLibraryError::TooManyPresets);
        }
        let mut identities = BTreeSet::new();
        for preset in &self.presets {
            preset
                .validate()
                .map_err(MotionPresetLibraryError::Preset)?;
            if !identities.insert((preset.id.clone(), preset.revision)) {
                return Err(MotionPresetLibraryError::DuplicateRevision {
                    id: preset.id.clone(),
                    revision: preset.revision,
                });
            }
        }
        validate_serialized_size(self, MAX_MOTION_PRESET_LIBRARY_TEXT_BYTES).map_err(|error| {
            match error {
                MotionStyleError::TooLarge => MotionPresetLibraryError::TooLarge,
                other => MotionPresetLibraryError::Preset(other),
            }
        })
    }

    pub fn get(&self, id: &str, revision: u32) -> Option<&MotionStylePresetData> {
        self.presets
            .iter()
            .find(|preset| preset.id == id && preset.revision == revision)
    }

    pub fn latest(&self, id: &str) -> Option<&MotionStylePresetData> {
        self.presets
            .iter()
            .filter(|preset| preset.id == id)
            .max_by_key(|preset| preset.revision)
    }

    pub fn insert(
        &mut self,
        preset: MotionStylePresetData,
    ) -> Result<bool, MotionPresetLibraryError> {
        // Normalize through the portable representation before identity
        // comparison. This prevents an in-memory f64 produced by arithmetic
        // from conflicting with its own standards-compliant JSON round trip.
        let preset: MotionStylePresetData = serde_json::from_str(
            &serde_json::to_string(&preset)
                .map_err(|error| MotionPresetLibraryError::Syntax(error.to_string()))?,
        )
        .map_err(|error| MotionPresetLibraryError::Syntax(error.to_string()))?;
        preset
            .validate()
            .map_err(MotionPresetLibraryError::Preset)?;
        if let Some(existing) = self.get(&preset.id, preset.revision) {
            if existing == &preset {
                return Ok(false);
            }
            return Err(MotionPresetLibraryError::RevisionConflict {
                id: preset.id,
                revision: preset.revision,
            });
        }
        let mut candidate = self.clone();
        candidate.presets.push(preset);
        candidate
            .presets
            .sort_by(|left, right| (&left.id, left.revision).cmp(&(&right.id, right.revision)));
        candidate.validate()?;
        *self = candidate;
        Ok(true)
    }

    pub fn import_preset_json(&mut self, text: &str) -> Result<bool, MotionPresetLibraryError> {
        let preset =
            MotionStylePresetData::from_json(text).map_err(MotionPresetLibraryError::Preset)?;
        self.insert(preset)
    }

    pub fn export_preset_json(
        &self,
        id: &str,
        revision: u32,
    ) -> Result<String, MotionPresetLibraryError> {
        self.get(id, revision)
            .ok_or_else(|| MotionPresetLibraryError::MissingPreset {
                id: id.into(),
                revision,
            })?
            .to_json_pretty()
            .map_err(MotionPresetLibraryError::Preset)
    }
}

fn legacy_preset(
    motion: &MotionData,
    id: &str,
    name: &str,
    revision: u32,
) -> Result<MotionStylePresetData, MotionStyleError> {
    let mut families = BTreeMap::new();
    for family in MotionSemanticFamilyData::ALL {
        let duration_ms = match family {
            MotionSemanticFamilyData::HoverIn => motion.durations.hover_in,
            MotionSemanticFamilyData::HoverOut => motion.durations.hover_out,
            MotionSemanticFamilyData::Press => motion.durations.press,
            MotionSemanticFamilyData::Release => motion.durations.release,
            MotionSemanticFamilyData::Focus => motion.durations.focus,
            MotionSemanticFamilyData::Toggle => motion.durations.toggle,
            MotionSemanticFamilyData::SliderTrail => motion.durations.slider_trail,
            MotionSemanticFamilyData::ListTransfer => motion.durations.list_transfer,
            MotionSemanticFamilyData::TextInputFocus => motion.durations.text_input_focus,
            MotionSemanticFamilyData::Validation => motion.durations.validation,
            MotionSemanticFamilyData::ScrollbarShow => motion.durations.scrollbar_show,
            MotionSemanticFamilyData::ScrollbarHide => motion.durations.scrollbar_hide,
            MotionSemanticFamilyData::Overscroll => motion.durations.overscroll,
            MotionSemanticFamilyData::TooltipEnter => motion.durations.tooltip_enter,
            MotionSemanticFamilyData::TooltipExit => motion.durations.tooltip_exit,
            MotionSemanticFamilyData::PopoverEnter => motion.durations.popover_enter,
            MotionSemanticFamilyData::PopoverExit => motion.durations.popover_exit,
            MotionSemanticFamilyData::PanelEnter => motion.durations.panel_enter,
            MotionSemanticFamilyData::PanelExit => motion.durations.panel_exit,
            MotionSemanticFamilyData::DrawerEnter => motion.durations.drawer_enter,
            MotionSemanticFamilyData::DrawerExit => motion.durations.drawer_exit,
            MotionSemanticFamilyData::Workspace => motion.durations.workspace,
            MotionSemanticFamilyData::Wallpaper => motion.durations.wallpaper,
        };
        let legacy_curve = match family {
            MotionSemanticFamilyData::Release
            | MotionSemanticFamilyData::Toggle
            | MotionSemanticFamilyData::ListTransfer
            | MotionSemanticFamilyData::Overscroll
            | MotionSemanticFamilyData::PanelEnter
            | MotionSemanticFamilyData::DrawerEnter => Some(motion.settle),
            MotionSemanticFamilyData::HoverOut
            | MotionSemanticFamilyData::ScrollbarHide
            | MotionSemanticFamilyData::TooltipExit
            | MotionSemanticFamilyData::PopoverExit
            | MotionSemanticFamilyData::PanelExit
            | MotionSemanticFamilyData::DrawerExit => Some(motion.exit),
            MotionSemanticFamilyData::Workspace | MotionSemanticFamilyData::Wallpaper => {
                Some(motion.soft)
            }
            _ => None,
        };
        families.insert(
            family,
            MotionFamilyNodeData {
                values: MotionValuesData {
                    curve: legacy_curve
                        .map(MotionCurveData::from_legacy_cubic)
                        .transpose()
                        .map_err(MotionStyleError::Curve)?,
                    duration_ms: Some(duration_ms),
                },
                components: BTreeMap::new(),
            },
        );
    }
    Ok(MotionStylePresetData {
        schema_version: MOTION_STYLE_SCHEMA_VERSION,
        id: id.into(),
        name: name.into(),
        revision,
        style: MotionStyleTreeData {
            values: MotionValuesData {
                curve: Some(
                    MotionCurveData::from_legacy_cubic(motion.standard)
                        .map_err(MotionStyleError::Curve)?,
                ),
                duration_ms: Some(180),
            },
            families,
        },
    })
}

fn validate_schema(version: u32) -> Result<(), MotionStyleError> {
    if version == MOTION_STYLE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(MotionStyleError::UnsupportedVersion(version))
    }
}

fn validate_metadata(field: &'static str, value: &str) -> Result<(), MotionStyleError> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err(MotionStyleError::InvalidMetadata(field))
    } else {
        Ok(())
    }
}

fn validate_stable_id(kind: &'static str, value: &str) -> Result<(), MotionStyleError> {
    let bytes = value.as_bytes();
    let edge_valid = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if bytes.is_empty()
        || bytes.len() > 128
        || !edge_valid(bytes[0])
        || !edge_valid(bytes[bytes.len() - 1])
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        Err(MotionStyleError::InvalidStableId {
            kind,
            value: value.into(),
        })
    } else {
        Ok(())
    }
}

fn validate_serialized_size<T: Serialize>(
    value: &T,
    maximum: usize,
) -> Result<(), MotionStyleError> {
    let size = serde_json::to_vec(value)
        .map_err(|error| MotionStyleError::Syntax(error.to_string()))?
        .len();
    if size > maximum {
        Err(MotionStyleError::TooLarge)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MotionStyleError {
    Syntax(String),
    TooLarge,
    UnsupportedVersion(u32),
    InvalidMetadata(&'static str),
    InvalidRevision,
    UnavailableBuiltInRevision {
        style: BuiltInMotionStyle,
        revision: u32,
    },
    InvalidStableId {
        kind: &'static str,
        value: String,
    },
    InvalidScope,
    IncompletePresetRoot,
    EmptyTransition,
    InvalidDuration,
    TooManyNodes,
    Curve(MotionCurveDataError),
}

impl fmt::Display for MotionStyleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => write!(formatter, "invalid motion-style JSON: {error}"),
            Self::TooLarge => formatter.write_str("motion-style document is too large"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported motion-style schema version {version}"
                )
            }
            Self::InvalidMetadata(field) => write!(formatter, "invalid motion-style {field}"),
            Self::InvalidRevision => formatter.write_str("motion-style revision must be positive"),
            Self::UnavailableBuiltInRevision { style, revision } => {
                write!(
                    formatter,
                    "built-in motion style {style:?} revision {revision} is unavailable"
                )
            }
            Self::InvalidStableId { kind, value } => {
                write!(formatter, "invalid stable {kind} identifier {value}")
            }
            Self::InvalidScope => formatter.write_str("motion scope hierarchy is incomplete"),
            Self::IncompletePresetRoot => {
                formatter.write_str("motion preset root must define a curve and duration")
            }
            Self::EmptyTransition => formatter.write_str("motion transition override is empty"),
            Self::InvalidDuration => formatter.write_str("motion duration exceeds 60000 ms"),
            Self::TooManyNodes => formatter.write_str("motion style contains too many scope nodes"),
            Self::Curve(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MotionStyleError {}

#[derive(Debug, Clone, PartialEq)]
pub enum MotionPresetLibraryError {
    Syntax(String),
    TooLarge,
    UnsupportedVersion(u32),
    TooManyPresets,
    DuplicateRevision { id: String, revision: u32 },
    RevisionConflict { id: String, revision: u32 },
    MissingPreset { id: String, revision: u32 },
    Preset(MotionStyleError),
}

impl fmt::Display for MotionPresetLibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => write!(formatter, "invalid motion preset library JSON: {error}"),
            Self::TooLarge => formatter.write_str("motion preset library is too large"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported motion preset library version {version}"
                )
            }
            Self::TooManyPresets => write!(
                formatter,
                "motion preset library exceeds {MAX_MOTION_PRESET_LIBRARY_PRESETS} presets"
            ),
            Self::DuplicateRevision { id, revision } => {
                write!(
                    formatter,
                    "duplicate motion preset {id} revision {revision}"
                )
            }
            Self::RevisionConflict { id, revision } => write!(
                formatter,
                "motion preset {id} revision {revision} is immutable and has different content"
            ),
            Self::MissingPreset { id, revision } => {
                write!(formatter, "missing motion preset {id} revision {revision}")
            }
            Self::Preset(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MotionPresetLibraryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_snapshot_preserves_every_legacy_family() {
        let legacy = MotionData::default();
        let profile = MotionStyleProfileData::from_legacy_motion(&legacy).unwrap();
        let resolved = profile.resolve().unwrap();
        for family in MotionSemanticFamilyData::ALL {
            let values = resolved
                .resolve_scope(&MotionScopeData::family(family))
                .unwrap();
            assert!(values.duration_ms <= MAX_MOTION_DURATION_MS);
            values.curve.validate().unwrap();
        }
    }

    #[test]
    fn curve_and_duration_inherit_independently_with_exact_provenance() {
        let mut profile = MotionStyleProfileData::default();
        let family = profile
            .overrides
            .families
            .entry(MotionSemanticFamilyData::Toggle)
            .or_default();
        family.values.duration_ms = Some(333);
        family.components.insert(
            "nkdhr.toggle".into(),
            MotionComponentNodeData {
                values: MotionValuesData {
                    curve: Some(MotionCurveData::linear()),
                    duration_ms: None,
                },
                transitions: BTreeMap::new(),
            },
        );
        let values = profile
            .resolve()
            .unwrap()
            .resolve_scope(&MotionScopeData::component(
                MotionSemanticFamilyData::Toggle,
                "nkdhr.toggle",
            ))
            .unwrap();
        assert_eq!(values.duration_ms, 333);
        assert_eq!(
            values.duration_provenance.level,
            MotionScopeLevelData::Family
        );
        assert_eq!(values.curve, MotionCurveData::linear());
        assert_eq!(
            values.curve_provenance.level,
            MotionScopeLevelData::Component
        );
    }

    #[test]
    fn reset_by_field_removal_restores_exact_parent() {
        let mut profile = MotionStyleProfileData::default();
        profile.overrides.values.curve = Some(MotionCurveData::linear());
        let inherited = profile
            .resolve()
            .unwrap()
            .resolve_scope(&MotionScopeData::family(MotionSemanticFamilyData::Focus))
            .unwrap();
        assert_eq!(inherited.curve, MotionCurveData::linear());
        profile.overrides.values.curve = None;
        let reset = profile
            .resolve()
            .unwrap()
            .resolve_scope(&MotionScopeData::family(MotionSemanticFamilyData::Focus))
            .unwrap();
        assert_ne!(reset.curve, MotionCurveData::linear());
        assert!(matches!(
            reset.curve_provenance.origin,
            MotionValueOriginData::BuiltIn { .. }
        ));
    }

    #[test]
    fn specificity_precedes_origin_and_same_level_override_wins() {
        let mut profile = MotionStyleProfileData::default();
        profile.overrides.values.curve = Some(MotionCurveData::linear());
        profile
            .overrides
            .families
            .entry(MotionSemanticFamilyData::Toggle)
            .or_default()
            .values
            .duration_ms = Some(444);
        let resolved = profile.resolve().unwrap();
        let toggle = resolved
            .resolve_scope(&MotionScopeData::family(MotionSemanticFamilyData::Toggle))
            .unwrap();
        assert_ne!(toggle.curve, MotionCurveData::linear());
        assert_eq!(toggle.curve_provenance.level, MotionScopeLevelData::Family);
        assert_eq!(toggle.duration_ms, 444);
        assert!(matches!(
            toggle.duration_provenance.origin,
            MotionValueOriginData::ProfileOverride { .. }
        ));
    }

    #[test]
    fn preset_revisions_are_immutable_and_import_is_atomic() {
        let mut library = MotionPresetLibraryData::default();
        let preset = MotionStylePresetData::built_in(
            BuiltInMotionStyle::Balanced,
            BALANCED_MOTION_STYLE_REVISION,
        )
        .unwrap();
        assert!(library.insert(preset.clone()).unwrap());
        assert!(!library.insert(preset.clone()).unwrap());
        let before = library.clone();
        let mut conflicting = preset;
        conflicting.name = "Different".into();
        assert!(matches!(
            library.insert(conflicting),
            Err(MotionPresetLibraryError::RevisionConflict { .. })
        ));
        assert_eq!(library, before);
        let mut round_trip =
            MotionPresetLibraryData::from_json(&library.to_json().unwrap()).unwrap();
        assert_eq!(round_trip, library);
        assert!(
            !round_trip
                .insert(
                    MotionStylePresetData::built_in(
                        BuiltInMotionStyle::Balanced,
                        BALANCED_MOTION_STYLE_REVISION,
                    )
                    .unwrap()
                )
                .unwrap()
        );
    }

    #[test]
    fn profile_snapshot_freezes_same_scope_overrides_into_embedded_revision() {
        let mut profile = MotionStyleProfileData::default();
        profile
            .overrides
            .families
            .entry(MotionSemanticFamilyData::Toggle)
            .or_default()
            .values
            .duration_ms = Some(375);
        let frozen = profile
            .resolve()
            .unwrap()
            .snapshot_as_preset("my-motion", "My Motion", 3)
            .unwrap();
        let restored = MotionStyleProfileData::from_embedded_preset(
            "active-my-motion",
            "My Motion",
            frozen.clone(),
        )
        .unwrap();
        let values = restored
            .resolve()
            .unwrap()
            .resolve_scope(&MotionScopeData::family(MotionSemanticFamilyData::Toggle))
            .unwrap();
        assert_eq!(values.duration_ms, 375);
        assert_eq!(frozen.id, "my-motion");
        assert_eq!(frozen.revision, 3);
        assert!(matches!(
            values.duration_provenance.origin,
            MotionValueOriginData::EmbeddedPreset { .. }
        ));
    }

    #[test]
    fn unavailable_built_in_revision_fails_without_fallback() {
        let profile = MotionStyleProfileData {
            base: MotionStyleBaseData::BuiltIn {
                style: BuiltInMotionStyle::Lively,
                revision: 1,
            },
            ..MotionStyleProfileData::default()
        };
        assert!(matches!(
            profile.resolve(),
            Err(MotionStyleError::UnavailableBuiltInRevision { .. })
        ));
    }

    #[test]
    fn hostile_legacy_data_returns_an_error_without_panicking() {
        let mut motion = MotionData::default();
        motion.standard[0] = -1.0;
        assert!(matches!(
            MotionStyleProfileData::from_legacy_motion(&motion),
            Err(MotionStyleError::Curve(
                MotionCurveDataError::InvalidLegacyCubic
            ))
        ));
    }
}

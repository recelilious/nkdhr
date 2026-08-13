//! Immutable compiled UI-7 motion-style snapshots.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use nkdhr_theme::{
    MotionComponentNodeData, MotionData, MotionFamilyNodeData, MotionScopeData,
    MotionScopeLevelData, MotionSemanticFamilyData, MotionStyleError, MotionStyleProfileData,
    MotionStyleTreeData, MotionValueOriginData, MotionValueProvenanceData, MotionValuesData,
    ResolvedMotionStyleData,
};

use crate::{CompiledMotionCurve, MotionCurveCompileError, MotionFamily};

#[derive(Debug, Clone)]
pub struct CompiledMotionStyle {
    inner: Arc<CompiledMotionStyleInner>,
}

#[derive(Debug)]
struct CompiledMotionStyleInner {
    resolved: ResolvedMotionStyleData,
    base: CompiledMotionStyleTree,
    overrides: CompiledMotionStyleTree,
}

#[derive(Debug, Clone)]
pub struct ResolvedMotionStyle {
    pub curve: CompiledMotionCurve,
    pub duration: Duration,
    pub curve_provenance: MotionValueProvenanceData,
    pub duration_provenance: MotionValueProvenanceData,
}

#[derive(Debug, Clone, Default)]
struct CompiledMotionValues {
    curve: Option<CompiledMotionCurve>,
    duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct CompiledMotionComponentNode {
    values: CompiledMotionValues,
    transitions: BTreeMap<String, CompiledMotionValues>,
}

#[derive(Debug, Clone, Default)]
struct CompiledMotionFamilyNode {
    values: CompiledMotionValues,
    components: BTreeMap<String, CompiledMotionComponentNode>,
}

#[derive(Debug, Clone, Default)]
struct CompiledMotionStyleTree {
    values: CompiledMotionValues,
    families: BTreeMap<MotionSemanticFamilyData, CompiledMotionFamilyNode>,
}

impl CompiledMotionStyle {
    /// Compile either the authored UI-7 style or an in-memory exact migration
    /// of the legacy four-cubic tokens. Every authored curve is compiled before
    /// publication, including a currently shadowed descendant.
    pub fn from_motion_data(data: &MotionData) -> Result<Self, MotionStyleCompileError> {
        let profile = match &data.style {
            Some(profile) => profile.clone(),
            None => MotionStyleProfileData::from_legacy_motion(data)
                .map_err(MotionStyleCompileError::Data)?,
        };
        Self::compile(profile)
    }

    pub fn compile(profile: MotionStyleProfileData) -> Result<Self, MotionStyleCompileError> {
        let resolved = profile.resolve().map_err(MotionStyleCompileError::Data)?;
        let base = compile_tree(&resolved.preset.style)?;
        let overrides = compile_tree(&resolved.profile.overrides)?;
        Ok(Self {
            inner: Arc::new(CompiledMotionStyleInner {
                resolved,
                base,
                overrides,
            }),
        })
    }

    pub fn profile(&self) -> &MotionStyleProfileData {
        &self.inner.resolved.profile
    }

    pub fn preset(&self) -> &nkdhr_theme::MotionStylePresetData {
        &self.inner.resolved.preset
    }

    pub fn resolve(
        &self,
        scope: &MotionScopeData,
    ) -> Result<ResolvedMotionStyle, MotionStyleCompileError> {
        scope.validate().map_err(MotionStyleCompileError::Data)?;
        let mut curve = None;
        let mut duration_ms = None;
        let mut curve_provenance = None;
        let mut duration_provenance = None;
        let override_origin = MotionValueOriginData::ProfileOverride {
            profile_id: self.inner.resolved.profile.id.clone(),
        };
        for level in [
            MotionScopeLevelData::Profile,
            MotionScopeLevelData::Family,
            MotionScopeLevelData::Component,
            MotionScopeLevelData::Transition,
        ] {
            for (tree, origin) in [
                (&self.inner.base, self.inner.resolved.base_origin()),
                (&self.inner.overrides, &override_origin),
            ] {
                if let Some(values) = compiled_values_at_level(tree, scope, level) {
                    let provenance = provenance(origin, level, scope);
                    if let Some(value) = &values.curve {
                        curve = Some(value.clone());
                        curve_provenance = Some(provenance.clone());
                    }
                    if let Some(value) = values.duration_ms {
                        duration_ms = Some(value);
                        duration_provenance = Some(provenance);
                    }
                }
            }
        }
        Ok(ResolvedMotionStyle {
            curve: curve.ok_or(MotionStyleCompileError::IncompleteSnapshot)?,
            duration: Duration::from_millis(
                duration_ms.ok_or(MotionStyleCompileError::IncompleteSnapshot)?,
            ),
            curve_provenance: curve_provenance
                .ok_or(MotionStyleCompileError::IncompleteSnapshot)?,
            duration_provenance: duration_provenance
                .ok_or(MotionStyleCompileError::IncompleteSnapshot)?,
        })
    }

    pub fn resolve_family(
        &self,
        family: MotionFamily,
    ) -> Result<ResolvedMotionStyle, MotionStyleCompileError> {
        self.resolve(&MotionScopeData::family(family.into()))
    }
}

impl From<MotionFamily> for MotionSemanticFamilyData {
    fn from(value: MotionFamily) -> Self {
        match value {
            MotionFamily::HoverIn => Self::HoverIn,
            MotionFamily::HoverOut => Self::HoverOut,
            MotionFamily::Press => Self::Press,
            MotionFamily::Release => Self::Release,
            MotionFamily::Focus => Self::Focus,
            MotionFamily::Toggle => Self::Toggle,
            MotionFamily::SliderTrail => Self::SliderTrail,
            MotionFamily::ListTransfer => Self::ListTransfer,
            MotionFamily::TextInputFocus => Self::TextInputFocus,
            MotionFamily::Validation => Self::Validation,
            MotionFamily::ScrollbarShow => Self::ScrollbarShow,
            MotionFamily::ScrollbarHide => Self::ScrollbarHide,
            MotionFamily::Overscroll => Self::Overscroll,
            MotionFamily::TooltipEnter => Self::TooltipEnter,
            MotionFamily::TooltipExit => Self::TooltipExit,
            MotionFamily::PopoverEnter => Self::PopoverEnter,
            MotionFamily::PopoverExit => Self::PopoverExit,
            MotionFamily::PanelEnter => Self::PanelEnter,
            MotionFamily::PanelExit => Self::PanelExit,
            MotionFamily::DrawerEnter => Self::DrawerEnter,
            MotionFamily::DrawerExit => Self::DrawerExit,
            MotionFamily::Workspace => Self::Workspace,
            MotionFamily::Wallpaper => Self::Wallpaper,
        }
    }
}

fn compile_tree(
    tree: &MotionStyleTreeData,
) -> Result<CompiledMotionStyleTree, MotionStyleCompileError> {
    let mut families = BTreeMap::new();
    for (family_id, family) in &tree.families {
        families.insert(*family_id, compile_family(family)?);
    }
    Ok(CompiledMotionStyleTree {
        values: compile_values(&tree.values)?,
        families,
    })
}

fn compile_family(
    family: &MotionFamilyNodeData,
) -> Result<CompiledMotionFamilyNode, MotionStyleCompileError> {
    let mut components = BTreeMap::new();
    for (component_id, component) in &family.components {
        components.insert(component_id.clone(), compile_component(component)?);
    }
    Ok(CompiledMotionFamilyNode {
        values: compile_values(&family.values)?,
        components,
    })
}

fn compile_component(
    component: &MotionComponentNodeData,
) -> Result<CompiledMotionComponentNode, MotionStyleCompileError> {
    let mut transitions = BTreeMap::new();
    for (transition_id, values) in &component.transitions {
        transitions.insert(transition_id.clone(), compile_values(values)?);
    }
    Ok(CompiledMotionComponentNode {
        values: compile_values(&component.values)?,
        transitions,
    })
}

fn compile_values(
    values: &MotionValuesData,
) -> Result<CompiledMotionValues, MotionStyleCompileError> {
    Ok(CompiledMotionValues {
        curve: values
            .curve
            .as_ref()
            .map(CompiledMotionCurve::compile)
            .transpose()
            .map_err(MotionStyleCompileError::Curve)?,
        duration_ms: values.duration_ms,
    })
}

fn compiled_values_at_level<'a>(
    tree: &'a CompiledMotionStyleTree,
    scope: &MotionScopeData,
    level: MotionScopeLevelData,
) -> Option<&'a CompiledMotionValues> {
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

#[derive(Debug)]
pub enum MotionStyleCompileError {
    Data(MotionStyleError),
    Curve(MotionCurveCompileError),
    IncompleteSnapshot,
}

impl fmt::Display for MotionStyleCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(error) => error.fmt(formatter),
            Self::Curve(error) => error.fmt(formatter),
            Self::IncompleteSnapshot => formatter.write_str("compiled motion style is incomplete"),
        }
    }
}

impl std::error::Error for MotionStyleCompileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use nkdhr_theme::{MotionCurveData, MotionFamilyNodeData, MotionValuesData};

    const FAMILIES: [MotionFamily; 23] = [
        MotionFamily::HoverIn,
        MotionFamily::HoverOut,
        MotionFamily::Press,
        MotionFamily::Release,
        MotionFamily::Focus,
        MotionFamily::Toggle,
        MotionFamily::SliderTrail,
        MotionFamily::ListTransfer,
        MotionFamily::TextInputFocus,
        MotionFamily::Validation,
        MotionFamily::ScrollbarShow,
        MotionFamily::ScrollbarHide,
        MotionFamily::Overscroll,
        MotionFamily::TooltipEnter,
        MotionFamily::TooltipExit,
        MotionFamily::PopoverEnter,
        MotionFamily::PopoverExit,
        MotionFamily::PanelEnter,
        MotionFamily::PanelExit,
        MotionFamily::DrawerEnter,
        MotionFamily::DrawerExit,
        MotionFamily::Workspace,
        MotionFamily::Wallpaper,
    ];

    #[test]
    fn in_memory_migration_preserves_all_legacy_family_output() {
        let data = MotionData::default();
        let compiled = CompiledMotionStyle::from_motion_data(&data).unwrap();
        let legacy = crate::Theme::from_data(&nkdhr_theme::ThemeData::default())
            .unwrap()
            .motion;
        for family in FAMILIES {
            let current = compiled.resolve_family(family).unwrap();
            let old = legacy.spec(family);
            assert_eq!(current.duration, old.duration);
            for step in 0..=1000 {
                let time = step as f64 / 1000.0;
                assert!(
                    (current.curve.sample(time) - f64::from(old.curve.sample(time as f32))).abs()
                        < 2.0e-5
                );
            }
        }
    }

    #[test]
    fn shadowed_invalid_curve_rejects_the_complete_snapshot() {
        let mut profile = MotionStyleProfileData::default();
        let family = profile
            .overrides
            .families
            .entry(MotionSemanticFamilyData::Focus)
            .or_insert_with(MotionFamilyNodeData::default);
        let mut invalid = MotionCurveData::linear();
        invalid.anchors[0].tangents = nkdhr_theme::MotionTangentsData::Broken {
            incoming: nkdhr_theme::MotionVectorData::ZERO,
            outgoing: nkdhr_theme::MotionVectorData::new(0.8, 0.2),
        };
        invalid.anchors[1].tangents = nkdhr_theme::MotionTangentsData::Broken {
            incoming: nkdhr_theme::MotionVectorData::new(-0.8, -0.2),
            outgoing: nkdhr_theme::MotionVectorData::ZERO,
        };
        family.values = MotionValuesData {
            curve: Some(invalid),
            duration_ms: None,
        };
        assert!(matches!(
            CompiledMotionStyle::compile(profile),
            Err(MotionStyleCompileError::Curve(
                MotionCurveCompileError::NonMonotonicHandles { .. }
            ))
        ));
    }
}

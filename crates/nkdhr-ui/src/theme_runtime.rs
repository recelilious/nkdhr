//! Atomic immutable theme snapshots and typed semantic token reads.

use std::collections::BTreeSet;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::thread;

use nkdhr_ipc::ConfigProxyBlocking;
use nkdhr_render::Color;
use nkdhr_theme::{
    ExtensionValue, ResolvedTheme, ThemeExtensionRegistry, ThemeProfile, ThemeProfileError,
    ThemeTokenChange, TokenImpact, diff_resolved,
};
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use crate::{
    CompiledMotionStyle, Density, Invalidation, MotionMode, MotionRuntimeError,
    MotionRuntimeProfile, MotionStyleCompileError, Theme, ThemeError,
};

#[derive(Debug, Clone)]
pub struct ThemeSnapshot {
    generation: u64,
    resolved: Arc<ResolvedTheme>,
    theme: Arc<Theme>,
    motion_style: Arc<CompiledMotionStyle>,
    motion_runtime: Arc<MotionRuntimeProfile>,
}

impl ThemeSnapshot {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn resolved(&self) -> &ResolvedTheme {
        &self.resolved
    }

    pub fn theme(&self) -> Arc<Theme> {
        Arc::clone(&self.theme)
    }

    /// UI-7's independently compiled authoring/introspection hierarchy.
    /// Existing widgets retain their accepted path; each future UI-7 visual
    /// adoption must execute through `motion_runtime` so policy remains final.
    pub fn motion_style(&self) -> Arc<CompiledMotionStyle> {
        Arc::clone(&self.motion_style)
    }

    /// UI-7C's final policy-governed execution snapshot. It is published in
    /// the same atomic generation as the portable profile and compiled style.
    pub fn motion_runtime(&self) -> Arc<MotionRuntimeProfile> {
        Arc::clone(&self.motion_runtime)
    }

    pub fn changes_from(&self, previous: &Self) -> Vec<ThemeTokenChange> {
        diff_resolved(&previous.resolved, &self.resolved)
    }

    pub fn read<T: Clone>(&self, token: ThemeToken<T>, reads: &mut ThemeReadSet) -> T {
        reads.record(token.path);
        (token.resolve)(&self.theme)
    }

    pub fn read_extension(
        &self,
        group: &str,
        token: &str,
        reads: &mut ThemeReadSet,
    ) -> Option<ExtensionValue> {
        reads.record(format!("{group}.{token}"));
        self.resolved.extension(group, token).cloned()
    }
}

#[derive(Debug)]
struct RuntimeState {
    snapshot: Arc<ThemeSnapshot>,
}

/// Thread-safe publication point. Candidate parsing, inheritance and
/// validation happen before the mutex is acquired; the visible generation is
/// swapped in one critical section, so readers can never observe a partial
/// palette or half-applied metric set.
#[derive(Clone)]
pub struct ThemeRuntime {
    state: Arc<Mutex<RuntimeState>>,
    extensions: Arc<ThemeExtensionRegistry>,
}

impl fmt::Debug for ThemeRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ThemeRuntime")
            .field("generation", &self.snapshot().generation())
            .field("extension_groups", &self.extensions.group_count())
            .finish()
    }
}

impl Default for ThemeRuntime {
    fn default() -> Self {
        Self::new(ThemeProfile::default()).expect("the built-in profile is valid")
    }
}

impl ThemeRuntime {
    /// Read `theme.profile` once and keep following CTRL-5 `Changed` signals.
    /// A missing daemon degrades to the immutable built-in profile; a malformed
    /// value is ignored so the runtime retains its last-known-good snapshot.
    pub fn watch_ctrl5() -> Self {
        Self::watch_ctrl5_with_extensions(Arc::new(ThemeExtensionRegistry::default()))
    }

    pub fn watch_ctrl5_with_extensions(extensions: Arc<ThemeExtensionRegistry>) -> Self {
        let runtime = Self::new_with_extensions(ThemeProfile::default(), extensions)
            .expect("the built-in profile is valid");
        let Ok(connection) = Connection::session() else {
            eprintln!("nkdhr-ui: no session D-Bus, using the built-in theme");
            return runtime;
        };
        if let Some(profile) = fetch_ctrl5_profile(&connection)
            && let Err(error) = runtime.publish_json(&profile)
        {
            eprintln!("nkdhr-ui: rejected initial CTRL-5 theme profile: {error}");
        }

        let watched = runtime.clone();
        thread::spawn(move || {
            let Ok(config) = ConfigProxyBlocking::new(&connection) else {
                return;
            };
            let Ok(changed) = config.receive_changed() else {
                return;
            };
            for signal in changed {
                let Ok(args) = signal.args() else {
                    continue;
                };
                if args.key() != "theme.profile" {
                    continue;
                }
                let Some(profile) = fetch_ctrl5_profile(&connection) else {
                    continue;
                };
                if let Err(error) = watched.publish_json(&profile) {
                    eprintln!("nkdhr-ui: rejected changed CTRL-5 theme profile: {error}");
                }
            }
        });
        runtime
    }

    pub fn new(profile: ThemeProfile) -> Result<Self, ThemeRuntimeError> {
        Self::new_with_extensions(profile, Arc::new(ThemeExtensionRegistry::default()))
    }

    pub fn new_with_extensions(
        profile: ThemeProfile,
        extensions: Arc<ThemeExtensionRegistry>,
    ) -> Result<Self, ThemeRuntimeError> {
        let resolved = profile.resolve_with_extensions(&extensions)?;
        let theme = Theme::from_data(&resolved.data)?;
        let motion_style = CompiledMotionStyle::from_motion_data(&resolved.data.motion)?;
        let motion_runtime = MotionRuntimeProfile::from_motion_data(
            &resolved.data.motion,
            theme.motion.fluid,
            motion_style.clone(),
        )?;
        Ok(Self {
            state: Arc::new(Mutex::new(RuntimeState {
                snapshot: Arc::new(ThemeSnapshot {
                    generation: 1,
                    resolved: Arc::new(resolved),
                    theme: Arc::new(theme),
                    motion_style: Arc::new(motion_style),
                    motion_runtime: Arc::new(motion_runtime),
                }),
            })),
            extensions,
        })
    }

    pub fn snapshot(&self) -> Arc<ThemeSnapshot> {
        Arc::clone(&self.state.lock().expect("theme runtime poisoned").snapshot)
    }

    pub fn publish_json(&self, text: &str) -> Result<ThemePublication, ThemeRuntimeError> {
        self.publish(ThemeProfile::from_json(text)?)
    }

    pub fn publish(&self, profile: ThemeProfile) -> Result<ThemePublication, ThemeRuntimeError> {
        let resolved = profile.resolve_with_extensions(&self.extensions)?;
        let theme = Theme::from_data(&resolved.data)?;
        let motion_style = CompiledMotionStyle::from_motion_data(&resolved.data.motion)?;
        let motion_runtime = MotionRuntimeProfile::from_motion_data(
            &resolved.data.motion,
            theme.motion.fluid,
            motion_style.clone(),
        )?;
        let mut state = self.state.lock().expect("theme runtime poisoned");
        let previous = Arc::clone(&state.snapshot);
        if previous.resolved.as_ref() == &resolved {
            return Ok(ThemePublication {
                previous_generation: previous.generation,
                snapshot: previous,
                changes: Arc::new(Vec::new()),
                published: false,
            });
        }
        let changes = diff_resolved(&previous.resolved, &resolved);
        let snapshot = Arc::new(ThemeSnapshot {
            generation: previous.generation.wrapping_add(1).max(1),
            resolved: Arc::new(resolved),
            theme: Arc::new(theme),
            motion_style: Arc::new(motion_style),
            motion_runtime: Arc::new(motion_runtime),
        });
        state.snapshot = Arc::clone(&snapshot);
        Ok(ThemePublication {
            previous_generation: previous.generation,
            snapshot,
            changes: Arc::new(changes),
            published: true,
        })
    }
}

fn fetch_ctrl5_profile(connection: &Connection) -> Option<String> {
    let config = ConfigProxyBlocking::new(connection).ok()?;
    let owned = config.get("theme.profile").ok()?;
    match Value::from(owned) {
        Value::Str(profile) => Some(profile.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct ThemePublication {
    previous_generation: u64,
    snapshot: Arc<ThemeSnapshot>,
    changes: Arc<Vec<ThemeTokenChange>>,
    published: bool,
}

impl ThemePublication {
    pub fn previous_generation(&self) -> u64 {
        self.previous_generation
    }

    pub fn snapshot(&self) -> Arc<ThemeSnapshot> {
        Arc::clone(&self.snapshot)
    }

    pub fn changes(&self) -> &[ThemeTokenChange] {
        &self.changes
    }

    pub fn was_published(&self) -> bool {
        self.published
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeReadSet {
    paths: BTreeSet<String>,
}

impl ThemeReadSet {
    pub fn from_paths<S: Into<String>>(paths: impl IntoIterator<Item = S>) -> Self {
        let mut reads = Self::default();
        reads.extend(paths);
        reads
    }

    pub fn record(&mut self, path: impl Into<String>) {
        self.paths.insert(path.into());
    }

    pub fn extend<S: Into<String>>(&mut self, paths: impl IntoIterator<Item = S>) {
        self.paths.extend(paths.into_iter().map(Into::into));
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    pub fn invalidation_for(&self, changes: &[ThemeTokenChange]) -> Invalidation {
        let mut invalidation = Invalidation::NONE;
        for change in changes.iter().filter(|change| self.contains(&change.path)) {
            invalidation |= match change.impact {
                TokenImpact::Paint => Invalidation::PAINT,
                TokenImpact::Layout => Invalidation::LAYOUT,
            };
        }
        invalidation
    }
}

#[derive(Clone, Copy)]
pub struct ThemeToken<T> {
    path: &'static str,
    resolve: fn(&Theme) -> T,
    marker: PhantomData<fn() -> T>,
}

impl<T> ThemeToken<T> {
    pub const fn new(path: &'static str, resolve: fn(&Theme) -> T) -> Self {
        Self {
            path,
            resolve,
            marker: PhantomData,
        }
    }

    pub const fn path(self) -> &'static str {
        self.path
    }
}

pub mod tokens {
    use super::*;

    pub const PALETTE_SURFACE: ThemeToken<Color> =
        ThemeToken::new("palette.surface", palette_surface);
    pub const PALETTE_ACCENT: ThemeToken<Color> = ThemeToken::new("palette.accent", palette_accent);
    pub const SPACING_MEDIUM: ThemeToken<f32> = ThemeToken::new("spacing.medium", spacing_medium);
    pub const RADII_CONTROL: ThemeToken<f32> = ThemeToken::new("radii.control", radii_control);
    pub const TYPOGRAPHY_SCALE: ThemeToken<f32> =
        ThemeToken::new("typography.scale", typography_scale);
    pub const DENSITY: ThemeToken<Density> = ThemeToken::new("density", density);
    pub const MOTION_MODE: ThemeToken<MotionMode> = ThemeToken::new("motion.mode", motion_mode);
    pub const CONTENT_SURFACE_OPACITY: ThemeToken<f32> =
        ThemeToken::new("materials.content_surface.opacity", content_surface_opacity);

    fn palette_surface(theme: &Theme) -> Color {
        theme.palette.surface
    }
    fn palette_accent(theme: &Theme) -> Color {
        theme.palette.accent
    }
    fn spacing_medium(theme: &Theme) -> f32 {
        theme.spacing.medium
    }
    fn radii_control(theme: &Theme) -> f32 {
        theme.radii.control
    }
    fn typography_scale(theme: &Theme) -> f32 {
        theme.typography.scale
    }
    fn density(theme: &Theme) -> Density {
        theme.density
    }
    fn motion_mode(theme: &Theme) -> MotionMode {
        theme.motion.mode
    }
    fn content_surface_opacity(theme: &Theme) -> f32 {
        theme.content_surface.opacity
    }
}

#[derive(Debug)]
pub enum ThemeRuntimeError {
    Profile(ThemeProfileError),
    Runtime(ThemeError),
    MotionStyle(MotionStyleCompileError),
    MotionRuntime(MotionRuntimeError),
}

impl fmt::Display for ThemeRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::MotionStyle(error) => error.fmt(formatter),
            Self::MotionRuntime(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ThemeRuntimeError {}

impl From<ThemeProfileError> for ThemeRuntimeError {
    fn from(value: ThemeProfileError) -> Self {
        Self::Profile(value)
    }
}

impl From<ThemeError> for ThemeRuntimeError {
    fn from(value: ThemeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<MotionStyleCompileError> for ThemeRuntimeError {
    fn from(value: MotionStyleCompileError) -> Self {
        Self::MotionStyle(value)
    }
}

impl From<MotionRuntimeError> for ThemeRuntimeError {
    fn from(value: MotionRuntimeError) -> Self {
        Self::MotionRuntime(value)
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use nkdhr_render::{DisplayListBuilder, Primitive, Rect};
    use nkdhr_theme::{
        ExtensionTokenDescriptor, ExtensionTokenGroup, ExtensionTokenType, ExtensionValue,
        MotionCurveData, MotionFamilyNodeData, MotionSemanticFamilyData, MotionStyleProfileData,
        MotionTangentsData, MotionValueOriginData, MotionValuesData, MotionVectorData,
        ThemeExtensionRegistry, ThemeProfile,
    };
    use serde_json::json;

    use super::*;
    use crate::{
        Constraints, Element, GlassSurface, MaterialTier, MeasureCtx, PaintCtx, Size, UiError,
        UiRoot, Widget,
    };

    struct ThemeProbe {
        theme: Arc<Theme>,
        measures: Rc<Cell<u32>>,
    }

    struct ExtensionProbe {
        token: &'static str,
        value: Rc<Cell<i64>>,
        measures: Rc<Cell<u32>>,
        paints: Rc<Cell<u32>>,
    }

    impl Widget for ExtensionProbe {
        fn theme_reads(&self) -> ThemeReadSet {
            if self.token == "tint" {
                ThemeReadSet::from_paths([
                    "extension.com.example.widget.extent",
                    "extension.com.example.widget.tint",
                ])
            } else {
                ThemeReadSet::from_paths(["extension.com.example.widget.extent"])
            }
        }

        fn apply_theme_snapshot(&mut self, snapshot: Arc<ThemeSnapshot>) {
            let mut reads = ThemeReadSet::default();
            let Some(ExtensionValue::Integer(value)) =
                snapshot.read_extension("extension.com.example.widget", self.token, &mut reads)
            else {
                panic!("registered test extension token must resolve")
            };
            self.value.set(value);
        }

        fn measure(
            &self,
            _ctx: &mut MeasureCtx<'_>,
            constraints: Constraints,
        ) -> Result<Size, UiError> {
            self.measures.set(self.measures.get() + 1);
            Ok(constraints.constrain(Size::new(10.0, 10.0)))
        }

        fn paint(&self, _ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
            self.paints.set(self.paints.get() + 1);
            Ok(())
        }
    }

    fn extension_registry() -> Arc<ThemeExtensionRegistry> {
        let mut registry = ThemeExtensionRegistry::default();
        registry
            .register(ExtensionTokenGroup::new(
                "extension.com.example.widget",
                [
                    ExtensionTokenDescriptor::new(
                        "extent",
                        ExtensionTokenType::Integer { min: 0, max: 64 },
                        ExtensionValue::Integer(8),
                        TokenImpact::Layout,
                    ),
                    ExtensionTokenDescriptor::new(
                        "tint",
                        ExtensionTokenType::Integer { min: 0, max: 255 },
                        ExtensionValue::Integer(1),
                        TokenImpact::Paint,
                    ),
                ],
            ))
            .unwrap();
        Arc::new(registry)
    }

    impl Widget for ThemeProbe {
        fn create_state(&self) -> Box<dyn Any> {
            Box::new(())
        }

        fn theme_reads(&self) -> ThemeReadSet {
            ThemeReadSet::from_paths(["palette.accent", "spacing.medium"])
        }

        fn apply_theme(&mut self, theme: Arc<Theme>) {
            self.theme = theme;
        }

        fn measure(
            &self,
            _ctx: &mut MeasureCtx<'_>,
            constraints: Constraints,
        ) -> Result<Size, UiError> {
            self.measures.set(self.measures.get() + 1);
            Ok(constraints.constrain(Size::new(
                self.theme.spacing.medium,
                self.theme.spacing.medium,
            )))
        }

        fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
            let rect = ctx.rect();
            ctx.builder().rect(rect, self.theme.palette.accent)?;
            Ok(())
        }
    }

    #[test]
    fn publication_is_atomic_and_rejection_preserves_last_good() {
        let runtime = ThemeRuntime::default();
        let initial = runtime.snapshot();
        let next = ThemeProfile {
            overrides: json!({"palette": {"accent": "#010203ff"}}),
            ..ThemeProfile::default()
        };
        let publication = runtime.publish(next).unwrap();
        assert!(publication.was_published());
        assert_eq!(publication.previous_generation(), initial.generation());
        assert_eq!(publication.changes().len(), 1);
        let accepted = runtime.snapshot();

        let invalid = ThemeProfile {
            overrides: json!({"spacing": {"small": 100.0}}),
            ..ThemeProfile::default()
        };
        assert!(runtime.publish(invalid).is_err());
        assert!(Arc::ptr_eq(&accepted, &runtime.snapshot()));
    }

    #[test]
    fn compiled_motion_style_is_atomic_and_rejection_preserves_last_good() {
        let runtime = ThemeRuntime::default();
        let accepted = runtime.snapshot();
        let legacy_toggle = accepted
            .motion_style()
            .resolve_family(crate::MotionFamily::Toggle)
            .unwrap();
        assert_eq!(legacy_toggle.duration, Duration::from_millis(220));
        assert!(matches!(
            legacy_toggle.curve_provenance.origin,
            MotionValueOriginData::EmbeddedPreset { .. }
        ));

        let mut style = MotionStyleProfileData::default();
        let mut invalid = MotionCurveData::linear();
        invalid.anchors[0].tangents = MotionTangentsData::Broken {
            incoming: MotionVectorData::ZERO,
            outgoing: MotionVectorData::new(0.8, 0.2),
        };
        invalid.anchors[1].tangents = MotionTangentsData::Broken {
            incoming: MotionVectorData::new(-0.8, -0.2),
            outgoing: MotionVectorData::ZERO,
        };
        style.overrides.families.insert(
            MotionSemanticFamilyData::Focus,
            MotionFamilyNodeData {
                values: MotionValuesData {
                    curve: Some(invalid),
                    duration_ms: None,
                    fluid: Default::default(),
                },
                components: Default::default(),
            },
        );
        let invalid = ThemeProfile {
            overrides: json!({"motion": {"style": style}}),
            ..ThemeProfile::default()
        };
        assert!(matches!(
            runtime.publish(invalid),
            Err(ThemeRuntimeError::MotionStyle(_))
        ));
        assert!(Arc::ptr_eq(&accepted, &runtime.snapshot()));
    }

    #[test]
    fn policy_runtime_is_published_in_the_same_theme_generation() {
        let runtime = ThemeRuntime::default();
        let scope = nkdhr_theme::MotionScopeData::family(MotionSemanticFamilyData::Toggle);
        let initial = runtime
            .snapshot()
            .motion_runtime()
            .resolve(&scope, crate::MotionPropertyDomain::Spatial)
            .unwrap();
        assert_eq!(initial.duration(), Duration::from_millis(220));
        assert_eq!(initial.source(), crate::MotionPolicySource::AuthoredStyle);

        let reduced = ThemeProfile {
            overrides: json!({"motion": {"mode": "reduced"}}),
            ..ThemeProfile::default()
        };
        let publication = runtime.publish(reduced).unwrap();
        let snapshot = publication.snapshot();
        assert_eq!(
            snapshot.generation(),
            publication.previous_generation().wrapping_add(1)
        );
        let execution = snapshot
            .motion_runtime()
            .resolve(&scope, crate::MotionPropertyDomain::Spatial)
            .unwrap();
        assert!(execution.is_immediate());
        assert_eq!(execution.source(), crate::MotionPolicySource::ReducedPolicy);
    }

    #[test]
    fn typed_reads_only_invalidate_for_tokens_actually_read() {
        let runtime = ThemeRuntime::default();
        let initial = runtime.snapshot();
        let mut reads = ThemeReadSet::default();
        initial.read(tokens::PALETTE_ACCENT, &mut reads);

        let profile = ThemeProfile {
            overrides: json!({"palette": {"surface": "#010203ff"}}),
            ..ThemeProfile::default()
        };
        let unrelated = runtime.publish(profile).unwrap();
        assert!(reads.invalidation_for(unrelated.changes()).is_empty());

        let profile = ThemeProfile {
            overrides: json!({"palette": {"accent": "#040506ff"}}),
            ..ThemeProfile::default()
        };
        let related = runtime.publish(profile).unwrap();
        assert_eq!(
            reads.invalidation_for(related.changes()),
            Invalidation::PAINT
        );
    }

    #[test]
    fn layout_and_paint_diffs_stay_distinct() {
        let runtime = ThemeRuntime::default();
        let snapshot = runtime.snapshot();
        let mut reads = ThemeReadSet::default();
        snapshot.read(tokens::PALETTE_ACCENT, &mut reads);
        snapshot.read(tokens::SPACING_MEDIUM, &mut reads);
        let profile = ThemeProfile {
            overrides: json!({"palette": {"accent": "#010203ff"}, "spacing": {"medium": 18.0}}),
            ..ThemeProfile::default()
        };
        let publication = runtime.publish(profile).unwrap();
        let invalidation = reads.invalidation_for(publication.changes());
        assert!(invalidation.contains(Invalidation::LAYOUT));
        assert!(invalidation.contains(Invalidation::PAINT));
    }

    #[test]
    fn ui_root_hot_swaps_at_boundaries_without_reconcile_or_restart() {
        let runtime = ThemeRuntime::default();
        let measures = Rc::new(Cell::new(0));
        let probe = ThemeProbe {
            theme: Arc::new(Theme::default()),
            measures: Rc::clone(&measures),
        };
        let mut root = UiRoot::new(Element::new(probe)).unwrap();
        root.set_theme_runtime(runtime.clone());
        root.layout(Size::new(80.0, 40.0)).unwrap();
        let mut builder = DisplayListBuilder::new();
        root.paint(&mut builder).unwrap();
        let measured_before_color = measures.get();

        let color = ThemeProfile {
            overrides: json!({"palette": {"accent": "#010203ff"}}),
            ..ThemeProfile::default()
        };
        runtime.publish(color).unwrap();
        let invalidation = root.invalidation();
        assert!(invalidation.contains(Invalidation::PAINT));
        assert!(!invalidation.contains(Invalidation::LAYOUT));
        let mut builder = DisplayListBuilder::new();
        root.paint(&mut builder).unwrap();
        assert_eq!(measures.get(), measured_before_color);
        let list = builder.finish();
        let Primitive::Shape(rect) = &list.primitives()[0] else {
            panic!("probe must paint one rectangle")
        };
        assert_eq!(rect.rect, Rect::new(0.0, 0.0, 80.0, 40.0));
        assert_eq!(
            rect.style,
            nkdhr_render::ShapeStyle::Fill(Color::from_srgba8(1, 2, 3, 255))
        );

        let layout = ThemeProfile {
            overrides: json!({"spacing": {"medium": 18.0}}),
            ..ThemeProfile::default()
        };
        runtime.publish(layout).unwrap();
        assert!(root.invalidation().contains(Invalidation::LAYOUT));
        root.layout(Size::new(80.0, 40.0)).unwrap();
        assert_eq!(measures.get(), measured_before_color + 1);
    }

    #[test]
    fn standard_glass_surface_consumes_the_live_snapshot() {
        let runtime = ThemeRuntime::default();
        let surface = GlassSurface::new(Arc::new(Theme::default()), MaterialTier::ContentSurface);
        let mut root = UiRoot::new(Element::new(surface)).unwrap();
        root.set_theme_runtime(runtime.clone());
        root.layout(Size::new(80.0, 40.0)).unwrap();
        let mut builder = DisplayListBuilder::new();
        root.paint(&mut builder).unwrap();

        let profile = ThemeProfile {
            overrides: json!({"materials": {"content_surface": {"opacity": 0.50}}}),
            ..ThemeProfile::default()
        };
        runtime.publish(profile).unwrap();
        let invalidation = root.invalidation();
        assert!(invalidation.contains(Invalidation::PAINT));
        assert!(!invalidation.contains(Invalidation::LAYOUT));
        let mut builder = DisplayListBuilder::new();
        root.paint(&mut builder).unwrap();
        let has_compensated_fill = builder.finish().primitives().iter().any(|primitive| {
            matches!(
                primitive,
                Primitive::Shape(shape)
                    if matches!(shape.style, nkdhr_render::ShapeStyle::Fill(color) if (color.components()[3] - 0.61).abs() < 0.0001)
            )
        });
        assert!(has_compensated_fill);
    }

    #[test]
    fn independent_roots_sync_extension_generations_only_at_local_boundaries() {
        let runtime =
            ThemeRuntime::new_with_extensions(ThemeProfile::default(), extension_registry())
                .unwrap();
        let first_value = Rc::new(Cell::new(0));
        let first_measures = Rc::new(Cell::new(0));
        let first_paints = Rc::new(Cell::new(0));
        let second_value = Rc::new(Cell::new(0));
        let second_measures = Rc::new(Cell::new(0));
        let second_paints = Rc::new(Cell::new(0));
        let mut first = UiRoot::new(Element::new(ExtensionProbe {
            token: "extent",
            value: Rc::clone(&first_value),
            measures: Rc::clone(&first_measures),
            paints: Rc::clone(&first_paints),
        }))
        .unwrap();
        let mut second = UiRoot::new(Element::new(ExtensionProbe {
            token: "tint",
            value: Rc::clone(&second_value),
            measures: Rc::clone(&second_measures),
            paints: Rc::clone(&second_paints),
        }))
        .unwrap();
        for root in [&mut first, &mut second] {
            root.set_theme_runtime(runtime.clone());
            root.layout(Size::new(40.0, 40.0)).unwrap();
            root.paint(&mut DisplayListBuilder::new()).unwrap();
        }
        assert_eq!(first_value.get(), 8);
        assert_eq!(second_value.get(), 1);
        let second_measures_before_skip = second_measures.get();

        runtime
            .publish(ThemeProfile {
                overrides: json!({
                    "extension": {"com.example.widget": {"extent": 12}}
                }),
                ..ThemeProfile::default()
            })
            .unwrap();
        assert!(first.invalidation().contains(Invalidation::LAYOUT));
        assert_eq!(first.theme_snapshot().unwrap().generation(), 2);
        assert_eq!(first_value.get(), 12);
        assert_eq!(second.theme_snapshot().unwrap().generation(), 1);
        assert_eq!(second_value.get(), 1);
        first.layout(Size::new(40.0, 40.0)).unwrap();
        first.paint(&mut DisplayListBuilder::new()).unwrap();

        runtime
            .publish(ThemeProfile {
                overrides: json!({
                    "extension": {"com.example.widget": {"tint": 2}}
                }),
                ..ThemeProfile::default()
            })
            .unwrap();

        let second_invalidation = second.invalidation();
        assert!(second_invalidation.contains(Invalidation::PAINT));
        assert!(!second_invalidation.contains(Invalidation::LAYOUT));
        assert_eq!(second.theme_snapshot().unwrap().generation(), 3);
        assert_eq!(second_value.get(), 2);
        second.paint(&mut DisplayListBuilder::new()).unwrap();
        assert_eq!(second_measures.get(), second_measures_before_skip);
        assert_eq!(first.theme_snapshot().unwrap().generation(), 2);
        assert_eq!(first_value.get(), 12);

        assert!(first.invalidation().contains(Invalidation::LAYOUT));
        assert_eq!(first.theme_snapshot().unwrap().generation(), 3);
        assert_eq!(first_value.get(), 8);
    }

    #[test]
    fn invalid_extension_publication_preserves_the_last_good_snapshot() {
        let runtime =
            ThemeRuntime::new_with_extensions(ThemeProfile::default(), extension_registry())
                .unwrap();
        runtime
            .publish(ThemeProfile {
                overrides: json!({
                    "extension": {"com.example.widget": {"extent": 12}}
                }),
                ..ThemeProfile::default()
            })
            .unwrap();
        let accepted = runtime.snapshot();
        let invalid = ThemeProfile {
            overrides: json!({
                "extension": {"com.example.widget": {"extent": 1000}}
            }),
            ..ThemeProfile::default()
        };
        assert!(runtime.publish(invalid).is_err());
        assert!(Arc::ptr_eq(&accepted, &runtime.snapshot()));
    }
}

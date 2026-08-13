//! UI-7E owner-reviewed professional motion workspace composition and local
//! authoring session. The session outlives retained-tree reconciliation, so
//! editing state survives resize, theme publication and responsive relayout.

use std::{
    cell::{Cell, RefCell},
    fmt,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use nkdhr_render::{Color, CornerRadii, Point, Rect, Shadow, Transform};
use nkdhr_ui::text::FontSlant;
use nkdhr_ui::{
    Align, Alignment, AnimationCtx, ArrangeCtx, Axis, Button, ButtonVariant, CompiledMotionCurve,
    Constraints, CrossAxisAlignment, CubicBezier, Element, EventCtx, Flex, Insets, Invalidation,
    Key, MainAxisAlignment, MaterialCapabilities, MaterialTier, MeasureCtx, Modifiers,
    MotionCurveConsumer, MotionCurveConsumerDomain, MotionCurveConsumerSet, MotionCurveEditor,
    MotionCurveEditorConfig, MotionCurveEditorSnapshot, MotionEditorClipboardAction,
    MotionEditorDevice, MotionEditorDirectInput, MotionEditorEditId, MotionEditorGesturePhase,
    MotionEditorInput, MotionEditorInputController, MotionEditorInputError,
    MotionEditorInputOutcome, MotionEditorKey, MotionEditorModifiers, MotionEditorPlayback,
    MotionEditorTarget, MotionEditorViewportInput, MotionFluidOverridesData, MotionGraphPoint,
    MotionGraphViewport, Padding, PaintCtx, PointerButton, Reactive, ScrollPhase, SemanticRole,
    Semantics, SemanticsCtx, Size, Slider, SliderError, SurfaceState, Text, TextInput,
    TextInputStatus, TextRole, Theme, ThemeReadSet, Toggle, UiError, UiEvent, Widget,
    paint_fluid_well, resolve_fluid_material_tones, resolve_motion_curve_handles,
};

const SCOPE_RAIL_HEIGHT: f32 = 88.0;
const PREVIEW_HEIGHT: f32 = 176.0;
const PREVIEW_RAIL_HEIGHT: f32 = 48.0;
const GRAPH_TOOLBAR_HEIGHT: f32 = 40.0;
const GRAPH_AXIS_HEIGHT: f32 = 28.0;
const GRAPH_CONTENT_INSET: f32 = 18.0;
const NAVIGATION_VERTICAL_INSET: f32 = 8.0;
const MAX_EDITABLE_DURATION_MS: u64 = 60_000;
const FLUID_PERCENT_PER_SEMANTIC_UNIT: f64 = 25.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct FluidMaterialValues {
    viscosity: f32,
    surface_tension: f32,
    attraction: f32,
}

impl Default for FluidMaterialValues {
    fn default() -> Self {
        Self {
            viscosity: 68.0,
            surface_tension: 72.0,
            attraction: 56.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FluidMaterialField {
    Viscosity,
    SurfaceTension,
    Attraction,
}

#[derive(Clone)]
pub(crate) struct MotionEditorSession {
    inner: Rc<MotionEditorSessionInner>,
}

struct MotionEditorSessionInner {
    editor: RefCell<MotionCurveEditor>,
    inherited_compiled: CompiledMotionCurve,
    input: RefCell<MotionEditorInputController>,
    next_edit_id: Cell<u64>,
    duration_text: Reactive<String>,
    duration_status: Reactive<TextInputStatus>,
    fluid_overrides: Cell<MotionFluidOverridesData>,
    composition_revision: Reactive<u64>,
    visual_revision: Reactive<u64>,
}

impl fmt::Debug for MotionEditorSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = self.snapshot();
        formatter
            .debug_struct("MotionEditorSession")
            .field("document_generation", &snapshot.document_generation)
            .field("playhead", &snapshot.playhead)
            .field("playback", &snapshot.playback)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct MotionEditorRenderState {
    snapshot: MotionCurveEditorSnapshot,
    compiled: CompiledMotionCurve,
    inherited_compiled: CompiledMotionCurve,
}

impl MotionEditorSession {
    pub(crate) fn new(composition_revision: Reactive<u64>) -> Self {
        let inherited = CubicBezier::SETTLE
            .to_motion_curve_data()
            .expect("the approved inherited curve is portable");
        let explicit = overshoot_curve()
            .to_motion_curve_data()
            .expect("the approved review curve is portable");
        let consumers = MotionCurveConsumerSet::new(vec![
            MotionCurveConsumer::new("settings.drawer.open", MotionCurveConsumerDomain::Spatial)
                .expect("the approved consumer identity is valid"),
        ])
        .expect("the approved consumer set is valid");
        let inherited_compiled = CompiledMotionCurve::compile(&inherited)
            .expect("the approved inherited curve compiles");
        let mut editor = MotionCurveEditor::new(
            inherited,
            Some(explicit),
            Duration::from_millis(280),
            None,
            consumers,
            MotionCurveEditorConfig::default(),
        )
        .expect("the approved editor seed is valid");
        editor.set_looping(false);
        editor
            .zoom_viewport(MotionGraphPoint::new(0.0, 0.0), 1.0, 1.0 / 1.2)
            .expect("the approved 0..1.2 review viewport is valid");
        editor
            .scrub_playhead(0.46)
            .expect("the approved resting playhead is finite");
        Self {
            inner: Rc::new(MotionEditorSessionInner {
                editor: RefCell::new(editor),
                inherited_compiled,
                input: RefCell::new(MotionEditorInputController::default()),
                next_edit_id: Cell::new(1),
                duration_text: Reactive::new("280 ms".to_owned()),
                duration_status: Reactive::new(TextInputStatus::Idle),
                fluid_overrides: Cell::new(MotionFluidOverridesData::default()),
                composition_revision,
                visual_revision: Reactive::new(1),
            }),
        }
    }

    pub(crate) fn snapshot(&self) -> MotionCurveEditorSnapshot {
        self.inner.editor.borrow().snapshot()
    }

    fn render_state(&self) -> MotionEditorRenderState {
        let editor = self.inner.editor.borrow();
        let snapshot = editor.snapshot();
        MotionEditorRenderState {
            inherited_compiled: self.inner.inherited_compiled.clone(),
            snapshot,
            compiled: editor.compiled().clone(),
        }
    }

    fn next_edit_id(&self) -> nkdhr_ui::MotionEditorEditId {
        let id = self.inner.next_edit_id.get().max(1);
        self.inner.next_edit_id.set(id.wrapping_add(1).max(1));
        nkdhr_ui::MotionEditorEditId(id)
    }

    fn handle(
        &self,
        input: MotionEditorInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        let outcome = self
            .inner
            .input
            .borrow_mut()
            .handle(&mut self.inner.editor.borrow_mut(), input)?;
        if outcome.document_changed || outcome.transient_changed {
            self.bump_visual_revision();
        }
        if outcome.document_changed {
            self.sync_duration_control();
            self.bump_composition_revision();
        }
        Ok(outcome)
    }

    fn fit_viewport(&self) {
        self.inner.editor.borrow_mut().fit_viewport();
        self.bump_visual_revision();
        self.bump_composition_revision();
    }

    fn reset_viewport(&self) {
        if self.inner.editor.borrow_mut().reset_viewport() {
            self.bump_visual_revision();
            self.bump_composition_revision();
        }
    }

    fn set_looping(&self, looping: bool) {
        if self.inner.editor.borrow_mut().set_looping(looping) {
            self.bump_composition_revision();
        }
    }

    fn advance_playback(&self, now: Duration) -> bool {
        let changed = self.inner.editor.borrow_mut().advance_playback(now);
        if changed {
            self.bump_visual_revision();
        }
        changed
    }

    fn reset(&self) {
        let mut editor = self.inner.editor.borrow_mut();
        let curve = editor.reset_curve().unwrap_or(false);
        let duration = editor.reset_duration();
        let fluid = self
            .inner
            .fluid_overrides
            .replace(MotionFluidOverridesData::default())
            != MotionFluidOverridesData::default();
        if curve || duration || fluid {
            editor.fit_viewport();
        }
        drop(editor);
        self.sync_duration_control();
        if curve || duration || fluid {
            self.bump_visual_revision();
            self.bump_composition_revision();
        }
    }

    fn duration_text(&self) -> Reactive<String> {
        self.inner.duration_text.clone()
    }

    fn duration_status(&self) -> Reactive<TextInputStatus> {
        self.inner.duration_status.clone()
    }

    fn duration_text_changed(&self) {
        self.inner.duration_status.set(TextInputStatus::Idle);
    }

    fn submit_duration(&self, text: &str) {
        let milliseconds = text
            .trim()
            .strip_suffix("ms")
            .unwrap_or(text.trim())
            .trim()
            .parse::<u64>();
        let Ok(milliseconds) = milliseconds else {
            self.inner.duration_status.set(TextInputStatus::Invalid(
                "请输入 1–60000 ms 的整数".to_owned(),
            ));
            return;
        };
        if !(1..=MAX_EDITABLE_DURATION_MS).contains(&milliseconds) {
            self.inner.duration_status.set(TextInputStatus::Invalid(
                "持续时间范围为 1–60000 ms".to_owned(),
            ));
            return;
        }
        let result = self
            .inner
            .editor
            .borrow_mut()
            .set_duration(Duration::from_millis(milliseconds));
        match result {
            Ok(changed) => {
                self.sync_duration_control();
                self.inner.duration_status.set(TextInputStatus::Valid);
                if changed {
                    self.bump_visual_revision();
                    self.bump_composition_revision();
                }
            }
            Err(_) => self.inner.duration_status.set(TextInputStatus::Invalid(
                "当前编辑事务暂时不能修改持续时间".to_owned(),
            )),
        }
    }

    fn sync_duration_control(&self) {
        let milliseconds = self.inner.editor.borrow().snapshot().duration.as_millis();
        self.inner.duration_text.set(format!("{milliseconds} ms"));
        self.inner.duration_status.set(TextInputStatus::Idle);
    }

    fn fluid_material(&self) -> FluidMaterialValues {
        let inherited = FluidMaterialValues::default();
        let overrides = self.fluid_overrides();
        FluidMaterialValues {
            viscosity: semantic_fluid_percent(overrides.viscosity, inherited.viscosity),
            surface_tension: semantic_fluid_percent(
                overrides.surface_tension,
                inherited.surface_tension,
            ),
            attraction: semantic_fluid_percent(overrides.attraction, inherited.attraction),
        }
    }

    fn fluid_overrides(&self) -> MotionFluidOverridesData {
        self.inner.fluid_overrides.get()
    }

    fn set_fluid_material(&self, field: FluidMaterialField, value: f32) {
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return;
        }
        let mut overrides = self.fluid_overrides();
        let semantic_value = f64::from(value) / FLUID_PERCENT_PER_SEMANTIC_UNIT;
        match field {
            FluidMaterialField::Viscosity => overrides.viscosity = Some(semantic_value),
            FluidMaterialField::SurfaceTension => {
                overrides.surface_tension = Some(semantic_value);
            }
            FluidMaterialField::Attraction => overrides.attraction = Some(semantic_value),
        }
        if self.inner.fluid_overrides.replace(overrides) != overrides {
            self.bump_visual_revision();
            self.bump_composition_revision();
        }
    }

    fn set_permissions(&self, overshoot: bool, reverse: bool) {
        let _ = self
            .inner
            .editor
            .borrow_mut()
            .set_permissions(overshoot, reverse);
        self.bump_visual_revision();
        // Recompose even when the editor rejects the permission change. The
        // replacement Toggle then reads the authoritative editor value instead
        // of leaving a locally toggled but invalid visual state behind.
        self.bump_composition_revision();
    }

    fn bump_composition_revision(&self) {
        self.inner
            .composition_revision
            .update(|revision| *revision = revision.wrapping_add(1).max(1));
    }

    fn watch_visual_revision(&self, ctx: &mut PaintCtx<'_>) {
        let _ = ctx.watch(&self.inner.visual_revision, Invalidation::PAINT);
    }

    fn bump_visual_revision(&self) {
        self.inner
            .visual_revision
            .update(|revision| *revision = revision.wrapping_add(1).max(1));
    }
}

pub(crate) fn workspace(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    session: MotionEditorSession,
) -> Result<Element, SliderError> {
    let scope = scope_rail(Arc::clone(&theme), capabilities, session.clone());
    let graph = motion_graph(Arc::clone(&theme), capabilities, session.clone());
    let preview = preview(Arc::clone(&theme), capabilities, session);
    Ok(Element::new(MotionWorkspaceLayout {
        divider: with_alpha(theme.palette.edge, 0.045),
    })
    .child(scope)
    .child(graph)
    .child(preview))
}

pub(crate) fn navigation_shell(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    navigation: Element,
) -> Element {
    Element::new(Align {
        horizontal: Alignment::Stretch,
        vertical: Alignment::Start,
    })
    .child(
        Element::new(GelSurface::new(
            Arc::clone(&theme),
            MaterialTier::CompactNode,
            capabilities,
            theme.radii.group,
            Insets::symmetric(0.0, NAVIGATION_VERTICAL_INSET),
            GelElevation::Raised,
        ))
        .child(navigation),
    )
}

pub(crate) fn inspector(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    drawer: bool,
    session: MotionEditorSession,
) -> Result<Element, SliderError> {
    let snapshot = session.snapshot();
    let inherited = text(
        "● 当前层覆盖  ·  未保存",
        TextRole::Caption,
        theme.palette.text_muted,
        &theme,
    );
    let heading = Element::new(Flex {
        axis: Axis::Vertical,
        gap: 2.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Start,
    })
    .child(text(
        "展开",
        TextRole::Section,
        theme.palette.text_primary,
        &theme,
    ))
    .child(text(
        "settings.drawer.open",
        TextRole::Mono,
        theme.palette.text_secondary,
        &theme,
    ))
    .child(inherited);

    let duration_source = match snapshot.duration_source {
        nkdhr_ui::MotionCurveSource::Inherited => "ms · 继承",
        nkdhr_ui::MotionCurveSource::Explicit => "ms · 当前层覆盖",
    };
    let duration_session = session.clone();
    let duration_submit = session.clone();
    let duration = property_row(
        "持续时间",
        duration_source,
        Element::new(
            TextInput::new(
                "持续时间（毫秒）",
                session.duration_text(),
                Arc::clone(&theme),
            )
            .status(session.duration_status())
            .capabilities(capabilities)
            .on_change(move |_| duration_session.duration_text_changed())
            .on_submit(move |value| duration_submit.submit_duration(value)),
        ),
        &theme,
    );
    let curve = property_row(
        "曲线",
        "Custom / Settle base",
        Element::new(
            Button::new("Custom", Arc::clone(&theme))
                .variant(ButtonVariant::FluidSelected)
                .capabilities(capabilities),
        ),
        &theme,
    );

    let overshoot = Reactive::new(snapshot.curve.allow_overshoot);
    let reverse = Reactive::new(snapshot.curve.allow_reverse);
    let overshoot_session = session.clone();
    let reverse_session = session.clone();
    let capability_rows = Element::new(Flex {
        axis: Axis::Vertical,
        gap: 4.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Stretch,
    })
    .child(property_row(
        "允许越界",
        "当前消费者允许",
        Element::new(
            Toggle::new("允许越界", overshoot, Arc::clone(&theme))
                .capabilities(capabilities)
                .on_change(move |value| {
                    let reverse = overshoot_session.snapshot().curve.allow_reverse;
                    overshoot_session.set_permissions(value, reverse);
                }),
        ),
        &theme,
    ))
    .child(property_row(
        "允许反向",
        "当前消费者允许",
        Element::new(
            Toggle::new("允许反向", reverse, Arc::clone(&theme))
                .capabilities(capabilities)
                .on_change(move |value| {
                    let overshoot = reverse_session.snapshot().curve.allow_overshoot;
                    reverse_session.set_permissions(overshoot, value);
                }),
        ),
        &theme,
    ));

    let material = session.fluid_material();
    let viscosity = fluid_slider(
        "黏度",
        material.viscosity,
        FluidMaterialField::Viscosity,
        session.clone(),
        Arc::clone(&theme),
        capabilities,
    )?;
    let tension = fluid_slider(
        "表面张力",
        material.surface_tension,
        FluidMaterialField::SurfaceTension,
        session.clone(),
        Arc::clone(&theme),
        capabilities,
    )?;
    let attraction = fluid_slider(
        "吸附力",
        material.attraction,
        FluidMaterialField::Attraction,
        session.clone(),
        Arc::clone(&theme),
        capabilities,
    )?;
    let fluid = Element::new(Flex {
        axis: Axis::Vertical,
        gap: 8.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Stretch,
    })
    .child(
        Element::new(Flex {
            axis: Axis::Vertical,
            gap: 2.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Start,
        })
        .child(text(
            "Fluid Material",
            TextRole::Label,
            theme.palette.text_primary,
            &theme,
        ))
        .child(italic_text(
            if session.fluid_overrides().is_empty() {
                "Shape · 继承"
            } else {
                "Shape · 当前层覆盖"
            },
            TextRole::Caption,
            theme.palette.text_muted,
            &theme,
        )),
    )
    .child(viscosity)
    .child(tension)
    .child(attraction);

    let reset_session = session.clone();
    let actions = Element::new(Flex {
        axis: Axis::Horizontal,
        gap: 8.0,
        main_alignment: MainAxisAlignment::End,
        cross_alignment: CrossAxisAlignment::Center,
    })
    .child(Element::new(
        Button::new("重置", Arc::clone(&theme))
            .variant(ButtonVariant::Fluid)
            .capabilities(capabilities)
            .enabled(true)
            .on_activate(move || reset_session.reset()),
    ))
    .child(Element::new(
        Button::new("保存", Arc::clone(&theme))
            .variant(ButtonVariant::FluidSelected)
            .capabilities(capabilities)
            .enabled(true),
    ));

    let content = Element::new(Padding {
        insets: Insets::all(20.0),
    })
    .child(
        Element::new(Flex {
            axis: Axis::Vertical,
            gap: 12.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Stretch,
        })
        .child(heading)
        .child(divider(with_alpha(theme.palette.edge, 0.055)))
        .child(duration)
        .child(curve)
        .child(divider(with_alpha(theme.palette.edge, 0.035)))
        .child(capability_rows)
        .child(divider(with_alpha(theme.palette.edge, 0.035)))
        .child(fluid)
        .child(actions),
    );
    let panel = Element::new(GelSurface::new(
        Arc::clone(&theme),
        if drawer {
            MaterialTier::ContentSurface
        } else {
            MaterialTier::ExpandedPanel
        },
        capabilities,
        if drawer {
            theme.radii.popover
        } else {
            theme.radii.major
        },
        Insets::ZERO,
        GelElevation::Raised,
    ))
    .child(content);
    Ok(panel)
}

fn scope_rail(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    session: MotionEditorSession,
) -> Element {
    let snapshot = session.snapshot();
    let heading = Element::new(Flex {
        axis: Axis::Horizontal,
        gap: 12.0,
        main_alignment: MainAxisAlignment::SpaceBetween,
        cross_alignment: CrossAxisAlignment::Center,
    })
    .child(text(
        "动画工作室",
        TextRole::Section,
        theme.palette.text_primary,
        &theme,
    ))
    .child(text(
        format!(
            "曲线 ● 本地预览   ·   {} ms ○ {}",
            snapshot.duration.as_millis(),
            match snapshot.duration_source {
                nkdhr_ui::MotionCurveSource::Inherited => "继承",
                nkdhr_ui::MotionCurveSource::Explicit => "覆盖",
            }
        ),
        TextRole::Caption,
        theme.palette.text_muted,
        &theme,
    ));

    let mut path = Element::new(ScopePathLayout {
        theme: Arc::clone(&theme),
        gap: 14.0,
    });
    for (index, label) in ["Balanced", "组件反馈", "设置面板", "展开"]
        .into_iter()
        .enumerate()
    {
        path = path.child(Element::new(
            Button::new(label, Arc::clone(&theme))
                .variant(if index == 3 {
                    ButtonVariant::FluidSelected
                } else {
                    ButtonVariant::Fluid
                })
                .capabilities(capabilities),
        ));
    }

    Element::new(Padding {
        insets: Insets::new(12.0, 8.0, 12.0, 8.0),
    })
    .child(
        Element::new(Flex {
            axis: Axis::Vertical,
            gap: 4.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Stretch,
        })
        .child(heading)
        .child(path),
    )
}

fn motion_graph(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    session: MotionEditorSession,
) -> Element {
    let snapshot = session.snapshot();
    let toolbar = Element::new(Padding {
        insets: Insets::symmetric(12.0, 4.0),
    })
    .child(
        Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 8.0,
            main_alignment: MainAxisAlignment::SpaceBetween,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 8.0,
                main_alignment: MainAxisAlignment::Start,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(text(
                "展开 · Custom overshoot",
                TextRole::Label,
                theme.palette.text_primary,
                &theme,
            ))
            .child(text(
                format!(
                    "进度 {}–{}",
                    format_graph_number(snapshot.viewport.progress_start()),
                    format_graph_number(snapshot.viewport.progress_end())
                ),
                TextRole::Caption,
                theme.palette.text_muted,
                &theme,
            )),
        )
        .child(
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 4.0,
                main_alignment: MainAxisAlignment::End,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(Element::new({
                let session = session.clone();
                Button::new("适应", Arc::clone(&theme))
                    .variant(ButtonVariant::Fluid)
                    .capabilities(capabilities)
                    .on_activate(move || session.fit_viewport())
            }))
            .child(Element::new({
                let session = session.clone();
                Button::new("100%", Arc::clone(&theme))
                    .variant(ButtonVariant::Fluid)
                    .capabilities(capabilities)
                    .on_activate(move || session.reset_viewport())
            })),
        ),
    );
    let plot = Element::new(MotionCurvePlot {
        theme: Arc::clone(&theme),
        capabilities,
        session: session.clone(),
    });
    let axis = Element::new(Padding {
        insets: Insets::new(16.0, 2.0, 16.0, 4.0),
    })
    .child(
        Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 8.0,
            main_alignment: MainAxisAlignment::SpaceBetween,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(text(
            format!(
                "{} ms",
                format_milliseconds(snapshot.duration, snapshot.viewport.time_start())
            ),
            TextRole::Caption,
            theme.palette.text_muted,
            &theme,
        ))
        .child(text(
            format!(
                "{} ms",
                format_milliseconds(
                    snapshot.duration,
                    (snapshot.viewport.time_start() + snapshot.viewport.time_end()) * 0.5,
                )
            ),
            TextRole::Caption,
            theme.palette.text_muted,
            &theme,
        ))
        .child(text(
            format!(
                "{} ms   时间",
                format_milliseconds(snapshot.duration, snapshot.viewport.time_end())
            ),
            TextRole::Caption,
            theme.palette.text_secondary,
            &theme,
        )),
    );
    Element::new(MotionGraphLayout)
        .child(toolbar)
        .child(plot)
        .child(axis)
}

fn preview(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    session: MotionEditorSession,
) -> Element {
    let snapshot = session.snapshot();
    let rail = Element::new(Padding {
        insets: Insets::symmetric(12.0, 6.0),
    })
    .child(
        Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 8.0,
            main_alignment: MainAxisAlignment::SpaceBetween,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 8.0,
                main_alignment: MainAxisAlignment::Start,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(text(
                "真实预览",
                TextRole::Label,
                theme.palette.text_primary,
                &theme,
            ))
            .child(italic_text(
                "设置面板 · seed 6A17",
                TextRole::Caption,
                theme.palette.text_muted,
                &theme,
            )),
        )
        .child(
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 4.0,
                main_alignment: MainAxisAlignment::End,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(Element::new({
                let session = session.clone();
                Button::new("单次", Arc::clone(&theme))
                    .variant(if !snapshot.looping {
                        ButtonVariant::FluidSelected
                    } else {
                        ButtonVariant::Fluid
                    })
                    .capabilities(capabilities)
                    .on_activate(move || session.set_looping(false))
            }))
            .child(Element::new({
                let session = session.clone();
                Button::new("循环", Arc::clone(&theme))
                    .variant(if snapshot.looping {
                        ButtonVariant::FluidSelected
                    } else {
                        ButtonVariant::Fluid
                    })
                    .capabilities(capabilities)
                    .on_activate(move || session.set_looping(true))
            }))
            .child(Element::new(
                Button::new("交互", Arc::clone(&theme))
                    .variant(ButtonVariant::Fluid)
                    .capabilities(capabilities),
            )),
        ),
    );

    let wallpaper_adaptive = Reactive::new(true);
    let drawer_content = Element::new(GelSurface::new(
        Arc::clone(&theme),
        MaterialTier::ExpandedPanel,
        capabilities,
        theme.radii.group,
        Insets::all(16.0),
        GelElevation::Embedded,
    ))
    .child(
        Element::new(Flex {
            axis: Axis::Vertical,
            gap: 8.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Stretch,
        })
        .child(
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 12.0,
                main_alignment: MainAxisAlignment::SpaceBetween,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(text(
                "显示设置",
                TextRole::Label,
                theme.palette.text_primary,
                &theme,
            ))
            .child(text(
                "展开完成",
                TextRole::Caption,
                theme.palette.success,
                &theme,
            )),
        )
        .child(
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 24.0,
                main_alignment: MainAxisAlignment::SpaceBetween,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(text(
                "壁纸自适应",
                TextRole::BodySmall,
                theme.palette.text_secondary,
                &theme,
            ))
            .child(Element::new(
                Toggle::new("预览中的壁纸自适应", wallpaper_adaptive, Arc::clone(&theme))
                    .capabilities(capabilities),
            )),
        ),
    );
    let stage = Element::new(PreviewStage {
        theme: Arc::clone(&theme),
        session,
    })
    .child(
        Element::new(Align {
            horizontal: Alignment::Center,
            vertical: Alignment::Center,
        })
        .child(drawer_content),
    );
    Element::new(PreviewLayout).child(rail).child(stage)
}

fn property_row(label: &str, provenance: &str, control: Element, theme: &Theme) -> Element {
    Element::new(Flex {
        axis: Axis::Horizontal,
        gap: 8.0,
        main_alignment: MainAxisAlignment::SpaceBetween,
        cross_alignment: CrossAxisAlignment::Center,
    })
    .child(
        Element::new(Flex {
            axis: Axis::Vertical,
            gap: 1.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Start,
        })
        .child(text(
            label,
            TextRole::BodySmall,
            theme.palette.text_primary,
            theme,
        ))
        .child(italic_text(
            provenance,
            TextRole::Caption,
            theme.palette.text_muted,
            theme,
        ))
        .flex(1.0),
    )
    .child(control)
}

fn fluid_slider(
    label: &str,
    value: f32,
    field: FluidMaterialField,
    session: MotionEditorSession,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
) -> Result<Element, SliderError> {
    let value_state = Reactive::new(value);
    Ok(Element::new(Flex {
        axis: Axis::Vertical,
        gap: 2.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Stretch,
    })
    .child(
        Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 8.0,
            main_alignment: MainAxisAlignment::SpaceBetween,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(text(
            label,
            TextRole::BodySmall,
            theme.palette.text_secondary,
            &theme,
        ))
        .child(text(
            format!("{value:.0}"),
            TextRole::Mono,
            theme.palette.text_primary,
            &theme,
        )),
    )
    .child(Element::new(
        Slider::new(label, value_state, 0.0, 100.0, theme)?
            .step(1.0)?
            .ideal_width(220.0)?
            .capabilities(capabilities)
            .on_change(move |value| session.set_fluid_material(field, value)),
    )))
}

fn semantic_fluid_percent(value: Option<f64>, inherited_percent: f32) -> f32 {
    value
        .map(|value| (value * FLUID_PERCENT_PER_SEMANTIC_UNIT) as f32)
        .unwrap_or(inherited_percent)
}

fn divider(color: Color) -> Element {
    Element::new(MotionDivider { color })
}

fn text(content: impl Into<String>, role: TextRole, color: Color, theme: &Theme) -> Element {
    Element::new(Text::new(content, theme.text_style(role), color))
}

fn italic_text(content: impl Into<String>, role: TextRole, color: Color, theme: &Theme) -> Element {
    let mut style = theme.text_style(role);
    style.slant = FontSlant::Italic;
    Element::new(Text::new(content, style, color))
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    let [red, green, blue, _] = color.components();
    Color::new(red, green, blue, alpha).expect("theme colors and static alpha are valid")
}

fn format_graph_number(value: f64) -> String {
    if (value - value.round()).abs() < 1.0e-6 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn format_milliseconds(duration: Duration, normalized_time: f64) -> String {
    format_graph_number(duration.as_secs_f64() * 1_000.0 * normalized_time)
}

fn mix_color(first: Color, second: Color, amount: f32) -> Color {
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

fn overshoot_curve() -> CubicBezier {
    CubicBezier::new(0.20, 1.70, 0.45, 0.90)
        .expect("the P1 owner-review curve is a valid CSS-time cubic")
}

/// A local clay/glass treatment for UI-7E review surfaces. Its volume comes
/// from broad inset light rather than decorative edge streaks, so large panels
/// read as inflated translucent material instead of outlined dark glass.
#[derive(Debug, Clone)]
struct GelSurface {
    theme: Arc<Theme>,
    tier: MaterialTier,
    capabilities: MaterialCapabilities,
    radius: f32,
    padding: Insets,
    elevation: GelElevation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GelElevation {
    Raised,
    Embedded,
}

impl GelSurface {
    fn new(
        theme: Arc<Theme>,
        tier: MaterialTier,
        capabilities: MaterialCapabilities,
        radius: f32,
        padding: Insets,
        elevation: GelElevation,
    ) -> Self {
        Self {
            theme,
            tier,
            capabilities,
            radius,
            padding,
            elevation,
        }
    }
}

impl Widget for GelSurface {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths([
            "palette.surface",
            "palette.surface_raised",
            "palette.backdrop",
            "palette.accent",
            "palette.accent_secondary",
            "palette.edge",
            "palette.inverse_edge",
            "palette.shadow",
            "materials.expanded_panel.opacity",
            "materials.expanded_panel.backdrop_blur",
            "materials.content_surface.opacity",
            "materials.content_surface.backdrop_blur",
        ])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        self.padding.validate()?;
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        if ctx.child_count() == 0 {
            return Ok(constraints.constrain(Size::new(
                self.padding.horizontal(),
                self.padding.vertical(),
            )));
        }
        let child = ctx.measure_child(0, constraints.deflate(self.padding)?)?;
        Ok(constraints.constrain(Size::new(
            child.width + self.padding.horizontal(),
            child.height + self.padding.vertical(),
        )))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() == 1 {
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x + self.padding.left,
                    rect.y + self.padding.top,
                    (rect.width - self.padding.horizontal()).max(0.0),
                    (rect.height - self.padding.vertical()).max(0.0),
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        let radii = CornerRadii::all(self.radius.max(0.0));
        let material = self.theme.resolve_material(self.tier, self.capabilities);
        let dimension = rect.width.min(rect.height);
        let (outer_depth, shadow_alpha, light_scale, dark_scale) = match self.elevation {
            GelElevation::Raised => ((dimension * 0.032).clamp(3.0, 6.0), 0.20, 0.48, 0.58),
            GelElevation::Embedded => ((dimension * 0.020).clamp(1.5, 3.0), 0.04, 0.23, 0.32),
        };
        let outer_blur = (outer_depth * 2.2).clamp(7.0, 16.0);
        let inner_depth = (outer_depth * 0.62).clamp(1.25, 3.5);
        let inner_blur = (inner_depth * 1.5).clamp(3.5, 7.0);
        let base_tint = mix_color(
            self.theme.palette.surface,
            self.theme.palette.surface_raised,
            match self.elevation {
                GelElevation::Raised => 0.24,
                GelElevation::Embedded => 0.08,
            },
        );
        let tones = resolve_fluid_material_tones(&self.theme, base_tint, false);
        ctx.builder().shadow(
            rect,
            radii,
            Shadow::new(
                outer_depth,
                outer_depth,
                outer_blur * 1.45,
                0.0,
                with_alpha(tones.shade, shadow_alpha),
            ),
        )?;
        if material.backdrop_blur > 0.0 {
            ctx.builder()
                .backdrop_blur(rect, radii, material.backdrop_blur)?;
        }
        ctx.builder().rounded_rect(
            rect,
            radii,
            with_alpha(
                base_tint,
                (material.fill.components()[3]
                    + if self.elevation == GelElevation::Raised {
                        0.06
                    } else {
                        0.0
                    })
                .min(0.92),
            ),
        )?;
        if !self.capabilities.high_contrast {
            ctx.builder().inset_shadow(
                rect,
                radii,
                Shadow::new(
                    inner_depth,
                    inner_depth,
                    inner_blur,
                    0.0,
                    with_alpha(tones.highlight, tones.highlight_strength * light_scale),
                ),
            )?;
            ctx.builder().inset_shadow(
                rect,
                radii,
                Shadow::new(
                    -inner_depth,
                    -inner_depth,
                    inner_blur * 0.90,
                    0.0,
                    with_alpha(tones.shade, tones.shade_strength * dark_scale),
                ),
            )?;
        }
        ctx.builder()
            .border(rect, radii, 1.0, with_alpha(tones.highlight, 0.035))?;
        ctx.paint_children()
    }
}

#[derive(Debug, Clone)]
struct ScopePathLayout {
    theme: Arc<Theme>,
    gap: f32,
}

impl Widget for ScopePathLayout {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths([
            "palette.surface",
            "palette.surface_raised",
            "palette.backdrop",
            "palette.accent",
            "palette.accent_secondary",
            "palette.edge",
            "palette.inverse_edge",
            "palette.shadow",
        ])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        let mut width = self.gap * ctx.child_count().saturating_sub(1) as f32;
        let mut height = 0.0_f32;
        for index in 0..ctx.child_count() {
            let child =
                ctx.measure_child(index, Constraints::new(Size::ZERO, constraints.max())?)?;
            width += child.width;
            height = height.max(child.height);
        }
        Ok(constraints.constrain(Size::new(width, height)))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let mut x = rect.x;
        for index in 0..ctx.child_count() {
            let child = ctx.child_size(index)?;
            ctx.arrange_child(
                index,
                Rect::new(
                    x,
                    rect.y + (rect.height - child.height).max(0.0) * 0.5,
                    child.width,
                    child.height.min(rect.height),
                ),
            )?;
            x += child.width + self.gap;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        for index in 0..ctx.child_count().saturating_sub(1) {
            let left = ctx.child_rect(index)?;
            let right = ctx.child_rect(index + 1)?;
            let start = left.right() - 3.0;
            let end = right.x + 3.0;
            let center_y = (left.y + left.height * 0.5 + right.y + right.height * 0.5) * 0.5;
            let width = (end - start).max(0.0);
            let neck = Rect::new(start, center_y - 6.0, width, 12.0);
            let radii = CornerRadii::all(6.0);
            let base = mix_color(
                self.theme.palette.surface,
                self.theme.palette.surface_raised,
                0.12,
            );
            let tones = resolve_fluid_material_tones(&self.theme, base, false);
            ctx.builder().shadow(
                neck,
                radii,
                Shadow::new(1.5, 1.5, 6.0, 0.0, with_alpha(tones.shade, 0.08)),
            )?;
            ctx.builder()
                .rounded_rect(neck, radii, with_alpha(base, 0.84))?;
            ctx.builder().inset_shadow(
                neck,
                radii,
                Shadow::new(
                    2.5,
                    2.5,
                    7.0,
                    0.0,
                    with_alpha(tones.highlight, tones.highlight_strength * 0.58),
                ),
            )?;
            ctx.builder().inset_shadow(
                neck,
                radii,
                Shadow::new(
                    -3.0,
                    -3.0,
                    6.0,
                    0.0,
                    with_alpha(tones.shade, tones.shade_strength * 0.70),
                ),
            )?;
        }
        ctx.paint_children()
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            role: SemanticRole::Group,
            label: Some("动画样式作用范围".to_owned()),
            ..Semantics::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MotionWorkspaceLayout {
    divider: Color,
}

impl Widget for MotionWorkspaceLayout {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 3 {
            return Err(UiError::ChildCountMismatch {
                expected: 3,
                actual: ctx.child_count(),
            });
        }
        let size = constraints.max();
        let preview = PREVIEW_HEIGHT.min((size.height - SCOPE_RAIL_HEIGHT).max(0.0));
        let graph = (size.height - SCOPE_RAIL_HEIGHT - preview).max(0.0);
        for (index, height) in [SCOPE_RAIL_HEIGHT, graph, preview].into_iter().enumerate() {
            ctx.measure_child(index, Constraints::tight(Size::new(size.width, height))?)?;
        }
        Ok(constraints.constrain(size))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let preview = PREVIEW_HEIGHT.min((rect.height - SCOPE_RAIL_HEIGHT).max(0.0));
        let graph = (rect.height - SCOPE_RAIL_HEIGHT - preview).max(0.0);
        ctx.arrange_child(0, Rect::new(rect.x, rect.y, rect.width, SCOPE_RAIL_HEIGHT))?;
        ctx.arrange_child(
            1,
            Rect::new(rect.x, rect.y + SCOPE_RAIL_HEIGHT, rect.width, graph),
        )?;
        ctx.arrange_child(
            2,
            Rect::new(rect.x, rect.bottom() - preview, rect.width, preview),
        )
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        ctx.builder().rect(
            Rect::new(rect.x, rect.y + SCOPE_RAIL_HEIGHT - 1.0, rect.width, 1.0),
            self.divider,
        )?;
        ctx.builder().rect(
            Rect::new(rect.x, rect.bottom() - PREVIEW_HEIGHT, rect.width, 1.0),
            self.divider,
        )?;
        ctx.paint_children()
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            role: SemanticRole::Group,
            label: Some("专业动画工作室".to_owned()),
            ..Semantics::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MotionGraphLayout;

impl Widget for MotionGraphLayout {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 3 {
            return Err(UiError::ChildCountMismatch {
                expected: 3,
                actual: ctx.child_count(),
            });
        }
        let size = constraints.max();
        let plot_height = (size.height - GRAPH_TOOLBAR_HEIGHT - GRAPH_AXIS_HEIGHT).max(0.0);
        for (index, height) in [GRAPH_TOOLBAR_HEIGHT, plot_height, GRAPH_AXIS_HEIGHT]
            .into_iter()
            .enumerate()
        {
            ctx.measure_child(index, Constraints::tight(Size::new(size.width, height))?)?;
        }
        Ok(constraints.constrain(size))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let plot_height = (rect.height - GRAPH_TOOLBAR_HEIGHT - GRAPH_AXIS_HEIGHT).max(0.0);
        ctx.arrange_child(
            0,
            Rect::new(rect.x, rect.y, rect.width, GRAPH_TOOLBAR_HEIGHT),
        )?;
        ctx.arrange_child(
            1,
            Rect::new(
                rect.x,
                rect.y + GRAPH_TOOLBAR_HEIGHT,
                rect.width,
                plot_height,
            ),
        )?;
        ctx.arrange_child(
            2,
            Rect::new(
                rect.x,
                rect.bottom() - GRAPH_AXIS_HEIGHT,
                rect.width,
                GRAPH_AXIS_HEIGHT,
            ),
        )
    }
}

#[derive(Debug, Clone)]
struct MotionCurvePlot {
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    session: MotionEditorSession,
}

#[derive(Debug, Clone, Copy, Default)]
struct MotionCurvePlotState {
    active: Option<MotionPlotInteraction>,
    focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MotionPlotInteraction {
    Direct(MotionEditorEditId),
    Viewport {
        id: MotionEditorEditId,
        device: MotionEditorDevice,
    },
}

impl Widget for MotionCurvePlot {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths([
            "palette.accent",
            "palette.accent_secondary",
            "palette.edge",
            "palette.inverse_edge",
            "palette.text_muted",
            "palette.surface",
            "palette.surface_raised",
            "palette.backdrop",
            "palette.shadow",
            "materials.compact_node.opacity",
            "materials.compact_node.backdrop_blur",
        ])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn create_state(&self) -> Box<dyn std::any::Any> {
        Box::<MotionCurvePlotState>::default()
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        self.session.watch_visual_revision(ctx);
        let state = self.session.render_state();
        let focused = ctx.state_mut::<MotionCurvePlotState>()?.focused;
        let viewport = state.snapshot.viewport;
        let frame = ctx.rect().inset(6.0);
        paint_fluid_well(
            ctx.builder(),
            frame,
            CornerRadii::all(self.theme.radii.group),
            &self.theme,
            self.capabilities,
            SurfaceState {
                focused,
                ..SurfaceState::default()
            },
        )?;
        let plot = frame.inset(GRAPH_CONTENT_INSET);
        let minor = with_alpha(self.theme.palette.edge, 0.022);
        let major = with_alpha(self.theme.palette.edge, 0.052);
        for index in 0..=8 {
            let x = plot.x + plot.width * index as f32 / 8.0;
            ctx.builder().rect(
                Rect::new(x, plot.y, 1.0, plot.height),
                if index % 2 == 0 { major } else { minor },
            )?;
        }
        for index in 0..=4 {
            let y = plot.y + plot.height * index as f32 / 4.0;
            ctx.builder()
                .rect(Rect::new(plot.x, y, plot.width, 1.0), major)?;
        }

        let inherited = with_alpha(self.theme.palette.text_muted, 0.58);
        draw_curve(
            ctx,
            plot,
            &state.inherited_compiled,
            viewport,
            CurveStroke::new(inherited, 1.25).dashed(),
        )?;
        draw_curve(
            ctx,
            plot,
            &state.compiled,
            viewport,
            CurveStroke::new(with_alpha(self.theme.palette.inverse_edge, 0.36), 6.0),
        )?;
        draw_curve(
            ctx,
            plot,
            &state.compiled,
            viewport,
            CurveStroke::new(with_alpha(self.theme.palette.accent_secondary, 0.56), 4.0),
        )?;
        draw_curve(
            ctx,
            Rect::new(plot.x, plot.y - 0.8, plot.width, plot.height),
            &state.compiled,
            viewport,
            CurveStroke::new(self.theme.palette.accent, 1.6),
        )?;

        let playhead_x = graph_to_screen(
            plot,
            MotionGraphPoint::new(state.snapshot.playhead, viewport.progress_start()),
            viewport,
        )
        .x;
        let info = with_alpha(self.theme.palette.accent_secondary, 0.82);
        if (plot.x..=plot.right()).contains(&playhead_x) {
            ctx.builder()
                .rect(Rect::new(playhead_x, plot.y, 1.0, plot.height), info)?;
            ctx.builder().rounded_rect(
                Rect::new(playhead_x - 5.0, plot.y - 3.0, 10.0, 10.0),
                CornerRadii::all(5.0),
                self.theme.palette.accent_secondary,
            )?;
        }

        if let Some(selected) = state.snapshot.primary_selection {
            let resolved = resolve_motion_curve_handles(&state.snapshot.curve)
                .map_err(|error| UiError::Text(error.to_string()))?;
            let Some(anchor) = resolved.anchors.get(selected) else {
                return Err(UiError::Text(
                    "selected motion anchor is missing".to_owned(),
                ));
            };
            let nkdhr_ui::MotionTangentsData::Broken { incoming, outgoing } = anchor.tangents
            else {
                return Err(UiError::Text(
                    "resolved motion anchor did not expose handles".to_owned(),
                ));
            };
            for (side, vector) in [
                (MotionEditorTarget::IncomingHandle(selected), incoming),
                (MotionEditorTarget::OutgoingHandle(selected), outgoing),
            ] {
                if (selected == 0 && matches!(side, MotionEditorTarget::IncomingHandle(_)))
                    || (selected + 1 == resolved.anchors.len()
                        && matches!(side, MotionEditorTarget::OutgoingHandle(_)))
                {
                    continue;
                }
                let anchor_point = graph_to_screen(
                    plot,
                    MotionGraphPoint::new(anchor.time, anchor.progress),
                    viewport,
                );
                let handle_point = graph_to_screen(
                    plot,
                    MotionGraphPoint::new(
                        anchor.time + vector.time,
                        anchor.progress + vector.progress,
                    ),
                    viewport,
                );
                if !graph_point_visible(plot, anchor_point)
                    || !graph_point_visible(plot, handle_point)
                {
                    continue;
                }
                draw_segment(
                    ctx,
                    anchor_point,
                    handle_point,
                    1.0,
                    with_alpha(self.theme.palette.accent_secondary, 0.48),
                )?;
                ctx.builder().rounded_rect(
                    Rect::new(handle_point.x - 4.0, handle_point.y - 4.0, 8.0, 8.0),
                    CornerRadii::all(4.0),
                    self.theme.palette.accent_secondary,
                )?;
            }
        }

        for (index, anchor) in state.snapshot.curve.anchors.iter().enumerate() {
            let point = graph_to_screen(
                plot,
                MotionGraphPoint::new(anchor.time, anchor.progress),
                viewport,
            );
            if !graph_point_visible(plot, point) {
                continue;
            }
            let selected = state.snapshot.selection.contains(&index);
            ctx.builder().rounded_rect(
                Rect::new(point.x - 6.0, point.y - 6.0, 12.0, 12.0),
                CornerRadii::all(6.0),
                with_alpha(self.theme.palette.inverse_edge, 0.72),
            )?;
            ctx.builder().rounded_rect(
                Rect::new(
                    point.x - if selected { 4.0 } else { 3.0 },
                    point.y - if selected { 4.0 } else { 3.0 },
                    if selected { 8.0 } else { 6.0 },
                    if selected { 8.0 } else { 6.0 },
                ),
                CornerRadii::all(if selected { 4.0 } else { 2.0 }),
                if selected {
                    self.theme.palette.accent_secondary
                } else {
                    self.theme.palette.accent
                },
            )?;
        }
        Ok(())
    }

    fn animation(&self, ctx: &mut AnimationCtx<'_>) {
        let changed = self.session.advance_playback(ctx.now());
        let playing = matches!(
            self.session.snapshot().playback,
            MotionEditorPlayback::Playing { .. }
        );
        if changed {
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
        }
        if playing {
            ctx.request_animation_frame();
        }
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let frame = ctx.rect().inset(6.0);
        let plot = frame.inset(GRAPH_CONTENT_INSET);
        let render = self.session.render_state();
        let viewport = render.snapshot.viewport;
        match event {
            UiEvent::PointerDown {
                position,
                button: PointerButton::Primary,
                modifiers,
                click_count,
            } => {
                let target = hit_graph_target(*position, plot, viewport, &render);
                let id = self.session.next_edit_id();
                let point = screen_to_graph(*position, plot, viewport);
                let input = MotionEditorInput::Direct(MotionEditorDirectInput {
                    id,
                    phase: MotionEditorGesturePhase::Begin,
                    device: MotionEditorDevice::Mouse,
                    target,
                    position: point,
                    modifiers: editor_modifiers(*modifiers),
                    activation_count: *click_count,
                    snapping: !modifiers.alt,
                });
                if self.session.handle(input).is_ok() {
                    let completed_double_click =
                        *click_count >= 2 && target == MotionEditorTarget::Curve;
                    ctx.state_mut::<MotionCurvePlotState>()?.active =
                        (!completed_double_click).then_some(MotionPlotInteraction::Direct(id));
                    ctx.request_focus();
                    if !completed_double_click {
                        ctx.capture_pointer();
                    }
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerMoved { position } => {
                let Some(MotionPlotInteraction::Direct(id)) =
                    ctx.state_mut::<MotionCurvePlotState>()?.active
                else {
                    return Ok(());
                };
                let point = screen_to_graph(*position, plot, viewport);
                let input = MotionEditorInput::Direct(MotionEditorDirectInput {
                    id,
                    phase: MotionEditorGesturePhase::Update,
                    device: MotionEditorDevice::Mouse,
                    target: MotionEditorTarget::Graph,
                    position: point,
                    modifiers: MotionEditorModifiers::default(),
                    activation_count: 1,
                    snapping: true,
                });
                let _ = self.session.handle(input);
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerUp {
                position,
                button: PointerButton::Primary,
                ..
            } => {
                let active = ctx.state_mut::<MotionCurvePlotState>()?.active;
                let Some(MotionPlotInteraction::Direct(id)) = active else {
                    return Ok(());
                };
                ctx.state_mut::<MotionCurvePlotState>()?.active = None;
                let point = screen_to_graph(*position, plot, viewport);
                let input = MotionEditorInput::Direct(MotionEditorDirectInput {
                    id,
                    phase: MotionEditorGesturePhase::End,
                    device: MotionEditorDevice::Mouse,
                    target: MotionEditorTarget::Graph,
                    position: point,
                    modifiers: MotionEditorModifiers::default(),
                    activation_count: 1,
                    snapping: true,
                });
                let _ = self.session.handle(input);
                ctx.release_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerScroll {
                position,
                delta_x,
                delta_y,
                modifiers,
            } => {
                if ctx.state_mut::<MotionCurvePlotState>()?.active.is_some() {
                    return Ok(());
                }
                let id = self.session.next_edit_id();
                let begin = viewport_input(
                    id,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::Mouse,
                    ViewportGestureSample::new(
                        *position,
                        Point::new(0.0, 0.0),
                        *modifiers,
                        plot,
                        viewport,
                    ),
                );
                if self.session.handle(begin).is_ok() {
                    let end = viewport_input(
                        id,
                        MotionEditorGesturePhase::End,
                        MotionEditorDevice::Mouse,
                        ViewportGestureSample::new(
                            *position,
                            Point::new(*delta_x, *delta_y),
                            *modifiers,
                            plot,
                            viewport,
                        ),
                    );
                    let _ = self.session.handle(end);
                    self.session.bump_composition_revision();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::ScrollGesture {
                position,
                delta_x,
                delta_y,
                phase,
                modifiers,
            } => {
                let device = MotionEditorDevice::PrecisionTouchpad { contacts: 2 };
                match phase {
                    ScrollPhase::Begin => {
                        if ctx.state_mut::<MotionCurvePlotState>()?.active.is_some() {
                            return Ok(());
                        }
                        let id = self.session.next_edit_id();
                        let input = viewport_input(
                            id,
                            MotionEditorGesturePhase::Begin,
                            device,
                            ViewportGestureSample::new(
                                *position,
                                Point::new(*delta_x, *delta_y),
                                *modifiers,
                                plot,
                                viewport,
                            ),
                        );
                        if self.session.handle(input).is_ok() {
                            ctx.state_mut::<MotionCurvePlotState>()?.active =
                                Some(MotionPlotInteraction::Viewport { id, device });
                            ctx.capture_pointer();
                            ctx.set_handled();
                        }
                    }
                    ScrollPhase::Update => {
                        let Some(MotionPlotInteraction::Viewport { id, device }) =
                            ctx.state_mut::<MotionCurvePlotState>()?.active
                        else {
                            return Ok(());
                        };
                        let input = viewport_input(
                            id,
                            MotionEditorGesturePhase::Update,
                            device,
                            ViewportGestureSample::new(
                                *position,
                                Point::new(*delta_x, *delta_y),
                                *modifiers,
                                plot,
                                viewport,
                            ),
                        );
                        let _ = self.session.handle(input);
                        ctx.set_handled();
                    }
                    ScrollPhase::End | ScrollPhase::Cancel => {
                        let active = ctx.state_mut::<MotionCurvePlotState>()?.active;
                        let Some(MotionPlotInteraction::Viewport { id, device }) = active else {
                            return Ok(());
                        };
                        ctx.state_mut::<MotionCurvePlotState>()?.active = None;
                        let editor_phase = if *phase == ScrollPhase::End {
                            MotionEditorGesturePhase::End
                        } else {
                            MotionEditorGesturePhase::Cancel
                        };
                        let input = viewport_input(
                            id,
                            editor_phase,
                            device,
                            ViewportGestureSample::new(
                                *position,
                                Point::new(*delta_x, *delta_y),
                                *modifiers,
                                plot,
                                viewport,
                            ),
                        );
                        let _ = self.session.handle(input);
                        self.session.bump_composition_revision();
                        ctx.release_pointer();
                        ctx.set_handled();
                    }
                }
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerCancel => {
                if let Some(active) = ctx.state_mut::<MotionCurvePlotState>()?.active.take() {
                    let input = match active {
                        MotionPlotInteraction::Direct(id) => {
                            MotionEditorInput::Direct(MotionEditorDirectInput {
                                id,
                                phase: MotionEditorGesturePhase::Cancel,
                                device: MotionEditorDevice::Mouse,
                                target: MotionEditorTarget::Graph,
                                position: MotionGraphPoint::default(),
                                modifiers: MotionEditorModifiers::default(),
                                activation_count: 1,
                                snapping: true,
                            })
                        }
                        MotionPlotInteraction::Viewport { id, device } => {
                            MotionEditorInput::Viewport(MotionEditorViewportInput {
                                id,
                                phase: MotionEditorGesturePhase::Cancel,
                                device,
                                anchor: MotionGraphPoint::default(),
                                translation: MotionGraphPoint::default(),
                                time_scale: 1.0,
                                progress_scale: 1.0,
                            })
                        }
                    };
                    let _ = self.session.handle(input);
                    ctx.release_pointer();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::KeyDown {
                key,
                modifiers,
                repeat,
            } => {
                if let Some(key) = editor_key(key) {
                    let outcome = self.session.handle(MotionEditorInput::Key {
                        key,
                        modifiers: editor_modifiers(*modifiers),
                        repeat: *repeat,
                        now: ctx.now(),
                    });
                    if let Ok(outcome) = outcome {
                        apply_editor_outcome(ctx, outcome);
                        ctx.set_handled();
                        ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                    }
                }
            }
            UiEvent::ClipboardText { text, .. } if ctx.focused() => {
                let _ = self
                    .session
                    .handle(MotionEditorInput::PasteText(text.clone()));
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::FocusChanged(focused) => {
                ctx.state_mut::<MotionCurvePlotState>()?.focused = *focused;
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let snapshot = self.session.snapshot();
        Semantics {
            role: SemanticRole::Group,
            label: Some(format!(
                "动画曲线图：{}，持续时间 {} 毫秒",
                if snapshot.curve.allow_overshoot {
                    "允许越界"
                } else {
                    "限制进度"
                },
                snapshot.duration.as_millis()
            )),
            value: Some(format!("播放位置 {:.0}%", snapshot.playhead * 100.0)),
            focusable: true,
            ..Semantics::default()
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn accepts_pointer(&self) -> bool {
        true
    }
}

fn draw_curve(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    curve: &CompiledMotionCurve,
    viewport: MotionGraphViewport,
    stroke: CurveStroke,
) -> Result<(), UiError> {
    let time_span = viewport.time_end() - viewport.time_start();
    let mut previous = graph_to_screen(
        rect,
        MotionGraphPoint::new(viewport.time_start(), curve.sample(viewport.time_start())),
        viewport,
    );
    for index in 1..=64 {
        let time = viewport.time_start() + time_span * f64::from(index) / 64.0;
        let next = graph_to_screen(
            rect,
            MotionGraphPoint::new(time, curve.sample(time)),
            viewport,
        );
        if !stroke.dashed || (index / 3) % 2 == 0 {
            draw_segment(ctx, previous, next, stroke.width, stroke.color)?;
        }
        previous = next;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct CurveStroke {
    color: Color,
    width: f32,
    dashed: bool,
}

impl CurveStroke {
    const fn new(color: Color, width: f32) -> Self {
        Self {
            color,
            width,
            dashed: false,
        }
    }

    const fn dashed(mut self) -> Self {
        self.dashed = true;
        self
    }
}

fn graph_to_screen(rect: Rect, point: MotionGraphPoint, viewport: MotionGraphViewport) -> Point {
    let time_span = viewport.time_end() - viewport.time_start();
    let progress_span = viewport.progress_end() - viewport.progress_start();
    Point::new(
        rect.x + rect.width * ((point.time - viewport.time_start()) / time_span) as f32,
        rect.bottom()
            - rect.height * ((point.progress - viewport.progress_start()) / progress_span) as f32,
    )
}

fn graph_point_visible(rect: Rect, point: Point) -> bool {
    (rect.x..=rect.right()).contains(&point.x) && (rect.y..=rect.bottom()).contains(&point.y)
}

fn screen_to_graph(point: Point, rect: Rect, viewport: MotionGraphViewport) -> MotionGraphPoint {
    let horizontal = f64::from(((point.x - rect.x) / rect.width.max(1.0)).clamp(0.0, 1.0));
    let vertical = f64::from(((rect.bottom() - point.y) / rect.height.max(1.0)).clamp(0.0, 1.0));
    MotionGraphPoint::new(
        viewport.time_start() + horizontal * (viewport.time_end() - viewport.time_start()),
        viewport.progress_start()
            + vertical * (viewport.progress_end() - viewport.progress_start()),
    )
}

fn hit_graph_target(
    point: Point,
    rect: Rect,
    viewport: MotionGraphViewport,
    state: &MotionEditorRenderState,
) -> MotionEditorTarget {
    if let Some(selected) = state.snapshot.primary_selection
        && let Ok(resolved) = resolve_motion_curve_handles(&state.snapshot.curve)
        && let Some(anchor) = resolved.anchors.get(selected)
        && let nkdhr_ui::MotionTangentsData::Broken { incoming, outgoing } = anchor.tangents
    {
        for (target, vector) in [
            (MotionEditorTarget::IncomingHandle(selected), incoming),
            (MotionEditorTarget::OutgoingHandle(selected), outgoing),
        ] {
            if (selected == 0 && matches!(target, MotionEditorTarget::IncomingHandle(_)))
                || (selected + 1 == resolved.anchors.len()
                    && matches!(target, MotionEditorTarget::OutgoingHandle(_)))
            {
                continue;
            }
            let handle = graph_to_screen(
                rect,
                MotionGraphPoint::new(anchor.time + vector.time, anchor.progress + vector.progress),
                viewport,
            );
            if (point.x - handle.x).abs() <= 10.0 && (point.y - handle.y).abs() <= 10.0 {
                return target;
            }
        }
    }
    for (index, anchor) in state.snapshot.curve.anchors.iter().enumerate() {
        let screen = graph_to_screen(
            rect,
            MotionGraphPoint::new(anchor.time, anchor.progress),
            viewport,
        );
        if (point.x - screen.x).abs() <= 10.0 && (point.y - screen.y).abs() <= 10.0 {
            return MotionEditorTarget::Anchor(index);
        }
    }
    let playhead = graph_to_screen(
        rect,
        MotionGraphPoint::new(state.snapshot.playhead, viewport.progress_start()),
        viewport,
    );
    if (point.x - playhead.x).abs() <= 7.0 {
        return MotionEditorTarget::Playhead;
    }
    let graph = screen_to_graph(point, rect, viewport);
    let curve_progress = state.compiled.sample(graph.time);
    let curve = graph_to_screen(
        rect,
        MotionGraphPoint::new(graph.time, curve_progress),
        viewport,
    );
    if (point.y - curve.y).abs() <= 10.0 {
        MotionEditorTarget::Curve
    } else {
        MotionEditorTarget::Playhead
    }
}

#[derive(Debug, Clone, Copy)]
struct ViewportGestureSample {
    position: Point,
    delta: Point,
    modifiers: Modifiers,
    rect: Rect,
    viewport: MotionGraphViewport,
}

impl ViewportGestureSample {
    const fn new(
        position: Point,
        delta: Point,
        modifiers: Modifiers,
        rect: Rect,
        viewport: MotionGraphViewport,
    ) -> Self {
        Self {
            position,
            delta,
            modifiers,
            rect,
            viewport,
        }
    }
}

fn viewport_input(
    id: MotionEditorEditId,
    phase: MotionEditorGesturePhase,
    device: MotionEditorDevice,
    sample: ViewportGestureSample,
) -> MotionEditorInput {
    let anchor = screen_to_graph(sample.position, sample.rect, sample.viewport);
    let time_span = sample.viewport.time_end() - sample.viewport.time_start();
    let progress_span = sample.viewport.progress_end() - sample.viewport.progress_start();
    let (translation, time_scale, progress_scale) = if sample.modifiers.control {
        let scale = (-f64::from(sample.delta.y) * 0.012).exp().clamp(0.25, 4.0);
        (MotionGraphPoint::default(), scale, scale)
    } else {
        let horizontal = sample.delta.x
            + if sample.modifiers.shift {
                sample.delta.y
            } else {
                0.0
            };
        let vertical = if sample.modifiers.shift {
            0.0
        } else {
            sample.delta.y
        };
        (
            MotionGraphPoint::new(
                f64::from(horizontal / sample.rect.width.max(1.0)) * time_span,
                f64::from(vertical / sample.rect.height.max(1.0)) * progress_span,
            ),
            1.0,
            1.0,
        )
    };
    MotionEditorInput::Viewport(MotionEditorViewportInput {
        id,
        phase,
        device,
        anchor,
        translation,
        time_scale,
        progress_scale,
    })
}

fn editor_modifiers(modifiers: Modifiers) -> MotionEditorModifiers {
    MotionEditorModifiers {
        shift: modifiers.shift,
        control: modifiers.control,
        alt: modifiers.alt,
        logo: modifiers.logo,
    }
}

fn editor_key(key: &Key) -> Option<MotionEditorKey> {
    Some(match key {
        Key::Tab => MotionEditorKey::Tab,
        Key::Enter => MotionEditorKey::Enter,
        Key::Space => MotionEditorKey::Space,
        Key::Escape => MotionEditorKey::Escape,
        Key::ArrowLeft => MotionEditorKey::ArrowLeft,
        Key::ArrowRight => MotionEditorKey::ArrowRight,
        Key::ArrowUp => MotionEditorKey::ArrowUp,
        Key::ArrowDown => MotionEditorKey::ArrowDown,
        Key::Home => MotionEditorKey::Home,
        Key::End => MotionEditorKey::End,
        Key::Backspace => MotionEditorKey::Backspace,
        Key::Delete => MotionEditorKey::Delete,
        Key::Character(value) => {
            let mut characters = value.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            MotionEditorKey::Character(character)
        }
        Key::PageUp | Key::PageDown | Key::Named(_) => return None,
    })
}

fn apply_editor_outcome(ctx: &mut EventCtx<'_>, outcome: MotionEditorInputOutcome) {
    match outcome.clipboard {
        Some(MotionEditorClipboardAction::WriteText(text)) => ctx.write_clipboard_text(text),
        Some(MotionEditorClipboardAction::ReadText) => ctx.read_clipboard_text(),
        None => {}
    }
    if outcome.preview_pending {
        ctx.request_animation_frame();
    }
}

fn draw_segment(
    ctx: &mut PaintCtx<'_>,
    first: Point,
    last: Point,
    width: f32,
    color: Color,
) -> Result<(), UiError> {
    let delta_x = last.x - first.x;
    let delta_y = last.y - first.y;
    let length = delta_x.hypot(delta_y);
    if length <= f32::EPSILON {
        return Ok(());
    }
    let transform = Transform::translation(first.x, first.y)
        .concat(Transform::rotation(delta_y.atan2(delta_x)));
    ctx.builder().with_transform(transform, |builder| {
        builder.rounded_rect(
            Rect::new(0.0, -width * 0.5, length + 0.5, width),
            CornerRadii::all(width * 0.5),
            color,
        )
    })?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct PreviewLayout;

impl Widget for PreviewLayout {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 2 {
            return Err(UiError::ChildCountMismatch {
                expected: 2,
                actual: ctx.child_count(),
            });
        }
        let size = constraints.max();
        let stage_height = (size.height - PREVIEW_RAIL_HEIGHT).max(0.0);
        ctx.measure_child(
            0,
            Constraints::tight(Size::new(size.width, PREVIEW_RAIL_HEIGHT))?,
        )?;
        ctx.measure_child(1, Constraints::tight(Size::new(size.width, stage_height))?)?;
        Ok(constraints.constrain(size))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        ctx.arrange_child(
            0,
            Rect::new(rect.x, rect.y, rect.width, PREVIEW_RAIL_HEIGHT),
        )?;
        ctx.arrange_child(
            1,
            Rect::new(
                rect.x,
                rect.y + PREVIEW_RAIL_HEIGHT,
                rect.width,
                (rect.height - PREVIEW_RAIL_HEIGHT).max(0.0),
            ),
        )
    }
}

#[derive(Debug, Clone)]
struct PreviewStage {
    theme: Arc<Theme>,
    session: MotionEditorSession,
}

impl Widget for PreviewStage {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths(["palette.edge", "palette.inverse_edge"])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        self.session.watch_visual_revision(ctx);
        let rect = ctx.rect();
        ctx.builder().rect(
            Rect::new(rect.x, rect.y, rect.width, 1.0),
            with_alpha(self.theme.palette.edge, 0.045),
        )?;
        ctx.builder().rect(
            Rect::new(rect.x, rect.bottom() - 1.0, rect.width, 1.0),
            with_alpha(self.theme.palette.inverse_edge, 0.10),
        )?;
        let state = self.session.render_state();
        let progress = state.compiled.sample(state.snapshot.playhead) as f32;
        let translation = (1.0 - progress) * 44.0;
        if ctx.child_count() == 1 {
            ctx.paint_child_translated(0, 0.0, translation)?;
        }
        Ok(())
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            role: SemanticRole::Group,
            label: Some("真实设置面板动画预览".to_owned()),
            ..Semantics::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MotionDivider {
    color: Color,
}

impl Widget for MotionDivider {
    fn measure(
        &self,
        _ctx: &mut MeasureCtx<'_>,
        constraints: Constraints,
    ) -> Result<Size, UiError> {
        Ok(constraints.constrain(Size::new(constraints.max().width, 1.0)))
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        ctx.builder()
            .rect(Rect::new(rect.x, rect.y, rect.width, 1.0), self.color)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_review_curve_visibly_overshoots_then_returns_to_the_endpoint() {
        let curve = overshoot_curve();
        let maximum = (0..=1_000)
            .map(|step| curve.sample(step as f32 / 1_000.0))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(maximum > 1.09);
        assert!(maximum < 1.2);
        assert_eq!(curve.sample(1.0), 1.0);
    }

    #[test]
    fn editor_session_keeps_authored_state_across_view_recomposition() {
        let revision = Reactive::new(1);
        let session = MotionEditorSession::new(revision.clone());
        let initial = session.snapshot();
        let point = MotionGraphPoint::new(0.5, session.render_state().compiled.sample(0.5));
        let outcome = session
            .handle(MotionEditorInput::Direct(MotionEditorDirectInput {
                id: session.next_edit_id(),
                phase: MotionEditorGesturePhase::Begin,
                device: MotionEditorDevice::Mouse,
                target: MotionEditorTarget::Curve,
                position: point,
                modifiers: MotionEditorModifiers::default(),
                activation_count: 2,
                snapping: true,
            }))
            .unwrap();

        assert!(outcome.document_changed);
        assert_eq!(session.snapshot().curve.anchors.len(), 3);
        assert!(session.snapshot().document_generation > initial.document_generation);
        assert!(revision.get() > 1);

        let recomposed = workspace(
            Arc::new(Theme::default()),
            MaterialCapabilities::default(),
            session.clone(),
        )
        .unwrap();
        drop(recomposed);
        assert_eq!(session.snapshot().curve.anchors.len(), 3);
        assert!(session.snapshot().can_undo);
    }

    #[test]
    fn editor_session_playback_uses_host_time_and_reaches_the_return_frame() {
        let session = MotionEditorSession::new(Reactive::new(1));
        let _ = session
            .handle(MotionEditorInput::Key {
                key: MotionEditorKey::Space,
                modifiers: MotionEditorModifiers::default(),
                repeat: false,
                now: Duration::from_secs(4),
            })
            .unwrap();
        assert!(matches!(
            session.snapshot().playback,
            MotionEditorPlayback::Playing { .. }
        ));

        assert!(session.advance_playback(Duration::from_millis(4_280)));
        let final_state = session.render_state();
        assert_eq!(final_state.snapshot.playhead, 1.0);
        assert_eq!(final_state.compiled.sample(1.0), 1.0);
        assert_eq!(final_state.snapshot.playback, MotionEditorPlayback::Paused);
    }

    #[test]
    fn inspector_values_are_authoritative_validated_and_reset_together() {
        let revision = Reactive::new(1);
        let session = MotionEditorSession::new(revision.clone());

        session.submit_duration("420 ms");
        let duration = session.snapshot();
        assert_eq!(duration.duration, Duration::from_millis(420));
        assert_eq!(
            duration.duration_source,
            nkdhr_ui::MotionCurveSource::Explicit
        );
        assert_eq!(session.duration_text().get(), "420 ms");
        assert_eq!(session.duration_status().get(), TextInputStatus::Valid);

        let document_generation = duration.document_generation;
        session.submit_duration("60001");
        assert_eq!(
            session.snapshot().document_generation,
            document_generation,
            "an invalid duration must preserve the last-good document"
        );
        assert!(matches!(
            session.duration_status().get(),
            TextInputStatus::Invalid(_)
        ));

        session.set_fluid_material(FluidMaterialField::Viscosity, 81.0);
        session.set_fluid_material(FluidMaterialField::SurfaceTension, 63.0);
        session.set_fluid_material(FluidMaterialField::Attraction, 74.0);
        assert_eq!(
            session.fluid_material(),
            FluidMaterialValues {
                viscosity: 81.0,
                surface_tension: 63.0,
                attraction: 74.0,
            }
        );
        let overrides = session.fluid_overrides();
        assert_eq!(overrides.viscosity, Some(81.0 / 25.0));
        assert_eq!(overrides.surface_tension, Some(63.0 / 25.0));
        assert_eq!(overrides.attraction, Some(74.0 / 25.0));
        assert!(revision.get() > 1);

        session.reset();
        assert_eq!(session.fluid_material(), FluidMaterialValues::default());
        assert!(session.fluid_overrides().is_empty());
        assert_eq!(session.snapshot().duration, Duration::from_millis(280));
        assert_eq!(session.duration_text().get(), "280 ms");
    }
}

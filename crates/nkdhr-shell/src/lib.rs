//! Output-local shell UI. Unlike canvas nodes, these surfaces remain fixed to
//! one physical display while its workspace viewport moves underneath them.

mod app_chain;

pub use app_chain::{
    AppChainModel, AppGroup, AppPageEncoding, AppSplitSnapshot, ChainNode, PreviewNode,
    SpatialRect, SplitDirection, SwitcherPhase, SwitcherRelease, WindowSnapshot, canonical_app_key,
};

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use nkdhr_render::{Color, CornerRadii, DisplayList, Rect, TextureStore, Transform};
use nkdhr_ui::text::{TextConfig, TextResources, TextStyle, TextWrap};
use nkdhr_ui::{
    Align, Alignment, ArrangeCtx, Constraints, DispatchResult, Element, EventCtx, GlassSurface,
    Insets, Invalidation, MaterialCapabilities, MaterialTier, MeasureCtx, MotionFeature,
    MotionPropertyDomain, MotionScopeData, MotionSemanticFamilyData, Padding, PaintCtx,
    PointerButton, Reactive, SelectionMassMotion, Size, Stack, SurfaceState, Text, TextRole, Theme,
    ThemeRuntime, UiError, UiEvent, UiHost, UiResult, UiRoot, UiSurface, UpdateCtx, Widget,
    WidgetId, paint_fluid_surface,
};

/// The eight shell attachment zones are stable semantic identities. A zone may
/// be hidden without removing its identity, so plugins and motion histories do
/// not depend on incidental layout nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeRegion {
    TopLeft,
    TopCenter,
    TopRight,
    LeftCenter,
    RightCenter,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppChainIntent {
    SelectWindow(u64),
}

/// First production output-local surface. Only the already-defined calm clock
/// state is composed here; the other seven regions attach to this same host as
/// their product models become available.
pub struct ShellSurface {
    clock_text: Reactive<String>,
    theme_runtime: ThemeRuntime,
    seen_theme_generation: u64,
    viewport: Size,
    /// What this host can actually draw, not what the user wants. The user's
    /// accessibility preferences live in the theme and are resolved against
    /// this on every read, so a live profile change takes effect without
    /// rebuilding the surface.
    host_backdrop_blur: bool,
    app_chain: AppChainVisual,
    selection_motion: Option<SelectionMassMotion>,
    selection_target: Option<u64>,
    motion_started_at: Instant,
    app_chain_intents: Rc<RefCell<VecDeque<AppChainIntent>>>,
    host: UiHost,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct AppChainVisual {
    nodes: Vec<ChainNode>,
    preview: Vec<PreviewNode>,
    phase: SwitcherPhase,
    selected: Option<u64>,
    selection_mass: Vec<SelectionVisualMass>,
}

#[derive(Debug, Clone, PartialEq)]
struct SelectionVisualMass {
    window: u64,
    mass: f32,
    velocity: f32,
}

/// The motion policy decisions the chain's procedural painting depends on,
/// resolved once per composition from the live runtime.
///
/// These are the only two effects that run without a state change, so they are
/// the only two that can keep requesting frames forever. Reduced and Off deny
/// both, which must also stop the animation requests — otherwise a user who
/// turned motion off still pays for a compositor that never goes idle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ChainMotion {
    /// Continuous water inside an aggregate node.
    idle_fluid: bool,
    /// Pulsing of the selected preview node.
    oscillation: bool,
}

impl ChainMotion {
    fn resolve(runtime: &nkdhr_ui::MotionRuntimeProfile) -> Self {
        Self {
            idle_fluid: runtime.allows(MotionFeature::IdleFluid),
            oscillation: runtime.allows(MotionFeature::Oscillation),
        }
    }
}

impl ShellSurface {
    pub fn new(
        viewport: Size,
        output_scale: f32,
        host_backdrop_blur: bool,
        theme_runtime: ThemeRuntime,
    ) -> UiResult<Self> {
        let text = TextResources::from_config(TextConfig::default(), output_scale)
            .map_err(|error| UiError::Text(error.to_string()))?;
        Self::with_text_resources(
            viewport,
            output_scale,
            host_backdrop_blur,
            theme_runtime,
            text,
        )
    }

    fn with_text_resources(
        viewport: Size,
        output_scale: f32,
        host_backdrop_blur: bool,
        theme_runtime: ThemeRuntime,
        text: TextResources,
    ) -> UiResult<Self> {
        let clock_text = Reactive::new(local_clock_text());
        let app_chain_intents = Rc::new(RefCell::new(VecDeque::new()));
        let snapshot = theme_runtime.snapshot();
        let app_chain = AppChainVisual::default();
        let element = shell_element(
            &snapshot.theme(),
            &clock_text,
            snapshot.theme().material_capabilities(host_backdrop_blur),
            &app_chain,
            ChainMotion::resolve(&snapshot.motion_runtime()),
            Rc::clone(&app_chain_intents),
        );
        let mut root = UiRoot::with_text(element, text)?;
        root.set_theme_runtime(theme_runtime.clone());
        let host = UiHost::new(root, viewport, output_scale)?;
        Ok(Self {
            clock_text,
            theme_runtime,
            seen_theme_generation: snapshot.generation(),
            viewport,
            host_backdrop_blur,
            app_chain,
            selection_motion: None,
            selection_target: None,
            motion_started_at: Instant::now(),
            app_chain_intents,
            host,
        })
    }

    fn refresh(&mut self, viewport: Size) -> UiResult<()> {
        let clock = local_clock_text();
        if self.clock_text.get() != clock {
            self.clock_text.set(clock);
        }
        let snapshot = self.theme_runtime.snapshot();
        if self.viewport != viewport || self.seen_theme_generation != snapshot.generation() {
            let capabilities = snapshot
                .theme()
                .material_capabilities(self.host_backdrop_blur);
            self.host.reconcile(shell_element(
                &snapshot.theme(),
                &self.clock_text,
                capabilities,
                &self.app_chain,
                ChainMotion::resolve(&snapshot.motion_runtime()),
                Rc::clone(&self.app_chain_intents),
            ))?;
            self.viewport = viewport;
            self.seen_theme_generation = snapshot.generation();
        }
        Ok(())
    }

    pub fn sync_app_chain(
        &mut self,
        nodes: Vec<ChainNode>,
        preview: Vec<PreviewNode>,
        phase: SwitcherPhase,
        selected: Option<u64>,
    ) -> UiResult<()> {
        let now = self.motion_started_at.elapsed();
        if self.selection_target != selected {
            match selected {
                Some(target) => {
                    let identity = selection_identity(target);
                    match self.selection_motion.as_mut() {
                        Some(motion) => {
                            let snapshot = self.theme_runtime.snapshot();
                            let runtime = snapshot.motion_runtime();
                            let scope = MotionScopeData::transition(
                                MotionSemanticFamilyData::ListTransfer,
                                "shell.app-chain",
                                "selection",
                            );
                            let spec = runtime
                                .resolve(&scope, MotionPropertyDomain::Spatial)
                                .map_err(|error| UiError::Text(error.to_string()))?;
                            if runtime.allows(MotionFeature::FluidTopology) && !spec.is_immediate()
                            {
                                let _ = motion.retarget(now, identity, spec);
                            } else {
                                motion.settle(identity);
                            }
                        }
                        None => {
                            self.selection_motion = Some(
                                SelectionMassMotion::new(identity, 1.0)
                                    .map_err(|error| UiError::Text(error.to_string()))?,
                            );
                        }
                    }
                }
                None => self.selection_motion = None,
            }
            self.selection_target = selected;
        }
        let selection_mass = self
            .selection_motion
            .as_mut()
            .map(|motion| motion.advance(now).sample)
            .into_iter()
            .flat_map(|sample| sample.entries)
            .filter_map(|entry| {
                parse_selection_identity(&entry.id).map(|window| SelectionVisualMass {
                    window,
                    mass: entry.mass as f32,
                    velocity: entry.velocity as f32,
                })
            })
            .collect();
        let next = AppChainVisual {
            nodes,
            preview,
            phase,
            selected,
            selection_mass,
        };
        if self.app_chain == next {
            return Ok(());
        }
        self.app_chain = next;
        let snapshot = self.theme_runtime.snapshot();
        let capabilities = snapshot
            .theme()
            .material_capabilities(self.host_backdrop_blur);
        self.host.reconcile(shell_element(
            &snapshot.theme(),
            &self.clock_text,
            capabilities,
            &self.app_chain,
            ChainMotion::resolve(&snapshot.motion_runtime()),
            Rc::clone(&self.app_chain_intents),
        ))?;
        Ok(())
    }

    pub fn take_app_chain_intents(&mut self) -> Vec<AppChainIntent> {
        self.app_chain_intents.borrow_mut().drain(..).collect()
    }
}

impl UiSurface for ShellSurface {
    fn render(&mut self, logical_size: Size, output_scale: f32) -> UiResult<()> {
        self.refresh(logical_size)?;
        self.host.resize(logical_size, output_scale)?;
        self.host.render().map(|_| ())
    }

    fn display_list(&self) -> &DisplayList {
        self.host.display_list()
    }

    fn textures(&self) -> &TextureStore {
        self.host.textures()
    }

    fn commit(&self) -> u64 {
        self.host.commit()
    }

    fn dispatch(&mut self, event: &UiEvent) -> UiResult<DispatchResult> {
        self.host.dispatch(event)
    }

    fn pointer_capture(&self) -> Option<WidgetId> {
        self.host.pointer_capture()
    }

    fn keyboard_focus(&self) -> Option<WidgetId> {
        self.host.keyboard_focus()
    }

    fn frame_requested(&mut self) -> bool {
        let clock = local_clock_text();
        if self.clock_text.get() != clock {
            self.clock_text.set(clock);
        }
        let selection_running = self.selection_motion.as_ref().is_some_and(|motion| {
            motion
                .sample(self.motion_started_at.elapsed())
                .active_run
                .is_some()
        });
        self.theme_runtime.snapshot().generation() != self.seen_theme_generation
            || selection_running
            || self.host.frame_requested()
    }
}

fn selection_identity(window: u64) -> Arc<str> {
    Arc::from(format!("window:{window}"))
}

fn parse_selection_identity(identity: &str) -> Option<u64> {
    identity.strip_prefix("window:")?.parse().ok()
}

fn shell_element(
    theme: &std::sync::Arc<nkdhr_ui::Theme>,
    clock_text: &Reactive<String>,
    capabilities: MaterialCapabilities,
    app_chain: &AppChainVisual,
    motion: ChainMotion,
    app_chain_intents: Rc<RefCell<VecDeque<AppChainIntent>>>,
) -> Element {
    let token = theme.typography.token(TextRole::Mono);
    let style = TextStyle {
        families: theme.typography.families.mono.clone(),
        weight: token.weight,
        font_size: token.font_size,
        line_height: token.line_height,
        wrap: TextWrap::None,
        ..TextStyle::default()
    };
    let clock = Element::new(InputShield).keyed(3_u64).child(
        Element::new(
            GlassSurface::new(theme.clone(), MaterialTier::CompactNode)
                .capabilities(capabilities)
                .radius(14.0)
                .padding(Insets::symmetric(14.0, 6.0)),
        )
        .keyed(4_u64)
        .child(
            Element::new(Text::bound(
                clock_text.clone(),
                style,
                theme.palette.text_primary,
            ))
            .keyed(5_u64),
        ),
    );
    let app_chain_rail = Element::new(InputShield)
        .keyed(10_u64)
        .child(app_chain_element(theme, capabilities, app_chain, motion));
    let mut shell = Element::new(Stack)
        .keyed(1_u64)
        .child(
            Element::new(Align {
                horizontal: Alignment::Center,
                vertical: Alignment::Start,
            })
            .keyed(2_u64)
            .child(
                Element::new(Padding {
                    insets: Insets::new(0.0, 14.0, 0.0, 0.0),
                })
                .child(clock),
            ),
        )
        .child(
            Element::new(Align {
                horizontal: Alignment::Start,
                vertical: Alignment::Start,
            })
            .keyed(9_u64)
            .child(
                Element::new(Padding {
                    insets: Insets::new(14.0, 14.0, 0.0, 0.0),
                })
                .child(app_chain_rail),
            ),
        );
    if app_chain.phase == SwitcherPhase::Expanded {
        shell = shell.child(
            Element::new(Align {
                horizontal: Alignment::Start,
                vertical: Alignment::Start,
            })
            .keyed(20_u64)
            .child(
                Element::new(Padding {
                    insets: Insets::new(14.0, 76.0, 0.0, 0.0),
                })
                .child(Element::new(InputShield).keyed(21_u64).child(
                    app_preview_element(theme, capabilities, app_chain, motion, app_chain_intents),
                )),
            ),
        );
    }
    shell
}

fn app_preview_element(
    theme: &Arc<Theme>,
    capabilities: MaterialCapabilities,
    visual: &AppChainVisual,
    motion: ChainMotion,
    intents: Rc<RefCell<VecDeque<AppChainIntent>>>,
) -> Element {
    let token = theme.typography.token(TextRole::Mono);
    let style = TextStyle {
        families: theme.typography.families.mono.clone(),
        weight: token.weight,
        font_size: 15.0,
        line_height: 19.0,
        wrap: TextWrap::None,
        ..TextStyle::default()
    };
    let mut element = Element::new(AppPreviewChrome {
        nodes: visual.preview.clone(),
        selection_mass: visual.selection_mass.clone(),
        motion,
        intents,
        theme: Arc::clone(theme),
        capabilities,
    })
    .keyed(22_u64);
    for node in &visual.preview {
        element = element.child(
            Element::new(Align {
                horizontal: Alignment::Center,
                vertical: Alignment::Center,
            })
            .keyed(node.window.wrapping_add(30))
            .child(Element::new(Text::new(
                app_glyph(&node.app_key),
                style.clone(),
                theme.palette.text_primary,
            ))),
        );
    }
    element
}

fn app_chain_element(
    theme: &Arc<Theme>,
    capabilities: MaterialCapabilities,
    visual: &AppChainVisual,
    motion: ChainMotion,
) -> Element {
    let token = theme.typography.token(TextRole::Mono);
    let style = TextStyle {
        families: theme.typography.families.mono.clone(),
        weight: token.weight,
        font_size: 14.0,
        line_height: 18.0,
        wrap: TextWrap::None,
        ..TextStyle::default()
    };
    let chrome = AppChainChrome {
        nodes: visual.nodes.clone(),
        selected: visual.selected,
        selection_mass: visual.selection_mass.clone(),
        motion,
        theme: Arc::clone(theme),
        capabilities,
    };
    let mut element = Element::new(chrome).keyed(11_u64).child(
        Element::new(Align {
            horizontal: Alignment::Center,
            vertical: Alignment::Center,
        })
        .keyed(12_u64)
        .child(Element::new(Text::new(
            "●",
            style.clone(),
            theme.palette.text_primary,
        ))),
    );
    for (index, node) in visual.nodes.iter().enumerate() {
        let label = match node {
            ChainNode::Application(group) => app_glyph(&group.app_key),
            ChainNode::Overflow { .. } => "…".to_owned(),
        };
        element = element.child(
            Element::new(Align {
                horizontal: Alignment::Center,
                vertical: Alignment::Center,
            })
            .keyed(stable_node_key(node, index))
            .child(Element::new(Text::new(
                label,
                style.clone(),
                theme.palette.text_primary,
            ))),
        );
    }
    element
}

fn app_glyph(app_key: &str) -> String {
    app_key
        .rsplit(['.', '-', '_'])
        .find_map(|part| part.chars().find(char::is_ascii_alphanumeric))
        .or_else(|| app_key.chars().find(char::is_ascii_alphanumeric))
        .map(|character| character.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned())
}

fn stable_node_key(node: &ChainNode, index: usize) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match node {
        ChainNode::Application(group) => group.app_key.hash(&mut hasher),
        ChainNode::Overflow { .. } => "nkdhr-overflow".hash(&mut hasher),
    }
    hasher.finish() ^ (u64::try_from(index).unwrap_or(u64::MAX) << 32)
}

#[derive(Clone)]
struct AppChainChrome {
    nodes: Vec<ChainNode>,
    selected: Option<u64>,
    selection_mass: Vec<SelectionVisualMass>,
    motion: ChainMotion,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
}

impl AppChainChrome {
    const NODE: f32 = 42.0;
    const GAP: f32 = 3.0;
    const LOGO_GAP: f32 = 11.0;

    fn node_x(index: usize) -> f32 {
        if index == 0 {
            0.0
        } else {
            Self::NODE + Self::LOGO_GAP + (index - 1) as f32 * (Self::NODE + Self::GAP)
        }
    }

    fn application_mass(&self, index: usize) -> f32 {
        let Some(ChainNode::Application(group)) = self.nodes.get(index) else {
            return 0.0;
        };
        group
            .windows
            .iter()
            .map(|window| {
                self.selection_mass
                    .iter()
                    .filter(|entry| entry.window == *window)
                    .map(|entry| entry.mass)
                    .sum::<f32>()
            })
            .sum::<f32>()
            .clamp(0.0, 1.0)
    }
}

impl Widget for AppChainChrome {
    fn create_state(&self) -> Box<dyn Any> {
        Box::new(())
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        if previous.nodes != self.nodes
            || previous.selected != self.selected
            || previous.selection_mass != self.selection_mass
            || previous.motion != self.motion
            || !Arc::ptr_eq(&previous.theme, &self.theme)
            || previous.capabilities != self.capabilities
        {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::PAINT);
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> UiResult<Size> {
        let count = self.nodes.len() + 1;
        for index in 0..ctx.child_count() {
            ctx.measure_child(
                index,
                Constraints::tight(Size::new(Self::NODE, Self::NODE))?,
            )?;
        }
        let width = if count == 1 {
            Self::NODE
        } else {
            Self::NODE * count as f32
                + Self::LOGO_GAP
                + Self::GAP * self.nodes.len().saturating_sub(1) as f32
        };
        Ok(constraints.constrain(Size::new(width, Self::NODE + 5.0)))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> UiResult<()> {
        for index in 0..ctx.child_count() {
            ctx.arrange_child(
                index,
                Rect::new(rect.x + Self::node_x(index), rect.y, Self::NODE, Self::NODE),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> UiResult<()> {
        let root = ctx.rect();
        let radius = CornerRadii::all(Self::NODE * 0.5);
        if self.nodes.len() > 1 {
            for index in 1..self.nodes.len() {
                let left = root.x + Self::node_x(index);
                let right = root.x + Self::node_x(index + 1) + 2.0;
                let connector = Rect::new(
                    left + Self::NODE - 2.0,
                    root.y + 17.0,
                    right - left - Self::NODE + 4.0,
                    8.0,
                );
                let transfer = self
                    .application_mass(index - 1)
                    .min(self.application_mass(index));
                ctx.builder().rounded_rect(
                    connector,
                    CornerRadii::all(4.0),
                    with_alpha(self.theme.palette.accent_secondary, 0.30),
                )?;
                if transfer > 0.001 {
                    let thickness = 8.0 + transfer * 5.0;
                    ctx.builder().rounded_rect(
                        Rect::new(
                            connector.x,
                            connector.y + connector.height * 0.5 - thickness * 0.5,
                            connector.width,
                            thickness,
                        ),
                        CornerRadii::all(thickness * 0.5),
                        with_alpha(self.theme.palette.accent, 0.38 + transfer * 0.30),
                    )?;
                    ctx.request_animation_frame();
                }
            }
        }
        for (index, node) in std::iter::once(None)
            .chain(self.nodes.iter().map(Some))
            .enumerate()
        {
            let rect = Rect::new(root.x + Self::node_x(index), root.y, Self::NODE, Self::NODE);
            let (aggregate, fill, selected, selection_mass) = match node {
                None => (false, 0.0, false, 0.0),
                Some(ChainNode::Application(group)) => (
                    group.windows.len() > 1,
                    group.water_fill,
                    self.selected
                        .is_some_and(|selected| group.windows.contains(&selected)),
                    self.application_mass(index - 1),
                ),
                Some(ChainNode::Overflow { .. }) => (true, 0.0, false, 0.0),
            };
            if aggregate {
                paint_fluid_surface(
                    ctx.builder(),
                    Rect::new(rect.x + 3.0, rect.y + 4.0, rect.width, rect.height),
                    radius,
                    &self.theme,
                    self.capabilities,
                    SurfaceState::default(),
                )?;
            }
            paint_fluid_surface(
                ctx.builder(),
                rect,
                radius,
                &self.theme,
                self.capabilities,
                SurfaceState {
                    selected: selected || selection_mass > 0.001,
                    accented: selected || selection_mass > 0.001,
                    ..SurfaceState::default()
                },
            )?;
            if selection_mass > 0.001 {
                ctx.builder().rounded_rect(
                    rect.inset(3.0),
                    CornerRadii::all((Self::NODE - 6.0) * 0.5),
                    with_alpha(self.theme.palette.accent, 0.08 + selection_mass * 0.20),
                )?;
            }
            if fill > 0.0 {
                let wave = if self.motion.idle_fluid {
                    (ctx.now().as_secs_f32() * 2.2 + index as f32 * 0.73).sin() * 1.2
                } else {
                    0.0
                };
                let water_top = rect.y + rect.height * (1.0 - fill) + wave;
                ctx.builder().with_clip(
                    Rect::new(
                        rect.x,
                        water_top,
                        rect.width,
                        (rect.bottom() - water_top).max(0.0),
                    ),
                    |builder| {
                        builder.rounded_rect(
                            rect,
                            radius,
                            with_alpha(self.theme.palette.accent, 0.34),
                        )
                    },
                )?;
                if self.motion.idle_fluid {
                    ctx.request_animation_frame();
                }
            }
            if let Some(ChainNode::Application(group)) = node {
                paint_page_encoding(ctx.builder(), rect, group.page_encoding, &self.theme)?;
            }
        }
        ctx.paint_children()
    }
}

fn paint_page_encoding(
    builder: &mut nkdhr_render::DisplayListBuilder,
    rect: Rect,
    encoding: AppPageEncoding,
    theme: &Theme,
) -> Result<(), nkdhr_render::BuildError> {
    let points = match encoding {
        AppPageEncoding::Single => return Ok(()),
        AppPageEncoding::Dots(count) => (0..count)
            .map(|index| (index as f32 - (count - 1) as f32 * 0.5, 0.0))
            .collect::<Vec<_>>(),
        AppPageEncoding::Star => vec![
            (0.0, -2.0),
            (-2.0, 0.0),
            (2.0, 0.0),
            (-1.3, 2.2),
            (1.3, 2.2),
        ],
    };
    for (x, y) in points {
        builder.rounded_rect(
            Rect::new(
                rect.x + rect.width * 0.5 + x * 3.1 - 1.1,
                rect.bottom() - 7.0 + y - 1.1,
                2.2,
                2.2,
            ),
            CornerRadii::all(1.1),
            with_alpha(theme.palette.text_primary, 0.86),
        )?;
    }
    Ok(())
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    let [red, green, blue, _] = color.components();
    Color::new(red, green, blue, alpha.clamp(0.0, 1.0))
        .expect("theme colors and clamped alpha are finite")
}

#[derive(Clone)]
struct AppPreviewChrome {
    nodes: Vec<PreviewNode>,
    selection_mass: Vec<SelectionVisualMass>,
    motion: ChainMotion,
    intents: Rc<RefCell<VecDeque<AppChainIntent>>>,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
}

impl AppPreviewChrome {
    const WIDTH: f32 = 430.0;
    const HEIGHT: f32 = 292.0;
    const NODE: f32 = 48.0;
    const INSET_X: f32 = 38.0;
    const INSET_Y: f32 = 34.0;

    fn center(&self, panel: Rect, node: &PreviewNode) -> (f32, f32) {
        (
            panel.x + Self::INSET_X + node.center_x * (Self::WIDTH - Self::INSET_X * 2.0),
            panel.y + Self::INSET_Y + node.center_y * (Self::HEIGHT - Self::INSET_Y * 2.0),
        )
    }

    fn window_mass(&self, window: u64) -> f32 {
        self.selection_mass
            .iter()
            .find(|entry| entry.window == window)
            .map_or(0.0, |entry| entry.mass.clamp(0.0, 1.0))
    }

    fn transfer_running(&self) -> bool {
        self.selection_mass.len() > 1
            || self
                .selection_mass
                .iter()
                .any(|entry| entry.velocity.abs() > 0.001)
    }
}

impl Widget for AppPreviewChrome {
    fn create_state(&self) -> Box<dyn Any> {
        Box::new(())
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        if previous.nodes != self.nodes
            || previous.selection_mass != self.selection_mass
            || previous.motion != self.motion
            || !Rc::ptr_eq(&previous.intents, &self.intents)
            || !Arc::ptr_eq(&previous.theme, &self.theme)
            || previous.capabilities != self.capabilities
        {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::PAINT);
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> UiResult<Size> {
        for index in 0..ctx.child_count() {
            ctx.measure_child(
                index,
                Constraints::tight(Size::new(Self::NODE, Self::NODE))?,
            )?;
        }
        Ok(constraints.constrain(Size::new(Self::WIDTH, Self::HEIGHT)))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> UiResult<()> {
        for (index, node) in self.nodes.iter().enumerate() {
            let (x, y) = self.center(rect, node);
            ctx.arrange_child(
                index,
                Rect::new(
                    x - Self::NODE * 0.5,
                    y - Self::NODE * 0.5,
                    Self::NODE,
                    Self::NODE,
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> UiResult<()> {
        let panel = ctx.rect();
        paint_fluid_surface(
            ctx.builder(),
            panel,
            CornerRadii::all(28.0),
            &self.theme,
            self.capabilities,
            SurfaceState::default(),
        )?;

        let mut chain_order = self.nodes.iter().collect::<Vec<_>>();
        chain_order.sort_by_key(|node| node.chain_index);
        for pair in chain_order.windows(2) {
            let start = self.center(panel, pair[0]);
            let end = self.center(panel, pair[1]);
            let edge_mass = self
                .window_mass(pair[0].window)
                .min(self.window_mass(pair[1].window));
            let selected_edge = edge_mass > 0.001 || pair[0].selected || pair[1].selected;
            paint_connector(
                ctx.builder(),
                start,
                end,
                if selected_edge {
                    6.0 + edge_mass * 7.0
                } else {
                    4.5
                },
                with_alpha(
                    if selected_edge {
                        self.theme.palette.accent
                    } else {
                        self.theme.palette.accent_secondary
                    },
                    if selected_edge { 0.58 } else { 0.30 },
                ),
            )?;
        }

        if self.transfer_running()
            && let Some(target) = self.nodes.iter().find(|node| node.selected)
        {
            let target_center = self.center(panel, target);
            let target_mass = self.window_mass(target.window);
            for source in self.nodes.iter().filter(|node| {
                node.window != target.window && self.window_mass(node.window) > 0.001
            }) {
                let source_center = self.center(panel, source);
                let progress = target_mass.clamp(0.0, 1.0);
                let center = (
                    source_center.0 + (target_center.0 - source_center.0) * progress,
                    source_center.1 + (target_center.1 - source_center.1) * progress,
                );
                let radius = 5.0 + (std::f32::consts::PI * progress).sin().abs() * 7.0;
                ctx.builder().rounded_rect(
                    Rect::new(
                        center.0 - radius,
                        center.1 - radius,
                        radius * 2.0,
                        radius * 2.0,
                    ),
                    CornerRadii::all(radius),
                    with_alpha(self.theme.palette.accent, 0.62),
                )?;
            }
            ctx.request_animation_frame();
        }

        let mut paint_order = (0..self.nodes.len()).collect::<Vec<_>>();
        paint_order.sort_by_key(|index| self.nodes[*index].stacking_index);
        for index in &paint_order {
            let node = &self.nodes[*index];
            let (x, y) = self.center(panel, node);
            let mass = self.window_mass(node.window);
            let pulse = if self.motion.oscillation && (node.selected || mass > 0.001) {
                (ctx.now().as_secs_f32() * 4.4).sin() * 1.4
            } else {
                0.0
            };
            let size = Self::NODE + pulse + mass * 4.0;
            let rect = Rect::new(x - size * 0.5, y - size * 0.5, size, size);
            paint_fluid_surface(
                ctx.builder(),
                rect,
                CornerRadii::all(size * 0.5),
                &self.theme,
                self.capabilities,
                SurfaceState {
                    selected: node.selected || mass > 0.001,
                    accented: node.selected || mass > 0.001,
                    ..SurfaceState::default()
                },
            )?;
            if self.motion.oscillation && (node.selected || mass > 0.001) {
                ctx.request_animation_frame();
            }
        }
        for index in paint_order {
            ctx.paint_child(index)?;
        }
        Ok(())
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> UiResult<()> {
        if let UiEvent::PointerDown {
            position,
            button: PointerButton::Primary,
            ..
        } = event
            && let Some(node) = self.nodes.iter().rev().find(|node| {
                let (x, y) = self.center(ctx.rect(), node);
                Rect::new(
                    x - Self::NODE * 0.5,
                    y - Self::NODE * 0.5,
                    Self::NODE,
                    Self::NODE,
                )
                .contains(*position)
            })
        {
            self.intents
                .borrow_mut()
                .push_back(AppChainIntent::SelectWindow(node.window));
            ctx.set_handled();
        }
        Ok(())
    }

    fn accepts_pointer(&self) -> bool {
        true
    }
}

fn paint_connector(
    builder: &mut nkdhr_render::DisplayListBuilder,
    start: (f32, f32),
    end: (f32, f32),
    thickness: f32,
    color: Color,
) -> Result<(), nkdhr_render::BuildError> {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length = dx.hypot(dy);
    if length <= f32::EPSILON {
        return Ok(());
    }
    let transform =
        Transform::translation(start.0, start.1).concat(Transform::rotation(dy.atan2(dx)));
    builder.with_transform(transform, |builder| {
        builder.rounded_rect(
            Rect::new(0.0, -thickness * 0.5, length, thickness),
            CornerRadii::all(thickness * 0.5),
            color,
        )
    })
}

/// A visible shell surface is one hit target even if its current child is
/// informational. This prevents clicks in glass from leaking to a client
/// visually behind it.
#[derive(Debug, Clone, Copy)]
struct InputShield;

impl Widget for InputShield {
    fn create_state(&self) -> Box<dyn Any> {
        Box::new(())
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> UiResult<Size> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        if ctx.child_count() == 0 {
            Ok(constraints.min())
        } else {
            ctx.measure_child(0, constraints)
        }
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> UiResult<()> {
        if event.is_pointer() {
            ctx.set_handled();
        }
        Ok(())
    }

    fn accepts_pointer(&self) -> bool {
        true
    }
}

fn local_clock_text() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let epoch = libc::time_t::try_from(seconds).unwrap_or(libc::time_t::MAX);
    let mut local = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: `local` points to writable storage for one `tm`, and `epoch`
    // remains alive for the entire call. `localtime_r` writes only that object.
    let result = unsafe { libc::localtime_r(&epoch, local.as_mut_ptr()) };
    if result.is_null() {
        return "--:--".to_owned();
    }
    // SAFETY: a non-null `localtime_r` return initialized the supplied `tm`.
    let local = unsafe { local.assume_init() };
    format_clock(local.tm_hour, local.tm_min)
}

fn format_clock(hour: i32, minute: i32) -> String {
    format!("{hour:02}:{minute:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::Arc};

    use cosmic_text::{FontSystem, fontdb};
    use nkdhr_render::{
        Color, DisplayListBuilder, Primitive, Rect, TextureStore, software::SoftwareRenderer,
    };
    use nkdhr_ui::text::TextSystem;

    fn fixture_text_resources() -> TextResources {
        let mut database = fontdb::Database::new();
        for bytes in [
            include_bytes!("../../nkdhr-settings/tests/fonts/MapleMonoNF-CN.appearance.subset.ttf")
                .as_slice(),
            include_bytes!(
                "../../nkdhr-settings/tests/fonts/MapleMonoNF-CN-Italic.appearance.subset.ttf"
            )
            .as_slice(),
        ] {
            database.load_font_source(fontdb::Source::Binary(Arc::new(bytes.to_vec())));
        }
        let system = TextSystem::with_font_system(
            FontSystem::new_with_locale_and_db("zh-CN".to_owned(), database),
            TextConfig::default(),
        )
        .expect("fixture text system should initialize");
        TextResources::new(system, TextureStore::new(), 1.0)
            .expect("fixture text resources should initialize")
    }

    #[test]
    fn clock_is_fixed_width_and_zero_padded() {
        assert_eq!(format_clock(0, 5), "00:05");
        assert_eq!(format_clock(23, 59), "23:59");
    }

    #[test]
    fn all_eight_regions_have_stable_distinct_identity() {
        let regions = [
            EdgeRegion::TopLeft,
            EdgeRegion::TopCenter,
            EdgeRegion::TopRight,
            EdgeRegion::LeftCenter,
            EdgeRegion::RightCenter,
            EdgeRegion::BottomLeft,
            EdgeRegion::BottomCenter,
            EdgeRegion::BottomRight,
        ];
        assert_eq!(
            regions
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            8
        );
    }

    #[test]
    fn reduced_transparency_takes_effect_live_on_a_blur_capable_host() {
        let size = Size::new(900.0, 620.0);
        let runtime = ThemeRuntime::default();
        let mut surface = ShellSurface::with_text_resources(
            size,
            1.0,
            true,
            runtime.clone(),
            fixture_text_resources(),
        )
        .unwrap();
        surface.render(size, 1.0).unwrap();
        assert!(
            has_backdrop_blur(&surface),
            "a blur-capable host with default preferences must record real blur"
        );

        // The surface must follow the preference without being rebuilt: a
        // cached capability here is exactly the bug this guards.
        let publication = runtime
            .publish(nkdhr_theme::ThemeProfile {
                overrides: serde_json::json!({"accessibility": {"reduced_transparency": true}}),
                ..nkdhr_theme::ThemeProfile::default()
            })
            .unwrap();
        assert!(publication.was_published());
        surface.render(size, 1.0).unwrap();
        assert!(
            !has_backdrop_blur(&surface),
            "reduced transparency must remove blur from an existing surface"
        );

        // A host that cannot blur is unaffected by the preference either way.
        let opaque_host = ShellSurface::with_text_resources(
            size,
            1.0,
            false,
            ThemeRuntime::default(),
            fixture_text_resources(),
        );
        let mut opaque_host = opaque_host.unwrap();
        opaque_host.render(size, 1.0).unwrap();
        assert!(!has_backdrop_blur(&opaque_host));
    }

    #[test]
    fn reduced_motion_stops_idle_water_and_lets_the_shell_go_idle() {
        let size = Size::new(900.0, 620.0);
        let aggregate = vec![ChainNode::Application(AppGroup {
            app_key: "org.mozilla.firefox".to_owned(),
            windows: vec![1, 2, 3],
            mother: 1,
            page_encoding: AppPageEncoding::Dots(3),
            water_fill: 0.4,
        })];

        let mut standard = ShellSurface::with_text_resources(
            size,
            1.0,
            true,
            ThemeRuntime::default(),
            fixture_text_resources(),
        )
        .unwrap();
        standard
            .sync_app_chain(aggregate.clone(), Vec::new(), SwitcherPhase::Dormant, None)
            .unwrap();
        standard.render(size, 1.0).unwrap();
        assert!(
            standard.host.frame_requested(),
            "water is approved continuous motion under the standard policy"
        );

        let reduced = ThemeRuntime::new(nkdhr_theme::ThemeProfile {
            overrides: serde_json::json!({"motion": {"mode": "reduced"}}),
            ..nkdhr_theme::ThemeProfile::default()
        })
        .unwrap();
        let mut reduced =
            ShellSurface::with_text_resources(size, 1.0, true, reduced, fixture_text_resources())
                .unwrap();
        reduced
            .sync_app_chain(aggregate, Vec::new(), SwitcherPhase::Dormant, None)
            .unwrap();
        reduced.render(size, 1.0).unwrap();
        assert!(
            !reduced.host.frame_requested(),
            "reduced motion must stop requesting frames, not merely look calmer"
        );
    }

    fn has_backdrop_blur(surface: &ShellSurface) -> bool {
        surface
            .display_list()
            .primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
    }

    #[test]
    fn expanded_switcher_paints_spatial_nodes_and_conserves_interrupted_selection() {
        let size = Size::new(900.0, 620.0);
        let mut surface = ShellSurface::with_text_resources(
            size,
            1.0,
            true,
            ThemeRuntime::default(),
            fixture_text_resources(),
        )
        .unwrap();
        let group = ChainNode::Application(AppGroup {
            app_key: "org.mozilla.firefox".to_owned(),
            windows: vec![1, 2, 3],
            mother: 1,
            page_encoding: AppPageEncoding::Dots(3),
            water_fill: 0.4,
        });
        let preview = vec![
            PreviewNode {
                window: 1,
                app_key: "org.mozilla.firefox".to_owned(),
                title: "first".to_owned(),
                center_x: 0.0,
                center_y: 0.0,
                chain_index: 0,
                stacking_index: 0,
                selected: false,
            },
            PreviewNode {
                window: 2,
                app_key: "org.mozilla.firefox".to_owned(),
                title: "second".to_owned(),
                center_x: 1.0,
                center_y: 1.0,
                chain_index: 1,
                stacking_index: 1,
                selected: true,
            },
        ];
        surface
            .sync_app_chain(
                vec![group.clone()],
                preview.clone(),
                SwitcherPhase::Expanded,
                Some(1),
            )
            .unwrap();
        surface
            .sync_app_chain(vec![group], preview, SwitcherPhase::Expanded, Some(2))
            .unwrap();
        let sample = surface
            .selection_motion
            .as_ref()
            .unwrap()
            .sample(surface.motion_started_at.elapsed());
        assert!((sample.entries.iter().map(|entry| entry.mass).sum::<f64>() - 1.0).abs() < 1.0e-12);
        assert!(sample.active_run.is_some());

        surface.render(size, 1.0).unwrap();
        assert!(surface.display_list().primitives().iter().any(|primitive| {
            matches!(
                primitive,
                Primitive::Shape(shape)
                    if shape.rect.width == AppPreviewChrome::WIDTH
                        && shape.rect.height == AppPreviewChrome::HEIGHT
            )
        }));
        assert!(surface.display_list().primitives().iter().any(|primitive| {
            matches!(primitive, Primitive::Shape(shape) if !shape.transform.is_axis_aligned())
        }));
    }

    #[test]
    fn calm_clock_records_glyphs_and_material() {
        let size = Size::new(1280.0, 800.0);
        let mut surface = ShellSurface::new(size, 1.0, true, ThemeRuntime::default())
            .expect("clock surface should initialize");
        surface.render(size, 1.0).expect("clock should render");

        let primitives = surface.display_list().primitives();
        assert!(
            primitives
                .iter()
                .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
        );
        assert!(
            primitives
                .iter()
                .any(|primitive| matches!(primitive, Primitive::Texture(_))),
            "clock text must be recorded as glyph textures"
        );
    }

    #[test]
    fn calm_clock_matches_the_committed_software_golden() {
        const WIDTH: u32 = 360;
        const HEIGHT: u32 = 140;
        let size = Size::new(WIDTH as f32, HEIGHT as f32);
        let mut surface = ShellSurface::with_text_resources(
            size,
            1.0,
            true,
            ThemeRuntime::default(),
            fixture_text_resources(),
        )
        .expect("clock surface should initialize");
        surface.render(size, 1.0).expect("clock should render");
        surface.clock_text.set("21:50".to_owned());
        surface
            .host
            .render()
            .expect("fixed clock value should repaint");
        assert_eq!(
            surface
                .display_list()
                .primitives()
                .iter()
                .filter(|primitive| {
                    matches!(primitive, Primitive::Texture(texture) if texture.rect.x > 120.0)
                })
                .count(),
            5,
            "all five clock glyphs must survive a reactive minute update"
        );
        for texture in surface
            .display_list()
            .primitives()
            .iter()
            .filter_map(|primitive| match primitive {
                Primitive::Texture(texture) => Some(texture),
                _ => None,
            })
        {
            let asset = surface.textures().get(texture.texture).unwrap();
            let source = texture.source.unwrap();
            let mut sum = 0_u64;
            for y in source.y as usize..source.bottom() as usize {
                for x in source.x as usize..source.right() as usize {
                    sum += u64::from(asset.pixels()[y * asset.width() as usize + x]);
                }
            }
            assert!(sum > 0, "every recorded clock glyph needs atlas coverage");
        }

        let mut wallpaper = DisplayListBuilder::new();
        wallpaper
            .rect(
                Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32),
                Color::from_srgba8(18, 23, 42, 255),
            )
            .unwrap();
        for index in 0..9 {
            wallpaper
                .rect(
                    Rect::new(index as f32 * 40.0, 0.0, 20.0, HEIGHT as f32),
                    if index % 2 == 0 {
                        Color::from_srgba8(62, 96, 156, 255)
                    } else {
                        Color::from_srgba8(126, 74, 146, 255)
                    },
                )
                .unwrap();
        }
        let mut renderer = SoftwareRenderer::new(WIDTH, HEIGHT).unwrap();
        renderer.clear(Color::from_srgba8(18, 23, 42, 255));
        renderer
            .render(&wallpaper.finish(), &TextureStore::new(), 1.0)
            .unwrap();
        renderer
            .render(surface.display_list(), surface.textures(), 1.0)
            .unwrap();
        let actual = renderer.ppm();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/calm-clock.ppm");
        if std::env::var_os("UPDATE_GOLDENS").is_some_and(|value| value != "0") {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &actual).unwrap();
        }
        let expected = fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {}: {error}; run UPDATE_GOLDENS=1 cargo test -p nkdhr-shell",
                path.display()
            )
        });
        assert_eq!(actual, expected, "calm clock golden changed");
    }
}

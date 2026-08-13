//! UI-7E owner-reviewable professional motion workspace composition.
//!
//! This module deliberately starts with the P1 resting frame. It uses the
//! production toolkit and motion tokens, but it does not yet bind editing
//! gestures to [`nkdhr_ui::MotionCurveEditor`]. That binding follows visual
//! acceptance of the workspace hierarchy.

use std::sync::Arc;

use nkdhr_render::{Color, CornerRadii, Point, Rect, Shadow, Transform};
use nkdhr_ui::text::FontSlant;
use nkdhr_ui::{
    Align, Alignment, ArrangeCtx, Axis, Button, ButtonVariant, Constraints, CrossAxisAlignment,
    CubicBezier, Element, Flex, Insets, MainAxisAlignment, MaterialCapabilities, MaterialTier,
    MeasureCtx, Padding, PaintCtx, Reactive, SemanticRole, Semantics, SemanticsCtx, Size, Slider,
    SliderError, SurfaceState, Text, TextRole, Theme, ThemeReadSet, Toggle, UiError, Widget,
    paint_fluid_well, resolve_fluid_material_tones,
};

const SCOPE_RAIL_HEIGHT: f32 = 88.0;
const PREVIEW_HEIGHT: f32 = 176.0;
const PREVIEW_RAIL_HEIGHT: f32 = 48.0;
const GRAPH_TOOLBAR_HEIGHT: f32 = 40.0;
const GRAPH_AXIS_HEIGHT: f32 = 28.0;

pub(crate) fn workspace(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
) -> Result<Element, SliderError> {
    let scope = scope_rail(Arc::clone(&theme), capabilities);
    let graph = motion_graph(Arc::clone(&theme), capabilities);
    let preview = preview(Arc::clone(&theme), capabilities);
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
    Element::new(GelSurface::new(
        Arc::clone(&theme),
        MaterialTier::CompactNode,
        capabilities,
        theme.radii.group,
        Insets::ZERO,
        GelElevation::Raised,
    ))
    .child(navigation)
}

pub(crate) fn inspector(
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    drawer: bool,
) -> Result<Element, SliderError> {
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

    let duration = property_row(
        "持续时间",
        "继承",
        Element::new(
            Button::new("280 ms", Arc::clone(&theme))
                .variant(ButtonVariant::Fluid)
                .capabilities(capabilities),
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

    let overshoot = Reactive::new(true);
    let reverse = Reactive::new(false);
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
            Toggle::new("允许越界", overshoot, Arc::clone(&theme)).capabilities(capabilities),
        ),
        &theme,
    ))
    .child(property_row(
        "允许反向",
        "当前消费者允许",
        Element::new(
            Toggle::new("允许反向", reverse, Arc::clone(&theme)).capabilities(capabilities),
        ),
        &theme,
    ));

    let viscosity = fluid_slider("黏度", 68.0, Arc::clone(&theme), capabilities)?;
    let tension = fluid_slider("表面张力", 72.0, Arc::clone(&theme), capabilities)?;
    let attraction = fluid_slider("吸附力", 56.0, Arc::clone(&theme), capabilities)?;
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
            "Shape · 继承",
            TextRole::Caption,
            theme.palette.text_muted,
            &theme,
        )),
    )
    .child(viscosity)
    .child(tension)
    .child(attraction);

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
            .enabled(true),
    ))
    .child(Element::new(
        Button::new("保存", Arc::clone(&theme))
            .variant(ButtonVariant::FluidSelected)
            .capabilities(capabilities)
            .enabled(true),
    ));

    let content = Element::new(Padding {
        insets: Insets::all(16.0),
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

fn scope_rail(theme: Arc<Theme>, capabilities: MaterialCapabilities) -> Element {
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
        "曲线 ● 本地预览   ·   280 ms ○ 继承",
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

fn motion_graph(theme: Arc<Theme>, capabilities: MaterialCapabilities) -> Element {
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
                "进度 0–1.2",
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
            .child(Element::new(
                Button::new("适应", Arc::clone(&theme))
                    .variant(ButtonVariant::Fluid)
                    .capabilities(capabilities),
            ))
            .child(Element::new(
                Button::new("100%", Arc::clone(&theme))
                    .variant(ButtonVariant::Fluid)
                    .capabilities(capabilities),
            )),
        ),
    );
    let plot = Element::new(MotionCurvePlot {
        theme: Arc::clone(&theme),
        capabilities,
        current: overshoot_curve(),
        inherited: CubicBezier::SETTLE,
        playhead: 0.46,
        progress_maximum: 1.2,
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
            "0 ms",
            TextRole::Caption,
            theme.palette.text_muted,
            &theme,
        ))
        .child(text(
            "140 ms",
            TextRole::Caption,
            theme.palette.text_muted,
            &theme,
        ))
        .child(text(
            "280 ms   时间",
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

fn preview(theme: Arc<Theme>, capabilities: MaterialCapabilities) -> Element {
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
            .child(Element::new(
                Button::new("单次", Arc::clone(&theme))
                    .variant(ButtonVariant::FluidSelected)
                    .capabilities(capabilities),
            ))
            .child(Element::new(
                Button::new("循环", Arc::clone(&theme))
                    .variant(ButtonVariant::Fluid)
                    .capabilities(capabilities),
            ))
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
        Insets::all(12.0),
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
            .capabilities(capabilities),
    )))
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
        let (depth, shadow_alpha, light_scale, dark_scale) = match self.elevation {
            GelElevation::Raised => ((dimension * 0.032).clamp(3.0, 6.0), 0.20, 0.48, 0.58),
            GelElevation::Embedded => ((dimension * 0.020).clamp(1.5, 3.0), 0.04, 0.23, 0.32),
        };
        let blur = (depth * 2.2).clamp(7.0, 16.0);
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
                depth,
                depth,
                blur * 1.45,
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
                    depth,
                    depth,
                    blur * 1.35,
                    0.0,
                    with_alpha(tones.highlight, tones.highlight_strength * light_scale),
                ),
            )?;
            ctx.builder().inset_shadow(
                rect,
                radii,
                Shadow::new(
                    -depth,
                    -depth,
                    blur,
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
    current: CubicBezier,
    inherited: CubicBezier,
    playhead: f32,
    progress_maximum: f32,
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

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let frame = ctx.rect().inset(6.0);
        paint_fluid_well(
            ctx.builder(),
            frame,
            CornerRadii::all(self.theme.radii.group),
            &self.theme,
            self.capabilities,
            SurfaceState::default(),
        )?;
        let plot = frame.inset(12.0);
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
            self.inherited,
            self.progress_maximum,
            inherited,
            1.25,
            true,
        )?;
        draw_curve(
            ctx,
            plot,
            self.current,
            self.progress_maximum,
            with_alpha(self.theme.palette.inverse_edge, 0.36),
            6.0,
            false,
        )?;
        draw_curve(
            ctx,
            plot,
            self.current,
            self.progress_maximum,
            with_alpha(self.theme.palette.accent_secondary, 0.56),
            4.0,
            false,
        )?;
        draw_curve(
            ctx,
            Rect::new(plot.x, plot.y - 0.8, plot.width, plot.height),
            self.current,
            self.progress_maximum,
            self.theme.palette.accent,
            1.6,
            false,
        )?;

        let playhead_x = plot.x + plot.width * self.playhead.clamp(0.0, 1.0);
        let info = with_alpha(self.theme.palette.accent_secondary, 0.82);
        ctx.builder()
            .rect(Rect::new(playhead_x, plot.y, 1.0, plot.height), info)?;
        ctx.builder().rounded_rect(
            Rect::new(playhead_x - 5.0, plot.y - 3.0, 10.0, 10.0),
            CornerRadii::all(5.0),
            self.theme.palette.accent_secondary,
        )?;

        for point in [
            Point::new(plot.x, plot.bottom()),
            Point::new(
                plot.right(),
                plot.bottom() - plot.height / self.progress_maximum,
            ),
        ] {
            ctx.builder().rounded_rect(
                Rect::new(point.x - 6.0, point.y - 6.0, 12.0, 12.0),
                CornerRadii::all(6.0),
                with_alpha(self.theme.palette.inverse_edge, 0.72),
            )?;
            ctx.builder().rounded_rect(
                Rect::new(point.x - 3.0, point.y - 3.0, 6.0, 6.0),
                CornerRadii::all(2.0),
                self.theme.palette.accent,
            )?;
        }
        Ok(())
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            role: SemanticRole::Group,
            label: Some("动画曲线图：自定义越界曲线，持续时间 280 毫秒".to_owned()),
            value: Some("播放位置 46%".to_owned()),
            ..Semantics::default()
        }
    }
}

fn draw_curve(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    curve: CubicBezier,
    progress_maximum: f32,
    color: Color,
    width: f32,
    dashed: bool,
) -> Result<(), UiError> {
    let mut previous = Point::new(rect.x, rect.bottom());
    for index in 1..=64 {
        let time = index as f32 / 64.0;
        let progress = curve.sample(time);
        let next = Point::new(
            rect.x + rect.width * time,
            rect.bottom() - rect.height * progress / progress_maximum,
        );
        if !dashed || (index / 3) % 2 == 0 {
            draw_segment(ctx, previous, next, width, color)?;
        }
        previous = next;
    }
    Ok(())
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
}

impl Widget for PreviewStage {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths(["palette.edge", "palette.inverse_edge"])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        ctx.builder().rect(
            Rect::new(rect.x, rect.y, rect.width, 1.0),
            with_alpha(self.theme.palette.edge, 0.045),
        )?;
        ctx.builder().rect(
            Rect::new(rect.x, rect.bottom() - 1.0, rect.width, 1.0),
            with_alpha(self.theme.palette.inverse_edge, 0.10),
        )?;
        ctx.paint_children()
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
}

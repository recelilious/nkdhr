use std::sync::Arc;

use nkdhr_render::{Color, CornerRadii, Rect, Shadow};

use crate::theme::{mix, with_alpha};
use crate::{
    Constraints, Insets, MaterialCapabilities, MaterialTier, MeasureCtx, PaintCtx,
    ResolvedMaterial, Size, Theme, ThemeReadSet, UiError, Widget,
};

/// Continuous visual state consumed by the material painter. Geometry remains
/// separate, so a component can animate the surface without changing hit bounds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SurfaceState {
    pub hovered: f32,
    pub pressed: f32,
    pub focused: bool,
    pub accented: bool,
    pub selected: bool,
    pub disabled: bool,
    pub destructive: bool,
}

/// Theme-derived colors for one fluid material surface. The tones are mixed
/// from the actual base, wallpaper/theme accents and contrast anchors rather
/// than assuming that every theme uses white highlights and black shadows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidMaterialTones {
    pub base: Color,
    pub highlight: Color,
    pub shade: Color,
    pub highlight_strength: f32,
    pub shade_strength: f32,
}

pub fn resolve_fluid_material_tones(
    theme: &Theme,
    base: Color,
    accented: bool,
) -> FluidMaterialTones {
    let [red, green, blue, _] = base.components();
    let luminance = red * 0.2126 + green * 0.7152 + blue * 0.0722;
    let dark = luminance < 0.46;
    let highlight_target = if accented {
        mix(theme.palette.edge, theme.palette.accent, 0.28)
    } else {
        mix(theme.palette.edge, theme.palette.surface_raised, 0.24)
    };
    let shade_target = if accented {
        mix(theme.palette.shadow, theme.palette.accent_secondary, 0.20)
    } else {
        mix(theme.palette.shadow, theme.palette.backdrop, 0.30)
    };
    FluidMaterialTones {
        base,
        highlight: mix(base, highlight_target, if dark { 0.34 } else { 0.58 }),
        shade: mix(base, shade_target, if dark { 0.62 } else { 0.46 }),
        highlight_strength: if dark { 0.40 } else { 0.58 },
        shade_strength: if dark { 0.47 } else { 0.40 },
    }
}

/// A calm frosted-glass container. Capable hosts receive a real painter-order
/// backdrop filter; reduced-transparency and incapable hosts receive the
/// theme's compensated opaque fill instead.
#[derive(Debug, Clone)]
pub struct GlassSurface {
    theme: Arc<Theme>,
    tier: MaterialTier,
    capabilities: MaterialCapabilities,
    radius: f32,
    radius_from_theme: bool,
    padding: Insets,
    state: SurfaceState,
}

impl GlassSurface {
    pub fn new(theme: Arc<Theme>, tier: MaterialTier) -> Self {
        let radius = match tier {
            MaterialTier::Ghost | MaterialTier::CompactNode | MaterialTier::HoverTransient => {
                theme.radii.control
            }
            MaterialTier::Popover => theme.radii.popover,
            MaterialTier::ExpandedPanel | MaterialTier::ContentSurface | MaterialTier::Terminal => {
                theme.radii.group
            }
        };
        Self {
            theme,
            tier,
            capabilities: MaterialCapabilities::default(),
            radius,
            radius_from_theme: true,
            padding: Insets::ZERO,
            state: SurfaceState::default(),
        }
    }

    pub fn capabilities(mut self, capabilities: MaterialCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self.radius_from_theme = false;
        self
    }

    pub fn padding(mut self, padding: Insets) -> Self {
        self.padding = padding;
        self
    }

    pub fn state(mut self, state: SurfaceState) -> Self {
        self.state = state;
        self
    }

    pub fn material_request(&self) -> ResolvedMaterial {
        self.theme.resolve_material(self.tier, self.capabilities)
    }
}

impl Widget for GlassSurface {
    fn theme_reads(&self) -> ThemeReadSet {
        let mut reads = surface_theme_reads(self.tier);
        if self.radius_from_theme {
            reads.record(match self.tier {
                MaterialTier::Ghost | MaterialTier::CompactNode | MaterialTier::HoverTransient => {
                    "radii.control"
                }
                MaterialTier::Popover => "radii.popover",
                MaterialTier::ExpandedPanel
                | MaterialTier::ContentSurface
                | MaterialTier::Terminal => "radii.group",
            });
        }
        reads
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        if self.radius_from_theme {
            self.radius = match self.tier {
                MaterialTier::Ghost | MaterialTier::CompactNode | MaterialTier::HoverTransient => {
                    theme.radii.control
                }
                MaterialTier::Popover => theme.radii.popover,
                MaterialTier::ExpandedPanel
                | MaterialTier::ContentSurface
                | MaterialTier::Terminal => theme.radii.group,
            };
        }
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

    fn arrange(&self, ctx: &mut crate::ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
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
        paint_surface(
            ctx.builder(),
            rect,
            CornerRadii::all(self.radius.max(0.0)),
            &self.theme,
            self.tier,
            self.capabilities,
            self.state,
        )?;
        ctx.paint_children()
    }
}

pub(crate) fn surface_theme_reads(tier: MaterialTier) -> ThemeReadSet {
    let mut reads = ThemeReadSet::from_paths([
        "palette.surface",
        "palette.surface_raised",
        "palette.backdrop",
        "palette.accent",
        "palette.accent_secondary",
        "palette.error",
        "palette.edge",
        "palette.inverse_edge",
        "palette.shadow",
    ]);
    reads.extend(match tier {
        MaterialTier::Ghost => [
            "materials.ghost.opacity",
            "materials.ghost.backdrop_blur",
            "materials.ghost.wallpaper_tint",
        ],
        MaterialTier::CompactNode => [
            "materials.compact_node.opacity",
            "materials.compact_node.backdrop_blur",
            "materials.compact_node.wallpaper_tint",
        ],
        MaterialTier::HoverTransient => [
            "materials.hover_transient.opacity",
            "materials.hover_transient.backdrop_blur",
            "materials.hover_transient.wallpaper_tint",
        ],
        MaterialTier::Popover => [
            "materials.popover.opacity",
            "materials.popover.backdrop_blur",
            "materials.popover.wallpaper_tint",
        ],
        MaterialTier::ExpandedPanel => [
            "materials.expanded_panel.opacity",
            "materials.expanded_panel.backdrop_blur",
            "materials.expanded_panel.wallpaper_tint",
        ],
        MaterialTier::ContentSurface => [
            "materials.content_surface.opacity",
            "materials.content_surface.backdrop_blur",
            "materials.content_surface.wallpaper_tint",
        ],
        MaterialTier::Terminal => [
            "materials.terminal.opacity",
            "materials.terminal.backdrop_blur",
            "materials.terminal.wallpaper_tint",
        ],
    });
    reads
}

pub(crate) fn paint_surface(
    builder: &mut nkdhr_render::DisplayListBuilder,
    rect: Rect,
    radii: CornerRadii,
    theme: &Theme,
    tier: MaterialTier,
    capabilities: MaterialCapabilities,
    state: SurfaceState,
) -> Result<(), nkdhr_render::BuildError> {
    let material = theme.resolve_material(tier, capabilities);
    let hover = state.hovered.clamp(0.0, 1.0);
    let press = state.pressed.clamp(0.0, 1.0);
    let shadow = shadow_for(tier, hover, press);
    if shadow.alpha > 0.0 {
        builder.shadow(
            rect,
            radii,
            Shadow::new(
                shadow.offset_y,
                shadow.offset_y,
                shadow.blur,
                0.0,
                with_alpha(theme.palette.shadow, shadow.alpha),
            ),
        )?;
    }

    if material.backdrop_blur > 0.0 {
        builder.backdrop_blur(rect, radii, material.backdrop_blur)?;
    }

    let base = if state.accented {
        mix(
            material.fill,
            with_alpha(theme.palette.accent, material.fill.components()[3]),
            0.72,
        )
    } else {
        material.fill
    };
    builder.rounded_rect(rect, radii, base)?;

    let overlay_alpha = 0.06 + hover * 0.04 + press * 0.04;
    let overlay_color = if state.destructive {
        with_alpha(theme.palette.error, overlay_alpha + 0.04)
    } else if state.selected || state.accented {
        with_alpha(theme.palette.accent_secondary, overlay_alpha + 0.12)
    } else {
        with_alpha(theme.palette.surface_raised, overlay_alpha)
    };
    builder.rounded_rect(rect, radii, overlay_color)?;

    if state.disabled {
        builder.rounded_rect(rect, radii, with_alpha(theme.palette.backdrop, 0.28))?;
    }

    builder.border(rect, radii, 1.0, material.edge)?;
    let inner = rect.inset(1.0);
    if !inner.is_empty() {
        builder.border(
            inner,
            CornerRadii::all((radii.top_left - 1.0).max(0.0)),
            1.0,
            material.inner_highlight,
        )?;
    }
    if state.focused {
        builder.border(
            rect.expand(3.0),
            CornerRadii::all(radii.top_left + 3.0),
            2.0,
            with_alpha(theme.palette.accent_secondary, 0.80),
        )?;
    }
    Ok(())
}

/// Paint a recessed, border-light fluid well for text fields, control tracks
/// and bounded editors. Concave lighting is deliberately opposite to raised
/// controls: shade enters from the top-left and the lower-right catches light.
pub fn paint_fluid_well(
    builder: &mut nkdhr_render::DisplayListBuilder,
    rect: Rect,
    radii: CornerRadii,
    theme: &Theme,
    capabilities: MaterialCapabilities,
    state: SurfaceState,
) -> Result<(), nkdhr_render::BuildError> {
    let material = theme.resolve_material(MaterialTier::CompactNode, capabilities);
    let radii = CornerRadii::all(radii.top_left.min(rect.height * 0.5));
    let accented = state.accented || state.selected;
    let base_tint = if accented {
        mix(theme.palette.surface, theme.palette.accent, 0.26)
    } else {
        mix(theme.palette.surface, theme.palette.backdrop, 0.18)
    };
    let tones = resolve_fluid_material_tones(theme, base_tint, accented);
    let depth = (rect.height.min(rect.width) * 0.10).clamp(1.0, 4.0);
    let blur = (depth * 2.4).clamp(3.0, 11.0);
    if material.backdrop_blur > 0.0 {
        builder.backdrop_blur(rect, radii, material.backdrop_blur)?;
    }
    builder.rounded_rect(
        rect,
        radii,
        with_alpha(tones.base, material.fill.components()[3]),
    )?;
    if !capabilities.high_contrast {
        builder.inset_shadow(
            rect,
            radii,
            Shadow::new(
                depth,
                depth,
                blur,
                0.0,
                with_alpha(tones.shade, tones.shade_strength * 0.86),
            ),
        )?;
        builder.inset_shadow(
            rect,
            radii,
            Shadow::new(
                -depth,
                -depth,
                blur * 1.12,
                0.0,
                with_alpha(tones.highlight, tones.highlight_strength * 0.48),
            ),
        )?;
    }
    if state.disabled {
        builder.rounded_rect(rect, radii, with_alpha(theme.palette.backdrop, 0.28))?;
    }
    let edge = if state.focused {
        with_alpha(theme.palette.accent, 0.44)
    } else if capabilities.high_contrast {
        with_alpha(theme.palette.edge, 0.86)
    } else {
        with_alpha(tones.highlight, 0.035)
    };
    builder.border(
        rect,
        radii,
        if capabilities.high_contrast { 2.0 } else { 1.0 },
        edge,
    )?;
    Ok(())
}

/// Paint a compact clay-and-glass hybrid from one outer shadow and two true
/// inset shadows. This mirrors the open clay.css model while retaining nkdhr's
/// real backdrop blur and interaction-driven compression.
pub(crate) fn paint_fluid_surface(
    builder: &mut nkdhr_render::DisplayListBuilder,
    rect: Rect,
    radii: CornerRadii,
    theme: &Theme,
    capabilities: MaterialCapabilities,
    state: SurfaceState,
) -> Result<(), nkdhr_render::BuildError> {
    let radii = CornerRadii::all(radii.top_left.min(rect.height * 0.5));
    let material = theme.resolve_material(MaterialTier::CompactNode, capabilities);
    let hover = state.hovered.clamp(0.0, 1.0);
    let press = state.pressed.clamp(0.0, 1.0);
    let depth = (rect.height * 0.16).clamp(3.0, 7.0) * (1.0 - press * 0.62);
    let blur = (rect.height * 0.36).clamp(8.0, 18.0);
    let base_tint = if state.destructive {
        mix(theme.palette.surface, theme.palette.error, 0.34)
    } else if state.accented {
        mix(theme.palette.surface, theme.palette.accent, 0.42)
    } else if state.selected {
        mix(theme.palette.surface, theme.palette.accent_secondary, 0.30)
    } else {
        mix(theme.palette.surface, theme.palette.surface_raised, 0.16)
    };
    let tones = resolve_fluid_material_tones(
        theme,
        base_tint,
        state.selected || state.accented || state.destructive,
    );
    let base = with_alpha(tones.base, (material.fill.components()[3] + 0.16).min(0.82));
    let outer_depth = (1.5 + hover * 1.5) * (1.0 - press * 0.72);

    builder.shadow(
        rect,
        radii,
        Shadow::new(
            outer_depth,
            outer_depth,
            blur * 0.92,
            0.0,
            with_alpha(tones.shade, 0.14 + hover * 0.05),
        ),
    )?;
    if material.backdrop_blur > 0.0 {
        builder.backdrop_blur(rect, radii, material.backdrop_blur)?;
    }
    builder.rounded_rect(rect, radii, base)?;
    if !capabilities.high_contrast {
        builder.inset_shadow(
            rect,
            radii,
            Shadow::new(
                -depth,
                -depth,
                blur,
                0.0,
                with_alpha(tones.shade, tones.shade_strength * (0.88 + press * 0.12)),
            ),
        )?;
        builder.inset_shadow(
            rect,
            radii,
            Shadow::new(
                depth * 0.82,
                depth * 0.82,
                blur * 1.28,
                0.0,
                with_alpha(
                    tones.highlight,
                    tones.highlight_strength * (0.78 - press * 0.20),
                ),
            ),
        )?;
    }
    if state.disabled {
        builder.rounded_rect(rect, radii, with_alpha(theme.palette.backdrop, 0.24))?;
    }
    builder.border(rect, radii, 1.0, with_alpha(tones.highlight, 0.035))?;
    if state.focused {
        builder.border(
            rect.expand(3.0),
            CornerRadii::all(radii.top_left + 3.0),
            2.0,
            with_alpha(theme.palette.accent, 0.74),
        )?;
    }
    Ok(())
}

fn shadow_for(tier: MaterialTier, hover: f32, press: f32) -> crate::ShadowToken {
    if press > 0.0 {
        return crate::ShadowToken {
            offset_y: 1.0,
            blur: 4.0,
            alpha: 0.10,
        };
    }
    if hover > 0.0 {
        return crate::ShadowToken {
            offset_y: 4.0 * hover,
            blur: 2.0 + 10.0 * hover,
            alpha: 0.08 + 0.04 * hover,
        };
    }
    match tier {
        MaterialTier::Ghost => crate::ShadowToken {
            offset_y: 0.0,
            blur: 0.0,
            alpha: 0.0,
        },
        MaterialTier::CompactNode | MaterialTier::HoverTransient => crate::ShadowToken {
            offset_y: 1.0,
            blur: 2.0,
            alpha: 0.08,
        },
        MaterialTier::Popover => crate::ShadowToken {
            offset_y: 12.0,
            blur: 32.0,
            alpha: 0.18,
        },
        MaterialTier::ExpandedPanel | MaterialTier::ContentSurface | MaterialTier::Terminal => {
            crate::ShadowToken {
                offset_y: 8.0,
                blur: 24.0,
                alpha: 0.16,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Element, UiRoot};
    use nkdhr_render::{DisplayListBuilder, Primitive, ShapeStyle};

    #[test]
    fn glass_surface_paints_blur_only_for_a_capable_host() {
        let theme = Arc::new(Theme::default());
        let surface = GlassSurface::new(Arc::clone(&theme), MaterialTier::ContentSurface);
        assert_eq!(surface.material_request().backdrop_blur, 0.0);

        let mut root = UiRoot::new(Element::new(surface)).unwrap();
        root.layout(Size::new(120.0, 80.0)).unwrap();
        let mut builder = DisplayListBuilder::new();
        root.paint(&mut builder).unwrap();
        assert!(
            !builder
                .finish()
                .primitives()
                .iter()
                .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
        );

        let capable = GlassSurface::new(theme, MaterialTier::ContentSurface).capabilities(
            MaterialCapabilities {
                backdrop_blur: true,
                ..MaterialCapabilities::default()
            },
        );
        assert_eq!(capable.material_request().backdrop_blur, 36.0);
        let mut root = UiRoot::new(Element::new(capable)).unwrap();
        root.layout(Size::new(120.0, 80.0)).unwrap();
        let mut builder = DisplayListBuilder::new();
        root.paint(&mut builder).unwrap();
        let list = builder.finish();
        assert!(matches!(
            list.primitives().first(),
            Some(Primitive::Shape(shape)) if matches!(shape.style, ShapeStyle::Shadow(_))
        ));
        assert!(list.primitives().iter().any(|primitive| matches!(
            primitive,
            Primitive::BackdropBlur(blur) if blur.radius == 36.0
        )));
        let blur_index = list
            .primitives()
            .iter()
            .position(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
            .unwrap();
        let fill_index = list
            .primitives()
            .iter()
            .position(|primitive| {
                matches!(primitive, Primitive::Shape(shape) if matches!(shape.style, ShapeStyle::Fill(_)))
            })
            .unwrap();
        assert!(blur_index < fill_index);
    }

    #[test]
    fn fluid_tones_follow_the_theme_and_do_not_reuse_one_fixed_pair() {
        let dark = Theme::default();
        let dark_tones = resolve_fluid_material_tones(&dark, dark.palette.surface, false);
        assert_ne!(dark_tones.highlight, dark_tones.shade);

        let mut light = dark.clone();
        light.palette.backdrop = Color::from_srgba8(225, 235, 244, 255);
        light.palette.surface = Color::from_srgba8(240, 246, 250, 255);
        light.palette.surface_raised = Color::WHITE;
        light.palette.edge = Color::WHITE;
        light.palette.shadow = Color::from_srgba8(75, 104, 128, 255);
        let light_tones = resolve_fluid_material_tones(&light, light.palette.surface, false);
        assert_ne!(dark_tones.highlight, light_tones.highlight);
        assert_ne!(dark_tones.shade, light_tones.shade);
        assert!(light_tones.highlight_strength > dark_tones.highlight_strength);
    }

    #[test]
    fn fluid_well_uses_concave_insets_without_an_attached_drop_shadow() {
        let theme = Theme::default();
        let mut builder = DisplayListBuilder::new();
        paint_fluid_well(
            &mut builder,
            Rect::new(0.0, 0.0, 120.0, 36.0),
            CornerRadii::all(10.0),
            &theme,
            MaterialCapabilities::default(),
            SurfaceState::default(),
        )
        .unwrap();
        let list = builder.finish();
        assert_eq!(
            list.primitives()
                .iter()
                .filter(|primitive| matches!(
                    primitive,
                    Primitive::Shape(shape)
                        if matches!(shape.style, ShapeStyle::InsetShadow(_))
                ))
                .count(),
            2
        );
        assert!(!list.primitives().iter().any(|primitive| matches!(
            primitive,
            Primitive::Shape(shape) if matches!(shape.style, ShapeStyle::Shadow(_))
        )));
    }
}

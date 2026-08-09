use std::sync::Arc;

use nkdhr_render::{CornerRadii, Rect, Shadow};

use crate::theme::{mix, with_alpha};
use crate::{
    Constraints, Insets, MaterialCapabilities, MaterialTier, MeasureCtx, PaintCtx,
    ResolvedMaterial, Size, Theme, UiError, Widget,
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

/// A calm frosted-glass container. Capable hosts receive a real painter-order
/// backdrop filter; reduced-transparency and incapable hosts receive the
/// theme's compensated opaque fill instead.
#[derive(Debug, Clone)]
pub struct GlassSurface {
    theme: Arc<Theme>,
    tier: MaterialTier,
    capabilities: MaterialCapabilities,
    radius: f32,
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
                0.0,
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
}

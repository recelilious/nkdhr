use std::{any::Any, sync::Arc};

use nkdhr_render::{Color, Point};

use crate::text::{TextAlign, TextLayout, TextStyle, TextWrap};
use crate::{
    ArrangeCtx, Constraints, Invalidation, MeasureCtx, PaintCtx, Reactive, SemanticRole, Semantics,
    SemanticsCtx, Size, UiError, UpdateCtx, Widget,
};

/// Retained Unicode text shaped by the [`crate::text::TextResources`] owned by
/// its [`crate::UiRoot`].
pub struct Text {
    content: Reactive<String>,
    style: TextStyle,
    color: Color,
}

impl Text {
    pub fn new(content: impl Into<String>, style: TextStyle, color: Color) -> Self {
        Self::bound(Reactive::new(content.into()), style, color)
    }

    pub fn bound(content: Reactive<String>, style: TextStyle, color: Color) -> Self {
        Self {
            content,
            style,
            color,
        }
    }

    pub fn style(mut self, style: TextStyle) -> Self {
        self.style = style;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

#[derive(Debug, Default)]
struct TextState {
    layout: Option<Arc<TextLayout>>,
}

impl Widget for Text {
    fn create_state(&self) -> Box<dyn Any> {
        Box::<TextState>::default()
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        if previous.style != self.style {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
        } else if previous.color != self.color {
            ctx.invalidate(Invalidation::PAINT);
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 0 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 0,
                actual: ctx.child_count(),
            });
        }
        let content = ctx.watch(
            &self.content,
            Invalidation::LAYOUT | Invalidation::SEMANTICS,
        );
        let width = (self.style.wrap != TextWrap::None).then_some(constraints.max().width);
        let layout = ctx.layout_text(&content, &self.style, width)?;
        let measured_width = if width.is_some() && self.style.align != TextAlign::Start {
            constraints.max().width
        } else {
            layout.width()
        };
        let size = constraints.constrain(Size::new(measured_width, layout.height()));
        ctx.state_mut::<TextState>()?.layout = Some(layout);
        Ok(size)
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, _rect: nkdhr_render::Rect) -> Result<(), UiError> {
        if ctx.child_count() != 0 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 0,
                actual: ctx.child_count(),
            });
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        let layout = ctx
            .state_mut::<TextState>()?
            .layout
            .as_ref()
            .cloned()
            .ok_or(UiError::LayoutRequired)?;
        ctx.draw_text(&layout, Point::new(rect.x, rect.y), self.color, Some(rect))?;
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            role: SemanticRole::Text,
            label: Some(ctx.watch(&self.content, Invalidation::SEMANTICS)),
            ..Semantics::default()
        }
    }
}

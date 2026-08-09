use std::{any::Any, fmt, sync::Arc};

use nkdhr_render::{CornerRadii, Rect};

use crate::theme::with_alpha;
use crate::{
    ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MeasureCtx, MotionFamily, PaintCtx,
    Reactive, ScalarMotion, SemanticRole, Semantics, SemanticsCtx, Size, Theme, UiError, UiEvent,
    Widget,
};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScrollOffset {
    pub x: f32,
    pub y: f32,
}

impl ScrollOffset {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

pub struct Scroll {
    label: String,
    content_size: Size,
    offset: Reactive<ScrollOffset>,
    theme: Arc<Theme>,
    horizontal: bool,
    vertical: bool,
    enabled: bool,
}

impl Scroll {
    pub fn new(
        label: impl Into<String>,
        content_size: Size,
        offset: Reactive<ScrollOffset>,
        theme: Arc<Theme>,
    ) -> Result<Self, ScrollError> {
        if !content_size.is_valid() {
            return Err(ScrollError::InvalidContentSize);
        }
        Ok(Self {
            label: label.into(),
            content_size,
            offset,
            theme,
            horizontal: true,
            vertical: true,
            enabled: true,
        })
    }

    pub fn horizontal(mut self, horizontal: bool) -> Self {
        self.horizontal = horizontal;
        self
    }

    pub fn vertical(mut self, vertical: bool) -> Self {
        self.vertical = vertical;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn clamp_offset(&self, offset: ScrollOffset, viewport: Size) -> ScrollOffset {
        ScrollOffset {
            x: if self.horizontal {
                offset
                    .x
                    .clamp(0.0, (self.content_size.width - viewport.width).max(0.0))
            } else {
                0.0
            },
            y: if self.vertical {
                offset
                    .y
                    .clamp(0.0, (self.content_size.height - viewport.height).max(0.0))
            } else {
                0.0
            },
        }
    }

    fn update_offset(&self, viewport: Size, update: impl FnOnce(&mut ScrollOffset)) -> bool {
        if !self.enabled {
            return false;
        }
        let mut next = self.offset.get();
        update(&mut next);
        next = self.clamp_offset(next, viewport);
        self.offset.set_if_changed(next)
    }
}

#[derive(Debug, Clone, Copy)]
struct ScrollState {
    visibility: ScalarMotion,
    focused: bool,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            visibility: ScalarMotion::settled(0.0),
            focused: false,
        }
    }
}

impl Widget for Scroll {
    fn create_state(&self) -> Box<dyn Any> {
        Box::<ScrollState>::default()
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        if ctx.child_count() == 1 {
            ctx.measure_child(0, Constraints::tight(self.content_size)?)?;
        }
        Ok(constraints.constrain(Size::new(
            self.content_size.width.min(constraints.max().width),
            self.content_size.height.min(constraints.max().height),
        )))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() == 1 {
            let offset = self.clamp_offset(self.offset.get(), Size::new(rect.width, rect.height));
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x - offset.x,
                    rect.y - offset.y,
                    self.content_size.width,
                    self.content_size.height,
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let offset = self.clamp_offset(
            ctx.watch(&self.offset, Invalidation::LAYOUT | Invalidation::SEMANTICS),
            Size::new(ctx.rect().width, ctx.rect().height),
        );
        if ctx.child_count() == 1 {
            ctx.paint_child(0)?;
        }
        let now = ctx.now();
        let (visibility, active) = {
            let state = ctx.state_mut::<ScrollState>()?;
            (
                state.visibility.value(now).clamp(0.0, 1.0),
                state.visibility.is_active(now),
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        if visibility <= 0.001 {
            return Ok(());
        }

        let rect = ctx.rect();
        let thickness = self.theme.density_metrics().scrollbar;
        if self.vertical && self.content_size.height > rect.height {
            let length = (rect.height * rect.height / self.content_size.height)
                .max(24.0)
                .min(rect.height);
            let travel = (rect.height - length).max(0.0);
            let maximum = (self.content_size.height - rect.height).max(1.0);
            let y = rect.y + travel * (offset.y / maximum);
            let thumb = Rect::new(rect.right() - thickness - 3.0, y, thickness, length);
            ctx.builder().rounded_rect(
                thumb,
                CornerRadii::all(thickness * 0.5),
                with_alpha(self.theme.palette.text_secondary, 0.62 * visibility),
            )?;
        }
        if self.horizontal && self.content_size.width > rect.width {
            let length = (rect.width * rect.width / self.content_size.width)
                .max(24.0)
                .min(rect.width);
            let travel = (rect.width - length).max(0.0);
            let maximum = (self.content_size.width - rect.width).max(1.0);
            let x = rect.x + travel * (offset.x / maximum);
            let thumb = Rect::new(x, rect.bottom() - thickness - 3.0, length, thickness);
            ctx.builder().rounded_rect(
                thumb,
                CornerRadii::all(thickness * 0.5),
                with_alpha(self.theme.palette.text_secondary, 0.62 * visibility),
            )?;
        }
        Ok(())
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let viewport = Size::new(ctx.rect().width, ctx.rect().height);
        match event {
            UiEvent::HoverChanged(hovered) => {
                let now = ctx.now();
                let state = ctx.state_mut::<ScrollState>()?;
                let visible = *hovered || state.focused;
                state.visibility.retarget(
                    now,
                    if visible { 1.0 } else { 0.0 },
                    self.theme.motion.spec(if visible {
                        MotionFamily::ScrollbarShow
                    } else {
                        MotionFamily::ScrollbarHide
                    }),
                );
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::FocusChanged(focused) => {
                let now = ctx.now();
                let state = ctx.state_mut::<ScrollState>()?;
                state.focused = *focused;
                state.visibility.retarget(
                    now,
                    if *focused { 1.0 } else { 0.0 },
                    self.theme.motion.spec(if *focused {
                        MotionFamily::ScrollbarShow
                    } else {
                        MotionFamily::ScrollbarHide
                    }),
                );
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                ctx.request_animation_frame();
            }
            UiEvent::PointerScroll {
                delta_x, delta_y, ..
            } if self.enabled => {
                let changed = self.update_offset(viewport, |offset| {
                    offset.x += *delta_x;
                    offset.y += *delta_y;
                });
                if changed {
                    ctx.request_focus();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::KeyDown { key, .. } if self.enabled => {
                let line = self.theme.density_metrics().row_height;
                let changed = match key {
                    Key::ArrowUp => self.update_offset(viewport, |offset| offset.y -= line),
                    Key::ArrowDown => self.update_offset(viewport, |offset| offset.y += line),
                    Key::ArrowLeft => self.update_offset(viewport, |offset| offset.x -= line),
                    Key::ArrowRight => self.update_offset(viewport, |offset| offset.x += line),
                    Key::PageUp => {
                        self.update_offset(viewport, |offset| offset.y -= viewport.height)
                    }
                    Key::PageDown | Key::Space => {
                        self.update_offset(viewport, |offset| offset.y += viewport.height)
                    }
                    Key::Home => self.update_offset(viewport, |offset| offset.y = 0.0),
                    Key::End => {
                        self.update_offset(viewport, |offset| offset.y = self.content_size.height)
                    }
                    _ => false,
                };
                if changed {
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let offset = ctx.watch(&self.offset, Invalidation::SEMANTICS);
        Semantics {
            role: SemanticRole::ScrollArea,
            label: Some(self.label.clone()),
            value: Some(format!("x={}, y={}", offset.x, offset.y)),
            enabled: self.enabled,
            focusable: self.enabled,
        }
    }

    fn focusable(&self) -> bool {
        self.enabled
    }

    fn accepts_pointer(&self) -> bool {
        self.enabled
    }

    fn clips_children(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollError {
    InvalidContentSize,
}

impl fmt::Display for ScrollError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("scroll content size must be finite and non-negative")
    }
}

impl std::error::Error for ScrollError {}

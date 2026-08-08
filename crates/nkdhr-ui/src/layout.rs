//! Finite constraint layout and style-neutral structural widgets.

use nkdhr_render::Rect;

use crate::{ArrangeCtx, MeasureCtx, PaintCtx, UiError, Widget};

/// Logical two-dimensional size.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const ZERO: Self = Self::new(0.0, 0.0);

    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    pub fn is_valid(self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width >= 0.0 && self.height >= 0.0
    }
}

/// Finite inclusive bounds passed from a parent to a child.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Constraints {
    min: Size,
    max: Size,
}

impl Constraints {
    pub fn new(min: Size, max: Size) -> Result<Self, UiError> {
        if !min.is_valid() || !max.is_valid() || min.width > max.width || min.height > max.height {
            return Err(UiError::InvalidConstraints);
        }
        Ok(Self { min, max })
    }

    pub fn tight(size: Size) -> Result<Self, UiError> {
        Self::new(size, size)
    }

    pub const fn min(self) -> Size {
        self.min
    }

    pub const fn max(self) -> Size {
        self.max
    }

    pub fn constrain(self, size: Size) -> Size {
        Size::new(
            size.width.clamp(self.min.width, self.max.width),
            size.height.clamp(self.min.height, self.max.height),
        )
    }

    pub fn contains(self, size: Size) -> bool {
        size.is_valid()
            && size.width >= self.min.width
            && size.width <= self.max.width
            && size.height >= self.min.height
            && size.height <= self.max.height
    }

    pub fn deflate(self, insets: Insets) -> Result<Self, UiError> {
        insets.validate()?;
        let horizontal = insets.left + insets.right;
        let vertical = insets.top + insets.bottom;
        Self::new(
            Size::new(
                (self.min.width - horizontal).max(0.0),
                (self.min.height - vertical).max(0.0),
            ),
            Size::new(
                (self.max.width - horizontal).max(0.0),
                (self.max.height - vertical).max(0.0),
            ),
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Insets {
    pub const ZERO: Self = Self::all(0.0);

    pub const fn all(value: f32) -> Self {
        Self {
            left: value,
            top: value,
            right: value,
            bottom: value,
        }
    }

    pub const fn symmetric(horizontal: f32, vertical: f32) -> Self {
        Self {
            left: horizontal,
            top: vertical,
            right: horizontal,
            bottom: vertical,
        }
    }

    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn validate(self) -> Result<(), UiError> {
        if [self.left, self.top, self.right, self.bottom]
            .into_iter()
            .all(|value| value.is_finite() && value >= 0.0)
            && self.horizontal().is_finite()
            && self.vertical().is_finite()
        {
            Ok(())
        } else {
            Err(UiError::InvalidInsets)
        }
    }

    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Axis {
    #[default]
    Horizontal,
    Vertical,
}

impl Axis {
    fn main(self, size: Size) -> f32 {
        match self {
            Self::Horizontal => size.width,
            Self::Vertical => size.height,
        }
    }

    fn cross(self, size: Size) -> f32 {
        match self {
            Self::Horizontal => size.height,
            Self::Vertical => size.width,
        }
    }

    fn size(self, main: f32, cross: f32) -> Size {
        match self {
            Self::Horizontal => Size::new(main, cross),
            Self::Vertical => Size::new(cross, main),
        }
    }

    fn rect(self, main: f32, cross: f32, main_size: f32, cross_size: f32) -> Rect {
        match self {
            Self::Horizontal => Rect::new(main, cross, main_size, cross_size),
            Self::Vertical => Rect::new(cross, main, cross_size, main_size),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MainAxisAlignment {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CrossAxisAlignment {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

/// Alignment along one arranged axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Alignment {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

/// Style-neutral row/column algorithm. Spacing and alignment are always
/// explicit data; the widget contributes no colors, typography or decoration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flex {
    pub axis: Axis,
    pub gap: f32,
    pub main_alignment: MainAxisAlignment,
    pub cross_alignment: CrossAxisAlignment,
}

impl Default for Flex {
    fn default() -> Self {
        Self {
            axis: Axis::Horizontal,
            gap: 0.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Start,
        }
    }
}

impl Flex {
    fn validate(self) -> Result<(), UiError> {
        if self.gap.is_finite() && self.gap >= 0.0 {
            Ok(())
        } else {
            Err(UiError::InvalidGap)
        }
    }
}

impl Widget for Flex {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        self.validate()?;
        let child_count = ctx.child_count();
        let gap_total = self.gap * child_count.saturating_sub(1) as f32;
        if !gap_total.is_finite() {
            return Err(UiError::InvalidGap);
        }
        let max_main = self.axis.main(constraints.max());
        let max_cross = self.axis.cross(constraints.max());
        let mut measured = vec![Size::ZERO; child_count];
        let mut fixed_main = gap_total;
        let mut cross = 0.0_f32;
        let mut total_flex = 0.0_f32;

        for (index, measured_size) in measured.iter_mut().enumerate() {
            let flex = ctx.child_flex(index)?;
            if flex > 0.0 {
                total_flex += flex;
                continue;
            }
            let child_constraints =
                Constraints::new(Size::ZERO, self.axis.size(max_main, max_cross))?;
            *measured_size = ctx.measure_child(index, child_constraints)?;
            fixed_main += self.axis.main(*measured_size);
            cross = cross.max(self.axis.cross(*measured_size));
            if !fixed_main.is_finite() {
                return Err(UiError::InvalidSize);
            }
        }

        if !total_flex.is_finite() {
            return Err(UiError::InvalidFlex);
        }

        let remaining = (max_main - fixed_main).max(0.0);
        if total_flex > 0.0 {
            for (index, measured_size) in measured.iter_mut().enumerate() {
                let flex = ctx.child_flex(index)?;
                if flex <= 0.0 {
                    continue;
                }
                let allocation = remaining * flex / total_flex;
                let child_constraints = Constraints::new(
                    self.axis.size(allocation, 0.0),
                    self.axis.size(allocation, max_cross),
                )?;
                *measured_size = ctx.measure_child(index, child_constraints)?;
                cross = cross.max(self.axis.cross(*measured_size));
            }
        }

        let children_main = measured
            .iter()
            .map(|size| self.axis.main(*size))
            .sum::<f32>()
            + gap_total;
        if !children_main.is_finite() {
            return Err(UiError::InvalidSize);
        }
        let desired_main = if total_flex > 0.0 {
            max_main
        } else {
            children_main
        };
        Ok(constraints.constrain(self.axis.size(desired_main, cross)))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        self.validate()?;
        let count = ctx.child_count();
        if count == 0 {
            return Ok(());
        }
        let sizes = (0..count)
            .map(|index| ctx.child_size(index))
            .collect::<Result<Vec<_>, _>>()?;
        let available_main = self.axis.main(Size::new(rect.width, rect.height));
        let available_cross = self.axis.cross(Size::new(rect.width, rect.height));
        let children_main = sizes.iter().map(|size| self.axis.main(*size)).sum::<f32>();
        let base_gaps = self.gap * count.saturating_sub(1) as f32;
        if !base_gaps.is_finite() {
            return Err(UiError::InvalidGap);
        }
        let free = (available_main - children_main - base_gaps).max(0.0);
        let (mut cursor, extra_gap) = match self.main_alignment {
            MainAxisAlignment::Start => (0.0, 0.0),
            MainAxisAlignment::Center => (free / 2.0, 0.0),
            MainAxisAlignment::End => (free, 0.0),
            MainAxisAlignment::SpaceBetween if count > 1 => (0.0, free / (count - 1) as f32),
            MainAxisAlignment::SpaceBetween => (free / 2.0, 0.0),
        };
        for (index, size) in sizes.into_iter().enumerate() {
            let child_main = self.axis.main(size);
            let measured_cross = self.axis.cross(size);
            let (cross_position, child_cross) = match self.cross_alignment {
                CrossAxisAlignment::Start => (0.0, measured_cross),
                CrossAxisAlignment::Center => (
                    (available_cross - measured_cross).max(0.0) / 2.0,
                    measured_cross,
                ),
                CrossAxisAlignment::End => {
                    ((available_cross - measured_cross).max(0.0), measured_cross)
                }
                CrossAxisAlignment::Stretch => (0.0, available_cross),
            };
            let mut child_rect = self
                .axis
                .rect(cursor, cross_position, child_main, child_cross);
            child_rect.x += rect.x;
            child_rect.y += rect.y;
            ctx.arrange_child(index, child_rect)?;
            cursor += child_main;
            if index + 1 < count {
                cursor += self.gap + extra_gap;
            }
        }
        Ok(())
    }
}

/// Insets one child. More than one child is rejected during layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Padding {
    pub insets: Insets,
}

impl Widget for Padding {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        self.insets.validate()?;
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        let Some(index) = (ctx.child_count() == 1).then_some(0) else {
            return Ok(
                constraints.constrain(Size::new(self.insets.horizontal(), self.insets.vertical()))
            );
        };
        let child = ctx.measure_child(index, constraints.deflate(self.insets)?)?;
        Ok(constraints.constrain(Size::new(
            child.width + self.insets.horizontal(),
            child.height + self.insets.vertical(),
        )))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        self.insets.validate()?;
        if ctx.child_count() == 1 {
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x + self.insets.left,
                    rect.y + self.insets.top,
                    (rect.width - self.insets.horizontal()).max(0.0),
                    (rect.height - self.insets.vertical()).max(0.0),
                ),
            )?;
        }
        Ok(())
    }
}

/// Positions one child within the rectangle assigned by its parent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Align {
    pub horizontal: Alignment,
    pub vertical: Alignment,
}

impl Widget for Align {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        let child = if ctx.child_count() == 1 {
            ctx.measure_child(0, Constraints::new(Size::ZERO, constraints.max())?)?
        } else {
            Size::ZERO
        };
        let desired = Size::new(
            if self.horizontal == Alignment::Stretch {
                constraints.max().width
            } else {
                child.width
            },
            if self.vertical == Alignment::Stretch {
                constraints.max().height
            } else {
                child.height
            },
        );
        Ok(constraints.constrain(desired))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() != 1 {
            return Ok(());
        }
        let measured = ctx.child_size(0)?;
        let width = if self.horizontal == Alignment::Stretch {
            rect.width
        } else {
            measured.width.min(rect.width)
        };
        let height = if self.vertical == Alignment::Stretch {
            rect.height
        } else {
            measured.height.min(rect.height)
        };
        let x = rect.x + alignment_offset(self.horizontal, (rect.width - width).max(0.0));
        let y = rect.y + alignment_offset(self.vertical, (rect.height - height).max(0.0));
        ctx.arrange_child(0, Rect::new(x, y, width, height))
    }
}

fn alignment_offset(alignment: Alignment, free: f32) -> f32 {
    match alignment {
        Alignment::Start | Alignment::Stretch => 0.0,
        Alignment::Center => free / 2.0,
        Alignment::End => free,
    }
}

/// Paint-order stack. Every child receives the same arranged rectangle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stack;

impl Widget for Stack {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        let mut desired = constraints.min();
        for index in 0..ctx.child_count() {
            let size = ctx.measure_child(index, constraints)?;
            desired.width = desired.width.max(size.width);
            desired.height = desired.height.max(size.height);
        }
        Ok(constraints.constrain(desired))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        ctx.arrange_children(rect)
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        ctx.paint_children()
    }
}

/// Structural clipping boundary with no decoration of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clip;

impl Widget for Clip {
    fn clips_children(&self) -> bool {
        true
    }
}

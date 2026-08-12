//! Shared retained-root lifecycle used by compositor and standalone hosts.

use nkdhr_render::{DisplayList, DisplayListBuilder, TextureStore};

use crate::{
    DispatchResult, Element, Invalidation, Size, UiError, UiEvent, UiResult, UiRoot, WidgetId,
};

/// One backend-neutral application surface.
///
/// This owns the same [`UiRoot`] whether its caller later submits into a
/// compositor scene or a standalone Wayland EGL surface. Native input,
/// clipboard/IME work and presentation remain explicit host responsibilities.
pub struct UiHost {
    root: UiRoot,
    logical_size: Size,
    output_scale: f32,
    list: DisplayList,
    commit: u64,
    layout_pending: bool,
    paint_pending: bool,
    empty_textures: TextureStore,
}

impl UiHost {
    pub fn new(mut root: UiRoot, logical_size: Size, output_scale: f32) -> UiResult<Self> {
        validate_surface(logical_size, output_scale)?;
        if root.text_resources().is_some() {
            root.set_text_output_scale(output_scale)?;
        }
        Ok(Self {
            root,
            logical_size,
            output_scale,
            list: DisplayList::default(),
            commit: 0,
            layout_pending: true,
            paint_pending: true,
            empty_textures: TextureStore::new(),
        })
    }

    pub fn root(&self) -> &UiRoot {
        &self.root
    }

    pub fn root_mut(&mut self) -> &mut UiRoot {
        &mut self.root
    }

    pub fn logical_size(&self) -> Size {
        self.logical_size
    }

    pub fn output_scale(&self) -> f32 {
        self.output_scale
    }

    pub fn commit(&self) -> u64 {
        self.commit
    }

    pub fn texture_store(&self) -> Option<&TextureStore> {
        self.root.texture_store()
    }

    pub fn resize(&mut self, logical_size: Size, output_scale: f32) -> UiResult<bool> {
        validate_surface(logical_size, output_scale)?;
        let changed = self.logical_size != logical_size || self.output_scale != output_scale;
        if !changed {
            return Ok(false);
        }
        self.logical_size = logical_size;
        if self.output_scale != output_scale {
            self.output_scale = output_scale;
            if self.root.text_resources().is_some() {
                self.root.set_text_output_scale(output_scale)?;
            }
        }
        self.layout_pending = true;
        self.paint_pending = true;
        Ok(true)
    }

    pub fn reconcile(&mut self, element: Element) -> UiResult<WidgetId> {
        self.layout_pending = true;
        self.paint_pending = true;
        self.root.reconcile(element)
    }

    pub fn dispatch(&mut self, event: &UiEvent) -> UiResult<DispatchResult> {
        self.root.dispatch(event)
    }

    /// Advance one host frame and retain the prior complete list when no pass
    /// changed it. The commit increments only after recording succeeds.
    pub fn render(&mut self) -> UiResult<UiHostFrame<'_>> {
        self.root.tick();
        let invalidation = self.root.invalidation();
        if self.layout_pending || invalidation.contains(Invalidation::LAYOUT) {
            self.root.layout(self.logical_size)?;
            self.layout_pending = false;
        }
        if self.paint_pending
            || self.commit == 0
            || self.root.invalidation().contains(Invalidation::PAINT)
        {
            let mut builder = DisplayListBuilder::new();
            self.root.paint(&mut builder)?;
            self.list = builder.finish();
            self.commit = self.commit.wrapping_add(1).max(1);
            self.paint_pending = false;
        }
        Ok(UiHostFrame {
            display_list: &self.list,
            textures: self.root.texture_store().unwrap_or(&self.empty_textures),
            logical_size: self.logical_size,
            output_scale: self.output_scale,
            commit: self.commit,
        })
    }

    pub fn frame_requested(&mut self) -> bool {
        self.layout_pending
            || self.paint_pending
            || self.root.frame_requested()
            || !self.root.invalidation().is_empty()
    }
}

fn validate_surface(logical_size: Size, output_scale: f32) -> UiResult<()> {
    if !logical_size.is_valid() || logical_size.width == 0.0 || logical_size.height == 0.0 {
        return Err(UiError::InvalidSize);
    }
    if !output_scale.is_finite() || output_scale <= 0.0 {
        return Err(UiError::Text(
            "output scale must be finite and positive".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct UiHostFrame<'a> {
    pub display_list: &'a DisplayList,
    pub textures: &'a TextureStore,
    pub logical_size: Size,
    pub output_scale: f32,
    pub commit: u64,
}

/// Object-safe application boundary shared by compositor and standalone
/// adapters. Product-specific state may reconcile its internal [`UiHost`]
/// before returning a frame, while both adapters retain identical input and
/// rendering semantics.
pub trait UiSurface {
    fn render(&mut self, logical_size: Size, output_scale: f32) -> UiResult<()>;
    fn display_list(&self) -> &DisplayList;
    fn textures(&self) -> &TextureStore;
    fn commit(&self) -> u64;
    fn dispatch(&mut self, event: &UiEvent) -> UiResult<DispatchResult>;
    fn pointer_capture(&self) -> Option<WidgetId>;
    fn keyboard_focus(&self) -> Option<WidgetId>;
    fn frame_requested(&mut self) -> bool;
}

impl UiSurface for UiHost {
    fn render(&mut self, logical_size: Size, output_scale: f32) -> UiResult<()> {
        self.resize(logical_size, output_scale)?;
        UiHost::render(self).map(|_| ())
    }

    fn display_list(&self) -> &DisplayList {
        &self.list
    }

    fn textures(&self) -> &TextureStore {
        self.root.texture_store().unwrap_or(&self.empty_textures)
    }

    fn commit(&self) -> u64 {
        self.commit
    }

    fn dispatch(&mut self, event: &UiEvent) -> UiResult<DispatchResult> {
        UiHost::dispatch(self, event)
    }

    fn pointer_capture(&self) -> Option<WidgetId> {
        self.root.pointer_capture()
    }

    fn keyboard_focus(&self) -> Option<WidgetId> {
        self.root.focused()
    }

    fn frame_requested(&mut self) -> bool {
        UiHost::frame_requested(self)
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use nkdhr_render::{Color, Rect};

    use super::*;
    use crate::{Constraints, MeasureCtx, PaintCtx, Widget};

    struct Probe;

    impl Widget for Probe {
        fn create_state(&self) -> Box<dyn Any> {
            Box::new(())
        }

        fn measure(&self, _ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> UiResult<Size> {
            Ok(constraints.max())
        }

        fn paint(&self, ctx: &mut PaintCtx<'_>) -> UiResult<()> {
            ctx.builder()
                .rect(Rect::new(0.0, 0.0, 12.0, 8.0), Color::WHITE)?;
            Ok(())
        }
    }

    #[test]
    fn host_reuses_complete_frames_and_resizes_at_one_boundary() {
        let root = UiRoot::new(Element::new(Probe)).unwrap();
        let mut host = UiHost::new(root, Size::new(80.0, 40.0), 1.0).unwrap();
        let first = host.render().unwrap();
        assert_eq!(first.commit, 1);
        assert_eq!(first.display_list.len(), 1);
        assert_eq!(host.render().unwrap().commit, 1);

        assert!(host.resize(Size::new(100.0, 50.0), 2.0).unwrap());
        let resized = host.render().unwrap();
        assert_eq!(resized.commit, 2);
        assert_eq!(resized.logical_size, Size::new(100.0, 50.0));
        assert_eq!(resized.output_scale, 2.0);
    }
}

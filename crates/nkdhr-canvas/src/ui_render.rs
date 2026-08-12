//! Smithay GLES adapter for compositor-owned retained UI display lists.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use nkdhr_render::gles::{GlesBackend, PreparedDisplayList};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer};
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::backend::renderer::{ErasedContextId, Renderer, RendererSuper};
use smithay::utils::{Buffer, Physical, Rectangle, Scale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementSignature {
    pub node_commit: u64,
    pub geometry: Rectangle<i32, Physical>,
    pub target: smithay::utils::Size<i32, Physical>,
    pub logical_x_bits: u64,
    pub logical_y_bits: u64,
    pub zoom_bits: u64,
    pub output_scale_bits: u64,
}

struct NodeContext {
    id: Id,
    commit: CommitCounter,
    signature: Option<PlacementSignature>,
    backend: Rc<RefCell<GlesBackend>>,
    prepared: Option<Rc<PreparedDisplayList>>,
}

/// Context-bound caches for retained UI nodes. A distinct backend per
/// `(node, GLES context)` prevents one node's prepare generation from making
/// another node stale before Smithay draws it.
#[derive(Default)]
pub struct PinnedGlesRenderer {
    contexts: HashMap<(String, ErasedContextId), NodeContext>,
}

impl PinnedGlesRenderer {
    pub fn prepare<R: GlesTargetRenderer>(
        &mut self,
        renderer: &mut R,
        node_id: &str,
        display_list: &nkdhr_render::DisplayList,
        textures: &nkdhr_render::TextureStore,
        signature: PlacementSignature,
        scale: f32,
    ) -> Result<UiRenderElement, String>
    where
        R::TextureId: 'static,
    {
        let key = (node_id.to_owned(), renderer.context_id().erased());
        if !self.contexts.contains_key(&key) {
            let backend = renderer.create_ui_backend()?;
            self.contexts.insert(
                key.clone(),
                NodeContext {
                    id: Id::new(),
                    commit: CommitCounter::default(),
                    signature: None,
                    backend: Rc::new(RefCell::new(backend)),
                    prepared: None,
                },
            );
        }
        let context = self
            .contexts
            .get_mut(&key)
            .expect("a GLES node context was inserted above");
        let prepared = if context.signature != Some(signature) {
            context.commit.increment();
            context.signature = Some(signature);
            let prepared = Rc::new(renderer.prepare_ui_backend(
                &mut context.backend.borrow_mut(),
                display_list,
                textures,
                signature.target,
                scale,
            )?);
            context.prepared = Some(Rc::clone(&prepared));
            prepared
        } else {
            Rc::clone(
                context
                    .prepared
                    .as_ref()
                    .expect("a matching signature has a prepared display list"),
            )
        };
        Ok(UiRenderElement {
            id: context.id.clone(),
            commit: context.commit,
            geometry: signature.geometry,
            backend: Rc::clone(&context.backend),
            prepared,
        })
    }
}

pub struct UiRenderElement {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
    backend: Rc<RefCell<GlesBackend>>,
    prepared: Rc<PreparedDisplayList>,
}

impl Element for UiRenderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size((self.geometry.size.w as f64, self.geometry.size.h as f64).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: GlesTargetRenderer> RenderElement<R> for UiRenderElement {
    fn draw(
        &self,
        frame: &mut <R as RendererSuper>::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <R as RendererSuper>::Error> {
        let output_damage = damage
            .iter()
            .map(|damage| Rectangle::new(damage.loc + dst.loc, damage.size))
            .collect::<Vec<_>>();
        let expanded = self.prepared.expand_damage(&output_damage);
        R::draw_ui_backend(
            frame,
            &mut self.backend.borrow_mut(),
            &self.prepared,
            &expanded,
        )
    }
}

/// The two compositor renderer shapes that can expose their active GLES
/// context/frame without leaking that distinction into canvas scene code.
pub trait GlesTargetRenderer: Renderer {
    fn create_ui_backend(&mut self) -> Result<GlesBackend, String>;

    fn prepare_ui_backend(
        &mut self,
        backend: &mut GlesBackend,
        display_list: &nkdhr_render::DisplayList,
        textures: &nkdhr_render::TextureStore,
        target: smithay::utils::Size<i32, Physical>,
        scale: f32,
    ) -> Result<PreparedDisplayList, String>;

    fn draw_ui_backend<'frame, 'buffer>(
        frame: &mut <Self as RendererSuper>::Frame<'frame, 'buffer>,
        backend: &mut GlesBackend,
        prepared: &PreparedDisplayList,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), <Self as RendererSuper>::Error>
    where
        'buffer: 'frame,
        Self: 'frame;
}

impl GlesTargetRenderer for GlesRenderer {
    fn create_ui_backend(&mut self) -> Result<GlesBackend, String> {
        GlesBackend::new(self).map_err(|error| error.to_string())
    }

    fn prepare_ui_backend(
        &mut self,
        backend: &mut GlesBackend,
        display_list: &nkdhr_render::DisplayList,
        textures: &nkdhr_render::TextureStore,
        target: smithay::utils::Size<i32, Physical>,
        scale: f32,
    ) -> Result<PreparedDisplayList, String> {
        backend
            .prepare(self, display_list, textures, target, scale)
            .map_err(|error| error.to_string())
    }

    fn draw_ui_backend<'frame, 'buffer>(
        frame: &mut GlesFrame<'frame, 'buffer>,
        backend: &mut GlesBackend,
        prepared: &PreparedDisplayList,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError>
    where
        'buffer: 'frame,
        Self: 'frame,
    {
        backend.draw(frame, prepared, damage).map_err(|error| {
            eprintln!("nkdhr-canvas: retained UI draw failed: {error}");
            GlesError::BlitError
        })
    }
}

#[cfg(feature = "tty")]
impl<'render, 'target, R, T> GlesTargetRenderer
    for smithay::backend::renderer::multigpu::MultiRenderer<'render, 'target, R, T>
where
    R: smithay::backend::renderer::multigpu::GraphicsApi + 'static,
    T: smithay::backend::renderer::multigpu::GraphicsApi + 'static,
    R::Error: 'static,
    T::Error: 'static,
    R::Device: smithay::backend::renderer::multigpu::ApiDevice<Renderer = GlesRenderer>,
    <T::Device as smithay::backend::renderer::multigpu::ApiDevice>::Renderer:
        smithay::backend::renderer::ImportDma + smithay::backend::renderer::ImportMem,
    <<T::Device as smithay::backend::renderer::multigpu::ApiDevice>::Renderer as RendererSuper>::Error:
        'static,
{
    fn create_ui_backend(&mut self) -> Result<GlesBackend, String> {
        GlesBackend::new(self.as_mut()).map_err(|error| error.to_string())
    }

    fn prepare_ui_backend(
        &mut self,
        backend: &mut GlesBackend,
        display_list: &nkdhr_render::DisplayList,
        textures: &nkdhr_render::TextureStore,
        target: smithay::utils::Size<i32, Physical>,
        scale: f32,
    ) -> Result<PreparedDisplayList, String> {
        backend
            .prepare(self.as_mut(), display_list, textures, target, scale)
            .map_err(|error| error.to_string())
    }

    fn draw_ui_backend<'frame, 'buffer>(
        frame: &mut <Self as RendererSuper>::Frame<'frame, 'buffer>,
        backend: &mut GlesBackend,
        prepared: &PreparedDisplayList,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), <Self as RendererSuper>::Error>
    where
        'buffer: 'frame,
        Self: 'frame,
    {
        backend
            .draw(frame.as_mut(), prepared, damage)
            .map_err(|error| {
                eprintln!("nkdhr-canvas: retained UI draw failed: {error}");
                smithay::backend::renderer::multigpu::Error::Render(GlesError::BlitError)
            })
    }
}

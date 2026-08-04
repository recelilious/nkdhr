use std::time::Instant;

use smithay::backend::renderer::element::AsRenderElements;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::utils::output_update;
use smithay::input::pointer::CursorImageStatus;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle, Size};
use smithay::wayland::compositor::{
    SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::fractional_scale::with_fractional_scale;

use crate::canvas::world::{Canvas, ManagedWindow, Viewport};
use crate::state::App;
use crate::widget_host::{PinnedLayer, PinnedRenderData};

smithay::backend::renderer::element::render_elements! {
    /// Unified element list for client surfaces and compositor-owned
    /// server-side decoration chrome.
    pub CanvasRenderElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Memory=MemoryRenderBufferRenderElement<R>,
    Decoration=SolidColorRenderElement,
}

/// Advance every output group's independent viewport animation once before
/// drawing the next set of frames.
pub fn advance_animations(app: &mut App) {
    app.popup_manager.cleanup();
    let reclaimed = app.cleanup_dead_client_state();
    if reclaimed > 0 {
        println!("nkdhr-canvas: reclaimed {reclaimed} dead window(s)");
    }
    let now = Instant::now();
    for view in app.group_views.values_mut() {
        let Some(animation) = &view.animation else {
            continue;
        };
        match animation.advance(now) {
            Some(viewport) => view.viewport = viewport,
            None => {
                view.viewport = animation.target();
                view.animation = None;
            }
        }
    }
}

/// Fire every pending `wl_surface.frame` callback in a surface tree after
/// the compositor has presented that canvas frame.
pub fn send_frame_callbacks(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surface, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

pub fn window_group_rect(
    window: &ManagedWindow,
    viewport: Viewport,
    group_size: Size<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let location = viewport
        .to_group_logical(window.position, group_size)
        .to_i32_round();
    let size = window.size();
    let size = (
        (size.w * viewport.zoom).round().max(0.0) as i32,
        (size.h * viewport.zoom).round().max(0.0) as i32,
    )
        .into();
    Rectangle::new(location, size)
}

pub fn window_render_elements<R>(
    renderer: &mut R,
    window: &ManagedWindow,
    viewport: Viewport,
    group_size: Size<i32, Logical>,
    output_group_location: smithay::utils::Point<i32, Logical>,
    output_scale: f64,
) -> Vec<CanvasRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    let group_point = viewport.to_group_logical(window.position, group_size);
    let local = group_point - output_group_location.to_f64();
    let offset = local.to_physical(output_scale).to_i32_round();
    let scale = viewport.zoom * output_scale;
    let mut elements = window.window.render_elements::<CanvasRenderElement<R>>(
        renderer,
        offset,
        scale.into(),
        1.0,
    );

    if let Some((rect, mut buffer)) = window.decoration() {
        let size = (
            rect.size.w.round().max(1.0) as i32,
            rect.size.h.round().max(1.0) as i32,
        );
        buffer.resize(size);
        let group_point = viewport.to_group_logical(rect.loc, group_size);
        let local = group_point - output_group_location.to_f64();
        let offset = local.to_physical(output_scale).to_i32_round();
        elements.push(
            SolidColorRenderElement::from_buffer(
                &buffer,
                offset,
                scale,
                1.0,
                smithay::backend::renderer::element::Kind::Unspecified,
            )
            .into(),
        );
    }

    elements
}

/// Adapt renderer-independent pinned-node payloads to the concrete renderer
/// used by either backend. Elements are returned front-to-back, matching the
/// ordering expected by Smithay's render helpers.
pub fn pinned_render_elements<R>(
    renderer: &mut R,
    canvas: &Canvas,
    layer: PinnedLayer,
    viewport: Viewport,
    group_size: Size<i32, Logical>,
    output_group_location: Point<i32, Logical>,
    output_scale: f64,
) -> Vec<CanvasRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    canvas
        .pinned_nodes()
        .rev()
        .filter(|node| node.layer() == layer)
        .filter_map(|node| {
            let rect = node.world_rect();
            let width = (rect.size.w * viewport.zoom * output_scale).round() as i32;
            let height = (rect.size.h * viewport.zoom * output_scale).round() as i32;
            if width <= 0 || height <= 0 {
                return None;
            }

            let group_point = viewport.to_group_logical(rect.loc, group_size);
            let local = group_point - output_group_location.to_f64();
            let offset = local.to_physical(output_scale);
            match node.render_data() {
                PinnedRenderData::Memory {
                    buffer,
                    source_size,
                } => MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    offset,
                    buffer,
                    None,
                    Some(Rectangle::from_size(source_size.to_f64())),
                    Some((width, height).into()),
                    Kind::Unspecified,
                )
                .map(CanvasRenderElement::from)
                .ok(),
            }
        })
        .collect()
}

/// Render the pointer in output-local physical coordinates. Client cursor
/// surfaces keep their committed hotspot; the compositor-owned arrow covers
/// the background and clients that do not set a cursor surface.
pub fn cursor_render_elements<R>(
    renderer: &mut R,
    app: &App,
    output_location: Point<i32, Logical>,
    output_scale: f64,
) -> Vec<CanvasRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let pointer_location = app.seat.get_pointer().map_or_else(
        || Point::<f64, Logical>::from((0.0, 0.0)),
        |pointer| pointer.current_location(),
    );
    let hotspot = app.cursor.hotspot().to_f64();
    let location = (pointer_location - output_location.to_f64() - hotspot)
        .to_physical(output_scale)
        .to_i32_round();

    match app.cursor.status() {
        CursorImageStatus::Hidden => Vec::new(),
        CursorImageStatus::Named(_) => MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location.to_f64(),
            app.cursor.fallback(),
            None,
            Some(Rectangle::from_size(app.cursor.fallback_size().to_f64())),
            Some(
                app.cursor
                    .fallback_size()
                    .to_f64()
                    .upscale(output_scale)
                    .to_i32_round(),
            ),
            Kind::Cursor,
        )
        .map(CanvasRenderElement::from)
        .into_iter()
        .collect(),
        CursorImageStatus::Surface(surface) => render_elements_from_surface_tree(
            renderer,
            &surface,
            location,
            output_scale,
            1.0,
            Kind::Cursor,
        ),
    }
}

pub fn dnd_icon_render_elements<R>(
    renderer: &mut R,
    app: &App,
    output_location: Point<i32, Logical>,
    output_scale: f64,
) -> Vec<CanvasRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let Some(icon) = app.dnd_icon.as_ref().filter(|icon| icon.surface.alive()) else {
        return Vec::new();
    };
    let pointer_location = app.seat.get_pointer().map_or_else(
        || Point::<f64, Logical>::from((0.0, 0.0)),
        |pointer| pointer.current_location(),
    );
    let location = (pointer_location - output_location.to_f64() + icon.offset.to_f64())
        .to_physical(output_scale)
        .to_i32_round();
    render_elements_from_surface_tree(
        renderer,
        &icon.surface,
        location,
        output_scale,
        1.0,
        Kind::Cursor,
    )
}

pub fn send_pointer_frame_callbacks(app: &App, time: u32) {
    if let Some(surface) = app.cursor.surface() {
        send_frame_callbacks(&surface, time);
    }
    if let Some(icon) = app.dnd_icon.as_ref().filter(|icon| icon.surface.alive()) {
        send_frame_callbacks(&icon.surface, time);
    }
}

/// Keep wl_surface output membership and fractional-scale preference in
/// sync with the same geometry the renderer uses.
pub fn update_window_output(
    window: &ManagedWindow,
    output: &Output,
    overlap: Option<Rectangle<i32, Logical>>,
    preferred_scale: f64,
) {
    let Some(surface) = window.wl_surface() else {
        return;
    };
    output_update(output, overlap, &surface);
    window.window.with_surfaces(|_, states| {
        with_fractional_scale(states, |state| state.set_preferred_scale(preferred_scale));
    });
}

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
use smithay::utils::{IsAlive, Logical, Point, Rectangle};
use smithay::wayland::compositor::{
    SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::fractional_scale::with_fractional_scale;

use crate::canvas::output_group::{ResolvedOutput, ResolvedOutputGroup};
use crate::canvas::world::{Canvas, ManagedWindow, Viewport};
use crate::state::{App, WorkspaceFade};
use crate::ui_render::{
    GlesTargetRenderer, PinnedGlesRenderer, PlacementSignature, UiRenderElement,
};
use crate::widget_host::{PinnedLayer, PinnedRenderData};

smithay::backend::renderer::element::render_elements! {
    /// Unified element list for client surfaces and compositor-owned
    /// server-side decoration chrome.
    pub CanvasRenderElement<R> where R: ImportAll + ImportMem + GlesTargetRenderer;
    Surface=WaylandSurfaceRenderElement<R>,
    Memory=MemoryRenderBufferRenderElement<R>,
    Decoration=SolidColorRenderElement,
    NkdhrUi=UiRenderElement,
}

/// Advance viewport and compositor-owned window-position animations once
/// before drawing the next set of frames.
pub fn advance_animations(app: &mut App) {
    app.popup_manager.cleanup();
    let reclaimed = app.cleanup_dead_client_state();
    if reclaimed > 0 {
        println!("nkdhr-canvas: reclaimed {reclaimed} dead window(s)");
    }
    let now = Instant::now();
    for view in app.group_views.values_mut() {
        if let Some(animation) = &view.animation {
            match animation.advance(now) {
                Some(viewport) => view.viewport = viewport,
                None => {
                    view.viewport = animation.target();
                    view.animation = None;
                }
            }
        }
        if view
            .workspace_fade
            .as_mut()
            .is_some_and(|fade| !fade.advance(now))
        {
            view.workspace_fade = None;
        }
    }
    for canvas in app.canvases.values_mut() {
        canvas.advance_animations(now);
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
    canvas_anchor: Point<f64, Logical>,
) -> Rectangle<i32, Logical> {
    let location = viewport
        .to_group_logical(window.position, canvas_anchor)
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
    canvas_anchor: Point<f64, Logical>,
    output_group_location: smithay::utils::Point<i32, Logical>,
    output_scale: f64,
    alpha: f32,
) -> Vec<CanvasRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    let group_point = viewport.to_group_logical(window.position, canvas_anchor);
    let local = group_point - output_group_location.to_f64();
    let offset = local.to_physical(output_scale).to_i32_round();
    let scale = viewport.zoom * output_scale;
    let mut elements = window.window.render_elements::<CanvasRenderElement<R>>(
        renderer,
        offset,
        scale.into(),
        alpha,
    );

    if let Some((rect, mut buffer)) = window.decoration() {
        let size = (
            rect.size.w.round().max(1.0) as i32,
            rect.size.h.round().max(1.0) as i32,
        );
        buffer.resize(size);
        let group_point = viewport.to_group_logical(rect.loc, canvas_anchor);
        let local = group_point - output_group_location.to_f64();
        let offset = local.to_physical(output_scale).to_i32_round();
        elements.push(
            SolidColorRenderElement::from_buffer(
                &buffer,
                offset,
                scale,
                alpha,
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
#[derive(Debug, Clone, Copy)]
pub struct PinnedOutputPlacement {
    pub viewport: Viewport,
    pub canvas_anchor: Point<f64, Logical>,
    pub output_group_location: Point<i32, Logical>,
    pub output_scale: f64,
    pub target: smithay::utils::Size<i32, smithay::utils::Physical>,
}

pub fn pinned_render_elements<R>(
    ui_renderer: &mut PinnedGlesRenderer,
    renderer: &mut R,
    canvas: &mut Canvas,
    layer: PinnedLayer,
    placement: PinnedOutputPlacement,
) -> Vec<CanvasRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem + GlesTargetRenderer,
    R::TextureId: Clone + Send + 'static,
{
    let PinnedOutputPlacement {
        viewport,
        canvas_anchor,
        output_group_location,
        output_scale,
        target,
    } = placement;
    canvas
        .pinned_nodes_mut()
        .rev()
        .filter(|node| node.layer() == layer)
        .filter_map(|node| {
            let rect = node.world_rect();
            let width = (rect.size.w * viewport.zoom * output_scale).round() as i32;
            let height = (rect.size.h * viewport.zoom * output_scale).round() as i32;
            if width <= 0 || height <= 0 {
                return None;
            }

            let group_point = viewport.to_group_logical(rect.loc, canvas_anchor);
            let local = group_point - output_group_location.to_f64();
            let offset = local.to_physical(output_scale);
            if let Err(error) = node.prepare_frame((viewport.zoom * output_scale) as f32) {
                eprintln!(
                    "nkdhr-canvas: retained UI node {:?} frame failed: {error}",
                    node.id()
                );
                return None;
            }
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
                PinnedRenderData::NkdhrUi {
                    display_list,
                    textures,
                    commit,
                } => {
                    let placement =
                        nkdhr_render::Transform::translation(local.x as f32, local.y as f32)
                            .concat(nkdhr_render::Transform::scale(
                                viewport.zoom as f32,
                                viewport.zoom as f32,
                            ));
                    let placed = match display_list.transformed(placement) {
                        Ok(placed) => placed,
                        Err(error) => {
                            eprintln!(
                                "nkdhr-canvas: retained UI node {:?} placement failed: {error}",
                                node.id()
                            );
                            return None;
                        }
                    };
                    let geometry = Rectangle::new(offset.to_i32_round(), (width, height).into());
                    let signature = PlacementSignature {
                        node_commit: commit,
                        geometry,
                        target,
                        logical_x_bits: local.x.to_bits(),
                        logical_y_bits: local.y.to_bits(),
                        zoom_bits: viewport.zoom.to_bits(),
                        output_scale_bits: output_scale.to_bits(),
                    };
                    ui_renderer
                        .prepare(
                            renderer,
                            node.id(),
                            &placed,
                            textures,
                            signature,
                            output_scale as f32,
                        )
                        .map(CanvasRenderElement::from)
                        .map_err(|error| {
                            eprintln!(
                                "nkdhr-canvas: retained UI node {:?} prepare failed: {error}",
                                node.id()
                            );
                        })
                        .ok()
                }
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

/// Publish wl_output membership for the two workspace stacks participating in
/// a transition and explicitly leave every inactive canvas. Without the leave
/// pass, a hidden workspace would keep stale output membership forever because
/// it no longer participates in the normal render traversal.
pub fn update_workspace_output_membership(
    app: &App,
    output: &Output,
    group: &ResolvedOutputGroup,
    resolved_output: &ResolvedOutput,
    canvas_name: &str,
    viewport: Viewport,
    workspace_fade: Option<&WorkspaceFade>,
) {
    let output_rect = Rectangle::new(resolved_output.group_location, resolved_output.logical_size);
    for (candidate_name, canvas) in &app.canvases {
        let candidate_viewport = if candidate_name == canvas_name {
            Some(viewport)
        } else {
            workspace_fade
                .filter(|fade| fade.canvas == *candidate_name)
                .map(|fade| fade.viewport)
        };
        for window in canvas.windows() {
            let Some(candidate_viewport) = candidate_viewport else {
                update_window_output(window, output, None, resolved_output.scale);
                continue;
            };
            let window_rect = window_group_rect(window, candidate_viewport, group.canvas_anchor);
            let overlap = window_rect.intersection(output_rect).map(|intersection| {
                Rectangle::new(intersection.loc - window_rect.loc, intersection.size)
            });
            let preferred_scale = group
                .outputs
                .iter()
                .filter(|candidate| {
                    Rectangle::new(candidate.group_location, candidate.logical_size)
                        .overlaps(window_rect)
                })
                .map(|candidate| candidate.scale)
                .fold(resolved_output.scale, f64::max);
            update_window_output(window, output, overlap, preferred_scale);
        }
    }
}

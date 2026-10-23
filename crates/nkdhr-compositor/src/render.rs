//! Per-output composition, shared by both backends, and the protocol
//! bookkeeping that follows each render pass: `wl_surface` output
//! membership, preferred fractional scales and frame callbacks.
//!
//! Every function here reads the window store and the output layout, never
//! a second notion of what is on screen, so what clients are told always
//! matches what is drawn.

use std::time::Duration;

use smithay::backend::renderer::element::AsRenderElements;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::{Color32F, ImportAll, ImportMem, Renderer};
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::desktop::{PopupManager, utils::output_update};
use smithay::input::pointer::CursorImageStatus;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle};
use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};
use smithay::wayland::fractional_scale::with_fractional_scale;

use crate::outputs::ResolvedOutput;
use crate::state::App;
use crate::windows::ManagedWindow;

smithay::backend::renderer::element::render_elements! {
    /// Every kind of element the compositor draws. Both backends draw the
    /// same list; a new kind added here works on both, as long as it stays
    /// renderer-independent.
    pub OutputRenderElement<R> where R: ImportAll + ImportMem;
    /// Client surfaces: windows, popups, subsurfaces, lock surfaces, client
    /// cursor surfaces and the drag-and-drop icon.
    Surface=WaylandSurfaceRenderElement<R>,
    /// The built-in fallback cursor.
    Memory=MemoryRenderBufferRenderElement<R>,
}

/// The frame's clear colour behind all windows: a neutral dark grey.
const BACKGROUND: Color32F = Color32F::new(0.11, 0.12, 0.16, 1.0);
/// The clear colour while the session is locked, visible wherever the lock
/// client has not (yet) provided a surface.
const LOCKED_BACKGROUND: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);

/// The colour a frame is cleared to before its elements are drawn.
pub fn clear_color(app: &App) -> Color32F {
    if app.session_locked() {
        LOCKED_BACKGROUND
    } else {
        BACKGROUND
    }
}

/// One output's elements, front to back:
///
/// 1. the cursor, unless `include_cursor` is false (a pending screencopy
///    asked for no cursor overlay);
/// 2. when unlocked, the drag-and-drop icon;
/// 3. when locked, the output's lock surface, if the locker provided one;
/// 4. when unlocked, every placed window that overlaps the output, top to
///    bottom, popups included.
pub fn output_elements<R>(
    renderer: &mut R,
    app: &App,
    output: &ResolvedOutput,
    include_cursor: bool,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let mut elements = Vec::new();
    if include_cursor {
        elements.extend(cursor_elements(renderer, app, output));
    }
    if app.session_locked() {
        if let Some(surface) = app.lock_surface_for_output(&output.name) {
            elements.extend(render_elements_from_surface_tree(
                renderer,
                &surface,
                (0, 0),
                output.scale,
                1.0,
                Kind::Unspecified,
            ));
        }
        return elements;
    }
    elements.extend(dnd_icon_elements(renderer, app, output));
    for window in app.windows.placed().rev() {
        let Some(location) = window
            .location()
            .filter(|_| overlaps_output(window, output))
        else {
            continue;
        };
        let origin = (location - output.global_location).to_physical_precise_round(output.scale);
        elements.extend(window.window().render_elements::<OutputRenderElement<R>>(
            renderer,
            origin,
            output.scale.into(),
            1.0,
        ));
    }
    elements
}

/// The client's cursor surface at the pointer minus its hotspot, or the
/// built-in arrow for named cursors and the background. Both are given
/// their logical size: the renderer multiplies it by the output scale it
/// draws with, so the cursor keeps its logical size on every output.
fn cursor_elements<R>(
    renderer: &mut R,
    app: &App,
    output: &ResolvedOutput,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let hotspot = app.cursor.hotspot().to_f64();
    let location = (app.pointer_location() - output.global_location.to_f64() - hotspot)
        .to_physical(output.scale)
        .to_i32_round();

    match app.cursor.status() {
        CursorImageStatus::Hidden => Vec::new(),
        CursorImageStatus::Named(_) => MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location.to_f64(),
            app.cursor.fallback(),
            None,
            Some(Rectangle::from_size(app.cursor.fallback_size().to_f64())),
            Some(app.cursor.fallback_size()),
            Kind::Cursor,
        )
        .map(OutputRenderElement::from)
        .into_iter()
        .collect(),
        CursorImageStatus::Surface(surface) => render_elements_from_surface_tree(
            renderer,
            &surface,
            location,
            output.scale,
            1.0,
            Kind::Cursor,
        ),
    }
}

/// The drag-and-drop icon at the pointer, offset by the hotspot recorded
/// when the drag started.
fn dnd_icon_elements<R>(
    renderer: &mut R,
    app: &App,
    output: &ResolvedOutput,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let Some(icon) = app.dnd_icon.as_ref().filter(|icon| icon.surface.alive()) else {
        return Vec::new();
    };
    let location = (app.pointer_location() - output.global_location.to_f64()
        + icon.offset.to_f64())
    .to_physical(output.scale)
    .to_i32_round();
    render_elements_from_surface_tree(
        renderer,
        &icon.surface,
        location,
        output.scale,
        1.0,
        Kind::Cursor,
    )
}

fn overlaps_output(window: &ManagedWindow, output: &ResolvedOutput) -> bool {
    window
        .global_bbox()
        .is_some_and(|bbox| bbox.overlaps(output.rect()))
}

/// Keeps `wl_surface` output membership and the preferred fractional scale
/// in step with what is drawn; backends call it after composing an output.
///
/// Each placed window whose bounding box overlaps the output enters it, its
/// popups and subsurfaces included (`wl_surface.enter`); every other window
/// leaves it. A window's preferred fractional scale is the highest scale
/// among the outputs it overlaps; a window that overlaps none keeps its
/// last preference. While the session is locked, the output's lock surface
/// covers it: the surface enters the output and prefers its scale, so a
/// locker draws at the output's resolution.
pub fn update_output_membership(app: &App, output: &Output, resolved: &ResolvedOutput) {
    if let Some(surface) = app.lock_surface_for_output(&resolved.name) {
        output_update(
            output,
            Some(Rectangle::from_size(resolved.logical_size)),
            &surface,
        );
        with_surface_tree_downward(
            &surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |_, states, _| {
                with_fractional_scale(states, |state| state.set_preferred_scale(resolved.scale));
            },
            |_, _, _| true,
        );
    }

    let output_rect = resolved.rect();
    for window in app.windows.iter() {
        let Some(root) = window.wl_surface() else {
            continue;
        };
        let overlap = window.global_bbox().and_then(|bbox| {
            let mut overlap = bbox.intersection(output_rect)?;
            overlap.loc -= window.location()?;
            Some(overlap)
        });
        update_membership_tree(window, &root, output, overlap);

        let preferred_scale = window.global_bbox().and_then(|bbox| {
            app.output_layout()
                .outputs()
                .iter()
                .filter(|candidate| candidate.rect().overlaps(bbox))
                .map(|candidate| candidate.scale)
                .reduce(f64::max)
        });
        if let Some(scale) = preferred_scale {
            window.window().with_surfaces(|_, states| {
                with_fractional_scale(states, |state| state.set_preferred_scale(scale));
            });
        }
    }
}

/// Applies one window's overlap with an output, in window-local
/// coordinates, to its root surface tree and to each popup tree, which
/// sits at its own offset inside the window.
fn update_membership_tree(
    window: &ManagedWindow,
    root: &WlSurface,
    output: &Output,
    overlap: Option<Rectangle<i32, Logical>>,
) {
    output_update(output, overlap, root);
    let geometry_origin = window.geometry().loc;
    for (popup, popup_location) in PopupManager::popups_for_surface(root) {
        let popup_origin: Point<i32, Logical> =
            geometry_origin + popup_location - popup.geometry().loc;
        let popup_overlap = overlap.map(|mut overlap| {
            overlap.loc -= popup_origin;
            overlap
        });
        output_update(output, popup_overlap, popup.wl_surface());
    }
}

/// Sends the frame callbacks for one output's render pass, to exactly the
/// surfaces [`output_elements`] draws there.
///
/// Unlocked, every placed window overlapping the output receives its
/// callbacks, popups and subsurfaces included, and so does the
/// drag-and-drop icon; locked, only the output's lock surface does. The
/// cursor surface receives its callbacks on every pass.
pub fn send_frame_callbacks(app: &App, output: &Output, resolved: &ResolvedOutput) {
    let time = app.start_time.elapsed();
    if app.session_locked() {
        if let Some(surface) = app.lock_surface_for_output(&resolved.name) {
            send_surface_frames(&surface, output, time);
        }
    } else {
        for window in app
            .windows
            .placed()
            .filter(|window| overlaps_output(window, resolved))
        {
            window
                .window()
                .send_frame(output, time, None, |_, _| Some(output.clone()));
        }
        if let Some(icon) = app.dnd_icon.as_ref().filter(|icon| icon.surface.alive()) {
            send_surface_frames(&icon.surface, output, time);
        }
    }
    if let Some(surface) = app.cursor.surface() {
        send_surface_frames(&surface, output, time);
    }
}

fn send_surface_frames(surface: &WlSurface, output: &Output, time: Duration) {
    send_frames_surface_tree(surface, output, time, None, |_, _| Some(output.clone()));
}

/// Runs once per loop iteration, before rendering: popup bookkeeping and
/// the reaping of state left behind by clients that died abruptly.
///
/// The order matters. `PopupManager::cleanup` notes the popups clients
/// destroyed, which can end a popup-grab chain; settling the popup grab
/// then releases a chain that is over and returns the keyboard focus it
/// held to the chain's root window; only then does reaping apply the focus
/// fallback to a focus that died for any other reason.
pub fn refresh(app: &mut App) {
    app.popup_manager.cleanup();
    app.settle_popup_grab();
    let reclaimed = app.reap_dead_clients();
    if reclaimed > 0 {
        println!("nkdhr-compositor: reclaimed {reclaimed} dead window(s)");
    }
}

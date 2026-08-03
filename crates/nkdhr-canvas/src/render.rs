use std::time::Instant;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{
    SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};

use crate::state::App;

/// Advance every output group's independent viewport animation once before
/// drawing the next set of frames.
pub fn advance_animations(app: &mut App) {
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

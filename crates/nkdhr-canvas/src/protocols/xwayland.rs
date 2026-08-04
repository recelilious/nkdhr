use std::os::fd::OwnedFd;

use smithay::delegate_xwayland_shell;
use smithay::desktop::Window;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle, SERIAL_COUNTER};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
    set_primary_selection,
};
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::X11Surface;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, X11Wm, XwmHandler, XwmId};

use crate::state::{App, KeyboardFocusTarget, SelectionOwner};

impl XWaylandShellHandler for App {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.protocols.xwayland_shell
    }

    fn surface_associated(&mut self, _xwm: XwmId, _wl_surface: WlSurface, surface: X11Surface) {
        if self.protocols.pending_x11_maps.remove(&surface.window_id()) {
            self.map_x11_window(surface, true);
        }
    }
}

impl App {
    pub fn install_xwm<D>(
        &mut self,
        xwm: X11Wm,
        display_number: u32,
        loop_handle: LoopHandle<'static, D>,
    ) where
        D: XwmHandler + 'static,
    {
        self.protocols.xwm = Some(xwm);
        self.protocols.x_display = Some(display_number);
        self.protocols.x_selection_writer = Some(Box::new(move |xwm, target, mime_type, fd| {
            xwm.send_selection(target, mime_type, fd, loop_handle.clone())?;
            Ok(())
        }));
        println!("nkdhr-canvas: XWayland ready on DISPLAY=:{display_number}");
    }

    fn map_x11_window(&mut self, surface: X11Surface, focus: bool) {
        if surface.wl_surface().is_none()
            || self
                .canvases
                .values()
                .flat_map(|canvas| canvas.windows())
                .any(|window| window.matches_x11(&surface))
        {
            return;
        }

        let group = self.active_group.clone();
        let canvas_name = self.active_view().canvas.clone();
        let position = self
            .active_canvas_mut()
            .map(Window::new_x11_window(surface.clone()));
        println!(
            "nkdhr-canvas: mapped X11 window {} on canvas {canvas_name:?} via group {group:?} at world {position:?}",
            surface.window_id()
        );

        if focus
            && !self.session_locked()
            && let Some(keyboard) = self.seat.get_keyboard()
        {
            keyboard.set_focus(
                self,
                Some(KeyboardFocusTarget::X11(surface)),
                SERIAL_COUNTER.next_serial(),
            );
        }
    }

    fn unmap_x11_window(&mut self, surface: &X11Surface) {
        for canvas in self.canvases.values_mut() {
            canvas.unmap_x11(surface);
        }
        for view in self.group_views.values_mut() {
            if matches!(view.keyboard_focus.as_ref(), Some(KeyboardFocusTarget::X11(focused)) if focused == surface)
            {
                view.keyboard_focus = None;
            }
        }
    }
}

impl XwmHandler for App {
    fn xwm_state(&mut self, xwm: XwmId) -> &mut X11Wm {
        let state = self
            .protocols
            .xwm
            .as_mut()
            .expect("XWM callbacks cannot run before the XWM is installed");
        assert_eq!(state.id(), xwm, "unexpected XWayland WM instance");
        state
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(error) = window.set_mapped(true) {
            eprintln!(
                "nkdhr-canvas: failed to map X11 window {}: {error}",
                window.window_id()
            );
            return;
        }
        if window.wl_surface().is_some() {
            self.map_x11_window(window, true);
        } else {
            self.protocols.pending_x11_maps.insert(window.window_id());
        }
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if window.wl_surface().is_some() {
            self.map_x11_window(window, false);
        } else {
            self.protocols.pending_x11_maps.insert(window.window_id());
        }
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.protocols.pending_x11_maps.remove(&window.window_id());
        self.unmap_x11_window(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.protocols.pending_x11_maps.remove(&window.window_id());
        self.unmap_x11_window(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut geometry = window.geometry();
        if let Some(x) = x {
            geometry.loc.x = x;
        }
        if let Some(y) = y {
            geometry.loc.y = y;
        }
        if let Some(width) = width {
            geometry.size.w = width.max(1) as i32;
        }
        if let Some(height) = height {
            geometry.size.h = height.max(1) as i32;
        }
        let _ = window.configure(geometry);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        if window.is_override_redirect() {
            let position = (f64::from(geometry.loc.x), f64::from(geometry.loc.y)).into();
            for canvas in self.canvases.values_mut() {
                canvas.set_x11_position(&window, position);
            }
        }
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        resize_edge: ResizeEdge,
    ) {
        let Some(surface) = window.wl_surface() else {
            return;
        };
        let edge = match resize_edge {
            ResizeEdge::Top => crate::canvas::world::ResizeEdge::Top,
            ResizeEdge::Bottom => crate::canvas::world::ResizeEdge::Bottom,
            ResizeEdge::Left => crate::canvas::world::ResizeEdge::Left,
            ResizeEdge::Right => crate::canvas::world::ResizeEdge::Right,
            ResizeEdge::TopLeft => crate::canvas::world::ResizeEdge::TopLeft,
            ResizeEdge::TopRight => crate::canvas::world::ResizeEdge::TopRight,
            ResizeEdge::BottomLeft => crate::canvas::world::ResizeEdge::BottomLeft,
            ResizeEdge::BottomRight => crate::canvas::world::ResizeEdge::BottomRight,
        };
        if self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed())
        {
            crate::input::begin_client_resize(self, &surface, edge);
        }
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        let Some(surface) = window.wl_surface() else {
            return;
        };
        if self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed())
        {
            crate::input::begin_client_move(self, &surface);
        }
    }

    fn allow_selection_access(&mut self, _xwm: XwmId, _selection: SelectionTarget) -> bool {
        !self.session_locked()
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        let result = match selection {
            SelectionTarget::Clipboard => {
                request_data_device_client_selection(&self.seat, mime_type, fd)
                    .map_err(|error| error.to_string())
            }
            SelectionTarget::Primary => request_primary_client_selection(&self.seat, mime_type, fd)
                .map_err(|error| error.to_string()),
        };
        if let Err(error) = result {
            eprintln!("nkdhr-canvas: failed to transfer Wayland selection to X11: {error}");
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        let owner = SelectionOwner::XWayland(selection);
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &self.seat, mime_types, owner)
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &self.seat, mime_types, owner)
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        let owned_by_xwayland = match selection {
            SelectionTarget::Clipboard => current_data_device_selection_userdata(&self.seat)
                .is_some_and(|owner| *owner == SelectionOwner::XWayland(selection)),
            SelectionTarget::Primary => current_primary_selection_userdata(&self.seat)
                .is_some_and(|owner| *owner == SelectionOwner::XWayland(selection)),
        };
        if owned_by_xwayland {
            match selection {
                SelectionTarget::Clipboard => {
                    clear_data_device_selection(&self.display_handle, &self.seat)
                }
                SelectionTarget::Primary => {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }
}

delegate_xwayland_shell!(App);

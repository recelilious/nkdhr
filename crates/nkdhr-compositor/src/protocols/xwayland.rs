//! The in-process X11 window manager and the selection bridge.
//!
//! Smithay's `XWayland` spawns the Xwayland server and the compositor itself
//! is the X11 window manager through `X11Wm`/`XwmHandler`. The X11 root
//! coordinate space is the compositor-global logical space, one to one:
//! managed windows are configured with their store location whenever it
//! changes, so the X server's idea of each window's position is always the
//! compositor's and X11 clients position their override-redirect menus
//! correctly from it.

use std::os::fd::OwnedFd;

use smithay::delegate_xwayland_shell;
use smithay::desktop::Window;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle, Serial};
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

use crate::grabs::{self, ResizeEdges};
use crate::input;
use crate::state::{App, SelectionOwner};
use crate::windows::{self, ManagedWindow, WindowId};

impl XWaylandShellHandler for App {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.protocols.xwayland_shell
    }

    /// An X11 window that asked to be mapped before its `wl_surface` was
    /// known is mapped once XWayland associates the two.
    fn surface_associated(&mut self, _xwm: XwmId, _wl_surface: WlSurface, surface: X11Surface) {
        if self.protocols.pending_x11_maps.remove(&surface.window_id()) {
            self.map_x11_window(surface);
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
        self.protocols.x_selection_writer = Some(Box::new(move |xwm, target, mime_type, fd| {
            xwm.send_selection(target, mime_type, fd, loop_handle.clone())?;
            Ok(())
        }));
        println!("nkdhr-compositor: XWayland ready on DISPLAY=:{display_number}");
    }

    /// Puts a mapped X11 window into the store.
    ///
    /// A managed window is placed by the initial-placement rule from its X11
    /// size, configured to that position, inserted on top and focused. An
    /// override-redirect window (a menu or tooltip) is inserted on top at
    /// its own X11 position and never focused. Either way the new window may
    /// lie under the pointer, so pointer focus is recomputed.
    fn map_x11_window(&mut self, surface: X11Surface) {
        if surface.wl_surface().is_none() || self.windows.find_x11(&surface).is_some() {
            return;
        }
        let x11_geometry = surface.geometry();
        if surface.is_override_redirect() {
            self.windows
                .insert_placed(Window::new_x11_window(surface), x11_geometry.loc);
            input::refresh_pointer_focus(self);
            return;
        }

        let window = Window::new_x11_window(surface.clone());
        let location = windows::initial_location(
            window.geometry(),
            self.output_layout(),
            self.pointer_location(),
        );
        let id = self.windows.insert_placed(window, location);
        self.set_x11_geometry(id, Rectangle::new(location, x11_geometry.size));
        println!(
            "nkdhr-compositor: mapped X11 window {} as window {id} at ({}, {})",
            surface.window_id(),
            location.x,
            location.y
        );
        self.focus_window(id);
        input::refresh_pointer_focus(self);
    }

    fn unmap_x11_window(&mut self, surface: &X11Surface) {
        self.protocols.pending_x11_maps.remove(&surface.window_id());
        if let Some(id) = self.windows.find_x11(surface).map(ManagedWindow::id) {
            self.remove_window(id);
        }
    }

    /// The managed window a `_NET_WM_MOVERESIZE` request may act on, with
    /// the serial of the pointer grab it builds on.
    ///
    /// Smithay reports the request whenever an X11 client sends it, so the
    /// pointer must be grabbed here, which in Smithay means a button is
    /// held; `grabs` additionally requires that grab to be the implicit grab
    /// of a press on this window, which also refuses a second request while
    /// a move or resize already runs. The X11 message names no serial, so
    /// the move or resize takes over the serial of that grab.
    fn x11_grab_target(&self, surface: &X11Surface) -> Option<(WindowId, Serial)> {
        let serial = self.seat.get_pointer()?.with_grab(|serial, _| serial)?;
        let id = self.windows.find_x11(surface)?.id();
        Some((id, serial))
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
                "nkdhr-compositor: failed to map X11 window {}: {error}",
                window.window_id()
            );
            return;
        }
        if window.wl_surface().is_some() {
            self.map_x11_window(window);
        } else {
            self.protocols.pending_x11_maps.insert(window.window_id());
        }
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if window.wl_surface().is_some() {
            self.map_x11_window(window);
        } else {
            self.protocols.pending_x11_maps.insert(window.window_id());
        }
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.unmap_x11_window(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.unmap_x11_window(&window);
    }

    /// A window that is not in the store yet gets the geometry it asks for;
    /// placement at map time then decides its position. A managed window in
    /// the store gets the size it asks for while its position stays the
    /// compositor's. Override-redirect windows configure themselves.
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
        if window.is_override_redirect() {
            return;
        }
        let mut geometry = window.geometry();
        if let Some(width) = width {
            geometry.size.w = width.max(1) as i32;
        }
        if let Some(height) = height {
            geometry.size.h = height.max(1) as i32;
        }
        match self
            .windows
            .find_x11(&window)
            .and_then(|managed| Some((managed.id(), managed.location()?)))
        {
            Some((id, location)) => {
                geometry.loc = location;
                self.set_x11_geometry(id, geometry);
            }
            None => {
                if let Some(x) = x {
                    geometry.loc.x = x;
                }
                if let Some(y) = y {
                    geometry.loc.y = y;
                }
                if let Err(error) = window.configure(geometry) {
                    eprintln!(
                        "nkdhr-compositor: failed to configure X11 window {}: {error}",
                        window.window_id()
                    );
                }
            }
        }
    }

    /// Override-redirect windows follow their own X11 position.
    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        if window.is_override_redirect()
            && let Some(id) = self.windows.find_x11(&window).map(ManagedWindow::id)
        {
            self.windows.set_location(id, geometry.loc);
            input::refresh_pointer_focus(self);
        }
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        resize_edge: ResizeEdge,
    ) {
        if let Some((id, serial)) = self.x11_grab_target(&window) {
            grabs::begin_resize(self, id, ResizeEdges::from_x11(resize_edge), serial);
        }
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        if let Some((id, serial)) = self.x11_grab_target(&window) {
            grabs::begin_move(self, id, serial);
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
            eprintln!("nkdhr-compositor: failed to transfer Wayland selection to X11: {error}");
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

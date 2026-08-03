use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmError, DrmEvent, DrmNode, NodeType};
use smithay::backend::egl::{self, EGLDevice, EGLDisplay, context::ContextPriority};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer};
use smithay::backend::renderer::{Color32F, ImportDma, ImportMemWl};
use smithay::backend::session::libseat::{self, LibSeatSession};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu as find_primary_gpu};
use smithay::output::{Mode as WlMode, Output, PhysicalProperties, Scale};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
};
use smithay::reexports::drm::control::{ModeTypeFlags, connector, crtc};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::DeviceFd;
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use crate::backends::{Backend, BackendResult};
use crate::canvas::output_group::{ConnectedOutput, OutputConfig, OutputLayout};
use crate::input;
use crate::render;
use crate::state::{App, ClientState};

const CANVAS_BACKGROUND: Color32F = Color32F::new(0.11, 0.12, 0.16, 1.0);
const SUPPORTED_FORMATS: &[Fourcc] = &[
    Fourcc::Abgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr8888,
    Fourcc::Argb8888,
];

type TtyRenderer<'a> = MultiRenderer<
    'a,
    'a,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
>;
type OutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;
type ManagedDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

pub struct TtyBackend;

impl Backend for TtyBackend {
    fn run(self) -> BackendResult {
        run()
    }
}

struct SurfaceData {
    display_handle: DisplayHandle,
    global: Option<GlobalId>,
    output: Output,
    mode: WlMode,
    render_node: Option<DrmNode>,
    drm_output: ManagedDrmOutput,
}

impl Drop for SurfaceData {
    fn drop(&mut self) {
        if let Some(global) = self.global.take() {
            self.display_handle.remove_global::<App>(global);
        }
    }
}

struct DeviceData {
    surfaces: HashMap<crtc::Handle, SurfaceData>,
    output_manager: OutputManager,
    scanner: DrmScanner,
    render_node: Option<DrmNode>,
    registration: RegistrationToken,
    scanout_enabled: bool,
}

struct TtyState {
    app: App,
    display_handle: DisplayHandle,
    loop_handle: LoopHandle<'static, Self>,
    session: LibSeatSession,
    primary_gpu: DrmNode,
    scanout_filter: Option<DrmNode>,
    gpus: GpuManager<GbmGlesBackend<GlesRenderer, DrmDeviceFd>>,
    devices: HashMap<DrmNode, DeviceData>,
    output_config: OutputConfig,
    output_config_generation: u64,
    output_layout: OutputLayout,
    running: bool,
}

fn run() -> BackendResult {
    let mut event_loop: EventLoop<TtyState> = EventLoop::try_new()?;
    let display: Display<App> = Display::new()?;
    let display_handle = display.handle();
    let (session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();

    let primary_gpu = select_primary_gpu(&session)?;
    let scanout_filter = std::env::var_os("NKDHR_DRM_SCANOUT_DEVICE")
        .map(DrmNode::from_path)
        .transpose()?;
    println!("nkdhr-canvas: using {primary_gpu} as the primary render GPU");
    if let Some(node) = scanout_filter {
        println!("nkdhr-canvas: limiting scanout to {node}");
    }
    let gpus = GpuManager::new(GbmGlesBackend::with_context_priority(ContextPriority::High))?;
    let output_config = OutputConfig::watch();
    let output_config_generation = output_config.generation();
    let app = App::new(&display_handle, DmabufState::new())?;
    let mut state = TtyState {
        app,
        display_handle: display_handle.clone(),
        loop_handle: event_loop.handle(),
        session: session.clone(),
        primary_gpu,
        scanout_filter,
        gpus,
        devices: HashMap::new(),
        output_config,
        output_config_generation,
        output_layout: OutputLayout::default(),
        running: true,
    };

    register_wayland_sources(&event_loop, display, &state.display_handle)?;

    let udev_backend = UdevBackend::new(&seat_name)?;
    let initial_devices = udev_backend
        .device_list()
        .map(|(device_id, path)| (device_id, path.to_owned()))
        .collect::<Vec<_>>();
    initialize_devices(&mut state, &initial_devices)?;
    state.initialize_buffer_globals()?;

    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, state| match event {
            UdevEvent::Added { device_id, path } => match DrmNode::from_dev_id(device_id) {
                Ok(node) => {
                    if state.should_manage_device(node)
                        && let Err(error) = state.device_added(node, &path)
                    {
                        eprintln!("nkdhr-canvas: failed to add DRM device {path:?}: {error}");
                    }
                }
                Err(error) => eprintln!("nkdhr-canvas: invalid DRM device {device_id}: {error}"),
            },
            UdevEvent::Changed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id)
                    && state.should_manage_device(node)
                {
                    state.device_changed(node);
                }
            }
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id)
                    && state.should_manage_device(node)
                {
                    state.device_removed(node);
                }
            }
        })?;

    let mut libinput_context =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput_context
        .udev_assign_seat(&seat_name)
        .map_err(|()| std::io::Error::other(format!("libinput rejected seat {seat_name:?}")))?;
    event_loop.handle().insert_source(
        LibinputInputBackend::new(libinput_context.clone()),
        |event, _, state| {
            if !matches!(
                event,
                InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. }
            ) {
                input::handle(&mut state.app, &state.output_layout, event);
            }
        },
    )?;

    event_loop
        .handle()
        .insert_source(session_notifier, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                libinput_context.suspend();
                for device in state.devices.values_mut() {
                    device.output_manager.pause();
                }
                println!("nkdhr-canvas: TTY session paused");
            }
            SessionEvent::ActivateSession => {
                if let Err(error) = libinput_context.resume() {
                    eprintln!("nkdhr-canvas: failed to resume libinput: {error:?}");
                    state.running = false;
                    return;
                }
                for device in state.devices.values_mut() {
                    if let Err(error) = device.output_manager.activate(false) {
                        eprintln!("nkdhr-canvas: failed to reactivate DRM device: {error}");
                    }
                }
                println!("nkdhr-canvas: TTY session resumed");
            }
        })?;

    println!("nkdhr-canvas: acquired seat {seat_name:?}; starting TTY event loop");
    while state.running {
        let animation_running = state
            .app
            .group_views
            .values()
            .any(|view| view.animation.is_some());
        let timeout = if animation_running {
            Duration::from_millis(16)
        } else {
            // Client commits, input, DRM and udev all wake calloop
            // immediately. Only the atomic config watcher needs a bounded
            // poll, so an idle compositor need not spin at 60 Hz.
            Duration::from_millis(250)
        };
        event_loop.dispatch(Some(timeout), &mut state)?;
        if state.output_config_generation != state.output_config.generation() {
            state.output_config_generation = state.output_config.generation();
            state.reconcile_output_layout();
        }
        render::advance_animations(&mut state.app);
        state.render_all();
        state.display_handle.flush_clients()?;
    }
    Ok(())
}

fn select_primary_gpu(session: &LibSeatSession) -> Result<DrmNode, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("NKDHR_DRM_DEVICE") {
        return Ok(DrmNode::from_path(path)?);
    }
    if let Some(node) = find_primary_gpu(session.seat())?
        .and_then(|path| DrmNode::from_path(path).ok())
        .and_then(|node| {
            node.node_with_type(NodeType::Render)
                .transpose()
                .ok()
                .flatten()
        })
    {
        return Ok(node);
    }
    all_gpus(session.seat())?
        .into_iter()
        .find_map(|path| DrmNode::from_path(path).ok())
        .ok_or_else(|| "no DRM GPU found on the active seat".into())
}

fn register_wayland_sources(
    event_loop: &EventLoop<TtyState>,
    display: Display<App>,
    display_handle: &DisplayHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = ListeningSocketSource::new_auto()?;
    println!(
        "nkdhr-canvas: listening on WAYLAND_DISPLAY={}",
        socket.socket_name().to_string_lossy()
    );
    event_loop
        .handle()
        .insert_source(socket, |stream, _, state| {
            if let Err(error) = state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                eprintln!("nkdhr-canvas: failed to accept Wayland client: {error}");
            }
        })?;
    event_loop.handle().insert_source(
        Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            // SAFETY: the display remains owned by this event source for the
            // entire loop lifetime and is never accessed through another path.
            unsafe {
                display
                    .get_mut()
                    .dispatch_clients(&mut state.app)
                    .map_err(std::io::Error::other)?;
            }
            Ok(PostAction::Continue)
        },
    )?;
    let _ = display_handle;
    Ok(())
}

fn initialize_devices(
    state: &mut TtyState,
    devices: &[(u64, std::path::PathBuf)],
) -> Result<(), Box<dyn std::error::Error>> {
    let primary_node = state
        .primary_gpu
        .node_with_type(NodeType::Primary)
        .transpose()
        .ok()
        .flatten();
    if let Some((device_id, path)) = devices.iter().find(|(device_id, _)| {
        primary_node.is_some_and(|node| *device_id == node.dev_id())
            || *device_id == state.primary_gpu.dev_id()
    }) {
        state.device_added(DrmNode::from_dev_id(*device_id)?, path)?;
    }
    for (device_id, path) in devices {
        let node = DrmNode::from_dev_id(*device_id)?;
        if state.should_manage_device(node) && !state.devices.contains_key(&node) {
            state.device_added(node, path)?;
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum DeviceAddError {
    #[error("libseat could not open the device: {0}")]
    DeviceOpen(libseat::Error),
    #[error("failed to initialize the DRM device: {0}")]
    Drm(DrmError),
    #[error("failed to initialize GBM: {0}")]
    Gbm(std::io::Error),
    #[error("failed to initialize EGL: {0}")]
    Egl(egl::Error),
    #[error("failed to acquire a renderer: {0}")]
    Renderer(String),
    #[error("the scanout-only device appeared before the primary render GPU")]
    MissingPrimaryGpu,
}

impl TtyState {
    fn should_manage_device(&self, node: DrmNode) -> bool {
        self.scanout_filter.is_none_or(|filter| node == filter)
            || node
                .node_with_type(NodeType::Render)
                .transpose()
                .ok()
                .flatten()
                == Some(self.primary_gpu)
    }

    fn initialize_buffer_globals(&mut self) -> BackendResult {
        let renderer = self.gpus.single_renderer(&self.primary_gpu)?;
        self.app.shm_state.update_formats(renderer.shm_formats());
        self.app
            .dmabuf_state
            .create_global::<App>(&self.display_handle, renderer.dmabuf_formats());
        Ok(())
    }

    fn device_added(&mut self, node: DrmNode, path: &Path) -> Result<(), DeviceAddError> {
        if self.devices.contains_key(&node) {
            return Ok(());
        }
        let fd = self
            .session
            .open(
                path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(DeviceAddError::DeviceOpen)?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, notifier) = DrmDevice::new(fd.clone(), true).map_err(DeviceAddError::Drm)?;
        let gbm = GbmDevice::new(fd).map_err(DeviceAddError::Gbm)?;

        let registration = self
            .loop_handle
            .insert_source(notifier, move |event, _, state| match event {
                DrmEvent::VBlank(crtc) => state.frame_finish(node, crtc),
                DrmEvent::Error(error) => {
                    eprintln!("nkdhr-canvas: DRM event error on {node}: {error}");
                }
            })
            .expect("DRM notifier registration must succeed");

        let render_node = (|| {
            // SAFETY: GBM owns a valid DRM fd for the lifetime of this EGL display.
            let display = unsafe { EGLDisplay::new(gbm.clone()) }.map_err(DeviceAddError::Egl)?;
            let egl_device =
                EGLDevice::device_for_display(&display).map_err(DeviceAddError::Egl)?;
            if egl_device.is_software() {
                return Ok(None);
            }
            let render_node = egl_device
                .try_get_render_node()
                .ok()
                .flatten()
                .unwrap_or(node);
            self.gpus
                .as_mut()
                .add_node(render_node, gbm.clone())
                .map_err(DeviceAddError::Egl)?;
            Ok::<_, DeviceAddError>(Some(render_node))
        })()?;

        let allocator = render_node
            .map(|_| {
                GbmAllocator::new(
                    gbm.clone(),
                    GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                )
            })
            .or_else(|| {
                self.devices
                    .values()
                    .find(|device| device.render_node == Some(self.primary_gpu))
                    .map(|device| device.output_manager.allocator().clone())
            })
            .ok_or(DeviceAddError::MissingPrimaryGpu)?;
        let exporter = GbmFramebufferExporter::new(gbm.clone(), render_node);
        let target_node = render_node.unwrap_or(self.primary_gpu);
        let mut renderer = self
            .gpus
            .single_renderer(&target_node)
            .map_err(|error| DeviceAddError::Renderer(error.to_string()))?;
        let render_formats = renderer
            .as_mut()
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .filter(|format| render_node.is_some() || format.modifier == Modifier::Linear)
            .copied()
            .collect::<FormatSet>();
        let output_manager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            SUPPORTED_FORMATS.iter().copied(),
            render_formats,
        );
        self.devices.insert(
            node,
            DeviceData {
                surfaces: HashMap::new(),
                output_manager,
                scanner: DrmScanner::new(),
                render_node,
                registration,
                scanout_enabled: self.scanout_filter.is_none_or(|filter| filter == node),
            },
        );
        if self.devices[&node].scanout_enabled {
            self.device_changed(node);
        }
        Ok(())
    }

    fn device_changed(&mut self, node: DrmNode) {
        let scan = {
            let Some(device) = self.devices.get_mut(&node) else {
                return;
            };
            if !device.scanout_enabled {
                return;
            }
            match device
                .scanner
                .scan_connectors(device.output_manager.device())
            {
                Ok(scan) => scan.into_iter().collect::<Vec<_>>(),
                Err(error) => {
                    eprintln!("nkdhr-canvas: failed to scan connectors on {node}: {error}");
                    return;
                }
            }
        };
        for event in scan {
            match event {
                DrmScanEvent::Connected {
                    connector,
                    crtc: Some(crtc),
                } => self.connector_connected(node, connector, crtc),
                DrmScanEvent::Disconnected {
                    connector: _,
                    crtc: Some(crtc),
                } => self.connector_disconnected(node, crtc),
                _ => {}
            }
        }
        self.reconcile_output_layout();
    }

    fn connector_connected(
        &mut self,
        node: DrmNode,
        connector: connector::Info,
        crtc: crtc::Handle,
    ) {
        let Some(device) = self.devices.get_mut(&node) else {
            return;
        };
        let Some(drm_mode) = connector
            .modes()
            .iter()
            .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| connector.modes().first())
            .copied()
        else {
            eprintln!(
                "nkdhr-canvas: connector {:?} has no usable mode",
                connector.handle()
            );
            return;
        };
        let mode = WlMode::from(drm_mode);
        let output_name = format!(
            "{}-{}",
            connector.interface().as_str(),
            connector.interface_id()
        );
        let (physical_width, physical_height) = connector.size().unwrap_or((0, 0));
        let output = Output::new(
            output_name.clone(),
            PhysicalProperties {
                size: (physical_width as i32, physical_height as i32).into(),
                subpixel: connector.subpixel().into(),
                make: "Unknown".to_owned(),
                model: "Unknown".to_owned(),
            },
        );
        output.set_preferred(mode);
        output.change_current_state(Some(mode), None, None, None);
        let global = output.create_global::<App>(&self.display_handle);

        let render_node = device.render_node.unwrap_or(self.primary_gpu);
        let mut renderer = match self.gpus.single_renderer(&render_node) {
            Ok(renderer) => renderer,
            Err(error) => {
                eprintln!("nkdhr-canvas: no renderer for {output_name}: {error}");
                return;
            }
        };
        let planes = match device.output_manager.device().planes(&crtc) {
            Ok(planes) => planes,
            Err(error) => {
                eprintln!("nkdhr-canvas: failed to query planes for {output_name}: {error}");
                return;
            }
        };
        let drm_output = match device
            .output_manager
            .initialize_output::<_, WaylandSurfaceRenderElement<TtyRenderer<'_>>>(
                crtc,
                drm_mode,
                &[connector.handle()],
                &output,
                Some(planes),
                &mut renderer,
                &DrmOutputRenderElements::default(),
            ) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("nkdhr-canvas: failed to initialize {output_name}: {error}");
                return;
            }
        };
        device.surfaces.insert(
            crtc,
            SurfaceData {
                display_handle: self.display_handle.clone(),
                global: Some(global),
                output,
                mode,
                render_node: device.render_node,
                drm_output,
            },
        );
        println!("nkdhr-canvas: connected output {output_name} at {mode:?}");
    }

    fn connector_disconnected(&mut self, node: DrmNode, crtc: crtc::Handle) {
        if let Some(device) = self.devices.get_mut(&node)
            && let Some(surface) = device.surfaces.remove(&crtc)
        {
            println!(
                "nkdhr-canvas: disconnected output {}",
                surface.output.name()
            );
        }
    }

    fn device_removed(&mut self, node: DrmNode) {
        let Some(device) = self.devices.remove(&node) else {
            return;
        };
        if let Some(render_node) = device.render_node {
            self.gpus.as_mut().remove_node(&render_node);
        }
        self.loop_handle.remove(device.registration);
        self.reconcile_output_layout();
        println!("nkdhr-canvas: removed DRM device {node}");
    }

    fn connected_outputs(&self) -> Vec<ConnectedOutput> {
        self.devices
            .values()
            .flat_map(|device| device.surfaces.values())
            .map(|surface| ConnectedOutput {
                name: surface.output.name(),
                physical_size: surface.mode.size,
            })
            .collect()
    }

    fn reconcile_output_layout(&mut self) {
        let connected = self.connected_outputs();
        let output_layout = OutputLayout::resolve(&self.output_config.snapshot(), &connected);
        if output_layout == self.output_layout {
            return;
        }
        self.output_layout = output_layout;
        for device in self.devices.values_mut() {
            for surface in device.surfaces.values_mut() {
                let Some(resolved) = self.output_layout.output(&surface.output.name()) else {
                    continue;
                };
                surface.output.change_current_state(
                    Some(surface.mode),
                    None,
                    Some(Scale::Fractional(resolved.scale)),
                    Some(resolved.global_location),
                );
            }
        }
        self.app.reconcile_output_layout(&self.output_layout);
        for group in &self.output_layout.groups {
            let outputs = group
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "nkdhr-canvas: output group {:?} -> canvas {:?} [{}]",
                group.name, group.canvas, outputs
            );
        }
    }

    fn render_all(&mut self) {
        let outputs = self
            .devices
            .iter()
            .flat_map(|(node, device)| device.surfaces.keys().map(|crtc| (*node, *crtc)))
            .collect::<Vec<_>>();
        for (node, crtc) in outputs {
            self.render_surface(node, crtc);
        }
    }

    fn render_surface(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let Some(device) = self.devices.get_mut(&node) else {
            return;
        };
        let Some(surface) = device.surfaces.get_mut(&crtc) else {
            return;
        };
        let output_name = surface.output.name();
        let Some(resolved_output) = self.output_layout.output(&output_name) else {
            return;
        };
        let Some(group) = self.output_layout.group_for_output(&output_name) else {
            return;
        };
        let render_node = surface.render_node.unwrap_or(self.primary_gpu);
        let renderer = if render_node == self.primary_gpu {
            self.gpus.single_renderer(&render_node)
        } else {
            self.gpus
                .renderer(&self.primary_gpu, &render_node, surface.drm_output.format())
        };
        let Ok(mut renderer) = renderer else {
            return;
        };

        let Some(view) = self.app.group_views.get(&group.name) else {
            return;
        };
        let Some(canvas) = self.app.canvases.get(&view.canvas) else {
            return;
        };
        let elements = canvas
            .windows()
            .iter()
            .flat_map(|window| {
                let group_point = view
                    .viewport
                    .to_group_logical(window.position, group.logical_size);
                let local = group_point - resolved_output.group_location.to_f64();
                let offset = local.to_physical(resolved_output.scale).to_i32_round();
                render_elements_from_surface_tree(
                    &mut renderer,
                    window.surface.wl_surface(),
                    offset,
                    view.viewport.zoom * resolved_output.scale,
                    1.0,
                    Kind::Unspecified,
                )
            })
            .collect::<Vec<WaylandSurfaceRenderElement<TtyRenderer<'_>>>>();

        match surface.drm_output.render_frame(
            &mut renderer,
            &elements,
            CANVAS_BACKGROUND,
            FrameFlags::empty(),
        ) {
            Ok(frame) => {
                if !frame.is_empty
                    && let Err(error) = surface.drm_output.queue_frame(())
                {
                    eprintln!("nkdhr-canvas: failed to queue {output_name}: {error}");
                    return;
                }
                let frame_time = self.app.start_time.elapsed().as_millis() as u32;
                for window in canvas.windows() {
                    render::send_frame_callbacks(window.surface.wl_surface(), frame_time);
                }
            }
            Err(error) => {
                eprintln!("nkdhr-canvas: failed to render {output_name}: {error}");
            }
        }
    }

    fn frame_finish(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let Some(surface) = self
            .devices
            .get_mut(&node)
            .and_then(|device| device.surfaces.get_mut(&crtc))
        else {
            return;
        };
        if let Err(error) = surface.drm_output.frame_submitted() {
            eprintln!(
                "nkdhr-canvas: failed to finish frame on {}: {error}",
                surface.output.name()
            );
        }
    }
}

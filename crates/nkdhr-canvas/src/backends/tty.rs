use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use smithay::backend::allocator::dmabuf::AsDmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::drm::compositor::{FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmError, DrmEvent, DrmNode, NodeType};
use smithay::backend::egl::{self, context::ContextPriority};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer};
use smithay::backend::renderer::{Color32F, ExportMem, ImportDma, ImportMemWl, TextureMapping};
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
use smithay::utils::{DeviceFd, Logical, Rectangle};
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, X11Wm, XwmHandler, XwmId};
use smithay::xwayland::{X11Surface, XWayland, XWaylandEvent};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use crate::backends::{Backend, BackendResult};
use crate::canvas::output_group::{ConnectedOutput, OutputConfig, OutputLayout};
use crate::input;
use crate::protocols::SCREENCOPY_FORMAT;
use crate::render;
use crate::state::{App, ClientState};
use crate::widget_host::PinnedLayer;

const CANVAS_BACKGROUND: Color32F = Color32F::new(0.11, 0.12, 0.16, 1.0);
const LOCK_BACKGROUND: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);
const SUPPORTED_FORMATS: &[Fourcc] = &[
    Fourcc::Abgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr8888,
    Fourcc::Argb8888,
];

type TtyRenderer<'a> = MultiRenderer<
    'a,
    'a,
    GbmGlesBackend<GlesRenderer, RenderDeviceFd>,
    GbmGlesBackend<GlesRenderer, RenderDeviceFd>,
>;
type OutputManager = DrmOutputManager<
    GbmAllocator<RenderDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;
type ManagedDrmOutput =
    DrmOutput<GbmAllocator<RenderDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// A DRM render-node descriptor that deliberately has no DRM-control traits.
///
/// Keeping renderer descriptors distinct from `DrmDeviceFd` is a safety
/// invariant: constructing `DrmDeviceFd` attempts to acquire DRM master,
/// while a renderer never needs modesetting privileges.
#[derive(Clone, Debug)]
struct RenderDeviceFd(DeviceFd);

impl AsFd for RenderDeviceFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

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
    frame_pending: bool,
    protected_frame_queued: bool,
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
    gpus: GpuManager<GbmGlesBackend<GlesRenderer, RenderDeviceFd>>,
    render_allocators: HashMap<DrmNode, GbmAllocator<RenderDeviceFd>>,
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

    let scanout_filter = std::env::var_os("NKDHR_DRM_SCANOUT_DEVICE")
        .map(DrmNode::from_path)
        .transpose()?;
    if let Some(node) = scanout_filter
        && node.ty() != NodeType::Primary
    {
        return Err(
            format!("NKDHR_DRM_SCANOUT_DEVICE must name a DRM primary node, not {node}").into(),
        );
    }
    let primary_gpu = select_primary_gpu(&session)?;
    println!("nkdhr-canvas: using {primary_gpu} as the primary render GPU");
    if let Some(node) = scanout_filter {
        println!("nkdhr-canvas: limiting scanout to {node}");
    }
    let mut gpus = GpuManager::new(GbmGlesBackend::with_context_priority(ContextPriority::High))?;
    let primary_allocator = register_render_node(&mut gpus, primary_gpu)?;
    let render_allocators = HashMap::from([(primary_gpu, primary_allocator)]);
    let output_config = OutputConfig::watch();
    let output_config_generation = output_config.generation();
    let mut app = App::new(&display_handle, DmabufState::new())?;
    app.enable_vt_switching();
    let mut state = TtyState {
        app,
        display_handle: display_handle.clone(),
        loop_handle: event_loop.handle(),
        session: session.clone(),
        primary_gpu,
        scanout_filter,
        gpus,
        render_allocators,
        devices: HashMap::new(),
        output_config,
        output_config_generation,
        output_layout: OutputLayout::default(),
        running: true,
    };

    state.start_xwayland()?;

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
                if let Some(vt) = state.app.take_vt_switch_request()
                    && let Err(error) = state.session.change_vt(vt)
                {
                    eprintln!("nkdhr-canvas: failed to switch to VT {vt}: {error:?}");
                }
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
                    for surface in device.surfaces.values_mut() {
                        surface.frame_pending = false;
                        surface.protected_frame_queued = false;
                    }
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
                    let activated = match device.output_manager.activate(false) {
                        Ok(()) => true,
                        Err(error) => {
                            eprintln!("nkdhr-canvas: failed to reactivate DRM device: {error}");
                            false
                        }
                    };
                    for surface in device.surfaces.values_mut() {
                        surface.frame_pending = false;
                        surface.protected_frame_queued = false;
                        if activated {
                            surface.drm_output.reset_buffers();
                        }
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
            .any(|view| view.animation.is_some())
            || state
                .app
                .canvases
                .values()
                .any(crate::canvas::world::Canvas::animations_running);
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
        let node = DrmNode::from_path(path)?;
        if node.ty() != NodeType::Render {
            return Err(format!("NKDHR_DRM_DEVICE must name a DRM render node, not {node}").into());
        }
        return Ok(node);
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
        .filter_map(|path| DrmNode::from_path(path).ok())
        .find_map(|node| {
            node.node_with_type(NodeType::Render)
                .transpose()
                .ok()
                .flatten()
        })
        .ok_or_else(|| "no DRM render node found on the active seat".into())
}

#[derive(Debug, thiserror::Error)]
enum RenderNodeError {
    #[error("{0} is not a DRM render node")]
    NotRenderNode(DrmNode),
    #[error("no device path exists for DRM render node {0}")]
    MissingPath(DrmNode),
    #[error("failed to open DRM render node {path:?}: {source}")]
    Open {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to initialize GBM on the render node: {0}")]
    Gbm(std::io::Error),
    #[error("failed to initialize EGL on the render node: {0}")]
    Egl(egl::Error),
}

fn register_render_node(
    gpus: &mut GpuManager<GbmGlesBackend<GlesRenderer, RenderDeviceFd>>,
    node: DrmNode,
) -> Result<GbmAllocator<RenderDeviceFd>, RenderNodeError> {
    if node.ty() != NodeType::Render {
        return Err(RenderNodeError::NotRenderNode(node));
    }
    let path = node.dev_path().ok_or(RenderNodeError::MissingPath(node))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| RenderNodeError::Open {
            path: path.clone(),
            source,
        })?;
    let fd: OwnedFd = file.into();
    let gbm = GbmDevice::new(RenderDeviceFd(DeviceFd::from(fd))).map_err(RenderNodeError::Gbm)?;
    gpus.as_mut()
        .add_node(node, gbm.clone())
        .map_err(RenderNodeError::Egl)?;
    Ok(GbmAllocator::new(
        gbm,
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    ))
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
    #[error(transparent)]
    RenderNode(#[from] RenderNodeError),
    #[error("failed to acquire a renderer: {0}")]
    Renderer(String),
    #[error("no render allocator is registered for {0}")]
    MissingRenderAllocator(DrmNode),
}

impl TtyState {
    fn start_xwayland(&mut self) -> BackendResult {
        let (xwayland, client) = match XWayland::spawn(
            &self.display_handle,
            None,
            crate::protocols::xwayland_environment(),
            true,
            Stdio::null(),
            Stdio::inherit(),
            |_| {},
        ) {
            Ok(instance) => instance,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("nkdhr-canvas: Xwayland is not installed; X11 compatibility disabled");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        let wm_handle = self.loop_handle.clone();
        self.loop_handle
            .insert_source(xwayland, move |event, _, state| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => match X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()) {
                    Ok(xwm) => state
                        .app
                        .install_xwm(xwm, display_number, wm_handle.clone()),
                    Err(error) => eprintln!("nkdhr-canvas: failed to start XWM: {error}"),
                },
                XWaylandEvent::Error => {
                    eprintln!("nkdhr-canvas: XWayland exited during startup")
                }
            })?;
        Ok(())
    }

    fn should_manage_device(&self, node: DrmNode) -> bool {
        self.scanout_filter.is_none_or(|filter| node == filter)
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

        let render_node = node
            .node_with_type(NodeType::Render)
            .transpose()
            .map_err(|error| DeviceAddError::Renderer(error.to_string()))?;
        let allocator_node = render_node.unwrap_or(self.primary_gpu);
        if !self.render_allocators.contains_key(&allocator_node) {
            let allocator = register_render_node(&mut self.gpus, allocator_node)?;
            self.render_allocators.insert(allocator_node, allocator);
        }
        let allocator = self
            .render_allocators
            .get(&allocator_node)
            .cloned()
            .ok_or(DeviceAddError::MissingRenderAllocator(allocator_node))?;
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
            .initialize_output::<_, render::CanvasRenderElement<TtyRenderer<'_>>>(
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
                frame_pending: false,
                protected_frame_queued: false,
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
        if let Some(render_node) = device.render_node
            && render_node != self.primary_gpu
        {
            self.gpus.as_mut().remove_node(&render_node);
            self.render_allocators.remove(&render_node);
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
        if surface.frame_pending {
            return;
        }
        let output_name = surface.output.name();
        let screencopies = self.app.take_pending_screencopies(&output_name);
        let include_cursor = screencopies
            .first()
            .is_none_or(|request| request.overlay_cursor());
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
        for window in canvas.windows() {
            let window_rect = render::window_group_rect(window, view.viewport, group.canvas_anchor);
            let output_rect =
                Rectangle::new(resolved_output.group_location, resolved_output.logical_size);
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
            render::update_window_output(window, &surface.output, overlap, preferred_scale);
        }
        let locked = self.app.session_locked();
        let lock_surface = self.app.lock_surface_for_output(&output_name);
        let mut elements = if include_cursor {
            render::cursor_render_elements(
                &mut renderer,
                &self.app,
                resolved_output.global_location,
                resolved_output.scale,
            )
        } else {
            Vec::new()
        };
        if !locked {
            elements.extend(render::dnd_icon_render_elements(
                &mut renderer,
                &self.app,
                resolved_output.global_location,
                resolved_output.scale,
            ));
        }
        if locked {
            elements.extend(lock_surface.iter().flat_map(
                |lock_surface| -> Vec<render::CanvasRenderElement<TtyRenderer<'_>>> {
                    render_elements_from_surface_tree(
                        &mut renderer,
                        lock_surface,
                        (0, 0),
                        resolved_output.scale,
                        1.0,
                        Kind::Unspecified,
                    )
                },
            ));
        } else {
            elements.extend(render::pinned_render_elements(
                &mut renderer,
                canvas,
                PinnedLayer::AboveWindows,
                view.viewport,
                group.canvas_anchor,
                resolved_output.group_location,
                resolved_output.scale,
            ));
            elements.extend(canvas.windows().iter().rev().flat_map(|window| {
                render::window_render_elements(
                    &mut renderer,
                    window,
                    view.viewport,
                    group.canvas_anchor,
                    resolved_output.group_location,
                    resolved_output.scale,
                )
            }));
            elements.extend(render::pinned_render_elements(
                &mut renderer,
                canvas,
                PinnedLayer::BehindWindows,
                view.viewport,
                group.canvas_anchor,
                resolved_output.group_location,
                resolved_output.scale,
            ));
        }

        let frame_flags = if screencopies.is_empty() {
            FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT
        } else {
            // Screencopy reads the composed primary framebuffer. Keep a
            // requested cursor in that composition for this frame rather
            // than assigning it to a hardware plane the readback cannot see.
            FrameFlags::empty()
        };
        match surface.drm_output.render_frame(
            &mut renderer,
            &elements,
            if locked {
                LOCK_BACKGROUND
            } else {
                CANVAS_BACKGROUND
            },
            frame_flags,
        ) {
            Ok(frame) => {
                if !screencopies.is_empty() {
                    match &frame.primary_element {
                        PrimaryPlaneElement::Swapchain(primary) => {
                            match primary.buffer().export() {
                                Ok(dmabuf) => match renderer.import_dmabuf(&dmabuf, None) {
                                    Ok(texture) => {
                                        for request in screencopies {
                                            match renderer.copy_texture(
                                                &texture,
                                                request.region,
                                                SCREENCOPY_FORMAT,
                                            ) {
                                                Ok(mapping) => {
                                                    let flipped = mapping.flipped();
                                                    match renderer.map_texture(&mapping) {
                                                        Ok(pixels) => {
                                                            if let Err(error) = request.complete(
                                                                pixels,
                                                                flipped,
                                                                self.app.start_time.elapsed(),
                                                            ) {
                                                                eprintln!(
                                                                    "nkdhr-canvas: {output_name} screencopy failed: {error}"
                                                                );
                                                            }
                                                        }
                                                        Err(error) => {
                                                            let message = error.to_string();
                                                            let _ = request.fail(message.clone());
                                                            eprintln!(
                                                                "nkdhr-canvas: {output_name} screencopy mapping failed: {message}"
                                                            );
                                                        }
                                                    }
                                                }
                                                Err(error) => {
                                                    let message = error.to_string();
                                                    let _ = request.fail(message.clone());
                                                    eprintln!(
                                                        "nkdhr-canvas: {output_name} screencopy readback failed: {message}"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        let message = error.to_string();
                                        for request in screencopies {
                                            let _ = request.fail(message.clone());
                                        }
                                        eprintln!(
                                            "nkdhr-canvas: could not import {output_name} for screencopy: {message}"
                                        );
                                    }
                                },
                                Err(error) => {
                                    let message = error.to_string();
                                    for request in screencopies {
                                        let _ = request.fail(message.clone());
                                    }
                                    eprintln!(
                                        "nkdhr-canvas: could not export {output_name} for screencopy: {message}"
                                    );
                                }
                            }
                        }
                        PrimaryPlaneElement::Element(_) => {
                            for request in screencopies {
                                let _ = request.fail(
                                    "direct scanout is unavailable during screencopy".to_owned(),
                                );
                            }
                        }
                    }
                }
                if !frame.is_empty {
                    if let Err(error) = surface.drm_output.queue_frame(()) {
                        eprintln!("nkdhr-canvas: failed to queue {output_name}: {error}");
                        return;
                    }
                    surface.frame_pending = true;
                    surface.protected_frame_queued = locked;
                }
                let frame_time = self.app.start_time.elapsed().as_millis() as u32;
                if let Some(lock_surface) = lock_surface {
                    render::send_frame_callbacks(&lock_surface, frame_time);
                } else if !locked {
                    for window in canvas.windows() {
                        if let Some(window_surface) = window.wl_surface() {
                            render::send_frame_callbacks(&window_surface, frame_time);
                        }
                    }
                }
                render::send_pointer_frame_callbacks(&self.app, frame_time);
            }
            Err(error) => {
                eprintln!("nkdhr-canvas: failed to render {output_name}: {error}");
            }
        }
    }

    fn frame_finish(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let presented = {
            let Some(surface) = self
                .devices
                .get_mut(&node)
                .and_then(|device| device.surfaces.get_mut(&crtc))
            else {
                return;
            };
            let output_name = surface.output.name();
            surface.frame_pending = false;
            if let Err(error) = surface.drm_output.frame_submitted() {
                eprintln!("nkdhr-canvas: failed to finish frame on {output_name}: {error}");
                None
            } else if std::mem::take(&mut surface.protected_frame_queued) {
                Some(output_name)
            } else {
                None
            }
        };
        if let Some(output_name) = presented {
            self.app.note_protected_frame(&output_name);
        }
    }
}

impl XWaylandShellHandler for TtyState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        XWaylandShellHandler::xwayland_shell_state(&mut self.app)
    }

    fn surface_associated(
        &mut self,
        xwm: XwmId,
        wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        surface: X11Surface,
    ) {
        XWaylandShellHandler::surface_associated(&mut self.app, xwm, wl_surface, surface);
    }
}

impl XwmHandler for TtyState {
    fn xwm_state(&mut self, xwm: XwmId) -> &mut X11Wm {
        XwmHandler::xwm_state(&mut self.app, xwm)
    }

    fn new_window(&mut self, xwm: XwmId, window: X11Surface) {
        XwmHandler::new_window(&mut self.app, xwm, window);
    }

    fn new_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        XwmHandler::new_override_redirect_window(&mut self.app, xwm, window);
    }

    fn map_window_request(&mut self, xwm: XwmId, window: X11Surface) {
        XwmHandler::map_window_request(&mut self.app, xwm, window);
    }

    fn mapped_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        XwmHandler::mapped_override_redirect_window(&mut self.app, xwm, window);
    }

    fn unmapped_window(&mut self, xwm: XwmId, window: X11Surface) {
        XwmHandler::unmapped_window(&mut self.app, xwm, window);
    }

    fn destroyed_window(&mut self, xwm: XwmId, window: X11Surface) {
        XwmHandler::destroyed_window(&mut self.app, xwm, window);
    }

    fn configure_request(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        reorder: Option<Reorder>,
    ) {
        XwmHandler::configure_request(&mut self.app, xwm, window, x, y, width, height, reorder);
    }

    fn configure_notify(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        above: Option<u32>,
    ) {
        XwmHandler::configure_notify(&mut self.app, xwm, window, geometry, above);
    }

    fn resize_request(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        button: u32,
        resize_edge: ResizeEdge,
    ) {
        XwmHandler::resize_request(&mut self.app, xwm, window, button, resize_edge);
    }

    fn move_request(&mut self, xwm: XwmId, window: X11Surface, button: u32) {
        XwmHandler::move_request(&mut self.app, xwm, window, button);
    }

    fn allow_selection_access(&mut self, xwm: XwmId, selection: SelectionTarget) -> bool {
        XwmHandler::allow_selection_access(&mut self.app, xwm, selection)
    }

    fn send_selection(
        &mut self,
        xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        XwmHandler::send_selection(&mut self.app, xwm, selection, mime_type, fd);
    }

    fn new_selection(&mut self, xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        XwmHandler::new_selection(&mut self.app, xwm, selection, mime_types);
    }

    fn cleared_selection(&mut self, xwm: XwmId, selection: SelectionTarget) {
        XwmHandler::cleared_selection(&mut self.app, xwm, selection);
    }
}

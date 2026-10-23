use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::{
    wl_buffer::WlBuffer, wl_output::WlOutput, wl_shm,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Buffer, Logical, Physical, Rectangle};
use smithay::wayland::shm::with_buffer_contents_mut;

use crate::state::App;

const VERSION: u32 = 3;
const BYTES_PER_PIXEL: usize = 4;

pub struct ScreencopyState {
    _global: GlobalId,
    pending: Vec<PendingScreencopy>,
}

impl ScreencopyState {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<App, ZwlrScreencopyManagerV1, _>(VERSION, ()),
            pending: Vec::new(),
        }
    }

    pub fn take_pending(&mut self, output_name: &str) -> Vec<PendingScreencopy> {
        let overlay_cursor = self
            .pending
            .iter()
            .find(|request| request.output_name == output_name)
            .map(|request| request.overlay_cursor);
        let mut selected = Vec::new();
        self.pending.retain(|request| {
            if request.output_name == output_name && Some(request.overlay_cursor) == overlay_cursor
            {
                selected.push(request.clone());
                false
            } else {
                true
            }
        });
        selected
    }

    pub fn reconcile_outputs(&mut self, connected_outputs: &BTreeSet<String>) {
        let pending = std::mem::take(&mut self.pending);
        self.pending = pending
            .into_iter()
            .filter_map(|request| {
                if connected_outputs.contains(&request.output_name) {
                    Some(request)
                } else {
                    let _ = request.fail("screencopy output was disconnected".to_owned());
                    None
                }
            })
            .collect();
    }
}

#[derive(Clone)]
pub struct PendingScreencopy {
    output_name: String,
    pub region: Rectangle<i32, Buffer>,
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    with_damage: bool,
    overlay_cursor: bool,
}

impl PendingScreencopy {
    pub fn overlay_cursor(&self) -> bool {
        self.overlay_cursor
    }

    pub fn complete(
        self,
        pixels: &[u8],
        flipped: bool,
        presentation_time: Duration,
    ) -> Result<(), String> {
        let width = self.region.size.w;
        let height = self.region.size.h;
        let source_stride = usize::try_from(width)
            .map_err(|_| "negative screencopy width".to_owned())?
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or_else(|| "screencopy source stride overflow".to_owned())?;
        let required = source_stride
            .checked_mul(usize::try_from(height).map_err(|_| "negative screencopy height")?)
            .ok_or_else(|| "screencopy source size overflow".to_owned())?;
        if pixels.len() < required {
            return self.fail(format!(
                "renderer returned {} bytes, expected at least {required}",
                pixels.len()
            ));
        }

        let copy_result = with_buffer_contents_mut(&self.buffer, |ptr, pool_len, data| {
            if data.format != wl_shm::Format::Xrgb8888
                || data.width != width
                || data.height != height
                || data.stride < width.saturating_mul(BYTES_PER_PIXEL as i32)
                || data.offset < 0
            {
                return Err("client supplied an incompatible SHM buffer".to_owned());
            }
            let offset = usize::try_from(data.offset).map_err(|_| "negative SHM offset")?;
            let destination_stride =
                usize::try_from(data.stride).map_err(|_| "negative SHM stride")?;
            let destination_len = destination_stride
                .checked_mul(usize::try_from(height).map_err(|_| "negative SHM height")?)
                .and_then(|size| offset.checked_add(size))
                .ok_or_else(|| "SHM buffer bounds overflow".to_owned())?;
            if destination_len > pool_len {
                return Err("SHM buffer extends beyond its pool".to_owned());
            }

            for row in 0..usize::try_from(height).map_err(|_| "negative height")? {
                let source = &pixels[row * source_stride..(row + 1) * source_stride];
                // SAFETY: bounds and stride were checked against the pool length
                // above; Smithay guarantees this pointer is valid for the closure.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        source.as_ptr(),
                        ptr.add(offset + row * destination_stride),
                        source_stride,
                    );
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;

        if let Err(error) = copy_result {
            return self.fail(error);
        }
        if self.with_damage {
            self.frame.damage(0, 0, width as u32, height as u32);
        }
        let flags = if flipped {
            zwlr_screencopy_frame_v1::Flags::YInvert
        } else {
            zwlr_screencopy_frame_v1::Flags::empty()
        };
        self.frame.flags(flags);
        let seconds = presentation_time.as_secs();
        self.frame.ready(
            (seconds >> 32) as u32,
            seconds as u32,
            presentation_time.subsec_nanos(),
        );
        self.buffer.release();
        Ok(())
    }

    pub fn fail(self, error: String) -> Result<(), String> {
        self.frame.failed();
        self.buffer.release();
        Err(error)
    }
}

struct FrameData {
    output_name: String,
    region: Rectangle<i32, Buffer>,
    overlay_cursor: bool,
    used: Mutex<bool>,
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, (), App> for App {
    fn bind(
        _state: &mut App,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, App>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, (), App> for App {
    fn request(
        _state: &mut App,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, App>,
    ) {
        match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => initialize_frame(frame, output, None, overlay_cursor != 0, data_init),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => initialize_frame(
                frame,
                output,
                Some(Rectangle::<i32, Logical>::new(
                    (x, y).into(),
                    (width, height).into(),
                )),
                overlay_cursor != 0,
                data_init,
            ),
            zwlr_screencopy_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

fn initialize_frame(
    frame: New<ZwlrScreencopyFrameV1>,
    output_resource: WlOutput,
    logical_region: Option<Rectangle<i32, Logical>>,
    overlay_cursor: bool,
    data_init: &mut DataInit<'_, App>,
) {
    let Some(output) = Output::from_resource(&output_resource) else {
        let frame = data_init.init(
            frame,
            Arc::new(FrameData {
                output_name: String::new(),
                region: Rectangle::from_size((0, 0).into()),
                overlay_cursor,
                used: Mutex::new(true),
            }),
        );
        frame.failed();
        return;
    };
    let Some(mode) = output.current_mode() else {
        let frame = data_init.init(
            frame,
            Arc::new(FrameData {
                output_name: output.name(),
                region: Rectangle::from_size((0, 0).into()),
                overlay_cursor,
                used: Mutex::new(true),
            }),
        );
        frame.failed();
        return;
    };
    let scale = output.current_scale().fractional_scale();
    let output_region = Rectangle::<i32, Physical>::from_size(mode.size);
    let physical_region = logical_region
        .map(|region| region.to_f64().to_physical(scale).to_i32_round())
        .unwrap_or(output_region)
        .intersection(output_region)
        .unwrap_or_else(|| Rectangle::from_size((0, 0).into()));
    let region = Rectangle::<i32, Buffer>::new(
        (physical_region.loc.x, physical_region.loc.y).into(),
        (physical_region.size.w, physical_region.size.h).into(),
    );
    let data = Arc::new(FrameData {
        output_name: output.name(),
        region,
        overlay_cursor,
        used: Mutex::new(false),
    });
    let frame = data_init.init(frame, data);
    if region.size.w <= 0 || region.size.h <= 0 {
        frame.failed();
        return;
    }
    frame.buffer(
        wl_shm::Format::Xrgb8888,
        region.size.w as u32,
        region.size.h as u32,
        region.size.w as u32 * BYTES_PER_PIXEL as u32,
    );
    if frame.version() >= 3 {
        frame.buffer_done();
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Arc<FrameData>, App> for App {
    fn request(
        state: &mut App,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &Arc<FrameData>,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, App>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            zwlr_screencopy_frame_v1::Request::Destroy => return,
            _ => unreachable!(),
        };
        let mut used = data.used.lock().unwrap();
        if *used {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed as u32,
                "screencopy frame has already been used",
            );
            return;
        }
        *used = true;
        if let Err(error) = validate_buffer(&buffer, data.region) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer as u32, error);
            return;
        }
        state.protocols.screencopy.pending.push(PendingScreencopy {
            output_name: data.output_name.clone(),
            region: data.region,
            frame: frame.clone(),
            buffer,
            with_damage,
            overlay_cursor: data.overlay_cursor,
        });
    }

    fn destroyed(
        state: &mut App,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        frame: &ZwlrScreencopyFrameV1,
        _data: &Arc<FrameData>,
    ) {
        let pending = std::mem::take(&mut state.protocols.screencopy.pending);
        state.protocols.screencopy.pending = pending
            .into_iter()
            .filter_map(|request| {
                if request.frame.id() == frame.id() {
                    request.buffer.release();
                    None
                } else {
                    Some(request)
                }
            })
            .collect();
    }
}

fn validate_buffer(buffer: &WlBuffer, region: Rectangle<i32, Buffer>) -> Result<(), String> {
    with_buffer_contents_mut(buffer, |_ptr, pool_len, data| {
        if data.format != wl_shm::Format::Xrgb8888 {
            return Err("screencopy requires wl_shm XRGB8888".to_owned());
        }
        if data.width != region.size.w || data.height != region.size.h {
            return Err(
                "screencopy buffer dimensions do not match the advertised frame".to_owned(),
            );
        }
        if data.offset < 0 || data.stride < region.size.w.saturating_mul(BYTES_PER_PIXEL as i32) {
            return Err("screencopy buffer has an invalid offset or stride".to_owned());
        }
        let end = usize::try_from(data.offset)
            .ok()
            .and_then(|offset| {
                usize::try_from(data.stride)
                    .ok()?
                    .checked_mul(usize::try_from(data.height).ok()?)?
                    .checked_add(offset)
            })
            .ok_or_else(|| "screencopy buffer bounds overflow".to_owned())?;
        if end > pool_len {
            return Err("screencopy buffer extends beyond its SHM pool".to_owned());
        }
        Ok(())
    })
    .map_err(|error| format!("screencopy requires a compositor-managed SHM buffer: {error}"))?
}

impl App {
    pub fn take_pending_screencopies(&mut self, output_name: &str) -> Vec<PendingScreencopy> {
        self.protocols.screencopy.take_pending(output_name)
    }
}

pub const SCREENCOPY_FORMAT: Fourcc = Fourcc::Xrgb8888;

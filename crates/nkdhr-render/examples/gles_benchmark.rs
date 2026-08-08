use std::{
    ffi::CStr,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use nkdhr_render::{
    Color, CornerRadii, DisplayList, DisplayListBuilder, Rect, Shadow, TextureStore,
    gles::GlesBackend,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{
            Bind, Color32F, ExportMem, Frame, Offscreen, Renderer, TextureMapping,
            gles::{GlesRenderbuffer, GlesRenderer, ffi},
        },
    },
    utils::{Rectangle, Transform},
};

const WIDTH: i32 = 2560;
const HEIGHT: i32 = 1600;
const WARM_UP_FRAMES: usize = 30;
const MEASURED_FRAMES: usize = 200;
const TIME_ELAPSED: u32 = 0x88bf;

fn main() {
    let device = select_device(Path::new("/dev/dri/renderD128"));
    let display = unsafe { EGLDisplay::new(device).expect("failed to create EGL display") };
    let context = EGLContext::new(&display).expect("failed to create EGL context");
    let mut renderer =
        unsafe { GlesRenderer::new(context).expect("failed to create GLES renderer") };
    let renderer_name = renderer
        .with_context(|gl| unsafe {
            CStr::from_ptr(gl.GetString(ffi::RENDERER).cast())
                .to_string_lossy()
                .into_owned()
        })
        .expect("failed to query GLES renderer");

    let display_list = benchmark_scene();
    assert_eq!(display_list.len(), 1_000);
    let textures = TextureStore::new();
    let target_size = (WIDTH, HEIGHT).into();
    let mut backend = GlesBackend::new(&mut renderer).expect("failed to create nkdhr GLES backend");
    let prepare_started = Instant::now();
    let prepared = backend
        .prepare(&mut renderer, &display_list, &textures, target_size, 1.0)
        .expect("failed to prepare display list");
    let prepare_duration = prepare_started.elapsed();
    let mut offscreen = Offscreen::<GlesRenderbuffer>::create_buffer(
        &mut renderer,
        Fourcc::Abgr8888,
        (WIDTH, HEIGHT).into(),
    )
    .expect("failed to allocate offscreen target");

    for _ in 0..WARM_UP_FRAMES {
        render_frame(
            &mut renderer,
            &mut backend,
            &prepared,
            &mut offscreen,
            false,
        );
    }
    let mut wall_durations = Vec::with_capacity(MEASURED_FRAMES);
    let mut gpu_durations = Vec::with_capacity(MEASURED_FRAMES);
    for _ in 0..MEASURED_FRAMES {
        let started = Instant::now();
        let gpu = render_frame(&mut renderer, &mut backend, &prepared, &mut offscreen, true)
            .expect("timer query was requested");
        wall_durations.push(started.elapsed());
        gpu_durations.push(gpu);
    }
    wall_durations.sort_unstable();
    gpu_durations.sort_unstable();

    let dump_path = PathBuf::from("/tmp/nkdhr-render-gles-benchmark.ppm");
    dump_target(&mut renderer, &mut offscreen, &dump_path);
    backend
        .destroy(&mut renderer)
        .expect("failed to destroy nkdhr GLES resources");

    println!("renderer: {renderer_name}");
    let batch_count = prepared.batch_count();
    let batch_label = if batch_count == 1 { "batch" } else { "batches" };
    println!(
        "scene: {} primitives, {batch_count} {batch_label}, {}x{}",
        prepared.primitive_count(),
        WIDTH,
        HEIGHT
    );
    println!(
        "display-list compile + VBO upload: {:.3} ms",
        milliseconds(prepare_duration)
    );
    println!(
        "UI draw GPU time: median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        milliseconds(percentile(&gpu_durations, 0.50)),
        milliseconds(percentile(&gpu_durations, 0.95)),
        milliseconds(*gpu_durations.last().unwrap())
    );
    println!(
        "full clear + UI + fence wall time: median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        milliseconds(percentile(&wall_durations, 0.50)),
        milliseconds(percentile(&wall_durations, 0.95)),
        milliseconds(*wall_durations.last().unwrap())
    );
    println!("frame dump: {}", dump_path.display());
}

fn select_device(path: &Path) -> EGLDevice {
    EGLDevice::enumerate()
        .expect("failed to enumerate EGL devices")
        .find(|device| {
            device.render_device_path().ok().as_deref() == Some(path)
                || device.drm_device_path().ok().as_deref() == Some(path)
        })
        .unwrap_or_else(|| panic!("no EGL device matches {}", path.display()))
}

fn benchmark_scene() -> DisplayList {
    let mut builder = DisplayListBuilder::new();
    for index in 0..250 {
        let column = index % 25;
        let row = index / 25;
        let x = 330.0 + column as f32 * 76.0;
        let y = 275.0 + row as f32 * 105.0;
        let card = Rect::new(x, y, 62.0, 80.0);
        let radii = CornerRadii::all(8.0 + (index % 4) as f32);
        if index % 5 == 0 {
            builder
                .shadow(
                    card,
                    radii,
                    Shadow::new(0.0, 3.0, 4.0, 0.0, Color::from_srgba8(0, 0, 0, 96)),
                )
                .unwrap();
        } else {
            builder
                .rect(
                    Rect::new(x + 8.0, y + 26.0, 38.0, 4.0),
                    Color::from_srgba8(238, 242, 255, 48),
                )
                .unwrap();
        }
        builder
            .rounded_rect(card, radii, Color::from_srgba8(36, 40, 59, 255))
            .unwrap();
        builder
            .border(card, radii, 1.0, Color::from_srgba8(130, 204, 255, 120))
            .unwrap();
        builder
            .rect(
                Rect::new(x + 8.0, y + 12.0, 46.0, 6.0),
                Color::from_srgba8(91, 182, 255, 220),
            )
            .unwrap();
    }
    builder.finish()
}

fn render_frame(
    renderer: &mut GlesRenderer,
    backend: &mut GlesBackend,
    prepared: &nkdhr_render::gles::PreparedDisplayList,
    offscreen: &mut GlesRenderbuffer,
    measure_gpu: bool,
) -> Option<Duration> {
    let mut target = renderer.bind(offscreen).expect("failed to bind target");
    let mut frame = renderer
        .render(&mut target, (WIDTH, HEIGHT).into(), Transform::Normal)
        .expect("failed to begin frame");
    let damage = [Rectangle::from_size((WIDTH, HEIGHT).into())];
    frame
        .clear(Color32F::new(0.075, 0.086, 0.122, 1.0), &damage)
        .expect("failed to clear frame");
    let mut query = 0;
    if measure_gpu {
        frame
            .with_context(|gl| unsafe {
                gl.GenQueries(1, &mut query);
                gl.BeginQuery(TIME_ELAPSED, query);
            })
            .expect("failed to begin GPU timer query");
    }
    backend
        .draw(&mut frame, prepared, &damage)
        .expect("failed to draw display list");
    if measure_gpu {
        frame
            .with_context(|gl| unsafe { gl.EndQuery(TIME_ELAPSED) })
            .expect("failed to end GPU timer query");
    }
    frame
        .finish()
        .expect("failed to finish frame")
        .wait()
        .expect("failed to wait for frame completion");
    measure_gpu.then(|| {
        let elapsed = renderer
            .with_context(|gl| unsafe {
                let mut nanoseconds = 0;
                gl.GetQueryObjectuiv(query, ffi::QUERY_RESULT, &mut nanoseconds);
                gl.DeleteQueries(1, &query);
                nanoseconds
            })
            .expect("failed to read GPU timer query");
        Duration::from_nanos(u64::from(elapsed))
    })
}

fn dump_target(renderer: &mut GlesRenderer, offscreen: &mut GlesRenderbuffer, path: &Path) {
    let target = renderer
        .bind(offscreen)
        .expect("failed to bind target for readback");
    let mapping = renderer
        .copy_framebuffer(
            &target,
            Rectangle::from_size((WIDTH, HEIGHT).into()),
            Fourcc::Abgr8888,
        )
        .expect("failed to copy target");
    let flipped = mapping.flipped();
    let pixels = renderer
        .map_texture(&mapping)
        .expect("failed to map target");
    let row_length = WIDTH as usize * 4;
    let mut ppm = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
    for output_y in 0..HEIGHT as usize {
        let source_y = if flipped {
            output_y
        } else {
            HEIGHT as usize - output_y - 1
        };
        let row = &pixels[source_y * row_length..(source_y + 1) * row_length];
        for pixel in row.chunks_exact(4) {
            ppm.extend_from_slice(&pixel[..3]);
        }
    }
    fs::write(path, ppm).expect("failed to write frame dump");
}

fn percentile(sorted: &[Duration], fraction: f32) -> Duration {
    let index = ((sorted.len() - 1) as f32 * fraction).round() as usize;
    sorted[index]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

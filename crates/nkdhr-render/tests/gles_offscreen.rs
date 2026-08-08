use std::path::Path;

use nkdhr_render::{
    AlphaMode, Color, CornerRadii, DisplayList, DisplayListBuilder, Rect, Sampling, Shadow,
    TextureStore, Transform, gles::GlesBackend, software::SoftwareRenderer,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{
            Bind, Color32F, ExportMem, Frame, Offscreen, Renderer, TextureMapping,
            gles::{GlesRenderbuffer, GlesRenderer},
        },
    },
    utils::{Rectangle, Transform as OutputTransform},
};

const WIDTH: i32 = 128;
const HEIGHT: i32 = 96;
const BACKGROUND: Color = Color::from_srgba8(19, 22, 31, 255);

#[test]
fn offscreen_gles_matches_the_software_oracle() {
    let Some(device) = render_device() else {
        eprintln!("skipping GLES oracle comparison: no EGL render device is available");
        return;
    };
    let (display_list, textures) = scene();
    let expected = software_image(&display_list, &textures);
    let actual = gles_image(device, &display_list, &textures);
    if std::env::var_os("NKDHR_RENDER_DUMP_MISMATCH").is_some() {
        write_ppm("/tmp/nkdhr-render-software.ppm", &expected);
        write_ppm("/tmp/nkdhr-render-gles.ppm", &actual);
    }

    let mut maximum_difference = 0_u8;
    let mut total_difference = 0_u64;
    let mut compared_channels = 0_u64;
    let mut differences = Vec::with_capacity(actual.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        let difference = actual.abs_diff(*expected);
        maximum_difference = maximum_difference.max(difference);
        total_difference += u64::from(difference);
        compared_channels += 1;
        differences.push(difference);
    }
    differences.sort_unstable();
    let mean_difference = total_difference as f64 / compared_channels as f64;
    let p95_difference = differences[(differences.len() as f32 * 0.95) as usize];
    assert!(
        maximum_difference <= 160 && mean_difference <= 1.5 && p95_difference <= 2,
        "GLES differs from the scalar oracle: maximum channel difference {maximum_difference}, p95 {p95_difference}, mean {mean_difference:.4}"
    );
}

fn write_ppm(path: &str, pixels: &[u8]) {
    let mut ppm = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
    for pixel in pixels.chunks_exact(4) {
        ppm.extend_from_slice(&pixel[..3]);
    }
    std::fs::write(path, ppm).unwrap();
}

fn render_device() -> Option<EGLDevice> {
    let preferred = Path::new("/dev/dri/renderD128");
    let devices: Vec<_> = EGLDevice::enumerate().ok()?.collect();
    devices
        .iter()
        .find(|device| {
            device.render_device_path().ok().as_deref() == Some(preferred)
                || device.drm_device_path().ok().as_deref() == Some(preferred)
        })
        .cloned()
        .or_else(|| devices.into_iter().next())
}

fn software_image(display_list: &DisplayList, textures: &TextureStore) -> Vec<u8> {
    let mut renderer = SoftwareRenderer::new(WIDTH as u32, HEIGHT as u32).unwrap();
    renderer.clear(BACKGROUND);
    renderer.render(display_list, textures, 1.0).unwrap();
    renderer.rgba8()
}

fn gles_image(device: EGLDevice, display_list: &DisplayList, textures: &TextureStore) -> Vec<u8> {
    let display = unsafe { EGLDisplay::new(device).expect("failed to create EGL display") };
    let context = EGLContext::new(&display).expect("failed to create EGL context");
    let mut renderer =
        unsafe { GlesRenderer::new(context).expect("failed to create GLES renderer") };
    let mut backend = GlesBackend::new(&mut renderer).expect("failed to create nkdhr GLES backend");
    let prepared = backend
        .prepare(
            &mut renderer,
            display_list,
            textures,
            (WIDTH, HEIGHT).into(),
            1.0,
        )
        .expect("failed to prepare display list");
    let mut offscreen = Offscreen::<GlesRenderbuffer>::create_buffer(
        &mut renderer,
        Fourcc::Abgr8888,
        (WIDTH, HEIGHT).into(),
    )
    .expect("failed to allocate offscreen target");
    let mut target = renderer
        .bind(&mut offscreen)
        .expect("failed to bind target");
    let mut frame = renderer
        .render(&mut target, (WIDTH, HEIGHT).into(), OutputTransform::Normal)
        .expect("failed to begin frame");
    let damage = [Rectangle::from_size((WIDTH, HEIGHT).into())];
    let [red, green, blue, alpha] = BACKGROUND.components();
    frame
        .clear(Color32F::new(red, green, blue, alpha), &damage)
        .expect("failed to clear target");
    backend
        .draw(&mut frame, &prepared, &damage)
        .expect("failed to draw display list");
    frame
        .finish()
        .expect("failed to finish frame")
        .wait()
        .expect("failed to wait for frame completion");

    let mapping = renderer
        .copy_framebuffer(
            &target,
            Rectangle::from_size((WIDTH, HEIGHT).into()),
            Fourcc::Abgr8888,
        )
        .expect("failed to copy target");
    let flipped = mapping.flipped();
    let mapped = renderer
        .map_texture(&mapping)
        .expect("failed to map target")
        .to_vec();
    drop(target);
    backend.destroy(&mut renderer).unwrap();

    if flipped {
        return mapped;
    }
    let row_length = WIDTH as usize * 4;
    let mut output = vec![0; mapped.len()];
    for y in 0..HEIGHT as usize {
        output[y * row_length..(y + 1) * row_length].copy_from_slice(
            &mapped[(HEIGHT as usize - y - 1) * row_length..(HEIGHT as usize - y) * row_length],
        );
    }
    output
}

fn scene() -> (DisplayList, TextureStore) {
    let mut pixels = Vec::new();
    for y in 0..4 {
        for x in 0..4 {
            let color = if (x + y) % 2 == 0 {
                [91, 182, 255, 255]
            } else {
                [238, 92, 120, 160]
            };
            pixels.extend_from_slice(&color);
        }
    }
    let mut textures = TextureStore::new();
    let texture = textures.insert(4, 4, pixels, AlphaMode::Straight).unwrap();

    let mut builder = DisplayListBuilder::new();
    builder
        .shadow(
            Rect::new(12.0, 12.0, 72.0, 52.0),
            CornerRadii::all(11.0),
            Shadow::new(2.0, 4.0, 5.0, 1.0, Color::from_srgba8(0, 0, 0, 180)),
        )
        .unwrap();
    builder
        .rounded_rect(
            Rect::new(12.0, 12.0, 72.0, 52.0),
            CornerRadii::new(11.0, 4.0, 16.0, 1.0),
            Color::from_srgba8(49, 55, 78, 255),
        )
        .unwrap();
    builder
        .border(
            Rect::new(12.0, 12.0, 72.0, 52.0),
            CornerRadii::new(11.0, 4.0, 16.0, 1.0),
            2.0,
            Color::from_srgba8(130, 204, 255, 220),
        )
        .unwrap();
    builder
        .with_clip(Rect::new(24.0, 20.0, 90.0, 62.0), |builder| {
            builder.with_transform(
                Transform::translation(88.0, 54.0)
                    .concat(Transform::rotation(0.22))
                    .concat(Transform::translation(-25.0, -22.0)),
                |builder| {
                    builder.texture(
                        Rect::new(0.0, 0.0, 50.0, 44.0),
                        texture,
                        None,
                        0.85,
                        Sampling::Linear,
                    )
                },
            )
        })
        .unwrap();
    (builder.finish(), textures)
}

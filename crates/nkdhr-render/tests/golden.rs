use std::{fs, path::PathBuf};

use nkdhr_render::{
    AlphaMode, Color, CornerRadii, DisplayList, DisplayListBuilder, Rect, Sampling, Shadow,
    TextureStore, Transform, software::SoftwareRenderer,
};

const WIDTH: u32 = 96;
const HEIGHT: u32 = 64;
const BACKGROUND: Color = Color::from_srgba8(19, 22, 31, 255);

#[test]
fn primitive_galleries_match_committed_goldens() {
    let cases = [
        ("rectangles", rectangles()),
        ("rounded-border", rounded_border()),
        ("shadow-transform", shadow_transform()),
        ("texture-clip", texture_clip()),
    ];
    let update = std::env::var_os("UPDATE_GOLDENS").is_some_and(|value| value != "0");
    for (name, (display_list, textures)) in cases {
        let mut renderer = SoftwareRenderer::new(WIDTH, HEIGHT).unwrap();
        renderer.clear(BACKGROUND);
        renderer.render(&display_list, &textures, 1.0).unwrap();
        let actual = renderer.ppm();
        let path = golden_path(name);
        if update {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &actual).unwrap();
        } else {
            let expected = fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "failed to read {}: {error}; run UPDATE_GOLDENS=1 cargo test -p nkdhr-render --test golden",
                    path.display()
                )
            });
            assert_eq!(
                actual, expected,
                "golden {name} changed; inspect the rendering, then update deliberately with UPDATE_GOLDENS=1"
            );
        }
    }
}

fn rectangles() -> (DisplayList, TextureStore) {
    let mut builder = DisplayListBuilder::new();
    builder
        .rect(
            Rect::new(8.0, 8.0, 44.0, 24.0),
            Color::from_srgba8(91, 182, 255, 255),
        )
        .unwrap();
    builder
        .rect(
            Rect::new(30.0, 22.0, 52.0, 32.0),
            Color::from_srgba8(238, 92, 120, 160),
        )
        .unwrap();
    builder
        .rect(
            Rect::new(8.5, 44.5, 18.0, 10.0),
            Color::from_srgba8(245, 211, 105, 220),
        )
        .unwrap();
    (builder.finish(), TextureStore::new())
}

fn rounded_border() -> (DisplayList, TextureStore) {
    let mut builder = DisplayListBuilder::new();
    builder
        .rounded_rect(
            Rect::new(8.0, 8.0, 80.0, 48.0),
            CornerRadii::new(20.0, 8.0, 24.0, 2.0),
            Color::from_srgba8(49, 55, 78, 255),
        )
        .unwrap();
    builder
        .border(
            Rect::new(8.0, 8.0, 80.0, 48.0),
            CornerRadii::new(20.0, 8.0, 24.0, 2.0),
            3.0,
            Color::from_srgba8(130, 204, 255, 230),
        )
        .unwrap();
    builder
        .border(
            Rect::new(31.0, 21.0, 34.0, 22.0),
            CornerRadii::all(11.0),
            20.0,
            Color::from_srgba8(255, 255, 255, 72),
        )
        .unwrap();
    (builder.finish(), TextureStore::new())
}

fn shadow_transform() -> (DisplayList, TextureStore) {
    let mut builder = DisplayListBuilder::new();
    let transform = Transform::translation(48.0, 31.0)
        .concat(Transform::rotation(-0.16))
        .concat(Transform::translation(-30.0, -18.0));
    builder
        .with_transform(transform, |builder| {
            builder.shadow(
                Rect::new(8.0, 8.0, 60.0, 36.0),
                CornerRadii::all(10.0),
                Shadow::new(3.0, 5.0, 6.0, 2.0, Color::from_srgba8(0, 0, 0, 180)),
            )?;
            builder.rounded_rect(
                Rect::new(8.0, 8.0, 60.0, 36.0),
                CornerRadii::all(10.0),
                Color::from_srgba8(113, 226, 174, 255),
            )
        })
        .unwrap();
    (builder.finish(), TextureStore::new())
}

fn texture_clip() -> (DisplayList, TextureStore) {
    let mut textures = TextureStore::new();
    let mut pixels = Vec::new();
    for y in 0..8 {
        for x in 0..8 {
            let color = if (x + y) % 2 == 0 {
                [91, 182, 255, 255]
            } else {
                [238, 92, 120, if x < 4 { 128 } else { 255 }]
            };
            pixels.extend_from_slice(&color);
        }
    }
    let texture = textures.insert(8, 8, pixels, AlphaMode::Straight).unwrap();
    let mut builder = DisplayListBuilder::new();
    builder
        .with_clip(Rect::new(8.0, 8.0, 80.0, 48.0), |builder| {
            builder.texture(
                Rect::new(0.0, 4.0, 48.0, 56.0),
                texture,
                None,
                1.0,
                Sampling::Nearest,
            )?;
            builder.with_transform(
                Transform::translation(65.0, 32.0)
                    .concat(Transform::rotation(0.25))
                    .concat(Transform::translation(-22.0, -20.0)),
                |builder| {
                    builder.texture(
                        Rect::new(0.0, 0.0, 44.0, 40.0),
                        texture,
                        Some(Rect::new(1.0, 1.0, 6.0, 6.0)),
                        0.8,
                        Sampling::Linear,
                    )
                },
            )
        })
        .unwrap();
    (builder.finish(), textures)
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(format!("{name}.ppm"))
}

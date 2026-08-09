use std::{fs, path::PathBuf, sync::Arc};

use cosmic_text::{FontSystem, fontdb};
use nkdhr_render::{
    Color, DisplayList, DisplayListBuilder, Point, Primitive, Rect, TextureStore,
    software::SoftwareRenderer,
};
use nkdhr_settings::{
    AppearanceSettings, INSPECTOR_WIDTH, LAYOUT_INSET, SettingsAssets, SettingsLayoutMode,
};
use nkdhr_ui::{
    MaterialCapabilities, SemanticRole, Size, Theme, UiRoot,
    text::{TextConfig, TextResources, TextSystem},
};

const GOLDEN_WIDTH: u32 = 1_160;
const GOLDEN_HEIGHT: u32 = 760;

fn fixture_text_resources() -> TextResources {
    let mut database = fontdb::Database::new();
    for bytes in [
        include_bytes!("../../nkdhr-ui/tests/fonts/NotoSansLatin.subset.ttf").as_slice(),
        include_bytes!("fonts/NotoSansCJKsc.appearance.subset.otf").as_slice(),
        include_bytes!("../../nkdhr-ui/tests/fonts/NotoColorEmoji.subset.ttf").as_slice(),
    ] {
        database.load_font_source(fontdb::Source::Binary(Arc::new(bytes.to_vec())));
    }
    database.set_sans_serif_family("Noto Sans");
    let system = TextSystem::with_font_system(
        FontSystem::new_with_locale_and_db("zh-CN".to_owned(), database),
        TextConfig::default(),
    )
    .unwrap();
    TextResources::new(system, TextureStore::new(), 1.0).unwrap()
}

fn capabilities() -> MaterialCapabilities {
    MaterialCapabilities {
        backdrop_blur: true,
        reduced_transparency: false,
        high_contrast: false,
    }
}

fn settings_list(
    model: &AppearanceSettings,
    size: Size,
    capabilities: MaterialCapabilities,
) -> (UiRoot, DisplayList) {
    let mut text_resources = fixture_text_resources();
    let assets = SettingsAssets::load(text_resources.textures_mut()).unwrap();
    let element = model
        .element(size, Arc::new(Theme::default()), &assets, capabilities)
        .unwrap();
    let mut root = UiRoot::with_text(element, text_resources).unwrap();
    root.layout(size).unwrap();
    let mut builder = DisplayListBuilder::new();
    wallpaper(&mut builder, size);
    root.paint(&mut builder).unwrap();
    (root, builder.finish())
}

fn wallpaper(builder: &mut DisplayListBuilder, size: Size) {
    builder
        .rect(
            Rect::new(0.0, 0.0, size.width, size.height),
            Color::from_srgba8(14, 19, 35, 255),
        )
        .unwrap();
    let stripe = size.width / 8.0;
    for index in 0..8 {
        let color = if index % 2 == 0 {
            Color::from_srgba8(63, 74, 120, 255)
        } else {
            Color::from_srgba8(94, 54, 116, 255)
        };
        builder
            .rect(
                Rect::new(index as f32 * stripe, 0.0, stripe, size.height),
                color,
            )
            .unwrap();
    }
    builder
        .rounded_rect(
            Rect::new(size.width * 0.12, size.height * 0.10, 330.0, 260.0),
            nkdhr_render::CornerRadii::all(96.0),
            Color::from_srgba8(80, 162, 180, 220),
        )
        .unwrap();
    builder
        .rounded_rect(
            Rect::new(size.width * 0.62, size.height * 0.52, 360.0, 260.0),
            nkdhr_render::CornerRadii::all(112.0),
            Color::from_srgba8(187, 104, 158, 220),
        )
        .unwrap();
}

#[test]
fn production_view_lays_out_at_every_accepted_width_mode() {
    let model = AppearanceSettings::new();
    model.set_professional_mode(true);
    for (width, mode) in [
        (1_160.0, SettingsLayoutMode::ThreeColumn),
        (1_000.0, SettingsLayoutMode::NavigationAndContent),
        (760.0, SettingsLayoutMode::CompactNavigation),
        (640.0, SettingsLayoutMode::SingleColumn),
    ] {
        let size = Size::new(width, 760.0);
        let (mut root, list) = settings_list(&model, size, capabilities());
        assert_eq!(SettingsLayoutMode::for_width(width), mode);
        assert_eq!(
            list.primitives()
                .iter()
                .filter(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
                .count(),
            1,
            "only the outer Settings glass may blur at {width}px"
        );
        if let Some(directory) = std::env::var_os("DUMP_SETTINGS_ORACLES") {
            let mut renderer = SoftwareRenderer::new(width as u32, size.height as u32).unwrap();
            renderer.clear(Color::from_srgba8(14, 19, 35, 255));
            renderer
                .render(&list, root.texture_store().unwrap(), 1.0)
                .unwrap();
            fs::write(
                PathBuf::from(directory).join(format!("appearance-{width:.0}.ppm")),
                renderer.ppm(),
            )
            .unwrap();
        }
        let semantics = root.semantic_tree();
        assert!(semantics.iter().any(|node| {
            node.semantics.role == SemanticRole::Group
                && node.semantics.label.as_deref() == Some("nkdhr 设置")
        }));
        assert!(semantics.iter().any(|node| {
            node.semantics.role == SemanticRole::Text
                && node.semantics.label.as_deref() == Some("外观与交互")
        }));
        if mode != SettingsLayoutMode::ThreeColumn {
            let shell = root.children(root.root_id()).unwrap()[0];
            let body_padding = root.children(shell).unwrap()[1];
            let body_layout = root.children(body_padding).unwrap()[0];
            let drawer_barrier = root.children(body_layout).unwrap()[2];
            let drawer_left =
                width - LAYOUT_INSET - INSPECTOR_WIDTH.min(width - LAYOUT_INSET * 2.0);
            assert_eq!(
                root.hit_test(Point::new(drawer_left + 20.0, 650.0)),
                Some(drawer_barrier),
                "blank drawer material must block pointer-through at {width}px"
            );
        }
    }
}

#[test]
fn blur_config_and_accessibility_capability_reach_the_outer_surface() {
    let model = AppearanceSettings::new();
    let size = Size::new(1_160.0, 760.0);
    let (_, capable) = settings_list(&model, size, capabilities());
    assert!(
        capable
            .primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
    );

    let (_, reduced) = settings_list(
        &model,
        size,
        MaterialCapabilities {
            reduced_transparency: true,
            ..capabilities()
        },
    );
    assert!(
        !reduced
            .primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
    );
}

#[test]
fn accepted_wide_composition_matches_the_committed_software_golden() {
    let model = AppearanceSettings::new();
    model.set_professional_mode(true);
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let (root, list) = settings_list(&model, size, capabilities());
    let mut renderer = SoftwareRenderer::new(GOLDEN_WIDTH, GOLDEN_HEIGHT).unwrap();
    renderer.clear(Color::from_srgba8(14, 19, 35, 255));
    renderer
        .render(&list, root.texture_store().unwrap(), 1.0)
        .unwrap();
    let actual = renderer.ppm();
    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        fs::write(&path, &actual).unwrap();
    }
    let expected = fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "failed to read {}: {error}; run UPDATE_GOLDENS=1 cargo test -p nkdhr-settings --test settings_view",
            path.display()
        )
    });
    assert_eq!(actual, expected, "Settings composition golden changed");
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/appearance-settings.ppm")
}

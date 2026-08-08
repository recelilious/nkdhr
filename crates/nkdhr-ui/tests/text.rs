use std::{fs, path::PathBuf, sync::Arc};

use cosmic_text::{FontSystem, fontdb};
use nkdhr_render::{Color, DisplayList, DisplayListBuilder, Point, Rect, TextureStore};
use nkdhr_ui::text::{AtlasConfig, TextConfig, TextStyle, TextSystem, TextWrap};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 104;
const BACKGROUND: Color = Color::from_srgba8(19, 22, 31, 255);

#[test]
fn mixed_latin_cjk_and_emoji_shape_through_distinct_fallbacks() {
    let mut text = fixture_system(TextConfig::default());
    let layout = text
        .layout(
            "nkdhr UI 你好，世界！ 🚀",
            &fixture_style(),
            Some(280.0),
            1.0,
        )
        .unwrap();
    let families = text.resolved_families(&layout);
    assert!(
        families.iter().any(|family| family == "Noto Sans"),
        "resolved families: {families:?}"
    );
    assert!(families.iter().any(|family| family == "Noto Sans CJK SC"));
    assert!(families.iter().any(|family| family == "Noto Color Emoji"));
    assert!(layout.distinct_font_count() >= 3);
}

#[test]
fn installed_color_emoji_rasterizes_as_rgba() {
    let mut text = TextSystem::new(TextConfig::default()).unwrap();
    let layout = text.layout("🚀", &fixture_style(), None, 1.0).unwrap();
    if !text
        .resolved_families(&layout)
        .iter()
        .any(|family| family == "Noto Color Emoji")
    {
        eprintln!("skipping color emoji raster check: Noto Color Emoji is not installed");
        return;
    }
    let mut textures = TextureStore::new();
    let _ = record_text(
        &mut text,
        &mut textures,
        &layout,
        Point::new(0.0, 0.0),
        None,
    );
    assert_eq!(text.atlas_stats().color_pages, 1);
}

#[test]
fn advanced_shaping_marks_bidirectional_runs() {
    let mut text = TextSystem::new(TextConfig::default()).unwrap();
    let layout = text
        .layout("English العربية 123", &fixture_style(), Some(280.0), 1.0)
        .unwrap();
    assert!(layout.has_right_to_left_glyphs());
    assert!(layout.glyph_count() > 10);
}

#[test]
fn layout_cache_is_color_independent_and_width_sensitive() {
    let mut text = fixture_system(TextConfig {
        layout_cache_capacity: 2,
        ..TextConfig::default()
    });
    let style = fixture_style();
    let first = text.layout("nkdhr text", &style, Some(200.0), 1.0).unwrap();
    let same = text.layout("nkdhr text", &style, Some(200.0), 1.0).unwrap();
    assert!(Arc::ptr_eq(&first, &same));
    let narrower = text.layout("nkdhr text", &style, Some(60.0), 1.0).unwrap();
    assert!(!Arc::ptr_eq(&first, &narrower));
    assert!(narrower.line_count() > first.line_count());
    assert_eq!(text.layout_cache_len(), 2);
}

#[test]
fn mixed_script_gallery_matches_the_committed_golden() {
    let mut text = fixture_system(TextConfig::default());
    let layout = text
        .layout(
            "nkdhr UI\n你好，世界！ 🚀",
            &fixture_style(),
            Some(280.0),
            1.0,
        )
        .unwrap();
    let mut textures = TextureStore::new();
    let display_list = record_text(
        &mut text,
        &mut textures,
        &layout,
        Point::new(16.0, 16.0),
        None,
    );
    assert_eq!(
        text.atlas_stats().color_pages,
        1,
        "emoji was not rasterized"
    );
    let actual = render(&display_list, &textures);
    let path = golden_path("mixed-script");
    if std::env::var_os("UPDATE_TEXT_GOLDENS").is_some_and(|value| value != "0") {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, actual).unwrap();
    } else {
        let expected = fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {}: {error}; run UPDATE_TEXT_GOLDENS=1 cargo test -p nkdhr-ui --test text",
                path.display()
            )
        });
        assert_eq!(actual, expected, "mixed-script text golden changed");
    }
}

#[test]
fn page_eviction_rebuilds_without_visual_artifacts() {
    let config = TextConfig {
        layout_cache_capacity: 32,
        atlas: AtlasConfig {
            page_size: 24,
            max_mask_pages: 1,
            max_color_pages: 1,
            max_empty_entries: 64,
        },
    };
    let mut text = fixture_system(config);
    let mut style = fixture_style();
    style.font_size = 15.0;
    style.line_height = 19.0;
    style.wrap = TextWrap::None;
    let first_layout = text.layout("A", &style, None, 1.0).unwrap();
    let mut textures = TextureStore::new();
    let first_list = record_text(
        &mut text,
        &mut textures,
        &first_layout,
        Point::new(4.0, 4.0),
        None,
    );
    let first_image = render_small(&first_list, &textures, 24, 24);
    let first_generation = text.atlas_generation();

    for character in 'B'..='Z' {
        let layout = text
            .layout(&character.to_string(), &style, None, 1.0)
            .unwrap();
        let _ = record_text(
            &mut text,
            &mut textures,
            &layout,
            Point::new(4.0, 4.0),
            None,
        );
    }
    assert!(
        text.atlas_generation() > first_generation,
        "atlas stats: {:?}",
        text.atlas_stats()
    );
    assert!(text.atlas_stats().evictions > 0);

    let rebuilt = record_text(
        &mut text,
        &mut textures,
        &first_layout,
        Point::new(4.0, 4.0),
        None,
    );
    assert_eq!(first_image, render_small(&rebuilt, &textures, 24, 24));
}

#[test]
fn scrolling_clip_visits_only_nearby_lines() {
    let mut text = fixture_system(TextConfig::default());
    let content = (0..500)
        .map(|index| format!("line {index:03}: nkdhr text"))
        .collect::<Vec<_>>()
        .join("\n");
    let layout = text
        .layout(&content, &fixture_style(), Some(280.0), 1.0)
        .unwrap();
    let mut textures = TextureStore::new();
    let mut builder = DisplayListBuilder::new();
    let stats = text
        .begin_frame()
        .draw(
            &mut builder,
            &mut textures,
            &layout,
            Point::new(8.0, -5000.0),
            Color::WHITE,
            Some(Rect::new(0.0, 0.0, 300.0, 120.0)),
        )
        .unwrap();
    assert!(
        stats.visible_glyphs < layout.glyph_count() / 20,
        "visible {}, total {}",
        stats.visible_glyphs,
        layout.glyph_count()
    );
    assert!(stats.recorded_glyphs > 0);
}

fn fixture_system(config: TextConfig) -> TextSystem {
    let mut database = fontdb::Database::new();
    for bytes in [
        include_bytes!("fonts/NotoSansLatin.subset.ttf").as_slice(),
        include_bytes!("fonts/NotoSansCJKsc.subset.otf").as_slice(),
        include_bytes!("fonts/NotoColorEmoji.subset.ttf").as_slice(),
    ] {
        database.load_font_source(fontdb::Source::Binary(Arc::new(bytes.to_vec())));
    }
    database.set_sans_serif_family("Noto Sans");
    let font_system = FontSystem::new_with_locale_and_db("zh-CN".to_owned(), database);
    TextSystem::with_font_system(font_system, config).unwrap()
}

fn fixture_style() -> TextStyle {
    TextStyle {
        families: vec!["Missing Fixture Family".to_owned(), "Noto Sans".to_owned()],
        font_size: 22.0,
        line_height: 30.0,
        ..TextStyle::default()
    }
}

fn record_text(
    text: &mut TextSystem,
    textures: &mut TextureStore,
    layout: &nkdhr_ui::text::TextLayout,
    origin: Point,
    clip: Option<Rect>,
) -> DisplayList {
    let mut builder = DisplayListBuilder::new();
    text.begin_frame()
        .draw(
            &mut builder,
            textures,
            layout,
            origin,
            Color::from_srgba8(228, 234, 255, 255),
            clip,
        )
        .unwrap();
    builder.finish()
}

fn render(display_list: &DisplayList, textures: &TextureStore) -> Vec<u8> {
    render_small(display_list, textures, WIDTH, HEIGHT)
}

fn render_small(
    display_list: &DisplayList,
    textures: &TextureStore,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut renderer = nkdhr_render::software::SoftwareRenderer::new(width, height).unwrap();
    renderer.clear(BACKGROUND);
    renderer.render(display_list, textures, 1.0).unwrap();
    renderer.ppm()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(format!("{name}.ppm"))
}

use std::{sync::Arc, time::Duration, time::Instant};

use nkdhr_render::{Color, DisplayListBuilder, Point, Rect, TextureStore};
use nkdhr_ui::text::{TextConfig, TextStyle, TextSystem};

const WIDTH: f32 = 1200.0;
const HEIGHT: f32 = 800.0;
const WARM_UP_FRAMES: usize = 40;
const MEASURED_FRAMES: usize = 300;

fn main() {
    let font_started = Instant::now();
    let mut text = TextSystem::new(TextConfig::default()).expect("failed to create text system");
    let font_duration = font_started.elapsed();
    let content = long_text();
    let style = TextStyle {
        font_size: 16.0,
        line_height: 22.0,
        ..TextStyle::default()
    };

    let layout_started = Instant::now();
    let layout = text
        .layout(&content, &style, Some(WIDTH), 1.0)
        .expect("failed to shape benchmark text");
    let layout_duration = layout_started.elapsed();
    let cached_started = Instant::now();
    let cached = text
        .layout(&content, &style, Some(WIDTH), 1.0)
        .expect("failed to retrieve cached layout");
    let cached_duration = cached_started.elapsed();
    assert!(Arc::ptr_eq(&layout, &cached));

    let mut textures = TextureStore::new();
    for frame in 0..WARM_UP_FRAMES {
        record_frame(&mut text, &mut textures, &layout, scroll(frame));
    }
    let mut durations = Vec::with_capacity(MEASURED_FRAMES);
    let mut visible_total = 0_usize;
    let mut primitive_total = 0_usize;
    for frame in 0..MEASURED_FRAMES {
        let started = Instant::now();
        let (visible, primitives) = record_frame(
            &mut text,
            &mut textures,
            &layout,
            scroll(frame + WARM_UP_FRAMES),
        );
        durations.push(started.elapsed());
        visible_total += visible;
        primitive_total += primitives;
    }
    durations.sort_unstable();

    println!("font discovery: {:.3} ms", milliseconds(font_duration));
    println!(
        "initial layout: {:.3} ms for {} lines / {} glyphs",
        milliseconds(layout_duration),
        layout.line_count(),
        layout.glyph_count()
    );
    println!(
        "cached layout lookup: {:.3} ms",
        milliseconds(cached_duration)
    );
    println!(
        "scroll record CPU time: median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        milliseconds(percentile(&durations, 0.50)),
        milliseconds(percentile(&durations, 0.95)),
        milliseconds(*durations.last().expect("measured frames are non-empty"))
    );
    println!(
        "per frame: {} visible glyphs, {} recorded primitives",
        visible_total / MEASURED_FRAMES,
        primitive_total / MEASURED_FRAMES
    );
    println!("atlas: {:?}", text.atlas_stats());
}

fn record_frame(
    text: &mut TextSystem,
    textures: &mut TextureStore,
    layout: &nkdhr_ui::text::TextLayout,
    scroll: f32,
) -> (usize, usize) {
    let mut builder = DisplayListBuilder::new();
    let stats = text
        .begin_frame()
        .draw(
            &mut builder,
            textures,
            layout,
            Point::new(0.0, -scroll),
            Color::from_srgba8(228, 234, 255, 255),
            Some(Rect::new(0.0, 0.0, WIDTH, HEIGHT)),
        )
        .expect("failed to record text frame");
    let display_list = builder.finish();
    (stats.visible_glyphs, display_list.len())
}

fn scroll(frame: usize) -> f32 {
    frame as f32 * 37.0
}

fn long_text() -> String {
    (0..5_000)
        .map(|index| {
            format!("{index:04} nkdhr shared text — smooth scrolling, 中文界面渲染测试 🚀")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn percentile(sorted: &[Duration], fraction: f32) -> Duration {
    let index = ((sorted.len() - 1) as f32 * fraction).round() as usize;
    sorted[index]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

//! Render the output-local shell in every enumerated state for design review.
//!
//! The shell's appearance is reviewed against real rendered output rather than
//! a mockup, because its material is frosted glass over live content and its
//! motion is procedural. This tool composes the real `ShellSurface` over a
//! synthetic backdrop and writes PPM frames that a reviewer can page through.
//!
//! ```text
//! cargo run -p nkdhr-shell --example review_sheet -- <output-directory>
//! ```
//!
//! Static states are written as `state-<nn>-<name>.ppm`. Motion is captured by
//! rendering the real surface in real time, so `motion-<nn>.ppm` is a faithful
//! sample of what the compositor draws rather than a reconstruction.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nkdhr_render::software::SoftwareRenderer;
use nkdhr_render::{Color, DisplayListBuilder, Rect, TextureStore};
use nkdhr_shell::{AppGroup, AppPageEncoding, ChainNode, PreviewNode, ShellSurface, SwitcherPhase};
use nkdhr_ui::{Size, ThemeRuntime, UiSurface};

const WIDTH: u32 = 1000;
const HEIGHT: u32 = 560;
const MOTION_FRAMES: usize = 44;
const MOTION_INTERVAL: Duration = Duration::from_millis(16);

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: review_sheet <output-directory>")?;
    fs::create_dir_all(&directory)?;

    for (index, state) in states().into_iter().enumerate() {
        let mut surface = ShellSurface::new(
            Size::new(WIDTH as f32, HEIGHT as f32),
            1.0,
            true,
            state.theme()?,
        )?;
        surface.sync_app_chain(state.nodes, state.preview, state.phase, state.selected)?;
        surface.render(Size::new(WIDTH as f32, HEIGHT as f32), 1.0)?;
        write_frame(
            &directory,
            &format!("state-{index:02}-{}", state.name),
            &surface,
        )?;
    }

    capture_motion(&directory)?;
    println!(
        "wrote {} frames to {}",
        MOTION_FRAMES + 12,
        directory.display()
    );
    Ok(())
}

/// Selection moving between two preview nodes, sampled in real time.
///
/// The conserved-mass executor reads a monotonic clock, so the honest way to
/// show what a reviewer will actually see is to let it run.
fn capture_motion(directory: &Path) -> Result<(), Box<dyn Error>> {
    let size = Size::new(WIDTH as f32, HEIGHT as f32);
    let mut surface = ShellSurface::new(size, 1.0, true, ThemeRuntime::default())?;
    let nodes = vec![ChainNode::Application(group("org.mozilla.firefox", 3, 0.4))];
    let preview = spread_preview(&[(0.0, 0.0), (1.0, 0.35), (0.55, 1.0)], 1);

    surface.sync_app_chain(
        nodes.clone(),
        preview.clone(),
        SwitcherPhase::Expanded,
        Some(1),
    )?;
    surface.render(size, 1.0)?;
    // Retarget to a distant node so the transfer has a visible path.
    let mut retargeted = preview.clone();
    for node in &mut retargeted {
        node.selected = node.window == 3;
    }
    let started = Instant::now();
    for frame in 0..MOTION_FRAMES {
        // `ShellHost::render_data` re-syncs every frame, and that is what
        // advances the conserved-mass executor. Rendering without it would
        // capture a frozen first sample and look like nothing moves.
        surface.sync_app_chain(
            nodes.clone(),
            retargeted.clone(),
            SwitcherPhase::Expanded,
            Some(3),
        )?;
        surface.render(size, 1.0)?;
        write_frame(directory, &format!("motion-{frame:02}"), &surface)?;
        let target = MOTION_INTERVAL * (frame as u32 + 1);
        if let Some(remaining) = target.checked_sub(started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
    Ok(())
}

struct State {
    name: &'static str,
    nodes: Vec<ChainNode>,
    preview: Vec<PreviewNode>,
    phase: SwitcherPhase,
    selected: Option<u64>,
    overrides: Option<&'static str>,
}

impl State {
    fn theme(&self) -> Result<ThemeRuntime, Box<dyn Error>> {
        let Some(overrides) = self.overrides else {
            return Ok(ThemeRuntime::default());
        };
        Ok(ThemeRuntime::new(nkdhr_theme::ThemeProfile {
            overrides: serde_json::from_str(overrides)?,
            ..nkdhr_theme::ThemeProfile::default()
        })?)
    }
}

fn states() -> Vec<State> {
    let overlapping = spread_preview(&[(0.1, 0.2), (0.7, 0.8), (0.7, 0.8)], 2);
    vec![
        state("empty", Vec::new()),
        state(
            "one-window",
            vec![ChainNode::Application(group("foot", 1, 0.0))],
        ),
        state(
            "two-windows",
            vec![ChainNode::Application(group("org.mozilla.firefox", 2, 0.2))],
        ),
        state(
            "five-windows",
            vec![ChainNode::Application(group("org.mozilla.firefox", 5, 0.8))],
        ),
        state(
            "seven-windows-star",
            vec![ChainNode::Application(group("org.mozilla.firefox", 7, 1.0))],
        ),
        state(
            "three-applications",
            vec![
                ChainNode::Application(group("org.mozilla.firefox", 2, 0.2)),
                ChainNode::Application(group("foot", 1, 0.0)),
                ChainNode::Application(group("org.gnome.nautilus", 3, 0.4)),
            ],
        ),
        state("overflow", overflow_chain()),
        State {
            name: "expanded-spread",
            nodes: vec![ChainNode::Application(group("org.mozilla.firefox", 3, 0.4))],
            preview: spread_preview(&[(0.0, 0.0), (1.0, 0.35), (0.55, 1.0)], 2),
            phase: SwitcherPhase::Expanded,
            selected: Some(2),
            overrides: None,
        },
        State {
            name: "expanded-overlapping",
            nodes: vec![ChainNode::Application(group("org.mozilla.firefox", 3, 0.4))],
            preview: overlapping,
            phase: SwitcherPhase::Expanded,
            selected: Some(3),
            overrides: None,
        },
        State {
            name: "reduced-transparency",
            overrides: Some(r#"{"accessibility": {"reduced_transparency": true}}"#),
            ..state(
                "",
                vec![ChainNode::Application(group("org.mozilla.firefox", 3, 0.4))],
            )
        },
        State {
            name: "high-contrast",
            overrides: Some(r#"{"accessibility": {"high_contrast": true}}"#),
            ..state(
                "",
                vec![ChainNode::Application(group("org.mozilla.firefox", 3, 0.4))],
            )
        },
        State {
            name: "reduced-motion",
            overrides: Some(r#"{"motion": {"mode": "reduced"}}"#),
            ..state(
                "",
                vec![ChainNode::Application(group("org.mozilla.firefox", 3, 0.4))],
            )
        },
    ]
}

fn state(name: &'static str, nodes: Vec<ChainNode>) -> State {
    State {
        name,
        nodes,
        preview: Vec::new(),
        phase: SwitcherPhase::Dormant,
        selected: None,
        overrides: None,
    }
}

fn group(app_key: &str, windows: usize, water_fill: f32) -> AppGroup {
    AppGroup {
        app_key: app_key.to_owned(),
        windows: (1..=windows as u64).collect(),
        mother: 1,
        page_encoding: AppPageEncoding::for_count(windows),
        water_fill,
    }
}

fn overflow_chain() -> Vec<ChainNode> {
    let mut nodes = (0..7)
        .map(|index| ChainNode::Application(group(&format!("app-{index}"), 1, 0.0)))
        .collect::<Vec<_>>();
    nodes.push(ChainNode::Overflow {
        applications: (7..11)
            .map(|index| group(&format!("app-{index}"), 1, 0.0))
            .collect(),
    });
    nodes
}

fn spread_preview(centers: &[(f32, f32)], selected: u64) -> Vec<PreviewNode> {
    centers
        .iter()
        .enumerate()
        .map(|(index, (x, y))| PreviewNode {
            window: index as u64 + 1,
            app_key: "org.mozilla.firefox".to_owned(),
            title: format!("window {}", index + 1),
            center_x: *x,
            center_y: *y,
            chain_index: index,
            stacking_index: index,
            selected: index as u64 + 1 == selected,
        })
        .collect()
}

/// Composite the surface over a backdrop with real structure, because frosted
/// glass over a flat fill shows nothing a reviewer can judge.
fn write_frame(directory: &Path, name: &str, surface: &ShellSurface) -> Result<(), Box<dyn Error>> {
    let mut backdrop = DisplayListBuilder::new();
    backdrop.rect(
        Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32),
        Color::from_srgba8(18, 23, 42, 255),
    )?;
    for index in 0..14 {
        backdrop.rect(
            Rect::new(index as f32 * 74.0, 0.0, 38.0, HEIGHT as f32),
            if index % 2 == 0 {
                Color::from_srgba8(62, 96, 156, 255)
            } else {
                Color::from_srgba8(126, 74, 146, 255)
            },
        )?;
    }
    // Stand-ins for client windows, so blur has edges to pick up.
    for (x, y, w, h) in [
        (60.0, 150.0, 380.0, 240.0),
        (520.0, 90.0, 400.0, 300.0),
        (300.0, 330.0, 340.0, 190.0),
    ] {
        backdrop.rect(Rect::new(x, y, w, h), Color::from_srgba8(28, 33, 56, 235))?;
        backdrop.rect(
            Rect::new(x, y, w, 26.0),
            Color::from_srgba8(52, 58, 88, 255),
        )?;
    }

    let mut renderer = SoftwareRenderer::new(WIDTH, HEIGHT)?;
    renderer.clear(Color::from_srgba8(18, 23, 42, 255));
    renderer.render(&backdrop.finish(), &TextureStore::new(), 1.0)?;
    renderer.render(surface.display_list(), surface.textures(), 1.0)?;
    fs::write(directory.join(format!("{name}.ppm")), renderer.ppm())?;
    Ok(())
}

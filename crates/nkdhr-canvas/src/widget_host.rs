use smithay::backend::allocator::Fourcc;
use smithay::backend::input::ButtonState;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

use nkdhr_render::{DisplayList, Point as UiPoint, TextureStore};
use nkdhr_ui::{Modifiers, PointerButton, UiEvent, UiSurface};

use crate::canvas::world::World;

/// Type-level marker for coordinates local to one pinned canvas node.
#[derive(Debug)]
pub struct PinnedLocal;

/// A pinned node can sit on either side of the normal window stack. Nodes
/// within one layer are ordered by registration, with the last node on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinnedLayer {
    BehindWindows,
    AboveWindows,
}

/// Renderer-independent payload exposed by a pinned node. The compositor's
/// backend adapter turns this into the concrete Smithay render element needed
/// by either the nested GLES renderer or the TTY multi-renderer.
pub enum PinnedRenderData<'a> {
    Memory {
        buffer: &'a MemoryRenderBuffer,
        source_size: Size<i32, Logical>,
    },
    /// A retained nkdhr display list. Placement in canvas/output space stays
    /// the compositor adapter's responsibility.
    NkdhrUi {
        display_list: &'a DisplayList,
        textures: &'a TextureStore,
        commit: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinnedPointerEvent {
    Motion {
        position: Point<f64, PinnedLocal>,
        time: u32,
    },
    Button {
        position: Point<f64, PinnedLocal>,
        button: u32,
        state: ButtonState,
        modifiers: Modifiers,
        time: u32,
    },
    Axis {
        position: Point<f64, PinnedLocal>,
        horizontal: f64,
        vertical: f64,
        modifiers: Modifiers,
        time: u32,
    },
    Leave,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputHandled {
    Captured,
    Ignored,
}

/// Object-safe boundary for compositor-owned content living in canvas world
/// space. It deliberately contains neither a concrete renderer type nor a
/// backend input event type.
pub trait PinnedNode {
    fn id(&self) -> &str;
    fn world_rect(&self) -> Rectangle<f64, World>;
    fn layer(&self) -> PinnedLayer;
    fn render_data(&self) -> PinnedRenderData<'_>;
    fn pointer_event(&mut self, event: PinnedPointerEvent) -> InputHandled;

    /// Give a retained node the effective logical extent and sampling scale
    /// before `render_data` is borrowed for this output.
    fn prepare_frame(&mut self, _output_scale: f32) -> Result<(), String> {
        Ok(())
    }

    fn pointer_capture_active(&self) -> bool {
        false
    }

    fn keyboard_focus_active(&self) -> bool {
        false
    }

    fn keyboard_event(&mut self, _event: &UiEvent) -> InputHandled {
        InputHandled::Ignored
    }
}

/// A compositor-owned canvas node backed by the exact same [`UiSurface`]
/// implementation that a standalone host can present.
pub struct UiPinnedNode {
    id: String,
    rect: Rectangle<f64, World>,
    layer: PinnedLayer,
    surface: Box<dyn UiSurface>,
    clicks: ClickSequence,
}

impl UiPinnedNode {
    pub fn new(
        id: impl Into<String>,
        rect: Rectangle<f64, World>,
        layer: PinnedLayer,
        surface: Box<dyn UiSurface>,
    ) -> Self {
        Self {
            id: id.into(),
            rect,
            layer,
            surface,
            clicks: ClickSequence::default(),
        }
    }
}

impl PinnedNode for UiPinnedNode {
    fn id(&self) -> &str {
        &self.id
    }

    fn world_rect(&self) -> Rectangle<f64, World> {
        self.rect
    }

    fn layer(&self) -> PinnedLayer {
        self.layer
    }

    fn render_data(&self) -> PinnedRenderData<'_> {
        PinnedRenderData::NkdhrUi {
            display_list: self.surface.display_list(),
            textures: self.surface.textures(),
            commit: self.surface.commit(),
        }
    }

    fn prepare_frame(&mut self, output_scale: f32) -> Result<(), String> {
        self.surface
            .render(
                nkdhr_ui::Size::new(self.rect.size.w as f32, self.rect.size.h as f32),
                output_scale,
            )
            .map_err(|error| error.to_string())
    }

    fn pointer_event(&mut self, event: PinnedPointerEvent) -> InputHandled {
        let event = match event {
            PinnedPointerEvent::Motion { position, .. } => UiEvent::PointerMoved {
                position: ui_point(position),
            },
            PinnedPointerEvent::Button {
                position,
                button,
                state,
                modifiers,
                time,
            } => {
                let button = pointer_button(button);
                let click_count = self.clicks.count(button, position, state, time);
                match state {
                    ButtonState::Pressed => UiEvent::PointerDown {
                        position: ui_point(position),
                        button,
                        modifiers,
                        click_count,
                    },
                    ButtonState::Released => UiEvent::PointerUp {
                        position: ui_point(position),
                        button,
                        modifiers,
                        click_count,
                    },
                }
            }
            PinnedPointerEvent::Axis {
                position,
                horizontal,
                vertical,
                modifiers,
                ..
            } => UiEvent::PointerScroll {
                position: ui_point(position),
                delta_x: horizontal as f32,
                delta_y: vertical as f32,
                modifiers,
            },
            PinnedPointerEvent::Leave => UiEvent::PointerLeft,
            PinnedPointerEvent::Cancel => UiEvent::PointerCancel,
        };
        match self.surface.dispatch(&event) {
            Ok(result) if result.handled => InputHandled::Captured,
            // The UI surface itself is one compositor hit target. Empty
            // padding must not click or pan content visually behind it.
            Ok(_) => InputHandled::Captured,
            Err(error) => {
                eprintln!("nkdhr-canvas: UI node {:?} input failed: {error}", self.id);
                InputHandled::Captured
            }
        }
    }

    fn pointer_capture_active(&self) -> bool {
        self.surface.pointer_capture().is_some()
    }

    fn keyboard_focus_active(&self) -> bool {
        self.surface.keyboard_focus().is_some()
    }

    fn keyboard_event(&mut self, event: &UiEvent) -> InputHandled {
        match self.surface.dispatch(event) {
            Ok(result) if result.handled => InputHandled::Captured,
            Ok(_) => InputHandled::Ignored,
            Err(error) => {
                eprintln!("nkdhr-canvas: UI node {:?} input failed: {error}", self.id);
                InputHandled::Captured
            }
        }
    }
}

fn ui_point(point: Point<f64, PinnedLocal>) -> UiPoint {
    UiPoint::new(point.x as f32, point.y as f32)
}

fn pointer_button(button: u32) -> PointerButton {
    match button {
        0x110 => PointerButton::Primary,
        0x111 => PointerButton::Secondary,
        0x112 => PointerButton::Middle,
        other => PointerButton::Other(other.try_into().unwrap_or(u16::MAX)),
    }
}

#[derive(Default)]
struct ClickSequence {
    last_press: Option<(PointerButton, Point<f64, PinnedLocal>, u32)>,
    count: u8,
}

impl ClickSequence {
    fn count(
        &mut self,
        button: PointerButton,
        position: Point<f64, PinnedLocal>,
        state: ButtonState,
        time: u32,
    ) -> u8 {
        if state == ButtonState::Released {
            return self.count.max(1);
        }
        let continues = self
            .last_press
            .is_some_and(|(last_button, last_position, last_time)| {
                last_button == button
                    && time.wrapping_sub(last_time) <= 500
                    && (position.x - last_position.x).abs() <= 5.0
                    && (position.y - last_position.y).abs() <= 5.0
            });
        self.count = if continues {
            self.count.saturating_add(1).min(3)
        } else {
            1
        };
        self.last_press = Some((button, position, time));
        self.count
    }
}

const DEMO_WIDTH: i32 = 192;
const DEMO_HEIGHT: i32 = 112;
const DEMO_ID: &str = "comp-7-static-image";

/// An opt-in developer fixture that exercises the permanent pinned-node seam
/// without becoming temporary product UI. Enable it with
/// `NKDHR_CANVAS_DEMO_PINNED_IMAGE=1`.
pub struct StaticPinnedImage {
    buffer: MemoryRenderBuffer,
    presses: u64,
}

impl StaticPinnedImage {
    fn new() -> Self {
        Self {
            buffer: MemoryRenderBuffer::from_slice(
                &demo_pixels(),
                Fourcc::Abgr8888,
                (DEMO_WIDTH, DEMO_HEIGHT),
                1,
                Transform::Normal,
                None,
            ),
            presses: 0,
        }
    }
}

impl PinnedNode for StaticPinnedImage {
    fn id(&self) -> &str {
        DEMO_ID
    }

    fn world_rect(&self) -> Rectangle<f64, World> {
        Rectangle::new(
            (-320.0, -160.0).into(),
            (f64::from(DEMO_WIDTH), f64::from(DEMO_HEIGHT)).into(),
        )
    }

    fn layer(&self) -> PinnedLayer {
        PinnedLayer::BehindWindows
    }

    fn render_data(&self) -> PinnedRenderData<'_> {
        PinnedRenderData::Memory {
            buffer: &self.buffer,
            source_size: (DEMO_WIDTH, DEMO_HEIGHT).into(),
        }
    }

    fn pointer_event(&mut self, event: PinnedPointerEvent) -> InputHandled {
        if let PinnedPointerEvent::Button {
            state: ButtonState::Pressed,
            ..
        } = event
        {
            self.presses += 1;
            println!(
                "nkdhr-canvas: pinned node {DEMO_ID:?} received press #{}",
                self.presses
            );
        }
        InputHandled::Captured
    }
}

pub fn demo_node_from_env() -> Option<Box<dyn PinnedNode>> {
    std::env::var_os("NKDHR_CANVAS_DEMO_PINNED_IMAGE")
        .filter(|value| !value.is_empty() && value != "0")
        .map(|_| Box::new(StaticPinnedImage::new()) as Box<dyn PinnedNode>)
}

pub fn ui_demo_node_from_env() -> Result<Option<Box<dyn PinnedNode>>, Box<dyn std::error::Error>> {
    let enabled = std::env::var_os("NKDHR_CANVAS_DEMO_UI")
        .is_some_and(|value| !value.is_empty() && value != "0");
    if !enabled {
        return Ok(None);
    }
    let size = nkdhr_ui::Size::new(1160.0, 720.0);
    let surface = nkdhr_settings::AppearanceSurface::new(
        size,
        1.0,
        nkdhr_ui::MaterialCapabilities {
            // COMP-7 does not yet propagate backdrop dependency damage into
            // lower Smithay elements, so the compositor adapter declares the
            // honest compensated-glass capability for this fixture.
            backdrop_blur: false,
            reduced_transparency: false,
            high_contrast: false,
        },
    )?;
    Ok(Some(Box::new(UiPinnedNode::new(
        "ui-5-appearance-settings",
        Rectangle::new((-580.0, -360.0).into(), (1160.0, 720.0).into()),
        PinnedLayer::AboveWindows,
        Box::new(surface),
    ))))
}

fn demo_pixels() -> Vec<u8> {
    let mut pixels = vec![0; (DEMO_WIDTH * DEMO_HEIGHT * 4) as usize];
    for y in 0..DEMO_HEIGHT {
        for x in 0..DEMO_WIDTH {
            let border = !(4..DEMO_WIDTH - 4).contains(&x) || !(4..DEMO_HEIGHT - 4).contains(&y);
            let diagonal = (x - y).rem_euclid(32) < 4;
            let checker = (x / 16 + y / 16) % 2 == 0;
            let color = if border {
                [238, 242, 255, 255]
            } else if diagonal {
                [91, 182, 255, 255]
            } else if checker {
                [49, 55, 78, 255]
            } else {
                [36, 40, 59, 255]
            };
            let offset = ((y * DEMO_WIDTH + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_has_stable_world_geometry_and_identity() {
        let node = StaticPinnedImage::new();
        assert_eq!(node.id(), DEMO_ID);
        assert_eq!(node.layer(), PinnedLayer::BehindWindows);
        assert_eq!(node.world_rect().loc, (-320.0, -160.0).into());
        assert_eq!(
            node.world_rect().size,
            (f64::from(DEMO_WIDTH), f64::from(DEMO_HEIGHT)).into()
        );
    }

    #[test]
    fn demo_captures_backend_neutral_pointer_input() {
        let mut node = StaticPinnedImage::new();
        assert_eq!(
            node.pointer_event(PinnedPointerEvent::Button {
                position: (10.0, 10.0).into(),
                button: 0x110,
                state: ButtonState::Pressed,
                modifiers: Modifiers::default(),
                time: 1,
            }),
            InputHandled::Captured
        );
        assert_eq!(node.presses, 1);
    }
}

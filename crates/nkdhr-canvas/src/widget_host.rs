use smithay::backend::allocator::Fourcc;
use smithay::backend::input::ButtonState;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

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
        time: u32,
    },
    Axis {
        position: Point<f64, PinnedLocal>,
        horizontal: f64,
        vertical: f64,
        time: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputHandled {
    Captured,
    Ignored,
}

/// Object-safe boundary for compositor-owned content living in canvas world
/// space. It deliberately contains neither a concrete renderer type nor a
/// backend input event type.
pub trait PinnedNode: Send {
    fn id(&self) -> &str;
    fn world_rect(&self) -> Rectangle<f64, World>;
    fn layer(&self) -> PinnedLayer;
    fn render_data(&self) -> PinnedRenderData<'_>;
    fn pointer_event(&mut self, event: PinnedPointerEvent) -> InputHandled;
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
                time: 1,
            }),
            InputHandled::Captured
        );
        assert_eq!(node.presses, 1);
    }
}

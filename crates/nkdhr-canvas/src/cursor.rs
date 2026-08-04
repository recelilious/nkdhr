use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::utils::{IsAlive, Logical, Point, Size, Transform};
use smithay::wayland::compositor::with_states;

const FALLBACK_SIZE: i32 = 24;

/// Client-selected pointer image plus a compositor-owned default cursor for
/// the background and clients that do not provide their own surface.
pub struct CursorState {
    status: CursorImageStatus,
    fallback: MemoryRenderBuffer,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            status: CursorImageStatus::default_named(),
            fallback: MemoryRenderBuffer::from_slice(
                &fallback_pixels(),
                Fourcc::Abgr8888,
                (FALLBACK_SIZE, FALLBACK_SIZE),
                1,
                Transform::Normal,
                None,
            ),
        }
    }
}

impl CursorState {
    pub fn set_status(&mut self, status: CursorImageStatus) {
        self.status = status;
    }

    pub fn status(&self) -> CursorImageStatus {
        match &self.status {
            CursorImageStatus::Surface(surface) if !surface.alive() => {
                CursorImageStatus::default_named()
            }
            status => status.clone(),
        }
    }

    pub fn fallback(&self) -> &MemoryRenderBuffer {
        &self.fallback
    }

    pub fn fallback_size(&self) -> Size<i32, Logical> {
        (FALLBACK_SIZE, FALLBACK_SIZE).into()
    }

    pub fn hotspot(&self) -> Point<i32, Logical> {
        match self.status() {
            CursorImageStatus::Surface(surface) => with_states(&surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|attributes| attributes.lock().unwrap().hotspot)
                    .unwrap_or_default()
            }),
            CursorImageStatus::Hidden | CursorImageStatus::Named(_) => (1, 1).into(),
        }
    }

    pub fn surface(
        &self,
    ) -> Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
        match self.status() {
            CursorImageStatus::Surface(surface) => Some(surface),
            CursorImageStatus::Hidden | CursorImageStatus::Named(_) => None,
        }
    }
}

fn fallback_pixels() -> Vec<u8> {
    const OUTER: [(f64, f64); 7] = [
        (1.0, 0.0),
        (1.0, 19.0),
        (6.0, 14.0),
        (10.0, 23.0),
        (14.0, 21.0),
        (10.0, 13.0),
        (19.0, 13.0),
    ];
    const INNER: [(f64, f64); 7] = [
        (3.0, 3.0),
        (3.0, 15.0),
        (6.5, 11.5),
        (10.5, 20.0),
        (11.5, 19.5),
        (7.5, 11.0),
        (15.0, 11.0),
    ];

    let mut pixels = vec![0; (FALLBACK_SIZE * FALLBACK_SIZE * 4) as usize];
    for y in 0..FALLBACK_SIZE {
        for x in 0..FALLBACK_SIZE {
            let point = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let color = if polygon_contains(&INNER, point) {
                [255, 255, 255, 255]
            } else if polygon_contains(&OUTER, point) {
                [0, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            };
            let offset = ((y * FALLBACK_SIZE + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    pixels
}

fn polygon_contains(polygon: &[(f64, f64)], point: (f64, f64)) -> bool {
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let (current_x, current_y) = polygon[current];
        let (previous_x, previous_y) = polygon[previous];
        if (current_y > point.1) != (previous_y > point.1)
            && point.0
                < (previous_x - current_x) * (point.1 - current_y) / (previous_y - current_y)
                    + current_x
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_cursor_has_visible_and_transparent_pixels() {
        let pixels = fallback_pixels();
        let alpha = pixels.chunks_exact(4).map(|pixel| pixel[3]);
        assert!(alpha.clone().any(|value| value == 255));
        assert!(alpha.into_iter().any(|value| value == 0));
    }
}

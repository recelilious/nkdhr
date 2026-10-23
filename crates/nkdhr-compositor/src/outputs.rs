//! The output layout: where every connected output sits in the
//! compositor-global logical coordinate space, and at what scale.
//!
//! That one space is shared by `wl_output` positions, the pointer, window
//! locations and every protocol coordinate the compositor sends. It is
//! resolved from CTRL-5's `outputs` namespace together with the connectors
//! a backend currently drives, and re-resolved whenever either changes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use nkdhr_ipc::{ConfigProxyBlocking, OutputPlacement, OutputPlacements};
use smithay::utils::{Logical, Physical, Point, Rectangle, Size};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

/// The CTRL-5 namespace that holds the arrangement.
const NAMESPACE: &str = "outputs";

/// A connector currently driven by a backend: the DRM connector name with
/// its mode size, or the nested backend's window output.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectedOutput {
    pub name: String,
    pub physical_size: Size<i32, Physical>,
}

/// One connected output after resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOutput {
    pub name: String,
    pub physical_size: Size<i32, Physical>,
    /// The physical size divided by the scale, rounded to the nearest whole
    /// logical pixel on each axis (at least 1): the room the output takes up
    /// in the global space. This is the rounding Smithay uses to advertise
    /// the size through xdg-output, which XWayland's RandR reports in turn,
    /// so clients see exactly the rectangles the compositor lays out.
    pub logical_size: Size<i32, Logical>,
    pub scale: f64,
    /// The output's top-left corner in the compositor-global logical space.
    pub global_location: Point<i32, Logical>,
}

impl ResolvedOutput {
    /// The output's area in the compositor-global logical space.
    pub fn rect(&self) -> Rectangle<i32, Logical> {
        Rectangle::new(self.global_location, self.logical_size)
    }

    fn contains(&self, point: Point<f64, Logical>) -> bool {
        self.rect().to_f64().contains(point)
    }

    /// The point inside this output's area nearest to `point`. The far edges
    /// are excluded, as they are from the area itself: the last reachable
    /// position is one logical pixel before them.
    fn clamp(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        let rect = self.rect().to_f64();
        let max_x = (rect.loc.x + rect.size.w - 1.0).max(rect.loc.x);
        let max_y = (rect.loc.y + rect.size.h - 1.0).max(rect.loc.y);
        (
            point.x.clamp(rect.loc.x, max_x),
            point.y.clamp(rect.loc.y, max_y),
        )
            .into()
    }
}

/// The resolved arrangement of the connected outputs: flat, deterministic
/// and normalized so that its bounding box starts at `(0, 0)`.
///
/// Outputs are kept in layout order: configured outputs in connector-name
/// order, then automatically packed ones in connector-name order. Every
/// "first output" fallback refers to this order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputLayout {
    outputs: Vec<ResolvedOutput>,
}

impl OutputLayout {
    /// Resolves `connected` against the configured placements.
    ///
    /// Configured connectors sit at their configured position and scale;
    /// entries for connectors that are not connected are ignored and reserve
    /// no space. Unconfigured connectors get scale 1.0 and are packed left to
    /// right, level with the top of the configured area and starting at its
    /// right edge (at the origin when nothing is configured), so a connected
    /// output is never left without a place. Finally the whole layout is
    /// translated so that its bounding box starts at `(0, 0)`, which keeps
    /// negative configured coordinates meaningful without ever exposing
    /// them to clients.
    pub fn resolve(config: &OutputPlacements, connected: &[ConnectedOutput]) -> Self {
        let mut configured = Vec::new();
        let mut unconfigured = Vec::new();
        for output in connected {
            match config.get(&output.name) {
                Some(placement) => configured.push((output, *placement)),
                None => unconfigured.push(output),
            }
        }
        configured.sort_by(|(a, _), (b, _)| a.name.cmp(&b.name));
        unconfigured.sort_by(|a, b| a.name.cmp(&b.name));

        let mut outputs = configured
            .into_iter()
            .map(|(output, placement)| {
                resolved(output, placement.scale, (placement.x, placement.y).into())
            })
            .collect::<Vec<_>>();

        // `nkdhrd` accepts any `i32` position, so packing saturates instead
        // of overflowing next to an output configured near the end of the
        // coordinate range; Smithay's own geometry arithmetic saturates the
        // same way.
        let configured_bounds = bounding_rect(&outputs);
        let mut next = configured_bounds.map_or_else(
            || Point::from((0, 0)),
            |bounds| (bounds.loc.x.saturating_add(bounds.size.w), bounds.loc.y).into(),
        );
        for output in unconfigured {
            let output = resolved(output, OutputPlacement::default().scale, next);
            next.x = next.x.saturating_add(output.logical_size.w);
            outputs.push(output);
        }

        if let Some(bounds) = bounding_rect(&outputs) {
            for output in &mut outputs {
                output.global_location -= bounds.loc;
            }
        }
        Self { outputs }
    }

    /// Every output, in layout order.
    pub fn outputs(&self) -> &[ResolvedOutput] {
        &self.outputs
    }

    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    pub fn output(&self, name: &str) -> Option<&ResolvedOutput> {
        self.outputs.iter().find(|output| output.name == name)
    }

    /// The first output, in layout order, whose area contains `point`.
    pub fn output_at(&self, point: Point<f64, Logical>) -> Option<&ResolvedOutput> {
        self.outputs.iter().find(|output| output.contains(point))
    }

    /// The size of the layout's bounding box (at least 1×1). Absolute
    /// pointing devices such as touchscreens and tablets map across it.
    pub fn bounding_size(&self) -> Size<i32, Logical> {
        bounding_rect(&self.outputs).map_or_else(
            || Size::from((1, 1)),
            |bounds| (bounds.size.w.max(1), bounds.size.h.max(1)).into(),
        )
    }

    /// Keeps a pointer position inside the union of the output areas.
    ///
    /// A point on some output is returned unchanged. Any other point is
    /// clamped into each output's area and the nearest result wins, so gaps
    /// between outputs are unreachable and motion that would leave the
    /// union slides along its edge instead of stopping. Clamping into the
    /// bounding box would leave dead zones where outputs of different sizes
    /// or offsets do not fill it. With no output at all the point is
    /// returned unchanged; the next motion after outputs return clamps it.
    pub fn clamp(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        if self.output_at(point).is_some() {
            return point;
        }
        self.outputs
            .iter()
            .map(|output| output.clamp(point))
            .min_by(|a, b| distance_squared(*a, point).total_cmp(&distance_squared(*b, point)))
            .unwrap_or(point)
    }
}

fn resolved(output: &ConnectedOutput, scale: f64, location: Point<i32, Logical>) -> ResolvedOutput {
    let logical_size = logical_size(output.physical_size, scale);
    ResolvedOutput {
        name: output.name.clone(),
        physical_size: output.physical_size,
        logical_size,
        scale,
        global_location: location,
    }
}

/// An output's logical size, rounded exactly as Smithay rounds it for
/// xdg-output (`to_logical(scale).to_i32_round()`), and never smaller than
/// 1×1. A cast from a huge quotient saturates rather than wrapping.
fn logical_size(physical_size: Size<i32, Physical>, scale: f64) -> Size<i32, Logical> {
    let size = physical_size
        .to_f64()
        .to_logical(scale)
        .to_i32_round::<i32>();
    (size.w.max(1), size.h.max(1)).into()
}

fn bounding_rect(outputs: &[ResolvedOutput]) -> Option<Rectangle<i32, Logical>> {
    outputs
        .iter()
        .map(ResolvedOutput::rect)
        .reduce(|a, b| a.merge(b))
}

fn distance_squared(a: Point<f64, Logical>, b: Point<f64, Logical>) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

/// A hot-reloadable snapshot of the `outputs` namespace.
///
/// The snapshot is read once at startup and replaced by a background thread
/// on every `Config1.Changed` signal for a key under `outputs.`; each
/// replacement bumps a generation counter. Backends compare the generation
/// once per loop iteration and re-resolve their layout when it moved, so
/// the D-Bus thread never touches compositor state directly.
pub struct OutputsConfig {
    placements: Arc<Mutex<OutputPlacements>>,
    generation: Arc<AtomicU64>,
}

impl OutputsConfig {
    /// Connects to the session bus and starts following the namespace.
    ///
    /// Without a session bus, without `nkdhrd`, or with an `nkdhrd` that
    /// does not know the namespace, the snapshot stays empty and every
    /// output is packed automatically; the compositor never depends on the
    /// control plane to light up its displays.
    pub fn watch() -> Self {
        let placements = Arc::new(Mutex::new(OutputPlacements::new()));
        let generation = Arc::new(AtomicU64::new(0));

        let Ok(connection) = Connection::session() else {
            eprintln!("nkdhr-compositor: no session D-Bus; arranging outputs automatically");
            return Self {
                placements,
                generation,
            };
        };
        *placements.lock().unwrap() = fetch(&connection);

        let watched = Arc::clone(&placements);
        let watched_generation = Arc::clone(&generation);
        thread::spawn(move || {
            let Ok(config) = ConfigProxyBlocking::new(&connection) else {
                return;
            };
            let Ok(changed) = config.receive_changed() else {
                return;
            };
            let prefix = format!("{NAMESPACE}.");
            for signal in changed {
                let Ok(args) = signal.args() else {
                    continue;
                };
                if args.key().starts_with(&prefix) {
                    *watched.lock().unwrap() = fetch(&connection);
                    watched_generation.fetch_add(1, Ordering::Release);
                }
            }
        });

        Self {
            placements,
            generation,
        }
    }

    pub fn snapshot(&self) -> OutputPlacements {
        self.placements.lock().unwrap().clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

fn fetch(connection: &Connection) -> OutputPlacements {
    let Ok(config) = ConfigProxyBlocking::new(connection) else {
        return OutputPlacements::new();
    };
    let Ok(values) = config.get_all(NAMESPACE) else {
        return OutputPlacements::new();
    };
    decode_flat(values)
}

/// Rebuilds the placements from `Config1.GetAll`'s flattened keys, which
/// look like `outputs.<connector>.<field>`. `nkdhrd` forbids `.` in
/// connector names, so the key splits unambiguously. Missing fields keep
/// their [`OutputPlacement`] defaults; values of an unexpected type, and
/// scales that are not finite and positive, are ignored the same way, so a
/// malformed value can never produce a degenerate output.
fn decode_flat(values: HashMap<String, OwnedValue>) -> OutputPlacements {
    let mut placements = OutputPlacements::new();
    for (key, owned) in values {
        let Some((connector, field)) = key
            .strip_prefix(NAMESPACE)
            .and_then(|path| path.strip_prefix('.'))
            .and_then(|path| path.split_once('.'))
        else {
            continue;
        };
        if connector.is_empty() || field.contains('.') {
            continue;
        }
        let placement = placements.entry(connector.to_owned()).or_default();
        let value = Value::from(owned);
        match field {
            "x" => assign_i32(&mut placement.x, &value),
            "y" => assign_i32(&mut placement.y, &value),
            "scale" => {
                if let Some(scale) =
                    as_f64(&value).filter(|scale| scale.is_finite() && *scale > 0.0)
                {
                    placement.scale = scale;
                }
            }
            _ => {}
        }
    }
    placements
}

fn assign_i32(target: &mut i32, value: &Value<'_>) {
    let converted = match value {
        Value::U8(value) => Some(i32::from(*value)),
        Value::I16(value) => Some(i32::from(*value)),
        Value::U16(value) => Some(i32::from(*value)),
        Value::I32(value) => Some(*value),
        Value::U32(value) => i32::try_from(*value).ok(),
        Value::I64(value) => i32::try_from(*value).ok(),
        Value::U64(value) => i32::try_from(*value).ok(),
        _ => None,
    };
    if let Some(converted) = converted {
        *target = converted;
    }
}

/// A scale normally arrives as a double, but a whole-number scale written
/// by a tool that does not round-trip through `nkdhrd`'s schema could
/// arrive as an integer (`nkdhrd` sends whole numbers as 64-bit integers);
/// both mean the same thing.
fn as_f64(value: &Value<'_>) -> Option<f64> {
    match value {
        Value::F64(value) => Some(*value),
        Value::U8(value) => Some(f64::from(*value)),
        Value::I16(value) => Some(f64::from(*value)),
        Value::U16(value) => Some(f64::from(*value)),
        Value::I32(value) => Some(f64::from(*value)),
        Value::U32(value) => Some(f64::from(*value)),
        Value::I64(value) => Some(*value as f64),
        Value::U64(value) => Some(*value as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, width: i32, height: i32) -> ConnectedOutput {
        ConnectedOutput {
            name: name.to_owned(),
            physical_size: (width, height).into(),
        }
    }

    fn placement(x: i32, y: i32, scale: f64) -> OutputPlacement {
        OutputPlacement { x, y, scale }
    }

    fn locations(layout: &OutputLayout) -> Vec<(&str, Point<i32, Logical>)> {
        layout
            .outputs()
            .iter()
            .map(|output| (output.name.as_str(), output.global_location))
            .collect()
    }

    #[test]
    fn without_configuration_outputs_are_packed_left_to_right_by_name() {
        let layout = OutputLayout::resolve(
            &OutputPlacements::new(),
            &[output("eDP-1", 1920, 1080), output("DP-1", 2560, 1440)],
        );

        assert_eq!(
            locations(&layout),
            [("DP-1", (0, 0).into()), ("eDP-1", (2560, 0).into())]
        );
        assert!(layout.outputs().iter().all(|output| output.scale == 1.0));
        assert_eq!(layout.bounding_size(), (4480, 1440).into());
    }

    #[test]
    fn negative_positions_are_normalized_and_keep_the_arrangement() {
        let config = OutputPlacements::from([
            ("eDP-1".to_owned(), placement(-1920, 360, 1.0)),
            ("DP-1".to_owned(), placement(0, 0, 1.0)),
        ]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("eDP-1", 1920, 1080), output("DP-1", 2560, 1440)],
        );

        assert_eq!(
            locations(&layout),
            [("DP-1", (1920, 0).into()), ("eDP-1", (0, 360).into())]
        );
        assert_eq!(layout.bounding_size(), (4480, 1440).into());
    }

    #[test]
    fn scale_determines_the_logical_size_rounded_to_the_nearest_pixel() {
        let config = OutputPlacements::from([
            ("eDP-1".to_owned(), placement(0, 0, 1.25)),
            ("DP-1".to_owned(), placement(1536, 0, 2.0)),
            ("DP-2".to_owned(), placement(0, 864, 1.5)),
        ]);
        let layout = OutputLayout::resolve(
            &config,
            &[
                output("eDP-1", 1920, 1080),
                output("DP-1", 3840, 2160),
                output("DP-2", 1001, 1001),
            ],
        );

        let edp = layout.output("eDP-1").unwrap();
        assert_eq!(edp.logical_size, (1536, 864).into());
        assert_eq!(edp.physical_size, (1920, 1080).into());
        assert_eq!(edp.scale, 1.25);
        assert_eq!(
            layout.output("DP-1").unwrap().logical_size,
            (1920, 1080).into()
        );
        // 1001 / 1.5 = 667.33: rounded down, as xdg-output advertises it.
        assert_eq!(
            layout.output("DP-2").unwrap().logical_size,
            (667, 667).into()
        );
    }

    #[test]
    fn the_layout_uses_the_size_xdg_output_advertises() {
        // A 2256×1504 panel at 1.75 is 1289.14×859.43 logical pixels; xdg-output
        // reports 1289×859, so an output packed after it starts at x = 1289,
        // leaving no gap that clients would not know about.
        let config = OutputPlacements::from([("eDP-1".to_owned(), placement(0, 0, 1.75))]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("eDP-1", 2256, 1504), output("DP-1", 1920, 1080)],
        );

        assert_eq!(
            layout.output("eDP-1").unwrap().logical_size,
            (1289, 859).into()
        );
        assert_eq!(
            layout.output("DP-1").unwrap().global_location,
            (1289, 0).into()
        );
        // 1366×768 at 1.25 is 1092.8×614.4: rounded up on one axis and down
        // on the other.
        assert_eq!(logical_size((1366, 768).into(), 1.25), (1093, 614).into());
        // A scale so large that the quotient rounds to zero still leaves a
        // 1×1 output.
        assert_eq!(logical_size((10, 10).into(), 1000.0), (1, 1).into());
    }

    #[test]
    fn packing_saturates_at_the_end_of_the_coordinate_range() {
        // Any `i32` position is valid configuration. An unconfigured output
        // packed after one configured near `i32::MAX`, and a scale so small
        // that the logical width saturates, must not overflow.
        let config = OutputPlacements::from([
            ("eDP-1".to_owned(), placement(i32::MAX - 100, 0, 1.0)),
            ("DP-1".to_owned(), placement(i32::MIN, 0, 0.000_000_5)),
        ]);
        let layout = OutputLayout::resolve(
            &config,
            &[
                output("eDP-1", 1920, 1080),
                output("DP-1", 1920, 1080),
                output("HDMI-A-1", 1920, 1080),
                output("HDMI-A-2", 1920, 1080),
            ],
        );

        assert_eq!(layout.outputs().len(), 4);
        assert_eq!(layout.output("DP-1").unwrap().logical_size.w, i32::MAX);
        let hdmi = layout.output("HDMI-A-1").unwrap();
        let next = layout.output("HDMI-A-2").unwrap();
        assert_eq!(hdmi.global_location.y, next.global_location.y);
        assert!(next.global_location.x >= hdmi.global_location.x);
        // Clamping and lookups keep working on such a layout.
        let _ = layout.clamp((f64::from(i32::MAX), 0.0).into());
        let _ = layout.bounding_size();
    }

    #[test]
    fn unconfigured_outputs_are_packed_after_the_configured_area() {
        let config = OutputPlacements::from([
            ("eDP-1".to_owned(), placement(0, 200, 1.0)),
            ("DP-1".to_owned(), placement(1920, 0, 1.0)),
        ]);
        let layout = OutputLayout::resolve(
            &config,
            &[
                output("HDMI-A-2", 800, 600),
                output("eDP-1", 1920, 1080),
                output("HDMI-A-1", 1024, 768),
                output("DP-1", 1280, 1024),
            ],
        );

        assert_eq!(
            locations(&layout),
            [
                ("DP-1", (1920, 0).into()),
                ("eDP-1", (0, 200).into()),
                ("HDMI-A-1", (3200, 0).into()),
                ("HDMI-A-2", (4224, 0).into()),
            ]
        );
        assert_eq!(layout.output("HDMI-A-1").unwrap().scale, 1.0);
    }

    #[test]
    fn entries_for_disconnected_outputs_reserve_no_space() {
        let config = OutputPlacements::from([
            ("DP-1".to_owned(), placement(-2560, 0, 1.0)),
            ("eDP-1".to_owned(), placement(0, 0, 1.0)),
        ]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("eDP-1", 1920, 1080), output("HDMI-A-1", 1920, 1080)],
        );

        assert_eq!(
            locations(&layout),
            [("eDP-1", (0, 0).into()), ("HDMI-A-1", (1920, 0).into())]
        );
        assert!(layout.output("DP-1").is_none());
    }

    #[test]
    fn output_at_uses_half_open_output_areas_and_reports_gaps() {
        let config = OutputPlacements::from([
            ("left".to_owned(), placement(0, 0, 1.0)),
            ("right".to_owned(), placement(2000, 500, 1.0)),
        ]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("left", 1000, 1000), output("right", 1000, 1000)],
        );

        assert_eq!(
            layout.output_at((500.0, 500.0).into()).unwrap().name,
            "left"
        );
        assert_eq!(
            layout.output_at((999.9, 999.9).into()).unwrap().name,
            "left"
        );
        assert!(layout.output_at((1000.0, 500.0).into()).is_none());
        assert!(layout.output_at((1500.0, 250.0).into()).is_none());
        assert_eq!(
            layout.output_at((2000.0, 500.0).into()).unwrap().name,
            "right"
        );
        assert_eq!(layout.bounding_size(), (3000, 1500).into());
    }

    #[test]
    fn clamping_keeps_the_pointer_inside_the_union_of_outputs() {
        let config = OutputPlacements::from([
            ("left".to_owned(), placement(0, 0, 1.0)),
            ("right".to_owned(), placement(1000, 500, 1.0)),
        ]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("left", 1000, 1000), output("right", 1000, 1000)],
        );

        // On an output: unchanged, including the shared vertical edge.
        assert_eq!(layout.clamp((999.5, 700.0).into()), (999.5, 700.0).into());
        assert_eq!(layout.clamp((1000.0, 700.0).into()), (1000.0, 700.0).into());
        // Below the left output, inside the bounding box: no dead zone, the
        // pointer slides along the left output's bottom edge.
        assert_eq!(layout.clamp((400.0, 1200.0).into()), (400.0, 999.0).into());
        // Above the right output: onto its top edge, not the left output.
        assert_eq!(layout.clamp((1600.0, 300.0).into()), (1600.0, 500.0).into());
        // Beyond the far corner of the union.
        assert_eq!(
            layout.clamp((2500.0, 1700.0).into()),
            (1999.0, 1499.0).into()
        );
        // Left of everything.
        assert_eq!(layout.clamp((-50.0, 20.0).into()), (0.0, 20.0).into());
    }

    #[test]
    fn an_empty_layout_has_no_outputs_and_leaves_points_alone() {
        let layout = OutputLayout::resolve(&OutputPlacements::new(), &[]);

        assert!(layout.is_empty());
        assert!(layout.output_at((0.0, 0.0).into()).is_none());
        assert_eq!(layout.clamp((-5.0, 7.0).into()), (-5.0, 7.0).into());
        assert_eq!(layout.bounding_size(), (1, 1).into());
    }

    #[test]
    fn flattened_config_keys_decode_into_placements() {
        let values = HashMap::from([
            ("outputs.eDP-1.x".to_owned(), OwnedValue::from(-1920_i64)),
            ("outputs.eDP-1.y".to_owned(), OwnedValue::from(360_i64)),
            ("outputs.eDP-1.scale".to_owned(), OwnedValue::from(1.25_f64)),
            ("outputs.DP-1.scale".to_owned(), OwnedValue::from(0.0_f64)),
            ("outputs.DP-1.x".to_owned(), OwnedValue::from(i64::MAX)),
            ("outputs.DP-2.scale".to_owned(), OwnedValue::from(2_i64)),
            ("outputs.DP-2.y".to_owned(), OwnedValue::from(1.5_f64)),
            ("outputs.DP-3.scale".to_owned(), OwnedValue::from(f64::NAN)),
            (
                "outputs.HDMI-A-1.unknown".to_owned(),
                OwnedValue::from(1_i64),
            ),
            ("outputs.eDP-2".to_owned(), OwnedValue::from(1_i64)),
            ("other.eDP-1.x".to_owned(), OwnedValue::from(5_i64)),
        ]);

        let placements = decode_flat(values);

        assert_eq!(placements["eDP-1"], placement(-1920, 360, 1.25));
        // Out-of-range, non-positive, non-finite and wrongly typed values
        // keep their defaults; a whole-number scale sent as an integer is a
        // scale like any other.
        assert_eq!(placements["DP-1"], OutputPlacement::default());
        assert_eq!(placements["DP-2"], placement(0, 0, 2.0));
        assert_eq!(placements["DP-3"], OutputPlacement::default());
        assert_eq!(placements["HDMI-A-1"], OutputPlacement::default());
        assert_eq!(placements.len(), 5);
    }
}

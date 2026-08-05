use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use nkdhr_ipc::{
    CanvasOutputGroup, CanvasOutputGroups, CanvasOutputPlacement, ConfigProxyBlocking,
};
use smithay::utils::{Logical, Physical, Point, Rectangle, Size};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

const DEFAULT_GROUP: &str = "default";
const DEFAULT_CANVAS: &str = "default";

/// A connector currently present in the DRM or nested backend.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectedOutput {
    pub name: String,
    pub physical_size: Size<i32, Physical>,
}

/// One connected output after applying `canvas.outputs` and laying every
/// output group into the compositor's global pointer coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOutput {
    pub name: String,
    pub physical_size: Size<i32, Physical>,
    pub logical_size: Size<i32, Logical>,
    pub scale: f64,
    /// Position within the group's rigid virtual display area, normalized
    /// so the group's top-left is `(0, 0)` even when config uses negatives.
    pub group_location: Point<i32, Logical>,
    /// Position in the compositor-wide pointer/output coordinate space.
    pub global_location: Point<i32, Logical>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOutputGroup {
    pub name: String,
    pub canvas: String,
    pub logical_size: Size<i32, Logical>,
    /// Logical point within the rigid group that represents the viewport's
    /// world-space center. Normally this is the primary output's center.
    pub canvas_anchor: Point<f64, Logical>,
    pub global_location: Point<i32, Logical>,
    pub outputs: Vec<ResolvedOutput>,
}

/// Deterministic, hotplug-resolved output layout. Configured group/member
/// names are stable identities; disconnected outputs are simply absent.
/// If no groups are configured, every connected output forms one default
/// horizontal group and views the default canvas. Once any explicit group
/// exists, each unmentioned connector falls back to its own group/canvas so
/// a hotplug can never leave it undisplayed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputLayout {
    pub groups: Vec<ResolvedOutputGroup>,
}

impl OutputLayout {
    pub fn resolve(config: &CanvasOutputGroups, connected: &[ConnectedOutput]) -> Self {
        let connected = connected
            .iter()
            .map(|output| (output.name.as_str(), output))
            .collect::<HashMap<_, _>>();

        let requested = if config.is_empty() {
            let mut members = BTreeMap::new();
            let mut x = 0;
            let mut names = connected.keys().copied().collect::<Vec<_>>();
            names.sort_unstable();
            let primary = names.first().copied().unwrap_or_default().to_owned();
            for name in names {
                members.insert(
                    name.to_owned(),
                    CanvasOutputPlacement {
                        x,
                        ..CanvasOutputPlacement::default()
                    },
                );
                x += connected[name].physical_size.w;
            }
            BTreeMap::from([(
                DEFAULT_GROUP.to_owned(),
                CanvasOutputGroup {
                    canvas: DEFAULT_CANVAS.to_owned(),
                    primary,
                    members,
                },
            )])
        } else {
            config.clone()
        };

        let mut assigned = HashSet::new();
        let mut groups = requested
            .into_iter()
            .filter_map(|(name, group)| {
                let primary = group.primary;
                let outputs = group
                    .members
                    .into_iter()
                    .filter_map(|(output_name, placement)| {
                        let output = connected.get(output_name.as_str())?;
                        assigned.insert(output_name.clone());
                        Some(unpositioned_output(output, output_name, placement))
                    })
                    .collect::<Vec<_>>();
                resolve_group(name, group.canvas, primary, outputs)
            })
            .collect::<Vec<_>>();

        if !config.is_empty() {
            let mut unassigned = connected
                .values()
                .filter(|output| !assigned.contains(&output.name))
                .copied()
                .collect::<Vec<_>>();
            unassigned.sort_by(|a, b| a.name.cmp(&b.name));
            for output in unassigned {
                let identity = format!("auto:{}", output.name);
                let resolved = unpositioned_output(
                    output,
                    output.name.clone(),
                    CanvasOutputPlacement::default(),
                );
                if let Some(group) = resolve_group(
                    identity.clone(),
                    identity,
                    output.name.clone(),
                    vec![resolved],
                ) {
                    groups.push(group);
                }
            }
        }

        // Different canvases have no meaningful spatial relationship, but
        // libinput and wl_output still require one non-overlapping global
        // coordinate space. Pack groups left-to-right deterministically;
        // configured positions remain unchanged *within* each group.
        let mut global_x = 0;
        for group in &mut groups {
            group.global_location = (global_x, 0).into();
            for output in &mut group.outputs {
                output.global_location = group.global_location + output.group_location;
            }
            global_x += group.logical_size.w;
        }

        Self { groups }
    }

    pub fn group_for_output(&self, output_name: &str) -> Option<&ResolvedOutputGroup> {
        self.groups.iter().find(|group| {
            group
                .outputs
                .iter()
                .any(|output| output.name == output_name)
        })
    }

    pub fn output(&self, output_name: &str) -> Option<&ResolvedOutput> {
        self.groups
            .iter()
            .flat_map(|group| &group.outputs)
            .find(|output| output.name == output_name)
    }

    /// The group whose actual output rectangle contains a compositor-global
    /// logical point. Gaps inside a rigid arrangement intentionally return
    /// `None`; input keeps its previous active group there.
    pub fn group_at(&self, point: Point<f64, Logical>) -> Option<&ResolvedOutputGroup> {
        self.groups.iter().find(|group| {
            group.outputs.iter().any(|output| {
                let location = output.global_location.to_f64();
                point.x >= location.x
                    && point.y >= location.y
                    && point.x < location.x + f64::from(output.logical_size.w)
                    && point.y < location.y + f64::from(output.logical_size.h)
            })
        })
    }

    pub fn logical_extent(&self) -> Size<i32, Logical> {
        self.groups.iter().flat_map(|group| &group.outputs).fold(
            (1, 1).into(),
            |extent: Size<i32, Logical>, output| {
                (
                    extent
                        .w
                        .max(output.global_location.x + output.logical_size.w),
                    extent
                        .h
                        .max(output.global_location.y + output.logical_size.h),
                )
                    .into()
            },
        )
    }
}

#[derive(Debug)]
struct UnpositionedOutput {
    name: String,
    physical_size: Size<i32, Physical>,
    logical_size: Size<i32, Logical>,
    scale: f64,
    configured_location: Point<i32, Logical>,
}

fn unpositioned_output(
    output: &ConnectedOutput,
    name: String,
    placement: CanvasOutputPlacement,
) -> UnpositionedOutput {
    let logical_size = (
        (f64::from(output.physical_size.w) / placement.scale).ceil() as i32,
        (f64::from(output.physical_size.h) / placement.scale).ceil() as i32,
    )
        .into();
    UnpositionedOutput {
        name,
        physical_size: output.physical_size,
        logical_size,
        scale: placement.scale,
        configured_location: (placement.x, placement.y).into(),
    }
}

fn resolve_group(
    name: String,
    canvas: String,
    primary: String,
    outputs: Vec<UnpositionedOutput>,
) -> Option<ResolvedOutputGroup> {
    let bounds = outputs
        .iter()
        .map(|output| Rectangle::new(output.configured_location, output.logical_size))
        .reduce(|left, right| left.merge(right))?;
    let primary_output = outputs
        .iter()
        .find(|output| output.name == primary)
        .or_else(|| outputs.first())?;
    let canvas_anchor = (
        f64::from(primary_output.configured_location.x - bounds.loc.x)
            + f64::from(primary_output.logical_size.w) / 2.0,
        f64::from(primary_output.configured_location.y - bounds.loc.y)
            + f64::from(primary_output.logical_size.h) / 2.0,
    )
        .into();
    let outputs = outputs
        .into_iter()
        .map(|output| ResolvedOutput {
            name: output.name,
            physical_size: output.physical_size,
            logical_size: output.logical_size,
            scale: output.scale,
            group_location: output.configured_location - bounds.loc,
            global_location: (0, 0).into(),
        })
        .collect();
    Some(ResolvedOutputGroup {
        name,
        canvas,
        logical_size: bounds.size,
        canvas_anchor,
        global_location: (0, 0).into(),
        outputs,
    })
}

/// A hot-reloadable snapshot of the CTRL-5 output configuration. Backends
/// compare `generation()` in their event loop and re-resolve their current
/// connector set only when the config actually changes.
pub struct OutputConfig {
    groups: Arc<Mutex<CanvasOutputGroups>>,
    generation: Arc<AtomicU64>,
}

impl OutputConfig {
    pub fn watch() -> Self {
        let groups = Arc::new(Mutex::new(CanvasOutputGroups::new()));
        let generation = Arc::new(AtomicU64::new(0));

        let Ok(connection) = Connection::session() else {
            eprintln!("nkdhr-canvas: no session D-Bus, using the default output group");
            return Self { groups, generation };
        };
        *groups.lock().unwrap() = fetch(&connection);

        let watched = Arc::clone(&groups);
        let watched_generation = Arc::clone(&generation);
        thread::spawn(move || {
            let Ok(config) = ConfigProxyBlocking::new(&connection) else {
                return;
            };
            let Ok(changed) = config.receive_changed() else {
                return;
            };
            for signal in changed {
                let Ok(args) = signal.args() else {
                    continue;
                };
                if args.key().starts_with("canvas.outputs.") {
                    *watched.lock().unwrap() = fetch(&connection);
                    watched_generation.fetch_add(1, Ordering::Release);
                }
            }
        });

        Self { groups, generation }
    }

    pub fn snapshot(&self) -> CanvasOutputGroups {
        self.groups.lock().unwrap().clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

fn fetch(connection: &Connection) -> CanvasOutputGroups {
    let Ok(config) = ConfigProxyBlocking::new(connection) else {
        return CanvasOutputGroups::new();
    };
    let Ok(values) = config.get_all("canvas.outputs") else {
        return CanvasOutputGroups::new();
    };
    decode_flat(values)
}

fn decode_flat(values: HashMap<String, OwnedValue>) -> CanvasOutputGroups {
    let mut groups = CanvasOutputGroups::new();
    for (key, owned) in values {
        let Some(path) = key.strip_prefix("canvas.outputs.") else {
            continue;
        };
        let parts = path.split('.').collect::<Vec<_>>();
        match parts.as_slice() {
            [group_name, "canvas"] => {
                if let Value::Str(canvas) = Value::from(owned) {
                    groups
                        .entry((*group_name).to_owned())
                        .or_insert_with(empty_group)
                        .canvas = canvas.to_string();
                }
            }
            [group_name, "primary"] => {
                if let Value::Str(primary) = Value::from(owned) {
                    groups
                        .entry((*group_name).to_owned())
                        .or_insert_with(empty_group)
                        .primary = primary.to_string();
                }
            }
            [group_name, "members", output_name, field] => {
                let group = groups
                    .entry((*group_name).to_owned())
                    .or_insert_with(empty_group);
                let placement = group.members.entry((*output_name).to_owned()).or_default();
                let value = Value::from(owned);
                match (*field, value) {
                    ("x", value) => assign_i32(&mut placement.x, &value),
                    ("y", value) => assign_i32(&mut placement.y, &value),
                    ("scale", Value::F64(scale)) => placement.scale = scale,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    groups.retain(|_, group| !group.canvas.is_empty() && !group.members.is_empty());
    groups
}

fn empty_group() -> CanvasOutputGroup {
    CanvasOutputGroup {
        canvas: String::new(),
        primary: String::new(),
        members: BTreeMap::new(),
    }
}

fn assign_i32(target: &mut i32, value: &Value<'_>) {
    let converted = match value {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, width: i32, height: i32) -> ConnectedOutput {
        ConnectedOutput {
            name: name.to_owned(),
            physical_size: (width, height).into(),
        }
    }

    #[test]
    fn no_config_groups_all_outputs_horizontally_on_the_default_canvas() {
        let layout = OutputLayout::resolve(
            &CanvasOutputGroups::new(),
            &[output("eDP-1", 1920, 1080), output("DP-1", 2560, 1440)],
        );

        assert_eq!(layout.groups.len(), 1);
        let group = &layout.groups[0];
        assert_eq!(group.name, DEFAULT_GROUP);
        assert_eq!(group.canvas, DEFAULT_CANVAS);
        assert_eq!(group.logical_size, (4480, 1440).into());
        assert_eq!(group.outputs[0].name, "DP-1");
        assert_eq!(group.outputs[1].group_location, (2560, 0).into());
        assert_eq!(group.canvas_anchor, (1280.0, 720.0).into());
    }

    #[test]
    fn configured_positions_are_normalized_and_scale_affects_geometry() {
        let members = BTreeMap::from([
            (
                "eDP-1".to_owned(),
                CanvasOutputPlacement {
                    x: -1920,
                    y: 360,
                    scale: 1.0,
                },
            ),
            (
                "DP-1".to_owned(),
                CanvasOutputPlacement {
                    x: 0,
                    y: 0,
                    scale: 2.0,
                },
            ),
        ]);
        let config = BTreeMap::from([(
            "desk".to_owned(),
            CanvasOutputGroup {
                canvas: "work".to_owned(),
                primary: "eDP-1".to_owned(),
                members,
            },
        )]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("eDP-1", 1920, 1080), output("DP-1", 3840, 2160)],
        );

        let group = &layout.groups[0];
        assert_eq!(group.logical_size, (3840, 1440).into());
        assert_eq!(group.outputs[0].group_location, (1920, 0).into());
        assert_eq!(group.outputs[0].logical_size, (1920, 1080).into());
        assert_eq!(group.outputs[1].group_location, (0, 360).into());
        assert_eq!(group.canvas_anchor, (960.0, 900.0).into());
    }

    #[test]
    fn unconfigured_hotplug_gets_a_visible_independent_fallback_group() {
        let config = BTreeMap::from([(
            "internal".to_owned(),
            CanvasOutputGroup {
                canvas: "main".to_owned(),
                primary: "eDP-1".to_owned(),
                members: BTreeMap::from([("eDP-1".to_owned(), CanvasOutputPlacement::default())]),
            },
        )]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("eDP-1", 1920, 1080), output("HDMI-A-1", 1920, 1080)],
        );

        assert_eq!(layout.groups.len(), 2);
        assert_eq!(layout.groups[1].name, "auto:HDMI-A-1");
        assert_eq!(layout.groups[1].global_location, (1920, 0).into());
        assert_eq!(layout.groups[1].canvas_anchor, (960.0, 540.0).into());
    }

    #[test]
    fn disconnected_primary_falls_back_to_a_connected_member() {
        let config = BTreeMap::from([(
            "desk".to_owned(),
            CanvasOutputGroup {
                canvas: "main".to_owned(),
                primary: "DP-1".to_owned(),
                members: BTreeMap::from([
                    ("eDP-1".to_owned(), CanvasOutputPlacement::default()),
                    (
                        "DP-1".to_owned(),
                        CanvasOutputPlacement {
                            x: 1920,
                            ..CanvasOutputPlacement::default()
                        },
                    ),
                ]),
            },
        )]);
        let layout = OutputLayout::resolve(&config, &[output("eDP-1", 1920, 1080)]);

        assert_eq!(layout.groups[0].canvas_anchor, (960.0, 540.0).into());
    }

    #[test]
    fn pointer_routing_uses_real_output_rectangles_not_group_gaps() {
        let config = BTreeMap::from([(
            "desk".to_owned(),
            CanvasOutputGroup {
                canvas: "main".to_owned(),
                primary: "left".to_owned(),
                members: BTreeMap::from([
                    (
                        "left".to_owned(),
                        CanvasOutputPlacement {
                            x: 0,
                            y: 0,
                            scale: 1.0,
                        },
                    ),
                    (
                        "right".to_owned(),
                        CanvasOutputPlacement {
                            x: 2000,
                            y: 500,
                            scale: 1.0,
                        },
                    ),
                ]),
            },
        )]);
        let layout = OutputLayout::resolve(
            &config,
            &[output("left", 1000, 1000), output("right", 1000, 1000)],
        );

        assert_eq!(layout.group_at((500.0, 500.0).into()).unwrap().name, "desk");
        assert!(layout.group_at((1500.0, 250.0).into()).is_none());
        assert_eq!(layout.logical_extent(), (3000, 1500).into());
    }
}

use std::collections::HashSet;

use nkdhr_ipc::CanvasOutputGroups;
use serde::{Deserialize, Serialize};

use crate::backends::config_store::Namespace;

/// The `canvas` namespace: `nkdhr-canvas`'s (COMP-3/4) persisted settings.
/// One file (`canvas.toml`), covering both its keybindings and its
/// position marks — related concerns of the same component, not enough
/// distinct structure between them to earn separate namespaces.
///
/// `close_window`/`cycle_focus`/`overview` are xkbcommon key names (e.g.
/// `"q"`, `"Tab"`, `"F4"` — the same names `xkb::keysym_from_name`
/// accepts), each combined with a fixed modifier `nkdhr-canvas` itself
/// decides (Super for `close_window`/`overview`, Alt for `cycle_focus`) —
/// only the key is configurable here, not the modifier, since a single
/// fixed modifier per action is enough to prove hot-reload works end to
/// end and more general modifier+key combos aren't needed by anything
/// yet.
///
/// `nkdhrd` deliberately does not depend on `xkbcommon` to validate that a
/// key value is a *real* key name — that's real keyboard-layout domain
/// knowledge belonging to `nkdhr-canvas`, not the config-store daemon.
/// `nkdhrd` only rejects what it can judge for itself (a key value can't
/// be empty); an unrecognized key name still round-trips through the
/// store correctly, and `nkdhr-canvas` logs and falls back to its own
/// built-in default rather than erroring, the same way an absent field
/// falls back to *this* struct's `Default`.
///
/// `snap_to_grid` and `grid_size` are the compositor's world-coordinate
/// placement policy. The interval is an integer number of logical world
/// units; `nkdhr-canvas` applies it to new-window positions, the moving
/// edges of interactive move/resize operations, and the work viewport's
/// primary-output anchor after canvas panning ends.
///
/// `marks` is a compact, versioned, `nkdhr-canvas`-owned encoding. COMP-4's
/// original `"<index>:<x>,<y>;..."` form remains readable as marks on the
/// `default` canvas; COMP-5 writes canvas-namespaced `v2` entries. It stays
/// one string rather than a nested table because CTRL-5's `Config1.Get`/`Set`
/// only support scalar
/// leaf values so far (see `docs-staging/control-plane/INTERNALS.md`'s
/// "Config store" section), and a `HashMap`-shaped field would hit a
/// second, sharper limit: `Set` only ever overwrites an *already-existing*
/// leaf (this is how "unknown keys are rejected" applies to writes, not
/// just to hand-edited files), so it could never create a mark's entry
/// the first time it's set. Encoding the whole set as one string sidesteps
/// both limits without changing CTRL-5's engine for one namespace's
/// needs; `nkdhrd` itself never parses the contents, same "don't take on
/// domain knowledge that belongs to the consumer" reasoning as the key
/// names above.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CanvasSettings {
    pub close_window: String,
    pub cycle_focus: String,
    pub overview: String,
    pub snap_to_grid: bool,
    pub grid_size: u32,
    pub marks: String,
    /// COMP-5's output-group configuration. Group and connector names are
    /// dynamic map keys so the TOML shape is both human-editable and
    /// addressable through CTRL-5 as
    /// `canvas.outputs.<group>.members.<connector>.<field>`.
    pub outputs: CanvasOutputGroups,
}

impl Default for CanvasSettings {
    fn default() -> Self {
        Self {
            close_window: "q".to_owned(),
            cycle_focus: "Tab".to_owned(),
            overview: "o".to_owned(),
            snap_to_grid: true,
            grid_size: 32,
            marks: String::new(),
            outputs: CanvasOutputGroups::new(),
        }
    }
}

impl Namespace for CanvasSettings {
    const NAME: &'static str = "canvas";

    fn validate(&self) -> Result<(), String> {
        if self.close_window.trim().is_empty() {
            return Err("close_window must not be empty".to_owned());
        }
        if self.cycle_focus.trim().is_empty() {
            return Err("cycle_focus must not be empty".to_owned());
        }
        if self.overview.trim().is_empty() {
            return Err("overview must not be empty".to_owned());
        }
        if !(1..=4096).contains(&self.grid_size) {
            return Err("grid_size must be between 1 and 4096".to_owned());
        }
        let mut assigned_outputs = HashSet::new();
        for (group_name, group) in &self.outputs {
            validate_path_segment("output group", group_name)?;
            if group.canvas.trim().is_empty() {
                return Err(format!("output group {group_name:?} must name a canvas"));
            }
            if group.members.is_empty() {
                return Err(format!(
                    "output group {group_name:?} must contain at least one output"
                ));
            }
            if !group.primary.is_empty() && !group.members.contains_key(&group.primary) {
                return Err(format!(
                    "primary output {:?} is not a member of output group {group_name:?}",
                    group.primary
                ));
            }
            for (output_name, placement) in &group.members {
                validate_path_segment("output", output_name)?;
                if !assigned_outputs.insert(output_name) {
                    return Err(format!(
                        "output {output_name:?} is assigned to more than one group"
                    ));
                }
                if !placement.scale.is_finite() || placement.scale <= 0.0 {
                    return Err(format!(
                        "output {output_name:?} has invalid scale {}",
                        placement.scale
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_path_segment(kind: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(format!("{kind} name must not be empty"));
    }
    if name.contains('.') {
        return Err(format!(
            "{kind} name {name:?} must not contain '.', which separates CTRL-5 keys"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nkdhr_ipc::{CanvasOutputGroup, CanvasOutputPlacement};

    use super::*;

    fn settings_with(groups: CanvasOutputGroups) -> CanvasSettings {
        CanvasSettings {
            outputs: groups,
            ..CanvasSettings::default()
        }
    }

    #[test]
    fn accepts_a_valid_rigid_output_group() {
        let mut members = BTreeMap::new();
        members.insert("eDP-1".to_owned(), CanvasOutputPlacement::default());
        members.insert(
            "DP-1".to_owned(),
            CanvasOutputPlacement {
                x: 1920,
                ..CanvasOutputPlacement::default()
            },
        );
        let groups = BTreeMap::from([(
            "desk".to_owned(),
            CanvasOutputGroup {
                canvas: "main".to_owned(),
                primary: "eDP-1".to_owned(),
                members,
            },
        )]);

        assert!(settings_with(groups).validate().is_ok());
    }

    #[test]
    fn rejects_an_output_assigned_to_two_groups() {
        let member = BTreeMap::from([("eDP-1".to_owned(), CanvasOutputPlacement::default())]);
        let groups = BTreeMap::from([
            (
                "one".to_owned(),
                CanvasOutputGroup {
                    canvas: "one".to_owned(),
                    primary: "eDP-1".to_owned(),
                    members: member.clone(),
                },
            ),
            (
                "two".to_owned(),
                CanvasOutputGroup {
                    canvas: "two".to_owned(),
                    primary: "eDP-1".to_owned(),
                    members: member,
                },
            ),
        ]);

        assert!(settings_with(groups).validate().is_err());
    }

    #[test]
    fn rejects_an_out_of_range_grid_size() {
        let mut settings = CanvasSettings {
            grid_size: 0,
            ..CanvasSettings::default()
        };
        assert_eq!(
            settings.validate().unwrap_err(),
            "grid_size must be between 1 and 4096"
        );

        settings.grid_size = 4097;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn rejects_a_primary_output_outside_its_group() {
        let groups = BTreeMap::from([(
            "desk".to_owned(),
            CanvasOutputGroup {
                canvas: "main".to_owned(),
                primary: "DP-1".to_owned(),
                members: BTreeMap::from([("eDP-1".to_owned(), CanvasOutputPlacement::default())]),
            },
        )]);

        assert_eq!(
            settings_with(groups).validate().unwrap_err(),
            "primary output \"DP-1\" is not a member of output group \"desk\""
        );
    }
}

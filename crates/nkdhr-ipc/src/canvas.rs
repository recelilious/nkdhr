use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Persisted COMP-5 output-group configuration.
///
/// The map keys are stable, user-chosen group names. Each group selects one
/// canvas and contains the physical outputs that form its rigid viewport.
/// Maps are used instead of arrays so every setting remains addressable by
/// CTRL-5's dotted-key API (`canvas.outputs.<group>...`).
pub type CanvasOutputGroups = BTreeMap<String, CanvasOutputGroup>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasOutputGroup {
    /// The canvas this output group views.
    pub canvas: String,
    /// Connector name to its fixed position within the group.
    pub members: BTreeMap<String, CanvasOutputPlacement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CanvasOutputPlacement {
    /// Logical x offset within the output group's rigid arrangement.
    pub x: i32,
    /// Logical y offset within the output group's rigid arrangement.
    pub y: i32,
    /// Logical scale advertised for this output. COMP-6 wires the matching
    /// fractional-scale protocol; COMP-5 already needs the value to derive
    /// output geometry without changing the config shape later.
    pub scale: f64,
}

impl Default for CanvasOutputPlacement {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            scale: 1.0,
        }
    }
}

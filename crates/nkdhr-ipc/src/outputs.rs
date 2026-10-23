use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The persisted multi-output arrangement: the value of CTRL-5's `outputs`
/// namespace (`~/.config/nkdhr/outputs.toml`), keyed by connector name as
/// the kernel reports it (`eDP-1`, `DP-2`, `HDMI-A-1`, ...).
///
/// A map rather than a list so the file is one `[<connector>]` table per
/// output and every setting stays addressable through CTRL-5's dotted keys
/// as `outputs.<connector>.<field>`. The map only needs to name the outputs
/// whose placement the user wants to fix: a connected output without an
/// entry is not an error, and the compositor places it automatically.
///
/// `nkdhrd` validates this value (it registers the namespace's schema); it
/// lives in this crate because the compositor decodes the same shape from
/// `Config1.GetAll("outputs")`.
pub type OutputPlacements = BTreeMap<String, OutputPlacement>;

/// Where one output sits in the compositor's layout, and at what scale.
///
/// Every field has a default, so an entry may omit any of them; an unknown
/// field is rejected rather than ignored, so a typo surfaces as a
/// validation error instead of silently doing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OutputPlacement {
    /// Horizontal position of the output's top-left corner in the
    /// compositor-global logical coordinate space: the one space that
    /// `wl_output` positions and the pointer share. Only the arrangement
    /// relative to the other outputs matters, since the compositor shifts
    /// the resolved layout so its bounding box starts at (0, 0); negative
    /// values are therefore as valid as positive ones.
    pub x: i32,
    /// Vertical position of the output's top-left corner, in the same space
    /// as [`x`](Self::x).
    pub y: i32,
    /// The output's logical scale: physical pixels per logical pixel. An
    /// output's logical size, and so the room it takes up in the layout, is
    /// its mode size divided by this value. Fractional values are allowed
    /// and are advertised to clients through the fractional-scale protocol.
    pub scale: f64,
}

impl Default for OutputPlacement {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            scale: 1.0,
        }
    }
}

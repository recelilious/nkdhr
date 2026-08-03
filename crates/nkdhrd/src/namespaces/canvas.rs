use serde::{Deserialize, Serialize};

use crate::backends::config_store::Namespace;

/// COMP-3's compositor-level keybindings. Values are xkbcommon key names
/// (e.g. `"q"`, `"Tab"`, `"F4"` — the same names `xkb::keysym_from_name`
/// accepts), each combined with a fixed modifier `nkdhr-canvas` itself
/// decides (Super for `close_window`, Alt for `cycle_focus`) — only the
/// key is configurable here, not the modifier, since a single fixed
/// modifier per action is enough to prove hot-reload works end to end and
/// more general modifier+key combos aren't needed by anything yet.
///
/// `nkdhrd` deliberately does not depend on `xkbcommon` to validate that a
/// value is a *real* key name — that's real keyboard-layout domain
/// knowledge belonging to `nkdhr-canvas`, not the config-store daemon.
/// `nkdhrd` only rejects what it can judge for itself (a value can't be
/// empty); an unrecognized key name still round-trips through the store
/// correctly, and `nkdhr-canvas` logs and falls back to its own built-in
/// default rather than erroring, the same way an absent field falls back
/// to *this* struct's `Default`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CanvasKeybindings {
    pub close_window: String,
    pub cycle_focus: String,
}

impl Default for CanvasKeybindings {
    fn default() -> Self {
        Self {
            close_window: "q".to_owned(),
            cycle_focus: "Tab".to_owned(),
        }
    }
}

impl Namespace for CanvasKeybindings {
    const NAME: &'static str = "canvas";

    fn validate(&self) -> Result<(), String> {
        if self.close_window.trim().is_empty() {
            return Err("close_window must not be empty".to_owned());
        }
        if self.cycle_focus.trim().is_empty() {
            return Err("cycle_focus must not be empty".to_owned());
        }
        Ok(())
    }
}

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
/// `marks` is a compact, `nkdhr-canvas`-owned encoding
/// (`"<index>:<x>,<y>;..."`, one entry per set mark, digits 0-9) rather
/// than a nested table — CTRL-5's `Config1.Get`/`Set` only support scalar
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
    pub marks: String,
}

impl Default for CanvasSettings {
    fn default() -> Self {
        Self {
            close_window: "q".to_owned(),
            cycle_focus: "Tab".to_owned(),
            overview: "o".to_owned(),
            marks: String::new(),
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
        Ok(())
    }
}

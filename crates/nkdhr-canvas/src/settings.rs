use std::sync::{Arc, Mutex};
use std::thread;

use nkdhr_ipc::ConfigProxyBlocking;
use smithay::input::keyboard::{Keysym, xkb};
use zbus::blocking::Connection;
use zbus::zvariant::Value;

const DEFAULT_GRID_SIZE: f64 = 32.0;
const MAX_GRID_SIZE: u64 = 4096;

/// Compositor-level keybindings resolved to real xkbcommon keysyms.
#[derive(Debug, Clone, Copy)]
pub struct Keybindings {
    pub close_window: Keysym,
    pub cycle_focus: Keysym,
    pub overview: Keysym,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            close_window: xkb::keysym_from_name("q", xkb::KEYSYM_NO_FLAGS),
            cycle_focus: xkb::keysym_from_name("Tab", xkb::KEYSYM_NO_FLAGS),
            overview: xkb::keysym_from_name("o", xkb::KEYSYM_NO_FLAGS),
        }
    }
}

/// World-coordinate grid used by window placement, interactive geometry,
/// and the work viewport's primary-output anchor. It does not apply to
/// pinned nodes or the transient overview camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSettings {
    pub enabled: bool,
    pub size: f64,
}

impl Default for GridSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            size: DEFAULT_GRID_SIZE,
        }
    }
}

impl GridSettings {
    pub fn snap(self, coordinate: f64) -> f64 {
        if self.enabled {
            (coordinate / self.size).round() * self.size
        } else {
            coordinate
        }
    }

    pub fn cascade_coordinate(self, index: usize) -> f64 {
        if self.enabled {
            self.snap(100.0) + self.size * (index % 10) as f64
        } else {
            100.0 + 40.0 * (index % 10) as f64
        }
    }
}

/// The hot-reloadable subset of the CTRL-5 `canvas` namespace consumed by
/// the input and window-placement paths.
#[derive(Debug, Clone, Copy, Default)]
pub struct InteractionSettings {
    pub keybindings: Keybindings,
    pub grid: GridSettings,
}

/// Reads interaction settings once and watches their `Config1.Changed`
/// leaves. A missing daemon degrades to the canonical built-in defaults.
pub fn watch() -> Arc<Mutex<InteractionSettings>> {
    let current = Arc::new(Mutex::new(InteractionSettings::default()));

    let Ok(connection) = Connection::session() else {
        eprintln!("nkdhr-canvas: no session D-Bus, using built-in interaction settings");
        return current;
    };

    *current.lock().unwrap() = fetch(&connection);

    let watched = Arc::clone(&current);
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
            if is_interaction_key(args.key()) {
                let updated = fetch(&connection);
                *watched.lock().unwrap() = updated;
                println!("nkdhr-canvas: interaction settings reloaded: {updated:?}");
            }
        }
    });

    current
}

fn is_interaction_key(key: &str) -> bool {
    matches!(
        key,
        "canvas.close_window"
            | "canvas.cycle_focus"
            | "canvas.overview"
            | "canvas.snap_to_grid"
            | "canvas.grid_size"
    )
}

fn fetch(connection: &Connection) -> InteractionSettings {
    let defaults = InteractionSettings::default();
    let Ok(config) = ConfigProxyBlocking::new(connection) else {
        return defaults;
    };
    InteractionSettings {
        keybindings: Keybindings {
            close_window: fetch_key(
                &config,
                "canvas.close_window",
                defaults.keybindings.close_window,
            ),
            cycle_focus: fetch_key(
                &config,
                "canvas.cycle_focus",
                defaults.keybindings.cycle_focus,
            ),
            overview: fetch_key(&config, "canvas.overview", defaults.keybindings.overview),
        },
        grid: GridSettings {
            enabled: fetch_bool(&config, "canvas.snap_to_grid", defaults.grid.enabled),
            size: fetch_grid_size(&config, defaults.grid.size),
        },
    }
}

fn fetch_key(config: &ConfigProxyBlocking<'_>, key: &str, fallback: Keysym) -> Keysym {
    let Ok(owned) = config.get(key) else {
        return fallback;
    };
    let Value::Str(name) = Value::from(owned) else {
        return fallback;
    };
    let sym = xkb::keysym_from_name(name.as_str(), xkb::KEYSYM_CASE_INSENSITIVE);
    if sym == Keysym::NoSymbol {
        eprintln!(
            "nkdhr-canvas: {key} = {name:?} is not a recognized key name, keeping the built-in default"
        );
        fallback
    } else {
        sym
    }
}

fn fetch_bool(config: &ConfigProxyBlocking<'_>, key: &str, fallback: bool) -> bool {
    let Ok(owned) = config.get(key) else {
        return fallback;
    };
    match Value::from(owned) {
        Value::Bool(value) => value,
        _ => fallback,
    }
}

fn fetch_grid_size(config: &ConfigProxyBlocking<'_>, fallback: f64) -> f64 {
    let Ok(owned) = config.get("canvas.grid_size") else {
        return fallback;
    };
    let value = match Value::from(owned) {
        Value::U8(value) => u64::from(value),
        Value::I16(value) => u64::try_from(value).unwrap_or_default(),
        Value::U16(value) => u64::from(value),
        Value::I32(value) => u64::try_from(value).unwrap_or_default(),
        Value::U32(value) => u64::from(value),
        Value::I64(value) => u64::try_from(value).unwrap_or_default(),
        Value::U64(value) => value,
        _ => return fallback,
    };
    if (1..=MAX_GRID_SIZE).contains(&value) {
        value as f64
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_snaps_positive_and_negative_coordinates() {
        let grid = GridSettings::default();
        assert_eq!(grid.snap(47.0), 32.0);
        assert_eq!(grid.snap(49.0), 64.0);
        assert_eq!(grid.snap(-47.0), -32.0);
        assert_eq!(grid.snap(-49.0), -64.0);
    }

    #[test]
    fn disabled_grid_preserves_exact_coordinates_and_legacy_cascade() {
        let grid = GridSettings {
            enabled: false,
            ..GridSettings::default()
        };
        assert_eq!(grid.snap(-47.25), -47.25);
        assert_eq!(grid.cascade_coordinate(3), 220.0);
    }

    #[test]
    fn reloads_only_for_interaction_leaves() {
        for key in [
            "canvas.close_window",
            "canvas.cycle_focus",
            "canvas.overview",
            "canvas.snap_to_grid",
            "canvas.grid_size",
        ] {
            assert!(is_interaction_key(key));
        }
        assert!(!is_interaction_key("canvas.marks"));
        assert!(!is_interaction_key(
            "canvas.outputs.desk.members.Virtual-1.x"
        ));
    }
}

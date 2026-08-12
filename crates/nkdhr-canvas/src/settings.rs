use std::sync::{Arc, Mutex};
use std::thread;

use nkdhr_ipc::ConfigProxyBlocking;
use nkdhr_ui::{
    ActionEnvironment, BindingDiagnostic, BindingRuntime, BindingSnapshot, DeviceClass,
    built_in_compositor_catalog, default_compositor_bindings,
};
use smithay::input::keyboard::{Keysym, xkb};
use zbus::blocking::Connection;
use zbus::zvariant::Value;

const DEFAULT_GRID_SIZE: f64 = 32.0;
const MAX_GRID_SIZE: u64 = 4096;

const DEFAULT_CLOSE_KEY: &str = "Escape";
const DEFAULT_CYCLE_KEY: &str = "Tab";
const DEFAULT_OVERVIEW_KEY: &str = "o";

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
#[derive(Debug)]
pub struct InteractionSettings {
    pub grid: GridSettings,
    bindings: BindingRuntime,
    binding_diagnostics: Arc<[BindingDiagnostic]>,
}

impl Default for InteractionSettings {
    fn default() -> Self {
        let catalog = built_in_compositor_catalog();
        let bindings = BindingRuntime::new(
            catalog,
            nested_environment(),
            default_compositor_bindings(DEFAULT_CLOSE_KEY, DEFAULT_CYCLE_KEY, DEFAULT_OVERVIEW_KEY),
        )
        .expect("built-in compositor bindings are valid");
        Self {
            grid: GridSettings::default(),
            binding_diagnostics: bindings.snapshot().diagnostics().to_vec().into(),
            bindings,
        }
    }
}

impl InteractionSettings {
    pub fn binding_snapshot(&self) -> Arc<BindingSnapshot> {
        self.bindings.snapshot()
    }

    pub fn binding_diagnostics(&self) -> &[BindingDiagnostic] {
        &self.binding_diagnostics
    }

    pub fn enable_tty_capabilities(&mut self) {
        let publication = self.bindings.set_environment(tty_environment());
        self.binding_diagnostics = publication.diagnostics;
    }

    fn reload_bindings(&mut self, config: &ConfigProxyBlocking<'_>) {
        let document = fetch_string(config, "canvas.bindings").unwrap_or_default();
        let publication = if document.trim().is_empty() {
            let close = fetch_legacy_key(config, "canvas.close_window", DEFAULT_CLOSE_KEY);
            let cycle = fetch_legacy_key(config, "canvas.cycle_focus", DEFAULT_CYCLE_KEY);
            let overview = fetch_legacy_key(config, "canvas.overview", DEFAULT_OVERVIEW_KEY);
            self.bindings
                .publish(default_compositor_bindings(close, cycle, overview))
        } else {
            self.bindings.publish_json(&document)
        };
        self.binding_diagnostics = Arc::clone(&publication.diagnostics);
        if publication.accepted {
            println!(
                "nkdhr-canvas: binding generation {} published with {} diagnostic(s)",
                publication.effective.generation(),
                publication.diagnostics.len()
            );
        } else {
            eprintln!(
                "nkdhr-canvas: rejected binding candidate; generation {} remains active: {:?}",
                publication.effective.generation(),
                publication.diagnostics
            );
        }
    }
}

/// Reads interaction settings once and watches their `Config1.Changed`
/// leaves. A missing daemon degrades to the canonical built-in defaults.
pub fn watch() -> Arc<Mutex<InteractionSettings>> {
    let current = Arc::new(Mutex::new(InteractionSettings::default()));

    let Ok(connection) = Connection::session() else {
        eprintln!("nkdhr-canvas: no session D-Bus, using built-in interaction settings");
        return current;
    };

    if let Ok(config) = ConfigProxyBlocking::new(&connection) {
        let mut settings = current.lock().unwrap();
        settings.grid = fetch_grid(&config);
        settings.reload_bindings(&config);
    }

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
                let mut settings = watched.lock().unwrap();
                if is_binding_key(args.key()) {
                    settings.reload_bindings(&config);
                } else {
                    settings.grid = fetch_grid(&config);
                    println!("nkdhr-canvas: grid settings reloaded: {:?}", settings.grid);
                }
            }
        }
    });

    current
}

fn is_interaction_key(key: &str) -> bool {
    matches!(
        key,
        "canvas.bindings"
            | "canvas.close_window"
            | "canvas.cycle_focus"
            | "canvas.overview"
            | "canvas.snap_to_grid"
            | "canvas.grid_size"
    )
}

fn is_binding_key(key: &str) -> bool {
    matches!(
        key,
        "canvas.bindings" | "canvas.close_window" | "canvas.cycle_focus" | "canvas.overview"
    )
}

fn fetch_grid(config: &ConfigProxyBlocking<'_>) -> GridSettings {
    let defaults = GridSettings::default();
    GridSettings {
        enabled: fetch_bool(config, "canvas.snap_to_grid", defaults.enabled),
        size: fetch_grid_size(config, defaults.size),
    }
}

fn fetch_string(config: &ConfigProxyBlocking<'_>, key: &str) -> Option<String> {
    let Ok(owned) = config.get(key) else {
        return None;
    };
    let Value::Str(name) = Value::from(owned) else {
        return None;
    };
    Some(name.to_string())
}

fn fetch_legacy_key(config: &ConfigProxyBlocking<'_>, key: &str, fallback: &'static str) -> String {
    let Some(name) = fetch_string(config, key) else {
        return fallback.to_owned();
    };
    let sym = xkb::keysym_from_name(&name, xkb::KEYSYM_CASE_INSENSITIVE);
    if sym == Keysym::NoSymbol {
        eprintln!(
            "nkdhr-canvas: {key} = {name:?} is not a recognized key name, keeping the built-in default"
        );
        fallback.to_owned()
    } else {
        xkb::keysym_get_name(sym)
    }
}

fn nested_environment() -> ActionEnvironment {
    ActionEnvironment::default()
        .with_device(DeviceClass::Keyboard)
        .with_device(DeviceClass::Mouse)
}

fn tty_environment() -> ActionEnvironment {
    nested_environment()
        .with_device(DeviceClass::Touchpad)
        .with_capability("tty-vt")
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
            "canvas.bindings",
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

    #[test]
    fn default_bindings_are_shared_with_the_toolkit_catalog() {
        let settings = InteractionSettings::default();
        let snapshot = settings.binding_snapshot();
        assert_eq!(snapshot.generation(), 1);
        assert!(snapshot.bindings().iter().any(|binding| {
            binding.id == "window-close"
                && binding.invocation.action.as_str() == "canvas.window.close"
        }));
        assert_eq!(
            settings
                .binding_diagnostics()
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code == nkdhr_ui::BindingDiagnosticCode::UnsupportedDevice
                })
                .count(),
            2
        );
    }
}

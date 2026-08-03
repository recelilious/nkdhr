use std::sync::{Arc, Mutex};
use std::thread;

use nkdhr_ipc::ConfigProxyBlocking;
use smithay::input::keyboard::{Keysym, xkb};
use zbus::blocking::Connection;
use zbus::zvariant::Value;

/// COMP-3's compositor-level keybindings, resolved to real xkbcommon
/// keysyms. Backed by CTRL-5's `canvas` namespace
/// (`nkdhrd/src/namespaces/canvas.rs` — the schema, and thus the
/// canonical defaults, lives there); this is nkdhr-canvas's own parsed,
/// hot-reloadable copy of it.
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

/// Connects to `nkdhrd`'s session-bus `Config1` interface, reads the
/// `canvas` namespace once, and spawns a background thread that watches
/// `Config1.Changed` for further updates — this is what makes
/// `nkdhrctl config set canvas.<field> <value>` take effect in a running
/// nkdhr-canvas without a restart. If `nkdhrd` isn't reachable at
/// startup, falls back to the built-in defaults rather than failing the
/// whole compositor over an optional integration — the same "log and
/// degrade, don't crash" treatment CTRL-2's Brightness module gives a
/// missing backlight device.
pub fn watch() -> Arc<Mutex<Keybindings>> {
    let current = Arc::new(Mutex::new(Keybindings::default()));

    let Ok(connection) = Connection::session() else {
        eprintln!("nkdhr-canvas: no session D-Bus, using built-in default keybindings");
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
            if args.key().starts_with("canvas.") {
                let updated = fetch(&connection);
                *watched.lock().unwrap() = updated;
                println!("nkdhr-canvas: keybindings reloaded: {updated:?}");
            }
        }
    });

    current
}

fn fetch(connection: &Connection) -> Keybindings {
    let defaults = Keybindings::default();
    let Ok(config) = ConfigProxyBlocking::new(connection) else {
        return defaults;
    };
    Keybindings {
        close_window: fetch_key(&config, "canvas.close_window", defaults.close_window),
        cycle_focus: fetch_key(&config, "canvas.cycle_focus", defaults.cycle_focus),
        overview: fetch_key(&config, "canvas.overview", defaults.overview),
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

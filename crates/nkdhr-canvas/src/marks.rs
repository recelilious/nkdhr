use std::collections::HashMap;

use nkdhr_ipc::ConfigProxyBlocking;
use smithay::utils::Point;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use crate::world::World;

/// Vim-style world-position bookmarks (ROADMAP.md §2.3): digit `0`-`9` ->
/// the world point recorded there. Loaded once at startup and saved
/// (via [`save`]) whenever `main.rs` sets one — unlike
/// [`crate::keybindings`], marks have no background hot-reload watcher:
/// nothing outside `nkdhr-canvas` itself needs to change them live, so a
/// load-once-and-write-through model is simpler and sufficient.
pub type Marks = HashMap<u8, Point<f64, World>>;

/// Reads CTRL-5's `canvas.marks` (see `nkdhrd/src/namespaces/canvas.rs`
/// for the on-disk encoding and why it's a plain string) into a live
/// [`Marks`] map. Falls back to an empty map — no marks set — if
/// `nkdhrd` isn't reachable or the stored value is unparsable, rather
/// than failing compositor startup over it; the same "log and degrade"
/// treatment `crate::keybindings::watch` gives a missing session bus.
pub fn load() -> Marks {
    let Ok(connection) = Connection::session() else {
        eprintln!("nkdhr-canvas: no session D-Bus, starting with no marks");
        return Marks::new();
    };
    let Ok(config) = ConfigProxyBlocking::new(&connection) else {
        return Marks::new();
    };
    let Ok(owned) = config.get("canvas.marks") else {
        return Marks::new();
    };
    let Value::Str(text) = Value::from(owned) else {
        return Marks::new();
    };
    parse(text.as_str())
}

/// Persists `marks` to CTRL-5 as a single `canvas.marks` string, so the
/// next [`load`] (typically the next `nkdhr-canvas` startup) sees them.
/// Best-effort: logs and gives up on failure rather than panicking —
/// losing a just-set mark to a transient D-Bus problem shouldn't take the
/// compositor down with it.
pub fn save(marks: &Marks) {
    let Ok(connection) = Connection::session() else {
        return;
    };
    let Ok(config) = ConfigProxyBlocking::new(&connection) else {
        return;
    };
    let text = format(marks);
    if let Err(err) = config.set("canvas.marks", Value::from(text.as_str())) {
        eprintln!("nkdhr-canvas: failed to persist marks: {err}");
    }
}

/// `"<index>:<x>,<y>;..."`, one entry per set mark. Deliberately a
/// hand-rolled format, not e.g. `serde_json` — a single flat string with
/// no nested structure doesn't need a general-purpose serializer, and
/// pulling one in would be for exactly one call site.
fn parse(text: &str) -> Marks {
    text.split(';')
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let (index, coords) = entry.split_once(':')?;
            let (x, y) = coords.split_once(',')?;
            let point = (x.parse().ok()?, y.parse().ok()?).into();
            Some((index.parse().ok()?, point))
        })
        .collect()
}

fn format(marks: &Marks) -> String {
    let mut entries: Vec<_> = marks.iter().collect();
    entries.sort_by_key(|(index, _)| **index);
    entries
        .into_iter()
        .map(|(index, point)| format!("{index}:{},{}", point.x, point.y))
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_encoded_string() {
        let mut marks = Marks::new();
        marks.insert(3, (120.5, -80.25).into());
        marks.insert(0, (0.0, 0.0).into());

        let encoded = format(&marks);
        let decoded = parse(&encoded);

        assert_eq!(decoded, marks);
    }

    #[test]
    fn empty_marks_encode_to_an_empty_string() {
        assert_eq!(format(&Marks::new()), "");
        assert_eq!(parse(""), Marks::new());
    }

    #[test]
    fn ignores_unparsable_entries_rather_than_erroring() {
        let decoded = parse("not-a-valid-entry;3:1.0,2.0");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded.get(&3), Some(&(1.0, 2.0).into()));
    }
}

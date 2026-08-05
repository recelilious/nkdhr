use std::collections::{BTreeMap, HashMap};

use nkdhr_ipc::ConfigProxyBlocking;
use smithay::utils::Point;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use super::world::World;

/// Vim-style world-position bookmarks (ROADMAP.md §2.3): digit `0`-`9` ->
/// the world point recorded there. Loaded once at startup and saved
/// (via [`save`]) whenever `main.rs` sets one — unlike
/// [`crate::settings`], marks have no background hot-reload watcher:
/// nothing outside `nkdhr-canvas` itself needs to change them live, so a
/// load-once-and-write-through model is simpler and sufficient.
pub type Marks = HashMap<u8, Point<f64, World>>;

/// Position marks belong to a canvas, not to an output group. Multiple
/// groups bound to the same canvas therefore see the same marks.
pub type CanvasMarks = BTreeMap<String, Marks>;

/// Reads CTRL-5's `canvas.marks` (see `nkdhrd/src/namespaces/canvas.rs`
/// for the on-disk encoding and why it's a plain string) into a live
/// [`Marks`] map. Falls back to an empty map — no marks set — if
/// `nkdhrd` isn't reachable or the stored value is unparsable, rather
/// than failing compositor startup over it; the same "log and degrade"
/// treatment `crate::settings::watch` gives a missing session bus.
pub fn load() -> CanvasMarks {
    let Ok(connection) = Connection::session() else {
        eprintln!("nkdhr-canvas: no session D-Bus, starting with no marks");
        return CanvasMarks::new();
    };
    let Ok(config) = ConfigProxyBlocking::new(&connection) else {
        return CanvasMarks::new();
    };
    let Ok(owned) = config.get("canvas.marks") else {
        return CanvasMarks::new();
    };
    let Value::Str(text) = Value::from(owned) else {
        return CanvasMarks::new();
    };
    parse(text.as_str())
}

/// Persists `marks` to CTRL-5 as a single `canvas.marks` string, so the
/// next [`load`] (typically the next `nkdhr-canvas` startup) sees them.
/// Best-effort: logs and gives up on failure rather than panicking —
/// losing a just-set mark to a transient D-Bus problem shouldn't take the
/// compositor down with it.
pub fn save(marks: &CanvasMarks) {
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

/// Version 2 is `"v2;<hex-canvas>:<index>:<x>,<y>;..."`. Hex-encoding the
/// UTF-8 canvas name makes every otherwise-valid config name unambiguous
/// without another serializer dependency. The COMP-4 single-canvas
/// `"<index>:<x>,<y>;..."` format remains accepted as `default`.
fn parse(text: &str) -> CanvasMarks {
    if let Some(entries) = text.strip_prefix("v2;") {
        let mut canvases = CanvasMarks::new();
        for entry in entries.split(';').filter(|entry| !entry.is_empty()) {
            let Some((canvas, rest)) = entry.split_once(':') else {
                continue;
            };
            let Some((index, coords)) = rest.split_once(':') else {
                continue;
            };
            let Some((x, y)) = coords.split_once(',') else {
                continue;
            };
            let Some(canvas) = decode_hex(canvas) else {
                continue;
            };
            let (Ok(index), Ok(x), Ok(y)) = (index.parse(), x.parse(), y.parse()) else {
                continue;
            };
            canvases
                .entry(canvas)
                .or_default()
                .insert(index, (x, y).into());
        }
        canvases
    } else {
        let marks = parse_legacy(text);
        if marks.is_empty() {
            CanvasMarks::new()
        } else {
            CanvasMarks::from([("default".to_owned(), marks)])
        }
    }
}

fn parse_legacy(text: &str) -> Marks {
    text.split(';')
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let (index, coords) = entry.split_once(':')?;
            let (x, y) = coords.split_once(',')?;
            Some((
                index.parse().ok()?,
                (x.parse().ok()?, y.parse().ok()?).into(),
            ))
        })
        .collect()
}

fn format(canvases: &CanvasMarks) -> String {
    let entries = canvases
        .iter()
        .flat_map(|(canvas, marks)| {
            let mut marks = marks.iter().collect::<Vec<_>>();
            marks.sort_by_key(|(index, _)| **index);
            let canvas = encode_hex(canvas);
            marks
                .into_iter()
                .map(move |(index, point)| format!("{canvas}:{index}:{},{}", point.x, point.y))
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        String::new()
    } else {
        format!("v2;{}", entries.join(";"))
    }
}

fn encode_hex(text: &str) -> String {
    text.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_hex(encoded: &str) -> Option<String> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_encoded_string() {
        let marks = CanvasMarks::from([
            (
                "main:work/世界".to_owned(),
                Marks::from([(3, (120.5, -80.25).into()), (0, (0.0, 0.0).into())]),
            ),
            ("chat".to_owned(), Marks::from([(7, (-4.0, 9.5).into())])),
        ]);

        let encoded = format(&marks);
        let decoded = parse(&encoded);

        assert_eq!(decoded, marks);
    }

    #[test]
    fn empty_marks_encode_to_an_empty_string() {
        assert_eq!(format(&CanvasMarks::new()), "");
        assert_eq!(parse(""), CanvasMarks::new());
    }

    #[test]
    fn loads_legacy_marks_into_the_default_canvas() {
        let decoded = parse("not-a-valid-entry;3:1.0,2.0");
        assert_eq!(decoded.len(), 1);
        assert_eq!(
            decoded.get("default").and_then(|marks| marks.get(&3)),
            Some(&(1.0, 2.0).into())
        );
    }

    #[test]
    fn ignores_unparsable_version_two_entries() {
        let decoded = parse("v2;zz:bad;64656661756c74:3:1.0,2.0");
        assert_eq!(
            decoded.get("default").and_then(|marks| marks.get(&3)),
            Some(&(1.0, 2.0).into())
        );
    }
}

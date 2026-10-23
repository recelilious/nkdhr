use nkdhr_ipc::OutputPlacements;
use serde::{Deserialize, Serialize};

use crate::backends::config_store::Namespace;

/// The `outputs` namespace: where the compositor places each output and at
/// what scale, keyed by connector name.
///
/// The root of `outputs.toml` is the connector map itself, one table per
/// configured output and no wrapper table around them:
///
/// ```toml
/// [eDP-1]
/// x = 0
/// y = 0
/// scale = 1.25
/// ```
///
/// `#[serde(transparent)]` gives exactly that shape and keeps every value
/// addressable as `outputs.<connector>.<field>`. The strictness the store
/// relies on still holds one level down: [`nkdhr_ipc::OutputPlacement`] is
/// `deny_unknown_fields` and fills absent fields from its defaults, so a
/// misspelled field is rejected and a bare `[DP-1]` table is a complete
/// entry. The default is an empty map, because a connected output without
/// an entry is not a problem — the compositor places it automatically.
///
/// `Config1.Set` only overwrites values that already exist, so it adjusts a
/// connector the file already names but never adds one. A new connector is
/// added by editing the file, which the store's watcher picks up
/// immediately.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutputsSettings(pub OutputPlacements);

impl Namespace for OutputsSettings {
    const NAME: &'static str = "outputs";

    /// Rejects what would make an entry unaddressable or meaningless.
    /// Connector names are not checked against the connectors that exist:
    /// configuring an output that is unplugged right now is normal, and
    /// only the compositor knows what is connected.
    fn validate(&self) -> Result<(), String> {
        for (connector, placement) in &self.0 {
            if connector.trim().is_empty() {
                return Err("output connector name must not be empty".to_owned());
            }
            // A dot would split the connector name across two segments of
            // the dotted key, making the entry's values unreachable through
            // `Config1.Get`/`Set`.
            if connector.contains('.') {
                return Err(format!(
                    "output connector name {connector:?} must not contain '.', \
                     which separates config key segments"
                ));
            }
            if !placement.scale.is_finite() || placement.scale <= 0.0 {
                return Err(format!(
                    "output {connector:?} has invalid scale {}; \
                     scale must be a finite number greater than 0",
                    placement.scale
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::PathBuf;

    use nkdhr_ipc::OutputPlacement;
    use serde_json::Value as Json;
    use zbus::zvariant::Value;

    use super::*;
    use crate::backends::config_store::{
        ConfigError, ConfigStore, NamespaceSchema, json_to_variant,
    };

    static SCHEMAS: &[NamespaceSchema] = &[NamespaceSchema::of::<OutputsSettings>()];

    /// A laptop panel at scale 1.25 and an external monitor to its right. The
    /// external monitor's entry is sparse on purpose: the store has to fill
    /// in its `y` and `scale`.
    const LAPTOP_AND_MONITOR: &str = "\
[eDP-1]
x = 0
y = 0
scale = 1.25

[DP-1]
x = 1536
";

    fn arrangement(entries: &[(&str, OutputPlacement)]) -> OutputsSettings {
        OutputsSettings(
            entries
                .iter()
                .map(|(connector, placement)| ((*connector).to_owned(), *placement))
                .collect(),
        )
    }

    /// A config directory owned by one test: named after the test and the
    /// process, so neither parallel tests nor concurrent `cargo test` runs
    /// share one, and removed again on drop, even when the test fails.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(test: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("nkdhrd-outputs-{test}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }

        fn with_file(test: &str, contents: &str) -> Self {
            let dir = Self::new(test);
            fs::create_dir_all(&dir.0).unwrap();
            fs::write(dir.file(), contents).unwrap();
            dir
        }

        fn file(&self) -> PathBuf {
            self.0.join("outputs.toml")
        }

        fn open_store(&self) -> ConfigStore {
            ConfigStore::open(self.0.clone(), SCHEMAS).unwrap()
        }

        fn read_file(&self) -> toml::Table {
            toml::from_str(&fs::read_to_string(self.file()).unwrap()).unwrap()
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn keys(values: &HashMap<String, Json>) -> HashSet<&str> {
        values.keys().map(String::as_str).collect()
    }

    #[test]
    fn accepts_a_side_by_side_arrangement() {
        let settings = arrangement(&[
            (
                "eDP-1",
                OutputPlacement {
                    scale: 1.25,
                    ..OutputPlacement::default()
                },
            ),
            (
                "DP-1",
                OutputPlacement {
                    x: -1920,
                    ..OutputPlacement::default()
                },
            ),
        ]);

        assert_eq!(settings.validate(), Ok(()));
    }

    #[test]
    fn rejects_an_empty_connector_name() {
        for connector in ["", "  "] {
            assert_eq!(
                arrangement(&[(connector, OutputPlacement::default())]).validate(),
                Err("output connector name must not be empty".to_owned()),
                "{connector:?} was accepted"
            );
        }
    }

    #[test]
    fn rejects_a_connector_name_containing_a_dot() {
        assert_eq!(
            arrangement(&[("DP-1.1", OutputPlacement::default())]).validate(),
            Err("output connector name \"DP-1.1\" must not contain '.', \
                 which separates config key segments"
                .to_owned())
        );
    }

    #[test]
    fn rejects_a_scale_that_is_not_a_positive_finite_number() {
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let settings = arrangement(&[(
                "eDP-1",
                OutputPlacement {
                    scale,
                    ..OutputPlacement::default()
                },
            )]);
            assert!(settings.validate().is_err(), "scale {scale} was accepted");
        }
    }

    #[test]
    fn a_missing_file_is_an_empty_arrangement() {
        let dir = ScratchDir::new("missing-file");
        let store = dir.open_store();

        assert!(store.get_all("outputs").unwrap().is_empty());
        assert!(store.get_all("").unwrap().is_empty());
        assert!(dir.read_file().is_empty());
    }

    #[test]
    fn the_file_holds_one_materialized_table_per_connector() {
        let dir = ScratchDir::with_file("file-shape", LAPTOP_AND_MONITOR);
        dir.open_store();

        let text = fs::read_to_string(dir.file()).unwrap();
        assert!(text.lines().any(|line| line == "[eDP-1]"), "{text}");
        assert!(text.lines().any(|line| line == "[DP-1]"), "{text}");

        let expected: toml::Table = toml::from_str(
            "[eDP-1]\nx = 0\ny = 0\nscale = 1.25\n\n[DP-1]\nx = 1536\ny = 0\nscale = 1.0\n",
        )
        .unwrap();
        assert_eq!(dir.read_file(), expected);
    }

    #[test]
    fn get_all_flattens_to_connector_and_field_keys() {
        let dir = ScratchDir::with_file("get-all", LAPTOP_AND_MONITOR);
        let store = dir.open_store();

        let all = store.get_all("outputs").unwrap();
        assert_eq!(
            keys(&all),
            HashSet::from([
                "outputs.eDP-1.x",
                "outputs.eDP-1.y",
                "outputs.eDP-1.scale",
                "outputs.DP-1.x",
                "outputs.DP-1.y",
                "outputs.DP-1.scale",
            ])
        );
        assert_eq!(all["outputs.eDP-1.scale"], Json::from(1.25));
        assert_eq!(all["outputs.DP-1.x"], Json::from(1536));
        assert_eq!(store.get_all("").unwrap(), all);

        let one_output = store.get_all("outputs.DP-1").unwrap();
        assert_eq!(
            keys(&one_output),
            HashSet::from(["outputs.DP-1.x", "outputs.DP-1.y", "outputs.DP-1.scale"])
        );

        // Every value crosses D-Bus as the type a client decodes it as.
        for (key, value) in &all {
            let variant = json_to_variant(value).unwrap();
            if key.ends_with(".scale") {
                assert!(matches!(variant, Value::F64(_)), "{key}: {variant:?}");
            } else {
                assert!(matches!(variant, Value::I64(_)), "{key}: {variant:?}");
            }
        }
    }

    #[test]
    fn get_reads_a_value_but_not_a_whole_entry() {
        let dir = ScratchDir::with_file("get", LAPTOP_AND_MONITOR);
        let store = dir.open_store();

        assert_eq!(store.get("outputs.eDP-1.scale").unwrap(), Json::from(1.25));
        assert!(matches!(
            store.get("outputs.eDP-1"),
            Err(ConfigError::NotALeaf(_))
        ));
        assert!(matches!(
            store.get("outputs.HDMI-A-1.x"),
            Err(ConfigError::UnknownKey(_))
        ));
    }

    #[test]
    fn set_overwrites_an_existing_value_and_persists_it() {
        let dir = ScratchDir::with_file("set", LAPTOP_AND_MONITOR);
        let store = dir.open_store();

        assert_eq!(
            store.set("outputs.DP-1.x", Json::from(-1920)).unwrap(),
            Json::from(-1920)
        );
        // An integer sent for the float field is stored as the float the
        // schema declares, which is what `Changed` then carries.
        let scale = store.set("outputs.eDP-1.scale", Json::from(2)).unwrap();
        assert_eq!(scale.as_f64(), Some(2.0));
        assert!(scale.is_f64());

        // A fresh store reads back what the first one wrote.
        drop(store);
        let reopened = dir.open_store();
        assert_eq!(reopened.get("outputs.DP-1.x").unwrap(), Json::from(-1920));
        assert_eq!(
            reopened.get("outputs.eDP-1.scale").unwrap(),
            Json::from(2.0)
        );
    }

    #[test]
    fn set_never_creates_a_connector_or_a_field() {
        let dir = ScratchDir::with_file("set-unknown", LAPTOP_AND_MONITOR);
        let store = dir.open_store();

        assert!(matches!(
            store.set("outputs.HDMI-A-1.x", Json::from(0)),
            Err(ConfigError::UnknownKey(_))
        ));
        assert!(matches!(
            store.set("outputs.eDP-1.rotation", Json::from(90)),
            Err(ConfigError::UnknownKey(_))
        ));
        assert_eq!(store.get_all("outputs").unwrap().len(), 6);
        assert!(!dir.read_file().contains_key("HDMI-A-1"));
    }

    #[test]
    fn set_rejects_an_invalid_value_and_keeps_the_last_good_one() {
        let dir = ScratchDir::with_file("set-invalid", LAPTOP_AND_MONITOR);
        let store = dir.open_store();

        assert!(matches!(
            store.set("outputs.eDP-1.scale", Json::from(0.0)),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(
            store.set("outputs.eDP-1.x", Json::from(1.5)),
            Err(ConfigError::Invalid(_))
        ));
        assert_eq!(store.get("outputs.eDP-1.scale").unwrap(), Json::from(1.25));
        assert_eq!(store.get("outputs.eDP-1.x").unwrap(), Json::from(0));
        assert_eq!(dir.read_file()["eDP-1"]["scale"].as_float(), Some(1.25));
    }

    #[test]
    fn an_external_edit_adds_a_connector_that_set_can_then_adjust() {
        let dir = ScratchDir::with_file("external-edit", LAPTOP_AND_MONITOR);
        let store = dir.open_store();

        fs::write(dir.file(), format!("{LAPTOP_AND_MONITOR}\n[HDMI-A-1]\n")).unwrap();
        let changed: HashMap<String, Json> = store.reload("outputs").unwrap().into_iter().collect();
        assert_eq!(
            changed,
            HashMap::from([
                ("outputs.HDMI-A-1.x".to_owned(), Json::from(0)),
                ("outputs.HDMI-A-1.y".to_owned(), Json::from(0)),
                ("outputs.HDMI-A-1.scale".to_owned(), Json::from(1.0)),
            ])
        );

        store.set("outputs.HDMI-A-1.x", Json::from(3456)).unwrap();
        assert_eq!(store.get("outputs.HDMI-A-1.x").unwrap(), Json::from(3456));
    }

    #[test]
    fn an_invalid_external_edit_keeps_the_last_good_arrangement() {
        let dir = ScratchDir::with_file("external-invalid", LAPTOP_AND_MONITOR);
        let store = dir.open_store();
        let before = store.get_all("outputs").unwrap();

        for edit in [
            "[eDP-1]\nrotation = 90\n",
            "[\"DP.1\"]\nx = 0\n",
            "[eDP-1]\nscale = -1.0\n",
        ] {
            fs::write(dir.file(), edit).unwrap();
            assert!(
                matches!(store.reload("outputs"), Err(ConfigError::Invalid(_))),
                "{edit:?} was accepted"
            );
            assert_eq!(store.get_all("outputs").unwrap(), before);
        }
    }
}

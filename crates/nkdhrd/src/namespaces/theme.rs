use nkdhr_theme::ThemeProfile;
use serde::{Deserialize, Serialize};

use crate::backends::config_store::Namespace;

/// UI-4's atomic CTRL-5 theme namespace.
///
/// The portable profile remains one JSON string leaf because a theme edit must
/// validate and publish as one transaction. It also keeps imported sparse
/// overrides, font-family arrays and future profile metadata intact while
/// CTRL-5's generic D-Bus leaf contract remains scalar-only. `theme.toml` is
/// still directly editable; the daemon parses the profile and resolves every
/// inherited token before accepting a write or external reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeSettings {
    pub profile: String,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            profile: serde_json::to_string(&ThemeProfile::default())
                .expect("the built-in default theme profile always serializes"),
        }
    }
}

impl Namespace for ThemeSettings {
    const NAME: &'static str = "theme";

    fn validate(&self) -> Result<(), String> {
        ThemeProfile::from_json(&self.profile)
            .and_then(|profile| profile.resolve())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use nkdhr_theme::{PaletteData, ThemeBase};
    use serde_json::json;

    use super::*;
    use crate::backends::config_store::{ConfigError, ConfigStore, NamespaceSchema};

    #[test]
    fn default_namespace_materializes_a_complete_profile() {
        let settings = ThemeSettings::default();
        settings.validate().unwrap();
        let resolved = ThemeProfile::from_json(&settings.profile)
            .unwrap()
            .resolve()
            .unwrap();
        assert_eq!(resolved.data.palette, PaletteData::tokyo_night());
    }

    #[test]
    fn rejects_an_invalid_import_as_one_atomic_leaf() {
        let profile = ThemeProfile {
            overrides: json!({"materials": {"content_surface": {"opacity": 2.0}}}),
            ..ThemeProfile::default()
        };
        let settings = ThemeSettings {
            profile: serde_json::to_string(&profile).unwrap(),
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn accepts_a_portable_frozen_wallpaper_palette() {
        let profile = ThemeProfile {
            id: "portable-wallpaper".into(),
            name: "Portable Wallpaper".into(),
            base: ThemeBase::Wallpaper {
                live: false,
                wallpaper_id: String::new(),
                frozen_palette: Box::new(PaletteData::nord()),
            },
            ..ThemeProfile::default()
        };
        ThemeSettings {
            profile: serde_json::to_string(&profile).unwrap(),
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn config_store_rejection_preserves_the_last_good_profile() {
        let dir = std::env::temp_dir().join(format!(
            "nkdhr-theme-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let schemas = Box::leak(vec![NamespaceSchema::of::<ThemeSettings>()].into_boxed_slice());
        let store = ConfigStore::open(dir.clone(), schemas).unwrap();

        let valid = ThemeProfile {
            overrides: json!({"palette": {"accent": "#010203ff"}}),
            ..ThemeProfile::default()
        };
        let valid = serde_json::to_string(&valid).unwrap();
        store
            .set("theme.profile", serde_json::Value::String(valid.clone()))
            .unwrap();

        let invalid = ThemeProfile {
            overrides: json!({"spacing": {"small": 100.0}}),
            ..ThemeProfile::default()
        };
        let invalid = serde_json::to_string(&invalid).unwrap();
        assert!(matches!(
            store.set("theme.profile", serde_json::Value::String(invalid)),
            Err(ConfigError::Invalid(_))
        ));
        assert_eq!(
            store.get("theme.profile").unwrap(),
            serde_json::Value::String(valid)
        );
        let _ = fs::remove_dir_all(dir);
    }
}

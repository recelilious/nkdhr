use nkdhr_theme::{MotionPresetLibraryData, ThemeProfile, ThemeProfileLibrary};
use serde::{Deserialize, Serialize};

use crate::backends::config_store::Namespace;

/// UI-4's atomic CTRL-5 theme namespace.
///
/// The active portable profile, saved-profile library and UI-7 motion-preset
/// library each remain one JSON string leaf because each edit must validate
/// and publish as one transaction. This also keeps imported sparse overrides,
/// font-family arrays and future profile metadata intact while CTRL-5's generic
/// D-Bus leaf contract remains scalar-only. `theme.toml` is still directly
/// editable; the daemon validates all three leaves before accepting a write or
/// external reload. UI runtime compilation adds the executable curve checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeSettings {
    pub profile: String,
    pub library: String,
    pub motion_library: String,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            profile: serde_json::to_string(&ThemeProfile::default())
                .expect("the built-in default theme profile always serializes"),
            library: serde_json::to_string(&ThemeProfileLibrary::default())
                .expect("the empty theme profile library always serializes"),
            motion_library: serde_json::to_string(&MotionPresetLibraryData::default())
                .expect("the empty motion preset library always serializes"),
        }
    }
}

impl Namespace for ThemeSettings {
    const NAME: &'static str = "theme";

    fn validate(&self) -> Result<(), String> {
        ThemeProfile::from_json(&self.profile)
            .and_then(|profile| profile.resolve())
            .map_err(|error| error.to_string())?;
        ThemeProfileLibrary::from_json(&self.library).map_err(|error| error.to_string())?;
        MotionPresetLibraryData::from_json(&self.motion_library)
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
    fn older_theme_file_receives_both_empty_library_defaults() {
        let profile = serde_json::to_string(&ThemeProfile::default()).unwrap();
        let legacy = format!("profile = '{}'\n", profile.replace('\'', "''"));
        let settings: ThemeSettings = toml::from_str(&legacy).unwrap();
        assert_eq!(
            ThemeProfileLibrary::from_json(&settings.library).unwrap(),
            ThemeProfileLibrary::default()
        );
        assert_eq!(
            MotionPresetLibraryData::from_json(&settings.motion_library).unwrap(),
            MotionPresetLibraryData::default()
        );
        settings.validate().unwrap();
    }

    #[test]
    fn rejects_an_invalid_import_as_one_atomic_leaf() {
        let profile = ThemeProfile {
            overrides: json!({"materials": {"content_surface": {"opacity": 2.0}}}),
            ..ThemeProfile::default()
        };
        let settings = ThemeSettings {
            profile: serde_json::to_string(&profile).unwrap(),
            ..ThemeSettings::default()
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
            ..ThemeSettings::default()
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

    #[test]
    fn config_store_rejection_preserves_the_last_good_library() {
        let dir = std::env::temp_dir().join(format!(
            "nkdhr-theme-library-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let schemas = Box::leak(vec![NamespaceSchema::of::<ThemeSettings>()].into_boxed_slice());
        let store = ConfigStore::open(dir.clone(), schemas).unwrap();

        let mut valid = ThemeProfileLibrary::default();
        valid
            .save(ThemeProfile {
                id: "saved".into(),
                name: "Saved".into(),
                ..ThemeProfile::default()
            })
            .unwrap();
        let valid = valid.to_json().unwrap();
        store
            .set("theme.library", serde_json::Value::String(valid.clone()))
            .unwrap();

        let invalid = r#"{"schema_version":1,"profiles":[{"broken":true}]}"#;
        assert!(matches!(
            store.set("theme.library", serde_json::Value::String(invalid.into())),
            Err(ConfigError::Invalid(_))
        ));
        assert_eq!(
            store.get("theme.library").unwrap(),
            serde_json::Value::String(valid)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_store_rejection_preserves_the_last_good_motion_library() {
        let dir = std::env::temp_dir().join(format!(
            "nkdhr-motion-library-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let schemas = Box::leak(vec![NamespaceSchema::of::<ThemeSettings>()].into_boxed_slice());
        let store = ConfigStore::open(dir.clone(), schemas).unwrap();

        let mut valid = MotionPresetLibraryData::default();
        valid
            .insert(
                nkdhr_theme::MotionStylePresetData::built_in(
                    nkdhr_theme::BuiltInMotionStyle::Balanced,
                    nkdhr_theme::BALANCED_MOTION_STYLE_REVISION,
                )
                .unwrap(),
            )
            .unwrap();
        let valid = valid.to_json().unwrap();
        store
            .set(
                "theme.motion_library",
                serde_json::Value::String(valid.clone()),
            )
            .unwrap();

        let invalid = r#"{"schema_version":1,"presets":[{"broken":true}]}"#;
        assert!(matches!(
            store.set(
                "theme.motion_library",
                serde_json::Value::String(invalid.into())
            ),
            Err(ConfigError::Invalid(_))
        ));
        assert_eq!(
            store.get("theme.motion_library").unwrap(),
            serde_json::Value::String(valid)
        );
        let _ = fs::remove_dir_all(dir);
    }
}

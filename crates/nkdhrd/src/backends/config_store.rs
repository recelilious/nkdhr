//! The generic engine behind CTRL-5, the config store: one schema-validated
//! TOML file per namespace under a config directory, get/set/flatten by
//! dotted key, and change detection for both `nkdhrd`'s own writes and
//! external edits to the files.
//!
//! CTRL-5 ships this engine with **zero namespaces registered** — see
//! `docs-staging/control-plane/INTERNALS.md`'s "Config store" section for
//! why: none of CTRL-1 … CTRL-4's modules have a setting that actually
//! needs persisting yet, and the ones sketched in USAGE.md (`theme`,
//! `canvas`) belong to phases (UI-4, COMP-3) that haven't designed their
//! real schemas. A later phase registers its own namespace by implementing
//! [`Namespace`] on a `serde`-derived struct and adding a
//! [`NamespaceSchema::of`] entry to the list it passes to
//! [`ConfigStore::open`] in `nkdhrd/src/main.rs`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use zbus::zvariant::Value;

/// A schema for one config namespace (one `<name>.toml` file). Implement
/// this on a `serde`-derived struct with `#[serde(deny_unknown_fields,
/// default)]` so an unrecognized key is rejected rather than silently
/// ignored and an absent one falls back to its default, then register it
/// via [`NamespaceSchema::of`].
///
/// `#[allow(dead_code)]` on this trait, [`NamespaceSchema::of`] and
/// [`parse_and_validate`]: with `NAMESPACES` empty (see this module's doc
/// comment), nothing in `nkdhrd` outside of tests calls them yet — that's
/// expected, not a sign any of this is actually unused.
#[allow(dead_code)]
pub trait Namespace: Default + Serialize + DeserializeOwned {
    /// The namespace name: the file's stem (`theme` for `theme.toml`) and
    /// the first dotted-key segment (`theme.accent-color`).
    const NAME: &'static str;

    /// Cross-field validation `#[serde(deny_unknown_fields)]` can't express
    /// (ranges, enums encoded as strings, mutual constraints, ...). Runs
    /// after every successful deserialize, both for [`ConfigStore::set`]
    /// and for re-validating an external file edit
    /// ([`ConfigStore::reload`]).
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A type-erased registry entry, produced from a concrete [`Namespace`] by
/// [`NamespaceSchema::of`]. Type-erased because [`ConfigStore`] holds one
/// flat list spanning every namespace a running `nkdhrd` registers, which
/// can't share a single concrete `Namespace` type.
pub struct NamespaceSchema {
    pub name: &'static str,
    parse: fn(&Json) -> Result<Json, String>,
}

impl NamespaceSchema {
    #[allow(dead_code)]
    pub fn of<T: Namespace>() -> Self {
        Self {
            name: T::NAME,
            parse: parse_and_validate::<T>,
        }
    }
}

/// Deserializes `value` as `T`, validates it, then re-serializes. The round
/// trip through `T` is what supplies defaults for absent fields and turns
/// unrecognized ones into an error (`deny_unknown_fields`), so the [`Json`]
/// a caller stores afterwards is always the *materialized* form — every
/// field present, nothing extra — never a sparse diff of whatever partial
/// table triggered this call.
#[allow(dead_code)]
fn parse_and_validate<T: Namespace>(value: &Json) -> Result<Json, String> {
    let parsed: T = serde_json::from_value(value.clone()).map_err(|err| err.to_string())?;
    parsed.validate()?;
    serde_json::to_value(&parsed).map_err(|err| err.to_string())
}

#[derive(Debug)]
pub enum ConfigError {
    UnknownNamespace(String),
    UnknownKey(String),
    NotALeaf(String),
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownNamespace(namespace) => {
                write!(f, "unknown config namespace: {namespace}")
            }
            Self::UnknownKey(key) => write!(f, "unknown config key: {key}"),
            Self::NotALeaf(key) => write!(f, "{key} is a table, not a single value; use get_all"),
            Self::Invalid(msg) => write!(f, "invalid config value: {msg}"),
            Self::Io(msg) => write!(f, "config store I/O error: {msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

pub struct ConfigStore {
    dir: PathBuf,
    schemas: &'static [NamespaceSchema],
    cache: Mutex<HashMap<&'static str, Json>>,
}

impl ConfigStore {
    /// Loads every registered namespace's backing file, creating `dir` and
    /// any missing file if needed (an absent file is exactly
    /// `T::default()`, materialized and written out immediately so the
    /// file on disk always reflects the running config once this
    /// returns). A present-but-invalid file is logged and treated the same
    /// as absent, rather than failing daemon startup over it.
    pub fn open(dir: PathBuf, schemas: &'static [NamespaceSchema]) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        let mut cache = HashMap::with_capacity(schemas.len());
        for schema in schemas {
            let path = namespace_path(&dir, schema.name);
            let on_disk = match fs::read_to_string(&path) {
                Ok(text) => match parse_toml(&text) {
                    Ok(value) => Some(value),
                    Err(err) => {
                        eprintln!(
                            "nkdhrd: {} is not valid TOML, using defaults: {err}",
                            path.display()
                        );
                        None
                    }
                },
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => return Err(err),
            };
            let materialized = on_disk
                .as_ref()
                .and_then(|value| match (schema.parse)(value) {
                    Ok(materialized) => Some(materialized),
                    Err(err) => {
                        eprintln!(
                            "nkdhrd: {} failed validation, using defaults: {err}",
                            path.display()
                        );
                        None
                    }
                })
                .unwrap_or_else(|| {
                    (schema.parse)(&Json::Object(Default::default()))
                        .expect("Namespace::default() must itself validate")
                });
            write_namespace_file(&path, &materialized)?;
            cache.insert(schema.name, materialized);
        }
        Ok(Self {
            dir,
            schemas,
            cache: Mutex::new(cache),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads one leaf value by its dotted key (`<namespace>.<field...>`).
    pub fn get(&self, key: &str) -> Result<Json, ConfigError> {
        let (namespace, path) = split_key(key)?;
        let cache = self.cache.lock().expect("config store cache poisoned");
        let root = cache
            .get(namespace)
            .ok_or_else(|| ConfigError::UnknownNamespace(namespace.to_owned()))?;
        navigate_leaf(root, &path, key)
    }

    /// Reads every leaf value under `prefix`: a namespace name, a deeper
    /// dotted path within one, or `""` for every registered namespace.
    /// Result keys are the leaves' own full dotted paths, not relative to
    /// `prefix`.
    pub fn get_all(&self, prefix: &str) -> Result<HashMap<String, Json>, ConfigError> {
        let cache = self.cache.lock().expect("config store cache poisoned");
        let mut out = HashMap::new();
        if prefix.is_empty() {
            for schema in self.schemas {
                if let Some(root) = cache.get(schema.name) {
                    flatten(schema.name, root, &mut out);
                }
            }
            return Ok(out);
        }
        let (namespace, path) = split_key(prefix)?;
        let root = cache
            .get(namespace)
            .ok_or_else(|| ConfigError::UnknownNamespace(namespace.to_owned()))?;
        let subtree = navigate(root, &path, prefix)?;
        flatten(prefix, subtree, &mut out);
        Ok(out)
    }

    /// Sets one leaf value by its dotted key. On success, returns the value
    /// actually stored — after the namespace's full round trip through its
    /// schema, so e.g. an integer sent where the schema expects a float
    /// comes back as `serde` represents it there — for the caller to emit
    /// `Changed` with. Rejected (leaving the previous value active in both
    /// memory and on disk) if the key is unknown or the resulting
    /// namespace fails validation.
    pub fn set(&self, key: &str, value: Json) -> Result<Json, ConfigError> {
        let (namespace, path) = split_key(key)?;
        let schema = self
            .schemas
            .iter()
            .find(|schema| schema.name == namespace)
            .ok_or_else(|| ConfigError::UnknownNamespace(namespace.to_owned()))?;

        let mut cache = self.cache.lock().expect("config store cache poisoned");
        let current = cache
            .get(namespace)
            .ok_or_else(|| ConfigError::UnknownNamespace(namespace.to_owned()))?;
        let mut candidate = current.clone();
        set_leaf(&mut candidate, &path, value, key)?;

        let materialized = (schema.parse)(&candidate).map_err(ConfigError::Invalid)?;
        let stored = navigate_leaf(&materialized, &path, key)?;

        write_namespace_file(&namespace_path(&self.dir, namespace), &materialized)
            .map_err(|err| ConfigError::Io(err.to_string()))?;
        cache.insert(schema.name, materialized);
        Ok(stored)
    }

    /// Re-reads `namespace`'s file from disk, validates it, and — only if
    /// valid — replaces the in-memory value, returning the flattened set
    /// of leaf keys whose value actually changed (for the caller to emit
    /// `Changed` for; a key whose value reverted to its default because it
    /// was removed from the file counts as changed too, since the
    /// materialized form always has every field present). Returns `Err`
    /// with the in-memory value left untouched if the file is missing,
    /// unparsable, or fails validation — the caller logs this rather than
    /// propagating it, per USAGE.md's troubleshooting section.
    pub fn reload(&self, namespace: &str) -> Result<Vec<(String, Json)>, ConfigError> {
        let schema = self
            .schemas
            .iter()
            .find(|schema| schema.name == namespace)
            .ok_or_else(|| ConfigError::UnknownNamespace(namespace.to_owned()))?;

        let path = namespace_path(&self.dir, namespace);
        let text = fs::read_to_string(&path)
            .map_err(|err| ConfigError::Io(format!("reading {}: {err}", path.display())))?;
        let on_disk = parse_toml(&text).map_err(ConfigError::Invalid)?;
        let materialized = (schema.parse)(&on_disk).map_err(ConfigError::Invalid)?;

        let mut cache = self.cache.lock().expect("config store cache poisoned");
        let mut old = HashMap::new();
        if let Some(previous) = cache.get(namespace) {
            flatten(namespace, previous, &mut old);
        }
        let mut new = HashMap::new();
        flatten(namespace, &materialized, &mut new);
        let changed: Vec<(String, Json)> = new
            .into_iter()
            .filter(|(key, value)| old.get(key) != Some(value))
            .collect();

        cache.insert(schema.name, materialized);
        Ok(changed)
    }
}

fn parse_toml(text: &str) -> Result<Json, String> {
    let value: toml::Value = toml::from_str(text).map_err(|err| err.to_string())?;
    serde_json::to_value(value).map_err(|err| err.to_string())
}

fn namespace_path(dir: &Path, namespace: &str) -> PathBuf {
    dir.join(format!("{namespace}.toml"))
}

/// Writes `value` to `path` by writing a sibling temp file and renaming it
/// into place, so a crash mid-write can never leave `path` truncated or
/// half-written.
fn write_namespace_file(path: &Path, value: &Json) -> io::Result<()> {
    let toml_value: toml::Value =
        serde_json::from_value(value.clone()).map_err(io::Error::other)?;
    let text = toml::to_string_pretty(&toml_value).map_err(io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

fn split_key(key: &str) -> Result<(&str, Vec<&str>), ConfigError> {
    let mut parts = key.split('.');
    let namespace = parts
        .next()
        .filter(|segment| !segment.is_empty())
        .ok_or_else(|| ConfigError::UnknownKey(key.to_owned()))?;
    let rest: Vec<&str> = parts.collect();
    if rest.iter().any(|segment| segment.is_empty()) {
        return Err(ConfigError::UnknownKey(key.to_owned()));
    }
    Ok((namespace, rest))
}

/// Walks `path` from `root`, returning whatever is there — a leaf or a
/// subtree. Used by `get_all`, which is allowed to land on either.
fn navigate<'a>(root: &'a Json, path: &[&str], full_key: &str) -> Result<&'a Json, ConfigError> {
    let mut current = root;
    for segment in path {
        current = current
            .as_object()
            .and_then(|object| object.get(*segment))
            .ok_or_else(|| ConfigError::UnknownKey(full_key.to_owned()))?;
    }
    Ok(current)
}

/// Like [`navigate`], but rejects landing on a subtree — used by `get` and
/// `set`, which only ever address a single value.
fn navigate_leaf(root: &Json, path: &[&str], full_key: &str) -> Result<Json, ConfigError> {
    let value = navigate(root, path, full_key)?;
    if path.is_empty() || matches!(value, Json::Object(_)) {
        return Err(ConfigError::NotALeaf(full_key.to_owned()));
    }
    Ok(value.clone())
}

/// Overwrites the leaf at `path` within `root` in place. The leaf must
/// already exist — `set` never creates a field the namespace's schema
/// doesn't already have, which is how "unknown keys are rejected" applies
/// to writes as well as to the file itself.
fn set_leaf(
    root: &mut Json,
    path: &[&str],
    value: Json,
    full_key: &str,
) -> Result<(), ConfigError> {
    let Some((last, ancestors)) = path.split_last() else {
        return Err(ConfigError::NotALeaf(full_key.to_owned()));
    };
    let mut current = root;
    for segment in ancestors {
        current = current
            .as_object_mut()
            .and_then(|object| object.get_mut(*segment))
            .ok_or_else(|| ConfigError::UnknownKey(full_key.to_owned()))?;
    }
    let object = current
        .as_object_mut()
        .ok_or_else(|| ConfigError::UnknownKey(full_key.to_owned()))?;
    if !object.contains_key(*last) {
        return Err(ConfigError::UnknownKey(full_key.to_owned()));
    }
    object.insert((*last).to_owned(), value);
    Ok(())
}

/// Recursively collects every leaf under `value` (a table found along the
/// way is descended into, not returned) keyed by its full dotted path,
/// rooted at `prefix`.
fn flatten(prefix: &str, value: &Json, out: &mut HashMap<String, Json>) {
    match value {
        Json::Object(object) => {
            for (key, val) in object {
                flatten(&format!("{prefix}.{key}"), val, out);
            }
        }
        leaf => {
            out.insert(prefix.to_owned(), leaf.clone());
        }
    }
}

/// Converts one config leaf value to its D-Bus wire representation.
/// Namespace schemas are limited to booleans, numbers and strings for now
/// (arrays and nested tables are unsupported over IPC, though supported
/// on disk as intermediate structure) — no registered namespace has needed
/// more yet; extending this is a matter of adding a match arm, not a
/// redesign.
pub fn json_to_variant(value: &Json) -> Result<Value<'static>, ConfigError> {
    Ok(match value {
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(f) = n.as_f64() {
                Value::from(f)
            } else {
                return Err(ConfigError::Invalid(format!("unsupported number: {n}")));
            }
        }
        Json::String(s) => Value::from(s.clone()),
        other => {
            return Err(ConfigError::Invalid(format!(
                "config values shaped like {other} aren't supported over D-Bus yet"
            )));
        }
    })
}

/// The inverse of [`json_to_variant`], for values arriving from `set`.
pub fn variant_to_json(value: &Value<'_>) -> Result<Json, ConfigError> {
    Ok(match value {
        Value::U8(n) => Json::from(*n),
        Value::Bool(b) => Json::Bool(*b),
        Value::I16(n) => Json::from(*n),
        Value::U16(n) => Json::from(*n),
        Value::I32(n) => Json::from(*n),
        Value::U32(n) => Json::from(*n),
        Value::I64(n) => Json::from(*n),
        Value::U64(n) => Json::from(*n),
        Value::F64(f) => Json::from(*f),
        Value::Str(s) => Json::String(s.to_string()),
        other => {
            return Err(ConfigError::Invalid(format!(
                "unsupported D-Bus value type for a config value: {}",
                other.value_signature()
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// Test-only namespace: proves the generic engine (schema validation,
    /// defaults, unknown-key rejection, dotted-key get/set, external-edit
    /// reload) without committing CTRL-5 to any real namespace's shape —
    /// see this module's doc comment for why none is registered yet.
    #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields, default)]
    struct TestNamespace {
        volume_step: u8,
        label: String,
    }

    impl Namespace for TestNamespace {
        const NAME: &'static str = "test";

        fn validate(&self) -> Result<(), String> {
            if self.volume_step > 100 {
                return Err(format!(
                    "volume_step must be 0-100, got {}",
                    self.volume_step
                ));
            }
            Ok(())
        }
    }

    fn schemas() -> &'static [NamespaceSchema] {
        // Leaked once per test process: the registry is `&'static` by
        // design (see `NamespaceSchema`), and a real `nkdhrd` builds its
        // list the same way, as a `static` in `main.rs`.
        Box::leak(vec![NamespaceSchema::of::<TestNamespace>()].into_boxed_slice())
    }

    #[test]
    fn missing_file_materializes_defaults() {
        let dir = tempdir();
        let store = ConfigStore::open(dir.clone(), schemas()).unwrap();
        assert_eq!(store.get("test.volume_step").unwrap(), Json::from(0));
        assert!(dir.join("test.toml").exists());
        cleanup(dir);
    }

    #[test]
    fn set_persists_and_rejects_invalid() {
        let dir = tempdir();
        let store = ConfigStore::open(dir.clone(), schemas()).unwrap();

        store.set("test.volume_step", Json::from(5)).unwrap();
        assert_eq!(store.get("test.volume_step").unwrap(), Json::from(5));

        let err = store.set("test.volume_step", Json::from(200)).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
        // Rejected: the last-known-good value is still active.
        assert_eq!(store.get("test.volume_step").unwrap(), Json::from(5));

        let err = store.set("test.nonexistent", Json::from(1)).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownKey(_)));

        cleanup(dir);
    }

    #[test]
    fn external_edit_is_detected_and_revalidated() {
        let dir = tempdir();
        let store = ConfigStore::open(dir.clone(), schemas()).unwrap();

        fs::write(dir.join("test.toml"), "volume_step = 42\nlabel = \"hi\"\n").unwrap();
        let changed = store.reload("test").unwrap();
        let expected = Json::from(42);
        assert!(
            changed
                .iter()
                .any(|(key, value)| key == "test.volume_step" && *value == expected)
        );
        assert_eq!(store.get("test.volume_step").unwrap(), Json::from(42));

        // An invalid external edit is rejected; the last-known-good value
        // (42, from just above) stays active.
        fs::write(dir.join("test.toml"), "volume_step = 999\n").unwrap();
        let err = store.reload("test").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
        assert_eq!(store.get("test.volume_step").unwrap(), Json::from(42));

        cleanup(dir);
    }

    #[test]
    fn get_all_flattens_a_namespace() {
        let dir = tempdir();
        let store = ConfigStore::open(dir.clone(), schemas()).unwrap();
        store.set("test.label", Json::from("hello")).unwrap();

        let all = store.get_all("test").unwrap();
        assert_eq!(all.get("test.volume_step"), Some(&Json::from(0)));
        assert_eq!(all.get("test.label"), Some(&Json::from("hello")));

        cleanup(dir);
    }

    fn tempdir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "nkdhrd-config-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cleanup(dir: PathBuf) {
        let _ = fs::remove_dir_all(dir);
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value as Json;

use crate::{ThemeTokenChange, TokenImpact, parse_color};

pub const MAX_EXTENSION_GROUPS: usize = 256;
pub const MAX_EXTENSION_TOKENS_PER_GROUP: usize = 256;
pub const MAX_EXTENSION_STRING_BYTES: usize = 64 * 1024;
pub const MAX_EXTENSION_CHOICES: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionTokenType {
    Boolean,
    Integer { min: i64, max: i64 },
    Number { min: f64, max: f64 },
    String { max_bytes: usize },
    Color,
    Choice { values: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionValue {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    String(String),
    Color(String),
    Choice(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionTokenDescriptor {
    name: String,
    value_type: ExtensionTokenType,
    default: ExtensionValue,
    impact: TokenImpact,
}

impl ExtensionTokenDescriptor {
    pub fn new(
        name: impl Into<String>,
        value_type: ExtensionTokenType,
        default: ExtensionValue,
        impact: TokenImpact,
    ) -> Self {
        Self {
            name: name.into(),
            value_type,
            default,
            impact,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value_type(&self) -> &ExtensionTokenType {
        &self.value_type
    }

    pub fn default_value(&self) -> &ExtensionValue {
        &self.default
    }

    pub fn impact(&self) -> TokenImpact {
        self.impact
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionTokenGroup {
    name: String,
    tokens: Vec<ExtensionTokenDescriptor>,
}

impl ExtensionTokenGroup {
    pub fn new(
        name: impl Into<String>,
        tokens: impl IntoIterator<Item = ExtensionTokenDescriptor>,
    ) -> Self {
        Self {
            name: name.into(),
            tokens: tokens.into_iter().collect(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tokens(&self) -> &[ExtensionTokenDescriptor] {
        &self.tokens
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RegisteredGroup {
    tokens: BTreeMap<String, ExtensionTokenDescriptor>,
}

/// Host-owned declarations for third-party theme leaves.
///
/// Registration is deliberately separate from profile data: a profile may
/// select values, but it cannot grant itself new token names or validation
/// rules. Registries are normally assembled before a UI runtime is created
/// and then shared immutably by every root using that runtime.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeExtensionRegistry {
    groups: BTreeMap<String, RegisteredGroup>,
}

impl ThemeExtensionRegistry {
    pub fn register(&mut self, group: ExtensionTokenGroup) -> Result<(), ThemeExtensionError> {
        validate_group_name(&group.name)?;
        if self.groups.contains_key(&group.name) {
            return Err(ThemeExtensionError::DuplicateGroup(group.name));
        }
        if self.groups.len() >= MAX_EXTENSION_GROUPS {
            return Err(ThemeExtensionError::TooManyGroups);
        }
        if group.tokens.is_empty() || group.tokens.len() > MAX_EXTENSION_TOKENS_PER_GROUP {
            return Err(ThemeExtensionError::InvalidGroupSize(group.name));
        }

        let mut tokens = BTreeMap::new();
        for token in group.tokens {
            validate_token_name(&token.name)?;
            validate_descriptor(&group.name, &token)?;
            let name = token.name.clone();
            if tokens.insert(name.clone(), token).is_some() {
                return Err(ThemeExtensionError::DuplicateToken(format!(
                    "{}.{}",
                    group.name, name
                )));
            }
        }
        self.groups.insert(group.name, RegisteredGroup { tokens });
        Ok(())
    }

    pub fn contains_group(&self, group: &str) -> bool {
        self.groups.contains_key(group)
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub(crate) fn resolve(
        &self,
        overrides: Option<&Json>,
    ) -> Result<ResolvedExtensionTokens, ThemeExtensionError> {
        let supplied_groups = match overrides {
            Some(Json::Object(groups)) => Some(groups),
            Some(_) => return Err(ThemeExtensionError::OverridesMustBeObject),
            None => None,
        };

        if let Some(groups) = supplied_groups {
            for storage_name in groups.keys() {
                let group_name = format!("extension.{storage_name}");
                if !self.groups.contains_key(&group_name) {
                    return Err(ThemeExtensionError::UnknownGroup(group_name));
                }
            }
        }

        let mut resolved = BTreeMap::new();
        for (group_name, group) in &self.groups {
            let storage_name = group_name
                .strip_prefix("extension.")
                .expect("registered extension groups are normalized");
            let supplied_tokens = match supplied_groups.and_then(|groups| groups.get(storage_name))
            {
                Some(Json::Object(tokens)) => Some(tokens),
                Some(_) => {
                    return Err(ThemeExtensionError::GroupMustBeObject(group_name.clone()));
                }
                None => None,
            };
            if let Some(tokens) = supplied_tokens {
                for token_name in tokens.keys() {
                    if !group.tokens.contains_key(token_name) {
                        return Err(ThemeExtensionError::UnknownToken(format!(
                            "{group_name}.{token_name}"
                        )));
                    }
                }
            }

            let mut resolved_group = BTreeMap::new();
            for (token_name, descriptor) in &group.tokens {
                let path = format!("{group_name}.{token_name}");
                let value = match supplied_tokens.and_then(|tokens| tokens.get(token_name)) {
                    Some(value) => parse_value(&path, &descriptor.value_type, value)?,
                    None => descriptor.default.clone(),
                };
                resolved_group.insert(
                    token_name.clone(),
                    ResolvedExtensionToken {
                        value,
                        impact: descriptor.impact,
                    },
                );
            }
            resolved.insert(group_name.clone(), resolved_group);
        }
        Ok(resolved)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedExtensionToken {
    value: ExtensionValue,
    impact: TokenImpact,
}

impl ResolvedExtensionToken {
    pub fn value(&self) -> &ExtensionValue {
        &self.value
    }

    pub fn impact(&self) -> TokenImpact {
        self.impact
    }
}

pub type ResolvedExtensionTokens = BTreeMap<String, BTreeMap<String, ResolvedExtensionToken>>;

pub(crate) fn diff_extensions(
    previous: &ResolvedExtensionTokens,
    current: &ResolvedExtensionTokens,
) -> Vec<ThemeTokenChange> {
    let mut paths = BTreeSet::new();
    collect_paths(previous, &mut paths);
    collect_paths(current, &mut paths);
    paths
        .into_iter()
        .filter_map(|(group, token)| {
            let old = previous.get(&group).and_then(|tokens| tokens.get(&token));
            let new = current.get(&group).and_then(|tokens| tokens.get(&token));
            (old != new).then(|| ThemeTokenChange {
                path: format!("{group}.{token}"),
                impact: new.or(old).expect("a collected token exists").impact,
            })
        })
        .collect()
}

fn collect_paths(extensions: &ResolvedExtensionTokens, out: &mut BTreeSet<(String, String)>) {
    for (group, tokens) in extensions {
        for token in tokens.keys() {
            out.insert((group.clone(), token.clone()));
        }
    }
}

fn validate_group_name(name: &str) -> Result<(), ThemeExtensionError> {
    let Some(suffix) = name.strip_prefix("extension.") else {
        return Err(ThemeExtensionError::InvalidGroupName(name.into()));
    };
    let labels: Vec<_> = suffix.split('.').collect();
    if name.len() > 192 || labels.len() < 3 || labels.iter().any(|label| !valid_label(label, true))
    {
        return Err(ThemeExtensionError::InvalidGroupName(name.into()));
    }
    Ok(())
}

fn validate_token_name(name: &str) -> Result<(), ThemeExtensionError> {
    if name.len() > 64 || !valid_label(name, false) {
        Err(ThemeExtensionError::InvalidTokenName(name.into()))
    } else {
        Ok(())
    }
}

fn valid_label(label: &str, allow_hyphen: bool) -> bool {
    !label.is_empty()
        && (!allow_hyphen || label.len() <= 63)
        && label
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && label.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (!allow_hyphen && byte == b'_')
                || (allow_hyphen && byte == b'-')
        })
}

fn validate_descriptor(
    group: &str,
    descriptor: &ExtensionTokenDescriptor,
) -> Result<(), ThemeExtensionError> {
    let path = format!("{group}.{}", descriptor.name);
    match &descriptor.value_type {
        ExtensionTokenType::Boolean | ExtensionTokenType::Color => {}
        ExtensionTokenType::Integer { min, max } if min <= max => {}
        ExtensionTokenType::Number { min, max }
            if min.is_finite() && max.is_finite() && min <= max => {}
        ExtensionTokenType::String { max_bytes }
            if (1..=MAX_EXTENSION_STRING_BYTES).contains(max_bytes) => {}
        ExtensionTokenType::Choice { values } => validate_choices(&path, values)?,
        _ => return Err(ThemeExtensionError::InvalidDescriptor(path)),
    }
    let default_json = value_as_json(&descriptor.default);
    let normalized = parse_value(&path, &descriptor.value_type, &default_json)
        .map_err(|_| ThemeExtensionError::InvalidDescriptor(path.clone()))?;
    if normalized != descriptor.default {
        return Err(ThemeExtensionError::InvalidDescriptor(path));
    }
    Ok(())
}

fn validate_choices(path: &str, values: &[String]) -> Result<(), ThemeExtensionError> {
    if values.is_empty() || values.len() > MAX_EXTENSION_CHOICES {
        return Err(ThemeExtensionError::InvalidDescriptor(path.into()));
    }
    let mut unique = BTreeSet::new();
    if values.iter().any(|value| {
        value.is_empty()
            || value.len() > 128
            || value.chars().any(char::is_control)
            || !unique.insert(value)
    }) {
        return Err(ThemeExtensionError::InvalidDescriptor(path.into()));
    }
    Ok(())
}

fn value_as_json(value: &ExtensionValue) -> Json {
    match value {
        ExtensionValue::Boolean(value) => Json::Bool(*value),
        ExtensionValue::Integer(value) => Json::from(*value),
        ExtensionValue::Number(value) => Json::from(*value),
        ExtensionValue::String(value)
        | ExtensionValue::Color(value)
        | ExtensionValue::Choice(value) => Json::String(value.clone()),
    }
}

fn parse_value(
    path: &str,
    value_type: &ExtensionTokenType,
    value: &Json,
) -> Result<ExtensionValue, ThemeExtensionError> {
    let invalid = || ThemeExtensionError::InvalidValue(path.into());
    match value_type {
        ExtensionTokenType::Boolean => value
            .as_bool()
            .map(ExtensionValue::Boolean)
            .ok_or_else(invalid),
        ExtensionTokenType::Integer { min, max } => value
            .as_i64()
            .filter(|value| *value >= *min && *value <= *max)
            .map(ExtensionValue::Integer)
            .ok_or_else(invalid),
        ExtensionTokenType::Number { min, max } => value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= *min && *value <= *max)
            .map(ExtensionValue::Number)
            .ok_or_else(invalid),
        ExtensionTokenType::String { max_bytes } => value
            .as_str()
            .filter(|value| value.len() <= *max_bytes && !value.chars().any(char::is_control))
            .map(|value| ExtensionValue::String(value.into()))
            .ok_or_else(invalid),
        ExtensionTokenType::Color => value
            .as_str()
            .and_then(|value| parse_color(value).ok())
            .map(|[r, g, b, a]| ExtensionValue::Color(format!("#{r:02x}{g:02x}{b:02x}{a:02x}")))
            .ok_or_else(invalid),
        ExtensionTokenType::Choice { values } => value
            .as_str()
            .filter(|value| values.iter().any(|allowed| allowed == value))
            .map(|value| ExtensionValue::Choice(value.into()))
            .ok_or_else(invalid),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeExtensionError {
    TooManyGroups,
    InvalidGroupName(String),
    DuplicateGroup(String),
    InvalidGroupSize(String),
    InvalidTokenName(String),
    DuplicateToken(String),
    InvalidDescriptor(String),
    OverridesMustBeObject,
    UnknownGroup(String),
    GroupMustBeObject(String),
    UnknownToken(String),
    InvalidValue(String),
}

impl fmt::Display for ThemeExtensionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyGroups => write!(
                formatter,
                "theme extension registry exceeds {MAX_EXTENSION_GROUPS} groups"
            ),
            Self::InvalidGroupName(name) => write!(formatter, "invalid extension group: {name}"),
            Self::DuplicateGroup(name) => write!(formatter, "duplicate extension group: {name}"),
            Self::InvalidGroupSize(name) => write!(
                formatter,
                "extension group must contain 1..={MAX_EXTENSION_TOKENS_PER_GROUP} tokens: {name}"
            ),
            Self::InvalidTokenName(name) => write!(formatter, "invalid extension token: {name}"),
            Self::DuplicateToken(path) => write!(formatter, "duplicate extension token: {path}"),
            Self::InvalidDescriptor(path) => {
                write!(formatter, "invalid extension token descriptor: {path}")
            }
            Self::OverridesMustBeObject => {
                formatter.write_str("extension overrides must be a JSON object")
            }
            Self::UnknownGroup(group) => write!(formatter, "unknown extension group: {group}"),
            Self::GroupMustBeObject(group) => {
                write!(
                    formatter,
                    "extension group overrides must be an object: {group}"
                )
            }
            Self::UnknownToken(path) => write!(formatter, "unknown extension token: {path}"),
            Self::InvalidValue(path) => write!(formatter, "invalid extension token value: {path}"),
        }
    }
}

impl std::error::Error for ThemeExtensionError {}

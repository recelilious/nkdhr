//! Typed, non-executable compositor actions and binding compilation.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

const BINDING_SCHEMA_VERSION: u32 = 1;
const MAX_ACTIONS: usize = 512;
const MAX_BINDINGS: usize = 2048;
const MAX_ARGUMENTS: usize = 32;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_DOCUMENT_BYTES: usize = 1_048_576;

/// Stable identifier used by configuration, Settings and the dispatcher.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(String);

impl ActionId {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        validate_identifier("action", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Borrow<str> for ActionId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

/// Scalar values accepted by the action grammar. No variant can contain code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ActionValue {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    String(String),
}

impl ActionValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(*value as f64),
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }
}

/// Closed validation schema for one configured action argument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionValueSchema {
    Boolean,
    Integer { minimum: i64, maximum: i64 },
    Number { minimum: f64, maximum: f64 },
    String { maximum_bytes: usize },
    Choice { values: Vec<String> },
}

impl ActionValueSchema {
    fn validate(&self, value: &ActionValue) -> Result<(), String> {
        match (self, value) {
            (Self::Boolean, ActionValue::Boolean(_)) => Ok(()),
            (Self::Integer { minimum, maximum }, ActionValue::Integer(value))
                if (*minimum..=*maximum).contains(value) =>
            {
                Ok(())
            }
            (Self::Number { minimum, maximum }, value)
                if value.as_f64().is_some_and(|value| {
                    value.is_finite() && value >= *minimum && value <= *maximum
                }) =>
            {
                Ok(())
            }
            (Self::String { maximum_bytes }, ActionValue::String(value))
                if value.len() <= *maximum_bytes =>
            {
                Ok(())
            }
            (Self::Choice { values }, ActionValue::String(value)) if values.contains(value) => {
                Ok(())
            }
            _ => Err(format!("value {value:?} does not satisfy {self:?}")),
        }
    }

    fn validate_definition(&self) -> Result<(), String> {
        match self {
            Self::Integer { minimum, maximum } if minimum > maximum => {
                Err("integer minimum exceeds maximum".to_owned())
            }
            Self::Number { minimum, maximum }
                if !minimum.is_finite() || !maximum.is_finite() || minimum > maximum =>
            {
                Err("number bounds must be finite and ordered".to_owned())
            }
            Self::String { maximum_bytes } if *maximum_bytes == 0 => {
                Err("string maximum_bytes must be positive".to_owned())
            }
            Self::String { maximum_bytes } if *maximum_bytes > MAX_DOCUMENT_BYTES => Err(format!(
                "string maximum_bytes must not exceed {MAX_DOCUMENT_BYTES}"
            )),
            Self::Choice { values } => {
                if values.is_empty() || values.len() > 256 {
                    return Err("choice values must contain 1..=256 entries".to_owned());
                }
                let unique = values.iter().collect::<BTreeSet<_>>();
                if unique.len() != values.len()
                    || values
                        .iter()
                        .any(|value| value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES)
                {
                    return Err("choice values must be bounded, non-empty and unique".to_owned());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionArgument {
    pub description: String,
    pub schema: ActionValueSchema,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ActionValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Instant,
    Continuous,
}

/// Public metadata used equally by bindings, Settings and the `/` grammar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDescriptor {
    pub id: ActionId,
    pub description: String,
    pub kind: ActionKind,
    #[serde(default)]
    pub arguments: BTreeMap<String, ActionArgument>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<String>,
}

impl ActionDescriptor {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        kind: ActionKind,
    ) -> Result<Self, String> {
        let descriptor = Self {
            id: ActionId::new(id)?,
            description: description.into(),
            kind,
            arguments: BTreeMap::new(),
            required_capabilities: BTreeSet::new(),
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn with_argument(
        mut self,
        name: impl Into<String>,
        argument: ActionArgument,
    ) -> Result<Self, String> {
        let name = name.into();
        validate_identifier("argument", &name)?;
        if self.arguments.insert(name.clone(), argument).is_some() {
            return Err(format!("duplicate argument {name:?}"));
        }
        self.validate()?;
        Ok(self)
    }

    pub fn requiring(mut self, capability: impl Into<String>) -> Result<Self, String> {
        let capability = capability.into();
        validate_identifier("capability", &capability)?;
        self.required_capabilities.insert(capability);
        Ok(self)
    }

    fn validate(&self) -> Result<(), String> {
        if self.description.trim().is_empty() || self.description.len() > 4096 {
            return Err(format!("action {} needs a description", self.id));
        }
        if self.arguments.len() > MAX_ARGUMENTS {
            return Err(format!("action {} has too many arguments", self.id));
        }
        for (name, argument) in &self.arguments {
            validate_identifier("argument", name)?;
            argument.schema.validate_definition()?;
            if argument.required && argument.default.is_some() {
                return Err(format!(
                    "required argument {name:?} on {} cannot also have a default",
                    self.id
                ));
            }
            if let Some(default) = &argument.default {
                argument
                    .schema
                    .validate(default)
                    .map_err(|error| format!("invalid default for {}.{name}: {error}", self.id))?;
            }
        }
        for capability in &self.required_capabilities {
            validate_identifier("capability", capability)?;
        }
        Ok(())
    }
}

/// Runtime capabilities are declarative so unavailable actions remain discoverable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionEnvironment {
    pub capabilities: BTreeSet<String>,
    pub devices: BTreeSet<DeviceClass>,
}

impl ActionEnvironment {
    pub fn with_capability(mut self, capability: impl Into<String>) -> Self {
        self.capabilities.insert(capability.into());
        self
    }

    pub fn with_device(mut self, device: DeviceClass) -> Self {
        self.devices.insert(device);
        self
    }

    fn supports_action(&self, descriptor: &ActionDescriptor) -> bool {
        descriptor
            .required_capabilities
            .iter()
            .all(|capability| self.capabilities.contains(capability))
    }

    fn supports_device(&self, device: DeviceClass) -> bool {
        match device {
            DeviceClass::AnyPointer => self.devices.iter().any(|candidate| {
                matches!(
                    candidate,
                    DeviceClass::Mouse | DeviceClass::Touchpad | DeviceClass::AnyPointer
                )
            }),
            _ => self.devices.contains(&device),
        }
    }
}

/// Read-only action metadata and validation shared by every consumer.
#[derive(Debug, Clone, Default)]
pub struct ActionCatalog {
    actions: BTreeMap<ActionId, ActionDescriptor>,
}

impl ActionCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, descriptor: ActionDescriptor) -> Result<(), String> {
        descriptor.validate()?;
        if self.actions.len() >= MAX_ACTIONS {
            return Err(format!("action catalog exceeds {MAX_ACTIONS} entries"));
        }
        if self.actions.contains_key(&descriptor.id) {
            return Err(format!("duplicate action {}", descriptor.id));
        }
        self.actions.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }

    pub fn descriptor(&self, id: &str) -> Option<&ActionDescriptor> {
        self.actions.get(id)
    }

    pub fn descriptors(&self) -> impl ExactSizeIterator<Item = &ActionDescriptor> {
        self.actions.values()
    }

    pub fn validate_invocation(
        &self,
        invocation: &ActionInvocation,
    ) -> Result<ValidatedActionInvocation, String> {
        let Some(descriptor) = self.descriptor(&invocation.action) else {
            return Err(format!("unknown action {:?}", invocation.action));
        };
        let mut arguments = BTreeMap::new();
        for supplied in invocation.arguments.keys() {
            if !descriptor.arguments.contains_key(supplied) {
                return Err(format!(
                    "unknown argument {supplied:?} for action {}",
                    descriptor.id
                ));
            }
        }
        for (name, argument) in &descriptor.arguments {
            let value = invocation
                .arguments
                .get(name)
                .cloned()
                .or_else(|| argument.default.clone());
            let Some(value) = value else {
                if argument.required {
                    return Err(format!(
                        "missing required argument {name:?} for action {}",
                        descriptor.id
                    ));
                }
                continue;
            };
            argument
                .schema
                .validate(&value)
                .map_err(|error| format!("invalid argument {}.{name}: {error}", descriptor.id))?;
            arguments.insert(name.clone(), value);
        }
        Ok(ValidatedActionInvocation {
            action: descriptor.id.clone(),
            arguments,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInvocation {
    pub action: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, ActionValue>,
}

impl ActionInvocation {
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            arguments: BTreeMap::new(),
        }
    }

    pub fn with_argument(mut self, name: impl Into<String>, value: ActionValue) -> Self {
        self.arguments.insert(name.into(), value);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedActionInvocation {
    pub action: ActionId,
    pub arguments: BTreeMap<String, ActionValue>,
}

impl ValidatedActionInvocation {
    pub fn string(&self, name: &str) -> Option<&str> {
        self.arguments.get(name).and_then(ActionValue::as_str)
    }

    pub fn integer(&self, name: &str) -> Option<i64> {
        self.arguments.get(name).and_then(ActionValue::as_i64)
    }

    pub fn number(&self, name: &str) -> Option<f64> {
        self.arguments.get(name).and_then(ActionValue::as_f64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Control,
    Alt,
    Shift,
    Logo,
}

/// Canonical modifier bit set. Ordering in source documents is intentionally irrelevant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModifierSet(u8);

impl ModifierSet {
    pub fn new(modifiers: impl IntoIterator<Item = Modifier>) -> Self {
        let mut value = 0;
        for modifier in modifiers {
            value |= match modifier {
                Modifier::Control => 1,
                Modifier::Alt => 2,
                Modifier::Shift => 4,
                Modifier::Logo => 8,
            };
        }
        Self(value)
    }

    pub fn contains(self, modifier: Modifier) -> bool {
        self.0 & Self::new([modifier]).0 != 0
    }

    pub fn ordered(self) -> impl Iterator<Item = Modifier> {
        [
            Modifier::Control,
            Modifier::Alt,
            Modifier::Shift,
            Modifier::Logo,
        ]
        .into_iter()
        .filter(move |modifier| self.contains(*modifier))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    Keyboard,
    Mouse,
    Touchpad,
    Touchscreen,
    AnyPointer,
}

impl DeviceClass {
    fn overlaps(self, other: Self) -> bool {
        self == other
            || matches!(self, Self::AnyPointer) && matches!(other, Self::Mouse | Self::Touchpad)
            || matches!(other, Self::AnyPointer) && matches!(self, Self::Mouse | Self::Touchpad)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingContext {
    Global,
    Canvas,
    Window,
    WindowFrame,
    EmptyCanvas,
    Overview,
}

impl BindingContext {
    fn overlaps(self, other: Self) -> bool {
        use BindingContext::{Canvas, EmptyCanvas, Global, Overview, Window, WindowFrame};
        self == other
            || matches!((self, other), (Global, _) | (_, Global))
            || matches!((self, other), (Canvas, _) | (_, Canvas))
            || matches!((self, other), (Window, WindowFrame) | (WindowFrame, Window))
            || matches!(
                (self, other),
                (Overview, EmptyCanvas) | (EmptyCanvas, Overview)
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPhase {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonCode {
    Primary,
    Secondary,
    Middle,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureKind {
    Swipe,
    Pinch,
    Hold,
    Touch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureActivation {
    Begin,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureOrigin {
    Anywhere,
    Window,
    WindowFrame,
    EmptyCanvas,
    Edge,
}

impl GestureOrigin {
    fn overlaps(self, other: Self) -> bool {
        self == other
            || matches!((self, other), (Self::Anywhere, _) | (_, Self::Anywhere))
            || matches!(
                (self, other),
                (Self::Window, Self::WindowFrame) | (Self::WindowFrame, Self::Window)
            )
    }
}

/// Serialized trigger grammar stored in the CTRL-5 scalar document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Key {
        key: String,
        #[serde(default)]
        modifiers: Vec<Modifier>,
        #[serde(default = "press_phase")]
        phase: KeyPhase,
    },
    Button {
        button: ButtonCode,
        #[serde(default)]
        modifiers: Vec<Modifier>,
        #[serde(default = "any_pointer")]
        device: DeviceClass,
        #[serde(default = "anywhere_origin")]
        origin: GestureOrigin,
        #[serde(default = "press_phase")]
        phase: KeyPhase,
    },
    Gesture {
        gesture: GestureKind,
        device: DeviceClass,
        fingers: u8,
        #[serde(default = "anywhere_origin")]
        origin: GestureOrigin,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<GestureDirection>,
        #[serde(default = "gesture_end")]
        activation: GestureActivation,
    },
}

const fn press_phase() -> KeyPhase {
    KeyPhase::Press
}

const fn any_pointer() -> DeviceClass {
    DeviceClass::AnyPointer
}

const fn anywhere_origin() -> GestureOrigin {
    GestureOrigin::Anywhere
}

const fn gesture_end() -> GestureActivation {
    GestureActivation::End
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingEntry {
    pub id: String,
    pub context: BindingContext,
    pub trigger: Trigger,
    pub invocation: ActionInvocation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingDocument {
    pub version: u32,
    pub bindings: Vec<BindingEntry>,
}

impl BindingDocument {
    pub fn new(bindings: Vec<BindingEntry>) -> Self {
        Self {
            version: BINDING_SCHEMA_VERSION,
            bindings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledTrigger {
    Key {
        key: String,
        modifiers: ModifierSet,
        phase: KeyPhase,
    },
    Button {
        button: ButtonCode,
        modifiers: ModifierSet,
        device: DeviceClass,
        origin: GestureOrigin,
        phase: KeyPhase,
    },
    Gesture {
        gesture: GestureKind,
        device: DeviceClass,
        fingers: u8,
        origin: GestureOrigin,
        direction: Option<GestureDirection>,
        activation: GestureActivation,
    },
}

impl CompiledTrigger {
    fn device(&self) -> DeviceClass {
        match self {
            Self::Key { .. } => DeviceClass::Keyboard,
            Self::Button { device, .. } | Self::Gesture { device, .. } => *device,
        }
    }

    fn conflicts(&self, other: &Self, own_kind: ActionKind, other_kind: ActionKind) -> bool {
        match (self, other) {
            (
                Self::Key {
                    key,
                    modifiers,
                    phase,
                },
                Self::Key {
                    key: other_key,
                    modifiers: other_modifiers,
                    phase: other_phase,
                },
            ) => key == other_key && modifiers == other_modifiers && phase == other_phase,
            (
                Self::Button {
                    button,
                    modifiers,
                    device,
                    origin,
                    phase,
                },
                Self::Button {
                    button: other_button,
                    modifiers: other_modifiers,
                    device: other_device,
                    origin: other_origin,
                    phase: other_phase,
                },
            ) => {
                button == other_button
                    && modifiers == other_modifiers
                    && device.overlaps(*other_device)
                    && origin.overlaps(*other_origin)
                    && (phase == other_phase
                        || own_kind == ActionKind::Continuous
                        || other_kind == ActionKind::Continuous)
            }
            (
                Self::Gesture {
                    gesture,
                    device,
                    fingers,
                    origin,
                    direction,
                    activation,
                },
                Self::Gesture {
                    gesture: other_gesture,
                    device: other_device,
                    fingers: other_fingers,
                    origin: other_origin,
                    direction: other_direction,
                    activation: other_activation,
                },
            ) => {
                gesture == other_gesture
                    && device.overlaps(*other_device)
                    && fingers == other_fingers
                    && origin.overlaps(*other_origin)
                    && (direction.is_none()
                        || other_direction.is_none()
                        || direction == other_direction)
                    && (activation == other_activation
                        || own_kind == ActionKind::Continuous
                        || other_kind == ActionKind::Continuous)
            }
            _ => false,
        }
    }

    fn matches(&self, runtime: &RuntimeTrigger, action_kind: ActionKind) -> bool {
        match (self, runtime) {
            (
                Self::Key {
                    key,
                    modifiers,
                    phase,
                },
                RuntimeTrigger::Key {
                    key: runtime_key,
                    modifiers: runtime_modifiers,
                    phase: runtime_phase,
                },
            ) => key == runtime_key && modifiers == runtime_modifiers && phase == runtime_phase,
            (
                Self::Button {
                    button,
                    modifiers,
                    device,
                    origin,
                    phase,
                },
                RuntimeTrigger::Button {
                    button: runtime_button,
                    modifiers: runtime_modifiers,
                    device: runtime_device,
                    origin: runtime_origin,
                    phase: runtime_phase,
                },
            ) => {
                button == runtime_button
                    && modifiers == runtime_modifiers
                    && device.overlaps(*runtime_device)
                    && origin.overlaps(*runtime_origin)
                    && if action_kind == ActionKind::Continuous {
                        *runtime_phase == KeyPhase::Press
                    } else {
                        phase == runtime_phase
                    }
            }
            (
                Self::Gesture {
                    gesture,
                    device,
                    fingers,
                    origin,
                    direction,
                    activation,
                },
                RuntimeTrigger::Gesture {
                    gesture: runtime_gesture,
                    device: runtime_device,
                    fingers: runtime_fingers,
                    origin: runtime_origin,
                    direction: runtime_direction,
                    activation: runtime_activation,
                },
            ) => {
                gesture == runtime_gesture
                    && device.overlaps(*runtime_device)
                    && fingers == runtime_fingers
                    && origin.overlaps(*runtime_origin)
                    && direction.is_none_or(|direction| Some(direction) == *runtime_direction)
                    && if action_kind == ActionKind::Continuous {
                        *runtime_activation == GestureActivation::Begin
                    } else {
                        activation == runtime_activation
                    }
            }
            _ => false,
        }
    }
}

/// One host-normalized input trigger used to query an effective binding snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeTrigger {
    Key {
        key: String,
        modifiers: ModifierSet,
        phase: KeyPhase,
    },
    Button {
        button: ButtonCode,
        modifiers: ModifierSet,
        device: DeviceClass,
        origin: GestureOrigin,
        phase: KeyPhase,
    },
    Gesture {
        gesture: GestureKind,
        device: DeviceClass,
        fingers: u8,
        origin: GestureOrigin,
        direction: Option<GestureDirection>,
        activation: GestureActivation,
    },
}

impl RuntimeTrigger {
    pub fn key(key: impl Into<String>, modifiers: ModifierSet, phase: KeyPhase) -> Self {
        Self::Key {
            key: normalize_key(&key.into()),
            modifiers,
            phase,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingAvailability {
    Effective,
    UnsupportedDevice,
    UnavailableAction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledBinding {
    pub id: String,
    pub context: BindingContext,
    pub trigger: CompiledTrigger,
    pub invocation: ValidatedActionInvocation,
    pub action_kind: ActionKind,
    pub availability: BindingAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingDiagnosticCode {
    InvalidDocument,
    UnsupportedVersion,
    TooManyBindings,
    InvalidIdentifier,
    DuplicateBinding,
    UnknownAction,
    InvalidArgument,
    IncompatibleTrigger,
    Conflict,
    ReservedClientGesture,
    UnsupportedDevice,
    UnavailableAction,
}

/// Structured diagnostic suitable for compositor logs and retained Settings rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingDiagnostic {
    pub severity: BindingSeverity,
    pub code: BindingDiagnosticCode,
    pub binding_id: Option<String>,
    pub related_binding_id: Option<String>,
    pub message: String,
}

impl BindingDiagnostic {
    fn error(
        code: BindingDiagnosticCode,
        binding_id: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: BindingSeverity::Error,
            code,
            binding_id: binding_id.map(str::to_owned),
            related_binding_id: None,
            message: message.into(),
        }
    }

    fn warning(code: BindingDiagnosticCode, binding_id: &str, message: impl Into<String>) -> Self {
        Self {
            severity: BindingSeverity::Warning,
            code,
            binding_id: Some(binding_id.to_owned()),
            related_binding_id: None,
            message: message.into(),
        }
    }
}

/// Immutable generation queried by both the compositor and Settings.
#[derive(Debug, Clone)]
pub struct BindingSnapshot {
    generation: u64,
    catalog: Arc<ActionCatalog>,
    bindings: Arc<[CompiledBinding]>,
    diagnostics: Arc<[BindingDiagnostic]>,
}

impl BindingSnapshot {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn catalog(&self) -> &Arc<ActionCatalog> {
        &self.catalog
    }

    pub fn bindings(&self) -> &[CompiledBinding] {
        &self.bindings
    }

    pub fn diagnostics(&self) -> &[BindingDiagnostic] {
        &self.diagnostics
    }

    pub fn effective_bindings(&self) -> impl Iterator<Item = &CompiledBinding> {
        self.bindings
            .iter()
            .filter(|binding| binding.availability == BindingAvailability::Effective)
    }

    pub fn find(
        &self,
        context: BindingContext,
        trigger: &RuntimeTrigger,
    ) -> Option<&CompiledBinding> {
        self.effective_bindings().find(|binding| {
            binding.context.overlaps(context)
                && binding.trigger.matches(trigger, binding.action_kind)
        })
    }
}

#[derive(Debug, Clone)]
pub struct BindingPublication {
    pub accepted: bool,
    pub effective: Arc<BindingSnapshot>,
    pub diagnostics: Arc<[BindingDiagnostic]>,
}

/// Last-known-good atomic binding publisher.
#[derive(Debug)]
pub struct BindingRuntime {
    catalog: Arc<ActionCatalog>,
    environment: ActionEnvironment,
    source: BindingDocument,
    effective: Arc<BindingSnapshot>,
}

impl BindingRuntime {
    pub fn new(
        catalog: Arc<ActionCatalog>,
        environment: ActionEnvironment,
        initial: BindingDocument,
    ) -> Result<Self, Vec<BindingDiagnostic>> {
        let compiled = compile_bindings(&catalog, &environment, &initial)?;
        let effective = Arc::new(BindingSnapshot {
            generation: 1,
            catalog: Arc::clone(&catalog),
            bindings: compiled.bindings.into(),
            diagnostics: compiled.diagnostics.into(),
        });
        Ok(Self {
            catalog,
            environment,
            source: initial,
            effective,
        })
    }

    pub fn snapshot(&self) -> Arc<BindingSnapshot> {
        Arc::clone(&self.effective)
    }

    pub fn publish_json(&mut self, source: &str) -> BindingPublication {
        if source.len() > MAX_DOCUMENT_BYTES {
            return self.rejected(vec![BindingDiagnostic::error(
                BindingDiagnosticCode::InvalidDocument,
                None,
                format!("binding document exceeds {MAX_DOCUMENT_BYTES} bytes"),
            )]);
        }
        let document = match serde_json::from_str::<BindingDocument>(source) {
            Ok(document) => document,
            Err(error) => {
                return self.rejected(vec![BindingDiagnostic::error(
                    BindingDiagnosticCode::InvalidDocument,
                    None,
                    format!("binding document is not valid schema-v1 JSON: {error}"),
                )]);
            }
        };
        self.publish(document)
    }

    pub fn publish(&mut self, document: BindingDocument) -> BindingPublication {
        match compile_bindings(&self.catalog, &self.environment, &document) {
            Ok(compiled) => {
                let effective = Arc::new(BindingSnapshot {
                    generation: self.effective.generation.saturating_add(1),
                    catalog: Arc::clone(&self.catalog),
                    bindings: compiled.bindings.into(),
                    diagnostics: compiled.diagnostics.into(),
                });
                self.source = document;
                self.effective = Arc::clone(&effective);
                BindingPublication {
                    accepted: true,
                    diagnostics: Arc::clone(&effective.diagnostics),
                    effective,
                }
            }
            Err(diagnostics) => self.rejected(diagnostics),
        }
    }

    pub fn set_environment(&mut self, environment: ActionEnvironment) -> BindingPublication {
        self.environment = environment;
        self.publish(self.source.clone())
    }

    fn rejected(&self, diagnostics: Vec<BindingDiagnostic>) -> BindingPublication {
        BindingPublication {
            accepted: false,
            effective: Arc::clone(&self.effective),
            diagnostics: diagnostics.into(),
        }
    }
}

#[derive(Debug)]
struct CompiledCandidate {
    bindings: Vec<CompiledBinding>,
    diagnostics: Vec<BindingDiagnostic>,
}

fn compile_bindings(
    catalog: &ActionCatalog,
    environment: &ActionEnvironment,
    document: &BindingDocument,
) -> Result<CompiledCandidate, Vec<BindingDiagnostic>> {
    let mut diagnostics = Vec::new();
    if document.version != BINDING_SCHEMA_VERSION {
        diagnostics.push(BindingDiagnostic::error(
            BindingDiagnosticCode::UnsupportedVersion,
            None,
            format!(
                "binding schema version {} is unsupported; expected {BINDING_SCHEMA_VERSION}",
                document.version
            ),
        ));
    }
    if document.bindings.len() > MAX_BINDINGS {
        diagnostics.push(BindingDiagnostic::error(
            BindingDiagnosticCode::TooManyBindings,
            None,
            format!("binding document exceeds {MAX_BINDINGS} entries"),
        ));
    }

    let mut identifiers = BTreeSet::new();
    let mut bindings = Vec::new();
    for entry in document.bindings.iter().take(MAX_BINDINGS) {
        if let Err(error) = validate_identifier("binding", &entry.id) {
            diagnostics.push(BindingDiagnostic::error(
                BindingDiagnosticCode::InvalidIdentifier,
                Some(&entry.id),
                error,
            ));
            continue;
        }
        if !identifiers.insert(entry.id.clone()) {
            diagnostics.push(BindingDiagnostic::error(
                BindingDiagnosticCode::DuplicateBinding,
                Some(&entry.id),
                format!("duplicate binding id {:?}", entry.id),
            ));
            continue;
        }
        let Some(descriptor) = catalog.descriptor(&entry.invocation.action) else {
            diagnostics.push(BindingDiagnostic::error(
                BindingDiagnosticCode::UnknownAction,
                Some(&entry.id),
                format!("unknown action {:?}", entry.invocation.action),
            ));
            continue;
        };
        let invocation = match catalog.validate_invocation(&entry.invocation) {
            Ok(invocation) => invocation,
            Err(error) => {
                diagnostics.push(BindingDiagnostic::error(
                    BindingDiagnosticCode::InvalidArgument,
                    Some(&entry.id),
                    error,
                ));
                continue;
            }
        };
        let trigger = match compile_trigger(&entry.id, &entry.trigger, descriptor.kind) {
            Ok(trigger) => trigger,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                continue;
            }
        };
        let availability = if !environment.supports_action(descriptor) {
            diagnostics.push(BindingDiagnostic::warning(
                BindingDiagnosticCode::UnavailableAction,
                &entry.id,
                format!(
                    "action {} requires unavailable capabilities: {}",
                    descriptor.id,
                    descriptor
                        .required_capabilities
                        .iter()
                        .filter(|capability| !environment.capabilities.contains(*capability))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
            BindingAvailability::UnavailableAction
        } else if !environment.supports_device(trigger.device()) {
            diagnostics.push(BindingDiagnostic::warning(
                BindingDiagnosticCode::UnsupportedDevice,
                &entry.id,
                format!("device {:?} is unavailable in this host", trigger.device()),
            ));
            BindingAvailability::UnsupportedDevice
        } else {
            BindingAvailability::Effective
        };
        bindings.push(CompiledBinding {
            id: entry.id.clone(),
            context: entry.context,
            trigger,
            invocation,
            action_kind: descriptor.kind,
            availability,
        });
    }

    for index in 0..bindings.len() {
        for other_index in index + 1..bindings.len() {
            let binding = &bindings[index];
            let other = &bindings[other_index];
            if binding.context.overlaps(other.context)
                && binding
                    .trigger
                    .conflicts(&other.trigger, binding.action_kind, other.action_kind)
            {
                let mut diagnostic = BindingDiagnostic::error(
                    BindingDiagnosticCode::Conflict,
                    Some(&binding.id),
                    format!(
                        "binding {:?} conflicts with {:?} in overlapping contexts",
                        binding.id, other.id
                    ),
                );
                diagnostic.related_binding_id = Some(other.id.clone());
                diagnostics.push(diagnostic);
            }
        }
    }

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == BindingSeverity::Error)
    {
        Err(diagnostics)
    } else {
        Ok(CompiledCandidate {
            bindings,
            diagnostics,
        })
    }
}

fn compile_trigger(
    binding_id: &str,
    trigger: &Trigger,
    action_kind: ActionKind,
) -> Result<CompiledTrigger, BindingDiagnostic> {
    match trigger {
        Trigger::Key {
            key,
            modifiers,
            phase,
        } => {
            if key.trim().is_empty() || key.len() > MAX_IDENTIFIER_BYTES {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "key name must be non-empty and bounded",
                ));
            }
            if action_kind == ActionKind::Continuous {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "continuous actions require a button or gesture trigger",
                ));
            }
            Ok(CompiledTrigger::Key {
                key: normalize_key(key),
                modifiers: ModifierSet::new(modifiers.iter().copied()),
                phase: *phase,
            })
        }
        Trigger::Button {
            button,
            modifiers,
            device,
            origin,
            phase,
        } => {
            if !matches!(
                device,
                DeviceClass::Mouse | DeviceClass::Touchpad | DeviceClass::AnyPointer
            ) {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "button triggers require mouse, touchpad or any_pointer",
                ));
            }
            if action_kind == ActionKind::Continuous && *phase != KeyPhase::Press {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "continuous button actions must begin on press",
                ));
            }
            Ok(CompiledTrigger::Button {
                button: *button,
                modifiers: ModifierSet::new(modifiers.iter().copied()),
                device: *device,
                origin: *origin,
                phase: *phase,
            })
        }
        Trigger::Gesture {
            gesture,
            device,
            fingers,
            origin,
            direction,
            activation,
        } => {
            if *fingers == 0 || *fingers > 10 {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "gesture finger count must be between 1 and 10",
                ));
            }
            if !matches!(device, DeviceClass::Touchpad | DeviceClass::Touchscreen) {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "gesture triggers require touchpad or touchscreen",
                ));
            }
            if *device == DeviceClass::Touchpad && *fingers == 2 {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::ReservedClientGesture,
                    Some(binding_id),
                    "two-finger touchpad gestures are reserved for clients",
                ));
            }
            if *device == DeviceClass::Touchscreen
                && !matches!(origin, GestureOrigin::EmptyCanvas | GestureOrigin::Edge)
            {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::ReservedClientGesture,
                    Some(binding_id),
                    "touchscreen gestures are client-owned unless they begin on empty canvas or an edge",
                ));
            }
            if action_kind == ActionKind::Continuous && direction.is_some() {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "continuous gestures cannot wait for a terminal direction",
                ));
            }
            if action_kind == ActionKind::Continuous && *activation != GestureActivation::Begin {
                return Err(BindingDiagnostic::error(
                    BindingDiagnosticCode::IncompatibleTrigger,
                    Some(binding_id),
                    "continuous gesture actions must begin at gesture begin",
                ));
            }
            Ok(CompiledTrigger::Gesture {
                gesture: *gesture,
                device: *device,
                fingers: *fingers,
                origin: *origin,
                direction: *direction,
                activation: *activation,
            })
        }
    }
}

fn normalize_key(key: &str) -> String {
    key.trim().to_lowercase()
}

fn validate_identifier(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
        return Err(format!(
            "{kind} identifier must contain 1..={MAX_IDENTIFIER_BYTES} bytes"
        ));
    }
    if value.split('.').any(|segment| {
        segment.is_empty()
            || segment.starts_with('-')
            || segment.ends_with('-')
            || segment.contains("--")
    }) || value.bytes().any(|byte| {
        !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-')
    }) {
        return Err(format!(
            "{kind} identifier {value:?} must use lowercase ASCII segments separated by '.' or '-'"
        ));
    }
    Ok(())
}

type ActionAdapter<C, P> = Arc<
    dyn Fn(&mut C, &ValidatedActionInvocation, ActionPhase<P>) -> Result<Option<String>, String>
        + Send
        + Sync,
>;

/// Typed descriptors plus their host-owned invocation adapters.
pub struct ActionRegistry<C, P> {
    catalog: Arc<ActionCatalog>,
    adapters: BTreeMap<ActionId, ActionAdapter<C, P>>,
}

impl<C, P> ActionRegistry<C, P> {
    pub fn new(catalog: Arc<ActionCatalog>) -> Self {
        Self {
            catalog,
            adapters: BTreeMap::new(),
        }
    }

    pub fn catalog(&self) -> &Arc<ActionCatalog> {
        &self.catalog
    }

    pub fn register_adapter(
        &mut self,
        action: &str,
        adapter: impl Fn(
            &mut C,
            &ValidatedActionInvocation,
            ActionPhase<P>,
        ) -> Result<Option<String>, String>
        + Send
        + Sync
        + 'static,
    ) -> Result<(), String> {
        let Some(descriptor) = self.catalog.descriptor(action) else {
            return Err(format!("cannot adapt unknown action {action:?}"));
        };
        if self.adapters.contains_key(&descriptor.id) {
            return Err(format!("action {action:?} already has an adapter"));
        }
        self.adapters
            .insert(descriptor.id.clone(), Arc::new(adapter));
        Ok(())
    }

    fn adapter(&self, action: &ActionId) -> Option<&ActionAdapter<C, P>> {
        self.adapters.get(action)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalReason {
    CancelledByInput,
    FocusChanged,
    DeviceRemoved,
    ConfigurationChanged,
    SessionLocked,
    TargetDestroyed,
    Superseded,
}

pub enum ActionPhase<P> {
    Invoke(P),
    Begin(P),
    Update(P),
    End,
    Cancel(TerminalReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionFeedbackKind {
    Invoked,
    Began,
    Updated,
    Ended,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionFeedback {
    pub action: ActionId,
    pub kind: ActionFeedbackKind,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    WrongActionKind,
    MissingAdapter(ActionId),
    InteractionBusy,
    StaleInteraction,
    Adapter { action: ActionId, message: String },
}

struct ActiveInteraction {
    id: InteractionId,
    invocation: ValidatedActionInvocation,
}

/// Central one-shot/continuous dispatcher. An accepted begin receives exactly one terminal phase.
pub struct ActionDispatcher<C, P> {
    registry: Arc<ActionRegistry<C, P>>,
    active: Option<ActiveInteraction>,
    next_id: u64,
}

impl<C, P> ActionDispatcher<C, P> {
    pub fn new(registry: Arc<ActionRegistry<C, P>>) -> Self {
        Self {
            registry,
            active: None,
            next_id: 1,
        }
    }

    pub fn active_id(&self) -> Option<InteractionId> {
        self.active.as_ref().map(|active| active.id)
    }

    pub fn invoke(
        &mut self,
        context: &mut C,
        invocation: &ValidatedActionInvocation,
        payload: P,
    ) -> Result<ActionFeedback, DispatchError> {
        self.require_kind(invocation, ActionKind::Instant)?;
        self.call(
            context,
            invocation,
            ActionPhase::Invoke(payload),
            ActionFeedbackKind::Invoked,
        )
    }

    pub fn begin(
        &mut self,
        context: &mut C,
        invocation: ValidatedActionInvocation,
        payload: P,
    ) -> Result<(InteractionId, ActionFeedback), DispatchError> {
        self.require_kind(&invocation, ActionKind::Continuous)?;
        if self.active.is_some() {
            return Err(DispatchError::InteractionBusy);
        }
        let feedback = self.call(
            context,
            &invocation,
            ActionPhase::Begin(payload),
            ActionFeedbackKind::Began,
        )?;
        let id = InteractionId(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.active = Some(ActiveInteraction { id, invocation });
        Ok((id, feedback))
    }

    pub fn update(
        &mut self,
        context: &mut C,
        id: InteractionId,
        payload: P,
    ) -> Result<ActionFeedback, DispatchError> {
        let Some(active) = self.active.as_ref() else {
            return Err(DispatchError::StaleInteraction);
        };
        if active.id != id {
            return Err(DispatchError::StaleInteraction);
        }
        let invocation = active.invocation.clone();
        match self.call(
            context,
            &invocation,
            ActionPhase::Update(payload),
            ActionFeedbackKind::Updated,
        ) {
            Ok(feedback) => Ok(feedback),
            Err(error) => {
                let _ = self.cancel(context, TerminalReason::CancelledByInput);
                Err(error)
            }
        }
    }

    pub fn end(
        &mut self,
        context: &mut C,
        id: InteractionId,
    ) -> Result<ActionFeedback, DispatchError> {
        let Some(active) = self.active.take() else {
            return Err(DispatchError::StaleInteraction);
        };
        if active.id != id {
            self.active = Some(active);
            return Err(DispatchError::StaleInteraction);
        }
        self.call(
            context,
            &active.invocation,
            ActionPhase::End,
            ActionFeedbackKind::Ended,
        )
    }

    pub fn cancel(
        &mut self,
        context: &mut C,
        reason: TerminalReason,
    ) -> Result<Option<ActionFeedback>, DispatchError> {
        let Some(active) = self.active.take() else {
            return Ok(None);
        };
        self.call(
            context,
            &active.invocation,
            ActionPhase::Cancel(reason),
            ActionFeedbackKind::Cancelled,
        )
        .map(Some)
    }

    fn require_kind(
        &self,
        invocation: &ValidatedActionInvocation,
        expected: ActionKind,
    ) -> Result<(), DispatchError> {
        let kind = self
            .registry
            .catalog
            .descriptor(invocation.action.as_str())
            .map(|descriptor| descriptor.kind);
        if kind == Some(expected) {
            Ok(())
        } else {
            Err(DispatchError::WrongActionKind)
        }
    }

    fn call(
        &self,
        context: &mut C,
        invocation: &ValidatedActionInvocation,
        phase: ActionPhase<P>,
        kind: ActionFeedbackKind,
    ) -> Result<ActionFeedback, DispatchError> {
        let Some(adapter) = self.registry.adapter(&invocation.action) else {
            return Err(DispatchError::MissingAdapter(invocation.action.clone()));
        };
        let message =
            adapter(context, invocation, phase).map_err(|message| DispatchError::Adapter {
                action: invocation.action.clone(),
                message,
            })?;
        Ok(ActionFeedback {
            action: invocation.action.clone(),
            kind,
            message,
        })
    }
}

/// Canonical built-in compositor metadata. Hosts attach implementations separately.
pub fn built_in_compositor_catalog() -> Arc<ActionCatalog> {
    let mut catalog = ActionCatalog::new();
    for descriptor in built_in_descriptors() {
        catalog
            .register(descriptor)
            .expect("built-in action descriptors are valid and unique");
    }
    Arc::new(catalog)
}

fn built_in_descriptors() -> Vec<ActionDescriptor> {
    let direction = || ActionArgument {
        description: "Cardinal direction".to_owned(),
        schema: ActionValueSchema::Choice {
            values: ["left", "right", "up", "down"].map(str::to_owned).into(),
        },
        required: true,
        default: None,
    };
    let index = ActionArgument {
        description: "Canvas mark index".to_owned(),
        schema: ActionValueSchema::Integer {
            minimum: 0,
            maximum: 9,
        },
        required: true,
        default: None,
    };
    let workspace = ActionArgument {
        description: "Global workspace number".to_owned(),
        schema: ActionValueSchema::Integer {
            minimum: 1,
            maximum: 10,
        },
        required: true,
        default: None,
    };
    vec![
        descriptor(
            "canvas.window.close",
            "Close the focused window",
            ActionKind::Instant,
        ),
        descriptor(
            "canvas.window.cycle-focus",
            "Cycle window focus",
            ActionKind::Instant,
        ),
        descriptor(
            "canvas.overview.toggle",
            "Toggle canvas overview",
            ActionKind::Instant,
        ),
        descriptor(
            "canvas.overview.exit",
            "Leave canvas overview",
            ActionKind::Instant,
        ),
        descriptor(
            "canvas.window.move",
            "Move a window continuously",
            ActionKind::Continuous,
        ),
        descriptor(
            "canvas.window.resize",
            "Resize a window continuously",
            ActionKind::Continuous,
        ),
        descriptor(
            "canvas.viewport.pan",
            "Pan the canvas continuously",
            ActionKind::Continuous,
        ),
        descriptor(
            "canvas.viewport.pinch",
            "Pan and zoom the canvas continuously",
            ActionKind::Continuous,
        ),
        descriptor_with(
            "canvas.viewport.pan-step",
            "Pan the canvas by one step",
            ActionKind::Instant,
            "direction",
            direction(),
        ),
        descriptor_with(
            "canvas.window.move-step",
            "Move the focused window by one grid step",
            ActionKind::Instant,
            "direction",
            direction(),
        ),
        descriptor_with(
            "canvas.window.resize-step",
            "Resize the focused window by one grid step",
            ActionKind::Instant,
            "direction",
            direction(),
        ),
        descriptor_with(
            "canvas.workspace.switch",
            "Switch the locally active output group to a numbered workspace",
            ActionKind::Instant,
            "workspace",
            workspace,
        ),
        descriptor_with(
            "canvas.mark.jump",
            "Jump to a canvas mark",
            ActionKind::Instant,
            "index",
            index.clone(),
        ),
        descriptor_with(
            "canvas.mark.set",
            "Set a canvas mark",
            ActionKind::Instant,
            "index",
            index,
        ),
        descriptor_with(
            "session.vt.switch",
            "Switch to a Linux virtual terminal",
            ActionKind::Instant,
            "vt",
            ActionArgument {
                description: "Virtual terminal number".to_owned(),
                schema: ActionValueSchema::Integer {
                    minimum: 1,
                    maximum: 12,
                },
                required: true,
                default: None,
            },
        )
        .requiring("tty-vt")
        .expect("built-in capability is valid"),
    ]
}

fn descriptor(id: &str, description: &str, kind: ActionKind) -> ActionDescriptor {
    ActionDescriptor::new(id, description, kind).expect("built-in descriptor is valid")
}

fn descriptor_with(
    id: &str,
    description: &str,
    kind: ActionKind,
    argument_name: &str,
    argument: ActionArgument,
) -> ActionDescriptor {
    descriptor(id, description, kind)
        .with_argument(argument_name, argument)
        .expect("built-in argument is valid")
}

/// Canonical defaults, parameterized only for the three legacy CTRL-5 key leaves.
pub fn default_compositor_bindings(
    close_key: impl Into<String>,
    cycle_key: impl Into<String>,
    overview_key: impl Into<String>,
) -> BindingDocument {
    let mut bindings = vec![
        key_binding(
            "overview-toggle",
            BindingContext::Canvas,
            overview_key,
            &[Modifier::Logo],
            "canvas.overview.toggle",
            None,
        ),
        key_binding(
            "overview-exit",
            BindingContext::Overview,
            "Escape",
            &[],
            "canvas.overview.exit",
            None,
        ),
        key_binding(
            "window-close",
            BindingContext::Window,
            close_key,
            &[Modifier::Logo],
            "canvas.window.close",
            None,
        ),
        key_binding(
            "window-cycle-focus",
            BindingContext::Canvas,
            cycle_key,
            &[Modifier::Alt],
            "canvas.window.cycle-focus",
            None,
        ),
    ];

    for (suffix, key, direction) in directional_keys() {
        bindings.push(key_binding(
            &format!("viewport-pan-{suffix}"),
            BindingContext::Canvas,
            key,
            &[Modifier::Logo],
            "canvas.viewport.pan-step",
            Some(("direction", ActionValue::String(direction.to_owned()))),
        ));
        bindings.push(key_binding(
            &format!("window-move-{suffix}"),
            BindingContext::Window,
            key,
            &[Modifier::Logo, Modifier::Shift],
            "canvas.window.move-step",
            Some(("direction", ActionValue::String(direction.to_owned()))),
        ));
        bindings.push(key_binding(
            &format!("window-resize-{suffix}"),
            BindingContext::Window,
            key,
            &[Modifier::Logo, Modifier::Control],
            "canvas.window.resize-step",
            Some(("direction", ActionValue::String(direction.to_owned()))),
        ));
    }

    for index in 0..=9 {
        let workspace = if index == 0 { 10 } else { index };
        bindings.push(key_binding(
            &format!("workspace-switch-{workspace}"),
            BindingContext::Canvas,
            index.to_string(),
            &[Modifier::Logo],
            "canvas.workspace.switch",
            Some(("workspace", ActionValue::Integer(workspace))),
        ));
        bindings.push(key_binding(
            &format!("mark-jump-{index}"),
            BindingContext::Canvas,
            index.to_string(),
            &[Modifier::Alt, Modifier::Logo],
            "canvas.mark.jump",
            Some(("index", ActionValue::Integer(index))),
        ));
        bindings.push(key_binding(
            &format!("mark-set-{index}"),
            BindingContext::Canvas,
            index.to_string(),
            &[Modifier::Alt, Modifier::Logo, Modifier::Shift],
            "canvas.mark.set",
            Some(("index", ActionValue::Integer(index))),
        ));
    }

    for vt in 1..=12 {
        bindings.push(key_binding(
            &format!("vt-chord-{vt}"),
            BindingContext::Global,
            format!("F{vt}"),
            &[Modifier::Control, Modifier::Alt],
            "session.vt.switch",
            Some(("vt", ActionValue::Integer(vt))),
        ));
        bindings.push(key_binding(
            &format!("vt-dedicated-{vt}"),
            BindingContext::Global,
            format!("XF86Switch_VT_{vt}"),
            &[],
            "session.vt.switch",
            Some(("vt", ActionValue::Integer(vt))),
        ));
    }

    bindings.extend([
        continuous_button_binding(
            "pointer-window-move",
            BindingContext::Window,
            ButtonCode::Primary,
            &[Modifier::Logo],
            GestureOrigin::Window,
            "canvas.window.move",
        ),
        continuous_button_binding(
            "pointer-window-resize",
            BindingContext::Window,
            ButtonCode::Secondary,
            &[Modifier::Logo],
            GestureOrigin::Window,
            "canvas.window.resize",
        ),
        continuous_button_binding(
            "pointer-frame-move",
            BindingContext::WindowFrame,
            ButtonCode::Primary,
            &[],
            GestureOrigin::WindowFrame,
            "canvas.window.move",
        ),
        continuous_button_binding(
            "pointer-empty-pan",
            BindingContext::EmptyCanvas,
            ButtonCode::Primary,
            &[],
            GestureOrigin::EmptyCanvas,
            "canvas.viewport.pan",
        ),
        continuous_gesture_binding(
            "touchpad-three-finger-pan",
            GestureKind::Swipe,
            3,
            "canvas.viewport.pan",
        ),
        continuous_gesture_binding(
            "touchpad-three-finger-pinch",
            GestureKind::Pinch,
            3,
            "canvas.viewport.pinch",
        ),
    ]);
    BindingDocument::new(bindings)
}

fn directional_keys() -> [(&'static str, &'static str, &'static str); 8] {
    [
        ("left", "Left", "left"),
        ("right", "Right", "right"),
        ("up", "Up", "up"),
        ("down", "Down", "down"),
        ("h", "h", "left"),
        ("j", "j", "down"),
        ("k", "k", "up"),
        ("l", "l", "right"),
    ]
}

fn key_binding(
    id: &str,
    context: BindingContext,
    key: impl Into<String>,
    modifiers: &[Modifier],
    action: &str,
    argument: Option<(&str, ActionValue)>,
) -> BindingEntry {
    let mut invocation = ActionInvocation::new(action);
    if let Some((name, value)) = argument {
        invocation = invocation.with_argument(name, value);
    }
    BindingEntry {
        id: id.to_owned(),
        context,
        trigger: Trigger::Key {
            key: key.into(),
            modifiers: modifiers.to_vec(),
            phase: KeyPhase::Press,
        },
        invocation,
    }
}

fn continuous_button_binding(
    id: &str,
    context: BindingContext,
    button: ButtonCode,
    modifiers: &[Modifier],
    origin: GestureOrigin,
    action: &str,
) -> BindingEntry {
    BindingEntry {
        id: id.to_owned(),
        context,
        trigger: Trigger::Button {
            button,
            modifiers: modifiers.to_vec(),
            device: DeviceClass::AnyPointer,
            origin,
            phase: KeyPhase::Press,
        },
        invocation: ActionInvocation::new(action),
    }
}

fn continuous_gesture_binding(
    id: &str,
    gesture: GestureKind,
    fingers: u8,
    action: &str,
) -> BindingEntry {
    BindingEntry {
        id: id.to_owned(),
        context: BindingContext::Canvas,
        trigger: Trigger::Gesture {
            gesture,
            device: DeviceClass::Touchpad,
            fingers,
            origin: GestureOrigin::Anywhere,
            direction: None,
            activation: GestureActivation::Begin,
        },
        invocation: ActionInvocation::new(action),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn full_environment() -> ActionEnvironment {
        ActionEnvironment::default()
            .with_device(DeviceClass::Keyboard)
            .with_device(DeviceClass::Mouse)
            .with_device(DeviceClass::Touchpad)
            .with_capability("tty-vt")
    }

    #[test]
    fn defaults_compile_and_normalize_modifier_and_key_order() {
        let runtime = BindingRuntime::new(
            built_in_compositor_catalog(),
            full_environment(),
            default_compositor_bindings("Escape", "Tab", "o"),
        )
        .unwrap();
        let snapshot = runtime.snapshot();
        assert!(snapshot.diagnostics().is_empty());
        let binding = snapshot
            .bindings()
            .iter()
            .find(|binding| binding.id == "window-move-left")
            .unwrap();
        assert!(matches!(
            &binding.trigger,
            CompiledTrigger::Key { key, modifiers, .. }
                if key == "left"
                    && modifiers.ordered().collect::<Vec<_>>()
                        == vec![Modifier::Shift, Modifier::Logo]
        ));
        let workspace = snapshot
            .bindings()
            .iter()
            .find(|binding| binding.id == "workspace-switch-10")
            .unwrap();
        assert!(matches!(
            &workspace.trigger,
            CompiledTrigger::Key { key, modifiers, .. }
                if key == "0"
                    && modifiers.ordered().collect::<Vec<_>>() == vec![Modifier::Logo]
        ));
        assert_eq!(workspace.invocation.integer("workspace"), Some(10));
        let mark = snapshot
            .bindings()
            .iter()
            .find(|binding| binding.id == "mark-jump-1")
            .unwrap();
        assert!(matches!(
            &mark.trigger,
            CompiledTrigger::Key { modifiers, .. }
                if modifiers.ordered().collect::<Vec<_>>()
                    == vec![Modifier::Alt, Modifier::Logo]
        ));
    }

    #[test]
    fn conflicting_contexts_reject_without_replacing_last_good() {
        let initial = default_compositor_bindings("Escape", "Tab", "o");
        let mut runtime = BindingRuntime::new(
            built_in_compositor_catalog(),
            full_environment(),
            initial.clone(),
        )
        .unwrap();
        let generation = runtime.snapshot().generation();
        let mut invalid = initial;
        invalid.bindings.push(key_binding(
            "duplicate-close",
            BindingContext::Global,
            "Escape",
            &[Modifier::Logo],
            "canvas.overview.toggle",
            None,
        ));
        let publication = runtime.publish(invalid);
        assert!(!publication.accepted);
        assert_eq!(publication.effective.generation(), generation);
        assert!(
            publication
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == BindingDiagnosticCode::Conflict)
        );
    }

    #[test]
    fn invalid_arguments_and_unknown_actions_are_structured_errors() {
        let catalog = built_in_compositor_catalog();
        let mut document = BindingDocument::new(vec![key_binding(
            "bad-mark",
            BindingContext::Canvas,
            "1",
            &[Modifier::Logo],
            "canvas.mark.jump",
            Some(("index", ActionValue::Integer(99))),
        )]);
        document.bindings.push(key_binding(
            "unknown",
            BindingContext::Canvas,
            "x",
            &[Modifier::Logo],
            "canvas.unknown",
            None,
        ));
        let diagnostics = compile_bindings(&catalog, &full_environment(), &document).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == BindingDiagnosticCode::InvalidArgument })
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == BindingDiagnosticCode::UnknownAction })
        );
    }

    #[test]
    fn client_two_finger_and_non_edge_touch_bindings_are_rejected() {
        let catalog = built_in_compositor_catalog();
        for (id, device, origin) in [
            (
                "client-scroll",
                DeviceClass::Touchpad,
                GestureOrigin::Anywhere,
            ),
            (
                "client-touch",
                DeviceClass::Touchscreen,
                GestureOrigin::Window,
            ),
        ] {
            let document = BindingDocument::new(vec![BindingEntry {
                id: id.to_owned(),
                context: BindingContext::Canvas,
                trigger: Trigger::Gesture {
                    gesture: GestureKind::Swipe,
                    device,
                    fingers: 2,
                    origin,
                    direction: None,
                    activation: GestureActivation::Begin,
                },
                invocation: ActionInvocation::new("canvas.viewport.pan"),
            }]);
            let diagnostics =
                compile_bindings(&catalog, &full_environment(), &document).unwrap_err();
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == BindingDiagnosticCode::ReservedClientGesture
            }));
        }
    }

    #[test]
    fn unsupported_devices_remain_visible_but_not_effective() {
        let environment = ActionEnvironment::default()
            .with_device(DeviceClass::Keyboard)
            .with_device(DeviceClass::Mouse);
        let runtime = BindingRuntime::new(
            built_in_compositor_catalog(),
            environment,
            default_compositor_bindings("Escape", "Tab", "o"),
        )
        .unwrap();
        let snapshot = runtime.snapshot();
        assert_eq!(
            snapshot
                .bindings()
                .iter()
                .filter(|binding| {
                    binding.availability == BindingAvailability::UnsupportedDevice
                })
                .count(),
            2
        );
        assert_eq!(
            snapshot
                .diagnostics()
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code == BindingDiagnosticCode::UnsupportedDevice
                })
                .count(),
            2
        );
    }

    #[test]
    fn remapping_a_phase_two_shortcut_requires_only_a_new_document() {
        let mut document = default_compositor_bindings("Escape", "Tab", "o");
        let close = document
            .bindings
            .iter_mut()
            .find(|binding| binding.id == "window-close")
            .unwrap();
        close.trigger = Trigger::Key {
            key: "w".to_owned(),
            modifiers: vec![Modifier::Control, Modifier::Logo],
            phase: KeyPhase::Press,
        };
        let runtime =
            BindingRuntime::new(built_in_compositor_catalog(), full_environment(), document)
                .unwrap();
        let snapshot = runtime.snapshot();
        assert!(
            snapshot
                .find(
                    BindingContext::Window,
                    &RuntimeTrigger::key(
                        "w",
                        ModifierSet::new([Modifier::Logo, Modifier::Control]),
                        KeyPhase::Press,
                    ),
                )
                .is_some_and(|binding| binding.invocation.action.as_str() == "canvas.window.close")
        );
        assert!(
            snapshot
                .find(
                    BindingContext::Window,
                    &RuntimeTrigger::key(
                        "Escape",
                        ModifierSet::new([Modifier::Logo]),
                        KeyPhase::Press,
                    ),
                )
                .is_none()
        );
    }

    #[test]
    fn malformed_json_preserves_the_exact_effective_generation() {
        let mut runtime = BindingRuntime::new(
            built_in_compositor_catalog(),
            full_environment(),
            default_compositor_bindings("Escape", "Tab", "o"),
        )
        .unwrap();
        let before = runtime.snapshot();
        let publication = runtime.publish_json("{ not-json");
        assert!(!publication.accepted);
        assert_eq!(publication.effective.generation(), before.generation());
        assert_eq!(publication.effective.bindings(), before.bindings());
        assert_eq!(
            publication.diagnostics[0].code,
            BindingDiagnosticCode::InvalidDocument
        );
    }

    #[test]
    fn gesture_conflicts_compare_device_fingers_origin_and_context() {
        let catalog = built_in_compositor_catalog();
        let gesture = |id: &str,
                       context: BindingContext,
                       device: DeviceClass,
                       fingers: u8,
                       origin: GestureOrigin| BindingEntry {
            id: id.to_owned(),
            context,
            trigger: Trigger::Gesture {
                gesture: GestureKind::Swipe,
                device,
                fingers,
                origin,
                direction: None,
                activation: GestureActivation::Begin,
            },
            invocation: ActionInvocation::new("canvas.viewport.pan"),
        };
        let non_conflicting = BindingDocument::new(vec![
            gesture(
                "empty-three",
                BindingContext::EmptyCanvas,
                DeviceClass::Touchpad,
                3,
                GestureOrigin::EmptyCanvas,
            ),
            gesture(
                "window-three",
                BindingContext::Window,
                DeviceClass::Touchpad,
                3,
                GestureOrigin::Window,
            ),
            gesture(
                "empty-four",
                BindingContext::EmptyCanvas,
                DeviceClass::Touchpad,
                4,
                GestureOrigin::EmptyCanvas,
            ),
            gesture(
                "touchscreen-three",
                BindingContext::EmptyCanvas,
                DeviceClass::Touchscreen,
                3,
                GestureOrigin::EmptyCanvas,
            ),
        ]);
        let environment = full_environment().with_device(DeviceClass::Touchscreen);
        assert!(compile_bindings(&catalog, &environment, &non_conflicting).is_ok());

        let conflicting = BindingDocument::new(vec![
            gesture(
                "global-three",
                BindingContext::Canvas,
                DeviceClass::Touchpad,
                3,
                GestureOrigin::Anywhere,
            ),
            gesture(
                "window-three",
                BindingContext::Window,
                DeviceClass::Touchpad,
                3,
                GestureOrigin::Window,
            ),
        ]);
        let diagnostics = compile_bindings(&catalog, &environment, &conflicting).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == BindingDiagnosticCode::Conflict)
        );
    }

    #[derive(Default)]
    struct DispatchProbe {
        phases: Mutex<Vec<&'static str>>,
    }

    #[test]
    fn continuous_dispatch_has_exactly_one_terminal_phase() {
        let catalog = built_in_compositor_catalog();
        let invocation = catalog
            .validate_invocation(&ActionInvocation::new("canvas.viewport.pan"))
            .unwrap();
        let mut registry = ActionRegistry::<DispatchProbe, ()>::new(catalog);
        registry
            .register_adapter("canvas.viewport.pan", |probe, _, phase| {
                probe.phases.lock().unwrap().push(match phase {
                    ActionPhase::Begin(()) => "begin",
                    ActionPhase::Update(()) => "update",
                    ActionPhase::End => "end",
                    ActionPhase::Cancel(_) => "cancel",
                    ActionPhase::Invoke(()) => "invoke",
                });
                Ok(None)
            })
            .unwrap();
        let mut dispatcher = ActionDispatcher::new(Arc::new(registry));
        let mut probe = DispatchProbe::default();
        let (id, _) = dispatcher.begin(&mut probe, invocation, ()).unwrap();
        dispatcher.update(&mut probe, id, ()).unwrap();
        dispatcher
            .cancel(&mut probe, TerminalReason::ConfigurationChanged)
            .unwrap();
        assert!(
            dispatcher
                .cancel(&mut probe, TerminalReason::DeviceRemoved)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            *probe.phases.lock().unwrap(),
            vec!["begin", "update", "cancel"]
        );
    }
}

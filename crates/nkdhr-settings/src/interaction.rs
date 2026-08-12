//! Host-independent Settings presentation for the shared action/binding registry.

use std::collections::BTreeMap;
use std::sync::Arc;

use nkdhr_ui::{
    ActionFeedback, ActionKind, ActionValue, BindingAvailability, BindingDiagnostic,
    BindingPublication, BindingSnapshot, ButtonCode, CompiledTrigger, GestureActivation,
    GestureDirection, GestureKind, KeyPhase, Modifier,
};

/// One discoverable row. A future SHELL-6 view chooses its visual composition;
/// this model owns no radius, spacing, color or motion decision.
#[derive(Debug, Clone, PartialEq)]
pub struct BindingSettingsRow {
    pub binding_id: String,
    pub action: String,
    pub action_description: String,
    pub action_kind: ActionKind,
    pub trigger: String,
    pub arguments: BTreeMap<String, ActionValue>,
    pub availability: BindingAvailability,
}

/// Settings and compositor consume the same immutable effective generation.
/// Rejected candidates update diagnostics but never replace the rows.
#[derive(Debug, Clone)]
pub struct BindingSettingsModel {
    effective: Arc<BindingSnapshot>,
    diagnostics: Arc<[BindingDiagnostic]>,
    feedback: Option<ActionFeedback>,
}

impl BindingSettingsModel {
    pub fn new(effective: Arc<BindingSnapshot>) -> Self {
        Self {
            diagnostics: effective.diagnostics().to_vec().into(),
            effective,
            feedback: None,
        }
    }

    pub fn generation(&self) -> u64 {
        self.effective.generation()
    }

    pub fn diagnostics(&self) -> &[BindingDiagnostic] {
        &self.diagnostics
    }

    pub fn feedback(&self) -> Option<&ActionFeedback> {
        self.feedback.as_ref()
    }

    pub fn apply_publication(&mut self, publication: &BindingPublication) {
        if publication.accepted {
            self.effective = Arc::clone(&publication.effective);
        }
        self.diagnostics = Arc::clone(&publication.diagnostics);
    }

    pub fn record_feedback(&mut self, feedback: ActionFeedback) {
        self.feedback = Some(feedback);
    }

    pub fn clear_feedback(&mut self) {
        self.feedback = None;
    }

    pub fn rows(&self) -> Vec<BindingSettingsRow> {
        self.effective
            .bindings()
            .iter()
            .filter_map(|binding| {
                let descriptor = self
                    .effective
                    .catalog()
                    .descriptor(binding.invocation.action.as_str())?;
                Some(BindingSettingsRow {
                    binding_id: binding.id.clone(),
                    action: descriptor.id.as_str().to_owned(),
                    action_description: descriptor.description.clone(),
                    action_kind: descriptor.kind,
                    trigger: format_trigger(&binding.trigger),
                    arguments: binding.invocation.arguments.clone(),
                    availability: binding.availability.clone(),
                })
            })
            .collect()
    }
}

fn format_trigger(trigger: &CompiledTrigger) -> String {
    match trigger {
        CompiledTrigger::Key {
            key,
            modifiers,
            phase,
        } => join_trigger(
            modifiers.ordered().map(modifier_name),
            key,
            (*phase != KeyPhase::Press).then_some("release"),
        ),
        CompiledTrigger::Button {
            button,
            modifiers,
            device,
            origin,
            phase,
        } => join_trigger(
            modifiers.ordered().map(modifier_name),
            &format!("{device:?}:{origin:?}:{}", button_name(*button)),
            (*phase != KeyPhase::Press).then_some("release"),
        ),
        CompiledTrigger::Gesture {
            gesture,
            device,
            fingers,
            origin,
            direction,
            activation,
        } => {
            let direction = direction.map(direction_name).unwrap_or("any");
            format!(
                "{device:?}:{fingers}-finger:{}:{origin:?}:{direction}:{}",
                gesture_name(*gesture),
                activation_name(*activation)
            )
            .to_lowercase()
        }
    }
}

fn join_trigger<'a>(
    modifiers: impl Iterator<Item = &'a str>,
    terminal: &str,
    suffix: Option<&str>,
) -> String {
    let mut parts = modifiers.map(str::to_owned).collect::<Vec<_>>();
    parts.push(terminal.to_owned());
    parts.extend(suffix.map(str::to_owned));
    parts.join("+")
}

const fn modifier_name(modifier: Modifier) -> &'static str {
    match modifier {
        Modifier::Control => "Ctrl",
        Modifier::Alt => "Alt",
        Modifier::Shift => "Shift",
        Modifier::Logo => "Super",
    }
}

const fn button_name(button: ButtonCode) -> &'static str {
    match button {
        ButtonCode::Primary => "primary",
        ButtonCode::Secondary => "secondary",
        ButtonCode::Middle => "middle",
        ButtonCode::Back => "back",
        ButtonCode::Forward => "forward",
    }
}

const fn gesture_name(gesture: GestureKind) -> &'static str {
    match gesture {
        GestureKind::Swipe => "swipe",
        GestureKind::Pinch => "pinch",
        GestureKind::Hold => "hold",
        GestureKind::Touch => "touch",
    }
}

const fn direction_name(direction: GestureDirection) -> &'static str {
    match direction {
        GestureDirection::Left => "left",
        GestureDirection::Right => "right",
        GestureDirection::Up => "up",
        GestureDirection::Down => "down",
    }
}

const fn activation_name(activation: GestureActivation) -> &'static str {
    match activation {
        GestureActivation::Begin => "begin",
        GestureActivation::End => "end",
    }
}

#[cfg(test)]
mod tests {
    use nkdhr_ui::{
        ActionEnvironment, BindingContext, BindingDocument, BindingEntry, BindingRuntime,
        DeviceClass, ModifierSet, RuntimeTrigger, Trigger, built_in_compositor_catalog,
        default_compositor_bindings,
    };

    use super::*;

    fn environment() -> ActionEnvironment {
        ActionEnvironment::default()
            .with_device(DeviceClass::Keyboard)
            .with_device(DeviceClass::Mouse)
            .with_device(DeviceClass::Touchpad)
            .with_capability("tty-vt")
    }

    #[test]
    fn settings_rows_are_the_exact_effective_compositor_snapshot() {
        let catalog = built_in_compositor_catalog();
        let runtime = BindingRuntime::new(
            Arc::clone(&catalog),
            environment(),
            default_compositor_bindings("Escape", "Tab", "o"),
        )
        .unwrap();
        let snapshot = runtime.snapshot();
        let model = BindingSettingsModel::new(Arc::clone(&snapshot));
        assert_eq!(model.generation(), snapshot.generation());
        assert_eq!(model.rows().len(), snapshot.bindings().len());
        let close = model
            .rows()
            .into_iter()
            .find(|row| row.binding_id == "window-close")
            .unwrap();
        assert_eq!(close.trigger, "Super+escape");
        assert_eq!(close.action, "canvas.window.close");
    }

    #[test]
    fn rejected_candidate_exposes_diagnostics_without_changing_rows() {
        let catalog = built_in_compositor_catalog();
        let mut runtime = BindingRuntime::new(
            Arc::clone(&catalog),
            environment(),
            default_compositor_bindings("Escape", "Tab", "o"),
        )
        .unwrap();
        let mut model = BindingSettingsModel::new(runtime.snapshot());
        let generation = model.generation();
        let rows = model.rows();
        let invalid = BindingDocument::new(vec![BindingEntry {
            id: "invalid".to_owned(),
            context: BindingContext::Global,
            trigger: Trigger::Key {
                key: "x".to_owned(),
                modifiers: vec![],
                phase: KeyPhase::Press,
            },
            invocation: nkdhr_ui::ActionInvocation::new("unknown.action"),
        }]);
        let publication = runtime.publish(invalid);
        model.apply_publication(&publication);
        assert!(!publication.accepted);
        assert_eq!(model.generation(), generation);
        assert_eq!(model.rows(), rows);
        assert!(!model.diagnostics().is_empty());
        assert!(
            model
                .effective
                .find(
                    BindingContext::Window,
                    &RuntimeTrigger::key(
                        "Escape",
                        ModifierSet::new([Modifier::Logo]),
                        KeyPhase::Press,
                    ),
                )
                .is_some()
        );
    }
}

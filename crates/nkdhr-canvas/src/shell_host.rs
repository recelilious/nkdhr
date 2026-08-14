//! Compositor ownership boundary for output-local retained shell surfaces.

use std::collections::{BTreeMap, BTreeSet};

use nkdhr_render::{DisplayList, TextureStore};
use nkdhr_ui::{
    DispatchResult, MaterialCapabilities, Modifiers, PointerButton, Size, ThemeRuntime, UiEvent,
    UiSurface,
};
use smithay::backend::input::ButtonState;
use smithay::utils::{Logical, Point};

use crate::canvas::output_group::OutputLayout;

pub struct ShellHost {
    theme_runtime: ThemeRuntime,
    outputs: BTreeMap<String, ShellNode>,
    pointer_output: Option<String>,
    keyboard_output: Option<String>,
    button_capture_output: Option<String>,
}

struct ShellNode {
    logical_size: Size,
    output_scale: f32,
    surface: Box<dyn UiSurface>,
}

pub struct ShellRenderData<'a> {
    pub display_list: &'a DisplayList,
    pub textures: &'a TextureStore,
    pub commit: u64,
}

impl Default for ShellHost {
    fn default() -> Self {
        Self::new(ThemeRuntime::watch_ctrl5())
    }
}

impl ShellHost {
    pub fn new(theme_runtime: ThemeRuntime) -> Self {
        Self {
            theme_runtime,
            outputs: BTreeMap::new(),
            pointer_output: None,
            keyboard_output: None,
            button_capture_output: None,
        }
    }

    pub fn reconcile(&mut self, layout: &OutputLayout) {
        let connected = layout
            .groups
            .iter()
            .flat_map(|group| &group.outputs)
            .map(|output| output.name.clone())
            .collect::<BTreeSet<_>>();
        if self
            .pointer_output
            .as_ref()
            .is_some_and(|output| !connected.contains(output))
            && let Some(output) = self.pointer_output.take()
        {
            self.dispatch(&output, UiEvent::PointerLeft);
        }
        for owner in [&mut self.keyboard_output, &mut self.button_capture_output] {
            if owner
                .as_ref()
                .is_some_and(|output| !connected.contains(output))
            {
                *owner = None;
            }
        }
        self.outputs.retain(|name, _| connected.contains(name));

        for output in layout.groups.iter().flat_map(|group| &group.outputs) {
            let logical_size =
                Size::new(output.logical_size.w as f32, output.logical_size.h as f32);
            let output_scale = output.scale as f32;
            if let Some(node) = self.outputs.get_mut(&output.name) {
                node.logical_size = logical_size;
                node.output_scale = output_scale;
                continue;
            }
            let capabilities = MaterialCapabilities {
                backdrop_blur: true,
                reduced_transparency: false,
                high_contrast: false,
            };
            match nkdhr_shell::ShellSurface::new(
                logical_size,
                output_scale,
                capabilities,
                self.theme_runtime.clone(),
            ) {
                Ok(surface) => {
                    self.outputs.insert(
                        output.name.clone(),
                        ShellNode {
                            logical_size,
                            output_scale,
                            surface: Box::new(surface),
                        },
                    );
                }
                Err(error) => {
                    eprintln!(
                        "nkdhr-canvas: output-local shell for {:?} failed: {error}",
                        output.name
                    );
                }
            }
        }
    }

    pub fn render_data(&mut self, output: &str) -> Option<ShellRenderData<'_>> {
        let node = self.outputs.get_mut(output)?;
        if let Err(error) = node.surface.render(node.logical_size, node.output_scale) {
            eprintln!("nkdhr-canvas: output-local shell frame failed: {error}");
            return None;
        }
        Some(ShellRenderData {
            display_list: node.surface.display_list(),
            textures: node.surface.textures(),
            commit: node.surface.commit(),
        })
    }

    pub fn frame_requested(&mut self) -> bool {
        self.outputs
            .values_mut()
            .any(|node| node.surface.frame_requested())
    }

    pub fn pointer_motion(&mut self, output: &str, position: Point<f64, Logical>) -> bool {
        self.leave_previous_output(output);
        self.pointer_output = Some(output.to_owned());
        self.dispatch(
            output,
            UiEvent::PointerMoved {
                position: nkdhr_render::Point::new(position.x as f32, position.y as f32),
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn pointer_button(
        &mut self,
        output: &str,
        position: Point<f64, Logical>,
        button: u32,
        state: ButtonState,
        modifiers: Modifiers,
        click_count: u8,
    ) -> bool {
        let position = nkdhr_render::Point::new(position.x as f32, position.y as f32);
        let button = pointer_button(button);
        let event = match state {
            ButtonState::Pressed => UiEvent::PointerDown {
                position,
                button,
                modifiers,
                click_count,
            },
            ButtonState::Released => UiEvent::PointerUp {
                position,
                button,
                modifiers,
                click_count,
            },
        };
        let captured = (state == ButtonState::Released)
            .then(|| self.button_capture_output.take())
            .flatten();
        let target = captured.as_deref().unwrap_or(output);
        let handled = self.dispatch(target, event);
        if state == ButtonState::Pressed && handled {
            self.button_capture_output = Some(output.to_owned());
        }
        if handled
            && self
                .outputs
                .get(output)
                .is_some_and(|node| node.surface.keyboard_focus().is_some())
        {
            self.keyboard_output = Some(output.to_owned());
        }
        handled || captured.is_some()
    }

    pub fn pointer_axis(
        &mut self,
        output: &str,
        position: Point<f64, Logical>,
        horizontal: f64,
        vertical: f64,
        modifiers: Modifiers,
    ) -> bool {
        self.dispatch(
            output,
            UiEvent::PointerScroll {
                position: nkdhr_render::Point::new(position.x as f32, position.y as f32),
                delta_x: horizontal as f32,
                delta_y: vertical as f32,
                modifiers,
            },
        )
    }

    pub fn keyboard(&mut self, event: UiEvent) -> bool {
        let Some(output) = self.keyboard_output.clone() else {
            return false;
        };
        self.dispatch(&output, event)
    }

    fn dispatch(&mut self, output: &str, event: UiEvent) -> bool {
        let Some(node) = self.outputs.get_mut(output) else {
            return false;
        };
        match node.surface.dispatch(&event) {
            Ok(DispatchResult { handled, .. }) => handled,
            Err(error) => {
                eprintln!("nkdhr-canvas: output-local shell input failed: {error}");
                true
            }
        }
    }

    fn leave_previous_output(&mut self, output: &str) {
        let Some(previous) = self
            .pointer_output
            .as_deref()
            .filter(|previous| *previous != output)
            .map(str::to_owned)
        else {
            return;
        };
        self.dispatch(&previous, UiEvent::PointerLeft);
    }
}

fn pointer_button(button: u32) -> PointerButton {
    match button {
        0x110 => PointerButton::Primary,
        0x111 => PointerButton::Secondary,
        0x112 => PointerButton::Middle,
        other => PointerButton::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::output_group::ConnectedOutput;
    use nkdhr_ipc::CanvasOutputGroups;
    use smithay::utils::Physical;

    fn layout(names: &[&str]) -> OutputLayout {
        let connected = names
            .iter()
            .map(|name| ConnectedOutput {
                name: (*name).to_owned(),
                physical_size: smithay::utils::Size::<i32, Physical>::from((1280, 800)),
            })
            .collect::<Vec<_>>();
        OutputLayout::resolve(&CanvasOutputGroups::new(), &connected)
    }

    #[test]
    fn reconcile_owns_exactly_the_connected_output_surfaces() {
        let mut host = ShellHost::new(ThemeRuntime::default());
        host.reconcile(&layout(&["A", "B"]));
        assert_eq!(host.outputs.keys().cloned().collect::<Vec<_>>(), ["A", "B"]);

        host.pointer_output = Some("A".to_owned());
        host.keyboard_output = Some("A".to_owned());
        host.button_capture_output = Some("A".to_owned());
        host.reconcile(&layout(&["B", "C"]));

        assert_eq!(host.outputs.keys().cloned().collect::<Vec<_>>(), ["B", "C"]);
        assert_eq!(host.pointer_output, None);
        assert_eq!(host.keyboard_output, None);
        assert_eq!(host.button_capture_output, None);
    }
}

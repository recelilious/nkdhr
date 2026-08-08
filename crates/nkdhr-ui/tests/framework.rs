use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use nkdhr_render::{Color, DisplayListBuilder, Point, Rect};
use nkdhr_ui::{
    Align, Alignment, ArrangeCtx, Axis, Clock, Constraints, CrossAxisAlignment, Element, EventCtx,
    Flex, Invalidation, Key, MainAxisAlignment, ManualClock, MeasureCtx, Modifiers, PaintCtx,
    PointerButton, Reactive, SemanticRole, Semantics, SemanticsCtx, Size, Stack, Timeline, UiError,
    UiEvent, UiRoot, UpdateCtx, Widget,
};

#[derive(Debug, Default)]
struct ProbeState {
    updates: usize,
    pointer_downs: usize,
    focused: bool,
    hovered: bool,
    observed_focus_during_key: bool,
}

#[derive(Debug, Clone)]
struct Probe {
    name: &'static str,
    desired: Size,
    color: Color,
    interactive: bool,
    handle_pointer: bool,
    log: Rc<RefCell<Vec<String>>>,
}

impl Probe {
    fn leaf(name: &'static str, width: f32) -> Self {
        Self {
            name,
            desired: Size::new(width, 20.0),
            color: Color::from_srgba8(120, 140, 180, 255),
            interactive: true,
            handle_pointer: true,
            log: Rc::default(),
        }
    }
}

impl Widget for Probe {
    fn create_state(&self) -> Box<dyn Any> {
        Box::<ProbeState>::default()
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous.downcast_ref::<Self>().unwrap();
        ctx.state_mut::<ProbeState>().unwrap().updates += 1;
        if previous.desired != self.desired {
            ctx.invalidate(Invalidation::LAYOUT);
        } else if previous.color != self.color {
            ctx.invalidate(Invalidation::PAINT);
        }
    }

    fn measure(
        &self,
        _ctx: &mut MeasureCtx<'_>,
        constraints: Constraints,
    ) -> Result<Size, UiError> {
        Ok(constraints.constrain(self.desired))
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        ctx.builder().rect(rect, self.color)?;
        Ok(())
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        self.log
            .borrow_mut()
            .push(format!("{}:{event:?}", self.name));
        match event {
            UiEvent::PointerDown { .. } => {
                ctx.state_mut::<ProbeState>()?.pointer_downs += 1;
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.invalidate(Invalidation::PAINT);
                if self.handle_pointer {
                    ctx.set_handled();
                }
            }
            UiEvent::PointerUp { .. } => {
                ctx.release_pointer();
                if self.handle_pointer {
                    ctx.set_handled();
                }
            }
            UiEvent::FocusChanged(focused) => {
                ctx.state_mut::<ProbeState>()?.focused = *focused;
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::HoverChanged(hovered) => {
                ctx.state_mut::<ProbeState>()?.hovered = *hovered;
                ctx.invalidate(Invalidation::PAINT);
            }
            UiEvent::KeyDown { .. } => {
                let focused = ctx.focused();
                ctx.state_mut::<ProbeState>()?.observed_focus_during_key = focused;
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let state = ctx.state_mut::<ProbeState>().unwrap();
        Semantics {
            role: SemanticRole::Button,
            label: Some(self.name.to_owned()),
            value: Some(format!("presses={}", state.pointer_downs)),
            enabled: true,
            focusable: self.interactive,
        }
    }

    fn focusable(&self) -> bool {
        self.interactive
    }

    fn accepts_pointer(&self) -> bool {
        self.interactive
    }
}

#[derive(Debug, Clone)]
struct EventContainer {
    log: Rc<RefCell<Vec<String>>>,
}

impl Widget for EventContainer {
    fn event(&self, _ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        self.log.borrow_mut().push(format!("parent:{event:?}"));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct FocusScope;

impl Widget for FocusScope {
    fn focus_scope(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy)]
struct Bare;

impl Widget for Bare {}

#[derive(Debug, Clone, Copy)]
struct Hidden;

impl Widget for Hidden {
    fn paint(&self, _ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct ClipHost;

impl Widget for ClipHost {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() == 1 {
            let _ = ctx.measure_child(0, Constraints::new(Size::ZERO, Size::new(40.0, 20.0))?)?;
        }
        Ok(constraints.max())
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() == 1 {
            ctx.arrange_child(0, Rect::new(rect.x + 80.0, rect.y + 10.0, 40.0, 20.0))?;
        }
        Ok(())
    }

    fn clips_children(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
struct ReactivePaint {
    value: Reactive<bool>,
    paints: Rc<Cell<usize>>,
}

#[derive(Debug, Clone)]
struct ReactiveSemantics {
    label: Reactive<String>,
}

impl Widget for ReactiveSemantics {
    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            label: Some(ctx.watch(&self.label, Invalidation::SEMANTICS)),
            ..Semantics::default()
        }
    }
}

impl Widget for ReactivePaint {
    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let active = ctx.watch(&self.value, Invalidation::PAINT);
        self.paints.set(self.paints.get() + 1);
        let color = if active {
            Color::from_srgba8(255, 255, 255, 255)
        } else {
            Color::from_srgba8(0, 0, 0, 255)
        };
        let rect = ctx.rect();
        ctx.builder().rect(rect, color)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct AnimationProbe {
    timeline: Timeline,
    samples: Rc<RefCell<Vec<f32>>>,
}

#[derive(Debug, Clone)]
struct DropProbe {
    drops: Rc<Cell<usize>>,
}

#[derive(Debug)]
struct DropState {
    drops: Rc<Cell<usize>>,
}

impl Drop for DropState {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

impl Widget for DropProbe {
    fn create_state(&self) -> Box<dyn Any> {
        Box::new(DropState {
            drops: Rc::clone(&self.drops),
        })
    }
}

impl Widget for AnimationProbe {
    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        self.samples
            .borrow_mut()
            .push(self.timeline.progress(ctx.now()));
        if !self.timeline.is_finished(ctx.now()) {
            ctx.request_animation_frame();
        }
        Ok(())
    }
}

fn flex_root(children: impl IntoIterator<Item = Element>) -> Element {
    Element::new(Flex {
        axis: Axis::Horizontal,
        gap: 10.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Start,
    })
    .children(children)
}

fn prepare(root: &mut UiRoot, size: Size) -> DisplayListBuilder {
    root.layout(size).unwrap();
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    builder
}

#[test]
fn keyed_reconciliation_preserves_state_and_generation_rejects_stale_ids() {
    let mut root = UiRoot::new(flex_root([
        Element::new(Probe::leaf("a", 40.0)).keyed(1),
        Element::new(Probe::leaf("b", 40.0)).keyed(2),
    ]))
    .unwrap();
    let root_id = root.root_id();
    let first = root.children(root_id).unwrap().to_vec();

    root.reconcile(flex_root([
        Element::new(Probe::leaf("b", 40.0)).keyed(2),
        Element::new(Probe::leaf("a", 40.0)).keyed(1),
    ]))
    .unwrap();
    let reordered = root.children(root.root_id()).unwrap().to_vec();
    assert_eq!(reordered, vec![first[1], first[0]]);
    assert_eq!(root.state::<ProbeState>(first[0]).unwrap().updates, 1);
    assert_eq!(root.state::<ProbeState>(first[1]).unwrap().updates, 1);

    prepare(&mut root, Size::new(100.0, 30.0));
    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(20.0, 10.0),
        button: PointerButton::Primary,
    })
    .unwrap();
    let captured = root.pointer_capture().unwrap();
    assert_eq!(captured, first[1]);

    root.reconcile(flex_root([Element::new(Probe::leaf("a", 40.0)).keyed(1)]))
        .unwrap();
    assert!(!root.is_alive(first[1]));
    assert_eq!(root.focused(), None);
    assert_eq!(root.pointer_capture(), None);

    root.reconcile(flex_root([
        Element::new(Probe::leaf("a", 40.0)).keyed(1),
        Element::new(Probe::leaf("c", 40.0)).keyed(3),
    ]))
    .unwrap();
    let replacement = root.children(root.root_id()).unwrap()[1];
    assert_eq!(replacement.index(), first[1].index());
    assert_ne!(replacement.generation(), first[1].generation());
}

#[test]
fn large_keyed_sibling_reorders_preserve_every_identity() {
    const COUNT: u64 = 2_000;
    let elements = || (0..COUNT).map(|key| Element::new(Bare).keyed(key));
    let mut root = UiRoot::new(Element::new(Stack).children(elements())).unwrap();
    let original = root.children(root.root_id()).unwrap().to_vec();
    root.reconcile(
        Element::new(Stack).children((0..COUNT).rev().map(|key| Element::new(Bare).keyed(key))),
    )
    .unwrap();
    let reordered = root.children(root.root_id()).unwrap();
    assert_eq!(
        reordered,
        original
            .iter()
            .rev()
            .copied()
            .collect::<Vec<_>>()
            .as_slice()
    );
}

#[test]
fn flex_measure_and_arrange_respond_to_explicit_constraints() {
    let mut root = UiRoot::new(flex_root([
        Element::new(Probe::leaf("fixed", 50.0)),
        Element::new(Probe::leaf("one", 1.0)).flex(1.0),
        Element::new(Probe::leaf("two", 1.0)).flex(2.0),
    ]))
    .unwrap();
    root.layout(Size::new(300.0, 50.0)).unwrap();
    let children = root.children(root.root_id()).unwrap();
    assert_eq!(
        root.rect(children[0]),
        Some(Rect::new(0.0, 0.0, 50.0, 20.0))
    );
    assert_eq!(
        root.rect(children[1]),
        Some(Rect::new(60.0, 0.0, 76.666664, 20.0))
    );
    assert_eq!(
        root.rect(children[2]),
        Some(Rect::new(146.66666, 0.0, 153.33333, 20.0))
    );

    root.layout(Size::new(180.0, 40.0)).unwrap();
    let children = root.children(root.root_id()).unwrap();
    assert_eq!(root.rect(children[0]).unwrap().width, 50.0);
    assert!((root.rect(children[1]).unwrap().width - 36.666668).abs() < 0.001);
    assert!((root.rect(children[2]).unwrap().width - 73.333336).abs() < 0.001);
}

#[test]
fn align_positions_a_natural_size_child_without_visual_defaults() {
    let mut root = UiRoot::new(
        Element::new(Align {
            horizontal: Alignment::Center,
            vertical: Alignment::End,
        })
        .child(Element::new(Probe::leaf("child", 20.0))),
    )
    .unwrap();
    root.layout(Size::new(100.0, 50.0)).unwrap();
    let child = root.children(root.root_id()).unwrap()[0];
    assert_eq!(root.rect(child), Some(Rect::new(40.0, 30.0, 20.0, 20.0)));
}

#[test]
fn paint_order_drives_reverse_hit_testing_and_clips_children() {
    let mut root = UiRoot::new(Element::new(Stack).children([
        Element::new(Probe::leaf("back", 100.0)).keyed(1),
        Element::new(Probe::leaf("front", 100.0)).keyed(2),
    ]))
    .unwrap();
    let front = root.children(root.root_id()).unwrap()[1];
    prepare(&mut root, Size::new(100.0, 50.0));
    assert_eq!(root.hit_test(Point::new(20.0, 10.0)), Some(front));

    let mut clipped =
        UiRoot::new(Element::new(ClipHost).child(Element::new(Probe::leaf("child", 40.0))))
            .unwrap();
    let child = clipped.children(clipped.root_id()).unwrap()[0];
    let builder = prepare(&mut clipped, Size::new(100.0, 50.0));
    assert_eq!(clipped.hit_test(Point::new(90.0, 15.0)), Some(child));
    assert_eq!(clipped.hit_test(Point::new(110.0, 15.0)), None);
    let list = builder.finish();
    assert_eq!(list.primitives().len(), 1);
    let nkdhr_render::Primitive::Shape(shape) = list.primitives()[0] else {
        panic!("expected shape")
    };
    assert_eq!(shape.clip, Some(Rect::new(0.0, 0.0, 100.0, 50.0)));
}

#[test]
fn intentionally_unpainted_children_do_not_leave_a_permanent_dirty_frame() {
    let mut root =
        UiRoot::new(Element::new(Hidden).child(Element::new(Probe::leaf("child", 20.0)))).unwrap();
    prepare(&mut root, Size::new(100.0, 30.0));
    assert!(!root.invalidation().contains(Invalidation::PAINT));
    assert_eq!(root.hit_test(Point::new(10.0, 10.0)), None);
}

#[test]
fn pointer_events_bubble_and_capture_while_tab_uses_semantic_tree_order() {
    let log = Rc::<RefCell<Vec<String>>>::default();
    let mut passive = Probe::leaf("child", 100.0);
    passive.handle_pointer = false;
    passive.log = Rc::clone(&log);
    let mut root = UiRoot::new(
        Element::new(EventContainer {
            log: Rc::clone(&log),
        })
        .child(Element::new(passive)),
    )
    .unwrap();
    let child = root.children(root.root_id()).unwrap()[0];
    prepare(&mut root, Size::new(100.0, 40.0));
    let result = root
        .dispatch(&UiEvent::PointerDown {
            position: Point::new(10.0, 10.0),
            button: PointerButton::Primary,
        })
        .unwrap();
    assert!(!result.handled);
    assert_eq!(result.focused, Some(child));
    assert_eq!(result.pointer_capture, Some(child));
    assert!(
        log.borrow()
            .iter()
            .any(|entry| entry.starts_with("parent:"))
    );

    let mut focus = UiRoot::new(Element::new(Stack).children([
        Element::new(FocusScope).children([
            Element::new(Probe::leaf("one", 10.0)),
            Element::new(Probe::leaf("two", 10.0)),
        ]),
        Element::new(Probe::leaf("outside", 10.0)),
    ]))
    .unwrap();
    let scope = focus.children(focus.root_id()).unwrap()[0];
    let inside = focus.children(scope).unwrap().to_vec();
    focus.set_focus(Some(inside[0])).unwrap();
    focus
        .dispatch(&UiEvent::KeyDown {
            key: Key::Tab,
            modifiers: Modifiers::default(),
            repeat: false,
        })
        .unwrap();
    assert_eq!(focus.focused(), Some(inside[1]));
    assert!(
        focus
            .state::<ProbeState>(inside[0])
            .unwrap()
            .observed_focus_during_key
    );
    focus
        .dispatch(&UiEvent::KeyDown {
            key: Key::Tab,
            modifiers: Modifiers::default(),
            repeat: false,
        })
        .unwrap();
    assert_eq!(focus.focused(), Some(inside[0]));
}

#[test]
fn pointer_hover_tracks_hit_order_independently_from_capture() {
    let mut root = UiRoot::new(flex_root([
        Element::new(Probe::leaf("one", 40.0)),
        Element::new(Probe::leaf("two", 40.0)),
    ]))
    .unwrap();
    prepare(&mut root, Size::new(100.0, 30.0));
    let children = root.children(root.root_id()).unwrap().to_vec();
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(10.0, 10.0),
    })
    .unwrap();
    assert_eq!(root.hovered(), Some(children[0]));
    assert!(root.state::<ProbeState>(children[0]).unwrap().hovered);

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(10.0, 10.0),
        button: PointerButton::Primary,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(60.0, 10.0),
    })
    .unwrap();
    assert_eq!(root.pointer_capture(), Some(children[0]));
    assert_eq!(root.hovered(), Some(children[1]));
    assert!(!root.state::<ProbeState>(children[0]).unwrap().hovered);
    assert!(root.state::<ProbeState>(children[1]).unwrap().hovered);

    root.dispatch(&UiEvent::PointerLeft).unwrap();
    assert_eq!(root.hovered(), None);
    root.dispatch(&UiEvent::PointerCancel).unwrap();
    assert_eq!(root.pointer_capture(), None);
}

#[test]
fn repaint_recomputes_hover_for_a_stationary_pointer_after_layout() {
    let mut root = UiRoot::new(flex_root([
        Element::new(Probe::leaf("one", 40.0)).keyed(1),
        Element::new(Probe::leaf("two", 40.0)).keyed(2),
    ]))
    .unwrap();
    prepare(&mut root, Size::new(140.0, 30.0));
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(60.0, 10.0),
    })
    .unwrap();
    let original = root.children(root.root_id()).unwrap().to_vec();
    assert_eq!(root.hovered(), Some(original[1]));

    root.reconcile(flex_root([
        Element::new(Probe::leaf("one", 90.0)).keyed(1),
        Element::new(Probe::leaf("two", 40.0)).keyed(2),
    ]))
    .unwrap();
    prepare(&mut root, Size::new(140.0, 30.0));
    assert_eq!(root.hovered(), Some(original[0]));
}

#[test]
fn reactive_values_queue_work_until_the_next_root_boundary() {
    let value = Reactive::new(false);
    let paints = Rc::new(Cell::new(0));
    let mut root = UiRoot::new(Element::new(ReactivePaint {
        value: value.clone(),
        paints: Rc::clone(&paints),
    }))
    .unwrap();
    prepare(&mut root, Size::new(20.0, 20.0));
    assert_eq!(paints.get(), 1);
    assert!(!root.invalidation().contains(Invalidation::PAINT));

    value.set(true);
    assert!(root.invalidation().contains(Invalidation::PAINT));
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    assert_eq!(paints.get(), 2);
    assert!(!root.invalidation().contains(Invalidation::PAINT));
}

#[test]
fn animation_frames_use_only_the_injected_clock() {
    let clock = ManualClock::default();
    let timeline = Timeline::new(clock.now(), Duration::from_millis(100)).unwrap();
    let samples = Rc::<RefCell<Vec<f32>>>::default();
    let mut root = UiRoot::with_clock(
        Element::new(AnimationProbe {
            timeline,
            samples: Rc::clone(&samples),
        }),
        clock.clone(),
    )
    .unwrap();
    prepare(&mut root, Size::new(10.0, 10.0));
    assert!(root.frame_requested());
    assert_eq!(samples.borrow().as_slice(), &[0.0]);

    clock.advance(Duration::from_millis(50));
    assert!(root.tick());
    assert!(root.invalidation().contains(Invalidation::PAINT));
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    assert!(root.frame_requested());

    clock.advance(Duration::from_millis(50));
    assert!(root.tick());
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    assert!(!root.frame_requested());
    assert_eq!(samples.borrow().as_slice(), &[0.0, 0.5, 1.0]);
}

#[test]
fn semantic_snapshot_uses_stable_ids_and_clears_its_dirty_pass() {
    let mut root = UiRoot::new(Element::new(Stack).children([
        Element::new(Probe::leaf("one", 10.0)),
        Element::new(Probe::leaf("two", 10.0)),
    ]))
    .unwrap();
    root.layout(Size::new(100.0, 40.0)).unwrap();
    let children = root.children(root.root_id()).unwrap().to_vec();
    let semantics = root.semantic_tree();
    assert_eq!(semantics.len(), 3);
    assert_eq!(semantics[1].id, children[0]);
    assert_eq!(semantics[1].semantics.role, SemanticRole::Button);
    assert_eq!(semantics[1].semantics.label.as_deref(), Some("one"));
    assert!(!root.invalidation().contains(Invalidation::SEMANTICS));
}

#[test]
fn semantics_can_watch_reactive_values_without_tree_reentry() {
    let label = Reactive::new("first".to_owned());
    let mut root = UiRoot::new(Element::new(ReactiveSemantics {
        label: label.clone(),
    }))
    .unwrap();
    assert_eq!(
        root.semantic_tree()[0].semantics.label.as_deref(),
        Some("first")
    );
    assert!(!root.invalidation().contains(Invalidation::SEMANTICS));
    label.set("second".to_owned());
    assert!(root.invalidation().contains(Invalidation::SEMANTICS));
    assert_eq!(
        root.semantic_tree()[0].semantics.label.as_deref(),
        Some("second")
    );
}

#[test]
fn invalid_declarative_inputs_fail_before_mutating_the_tree() {
    let duplicate =
        Element::new(Stack).children([Element::new(Stack).keyed(7), Element::new(Stack).keyed(7)]);
    assert!(matches!(
        UiRoot::new(duplicate),
        Err(UiError::DuplicateKey(_))
    ));
    assert!(matches!(
        UiRoot::new(Element::new(Stack).child(Element::new(Stack).flex(f32::NAN))),
        Err(UiError::InvalidFlex)
    ));

    let mut overflow = UiRoot::new(
        Element::new(Flex {
            axis: Axis::Horizontal,
            gap: f32::MAX,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Start,
        })
        .children([
            Element::new(Stack),
            Element::new(Stack),
            Element::new(Stack),
        ]),
    )
    .unwrap();
    assert_eq!(
        overflow.layout(Size::new(100.0, 100.0)),
        Err(UiError::InvalidGap)
    );

    let mut input = UiRoot::new(Element::new(Stack)).unwrap();
    assert_eq!(
        input.dispatch(&UiEvent::ImePreedit {
            text: "你好".to_owned(),
            selection: Some((1, 3)),
        }),
        Err(UiError::InvalidEvent)
    );

    let mut unlaid_out = UiRoot::new(Element::new(Stack)).unwrap();
    assert_eq!(
        unlaid_out.paint(&mut DisplayListBuilder::new()),
        Err(UiError::LayoutRequired)
    );
}

#[test]
fn removing_a_node_drops_retained_state_at_the_lifecycle_boundary() {
    let drops = Rc::new(Cell::new(0));
    let mut root = UiRoot::new(
        Element::new(Stack).child(
            Element::new(DropProbe {
                drops: Rc::clone(&drops),
            })
            .keyed(1),
        ),
    )
    .unwrap();
    assert_eq!(drops.get(), 0);
    root.reconcile(Element::new(Stack)).unwrap();
    assert_eq!(drops.get(), 1);
}

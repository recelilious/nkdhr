use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use cosmic_text::{FontSystem, fontdb};
use nkdhr_render::{
    Color, DisplayListBuilder, Point, Primitive, TextureStore, software::SoftwareRenderer,
};
use nkdhr_ui::text::{TextConfig, TextResources, TextSystem};
use nkdhr_ui::{
    Axis, Button, ButtonVariant, ClipboardRequest, Constraints, CrossAxisAlignment, Density,
    Element, Flex, GlassSurface, Insets, Key, List, ListEntry, ListItem, ListItemBehavior,
    ListMultiSelection, ListReorder, ListTreeToggle, ListVirtualWindow, MainAxisAlignment,
    ManualClock, MaterialTier, Modifiers, MotionMode, Padding, PasswordCopyPolicy, PointerButton,
    Reactive, Scroll, ScrollAnchor, ScrollAxis, ScrollOffset, ScrollPhase, ScrollReveal,
    SemanticRole, Size, Slider, Text, TextInput, TextInputEdit, TextInputTabBehavior,
    TextInputValidationRequest, TextInputValidationResult, TextInputValidationTrigger, TextRole,
    Theme, Toggle, UiEvent, UiRoot, Widget,
};

fn prepare(root: &mut UiRoot, size: Size) -> nkdhr_render::DisplayList {
    root.layout(size).unwrap();
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    builder.finish()
}

fn primary_click(root: &mut UiRoot, position: Point, modifiers: Modifiers) {
    primary_click_count(root, position, modifiers, 1);
}

fn primary_click_count(root: &mut UiRoot, position: Point, modifiers: Modifiers, click_count: u8) {
    root.dispatch(&UiEvent::PointerDown {
        position,
        button: PointerButton::Primary,
        modifiers,
        click_count,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position,
        button: PointerButton::Primary,
        modifiers,
        click_count,
    })
    .unwrap();
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn fixture_text_resources() -> TextResources {
    let mut database = fontdb::Database::new();
    for bytes in [
        include_bytes!("fonts/NotoSansLatin.subset.ttf").as_slice(),
        include_bytes!("fonts/NotoSansCJKsc.subset.otf").as_slice(),
        include_bytes!("fonts/NotoColorEmoji.subset.ttf").as_slice(),
    ] {
        database.load_font_source(fontdb::Source::Binary(Arc::new(bytes.to_vec())));
    }
    database.set_sans_serif_family("Noto Sans");
    let system = TextSystem::with_font_system(
        FontSystem::new_with_locale_and_db("zh-CN".to_owned(), database),
        TextConfig::default(),
    )
    .unwrap();
    TextResources::new(system, TextureStore::new(), 1.0).unwrap()
}

#[test]
fn button_activates_on_valid_release_and_keyboard_but_not_outside_release() {
    let theme = Arc::new(Theme::default());
    let activations = Rc::new(Cell::new(0));
    let callback_count = Rc::clone(&activations);
    let mut root = UiRoot::with_text(
        Element::new(
            Button::new("Apply", theme)
                .variant(ButtonVariant::Primary)
                .on_activate(move || callback_count.set(callback_count.get() + 1)),
        ),
        fixture_text_resources(),
    )
    .unwrap();
    prepare(&mut root, Size::new(120.0, 44.0));

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(20.0, 20.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(200.0, 20.0),
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(200.0, 20.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert_eq!(activations.get(), 0);

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(20.0, 20.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(20.0, 20.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert_eq!(activations.get(), 1);

    root.dispatch(&UiEvent::KeyDown {
        key: Key::Enter,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(activations.get(), 2);
    assert_eq!(root.semantic_tree()[0].semantics.role, SemanticRole::Button);
}

#[test]
fn button_context_activation_supports_targeted_clipboard_round_trips() {
    let received = Rc::new(RefCell::new(None));
    let received_text = Rc::clone(&received);
    let mut root = UiRoot::with_text(
        Element::new(
            Button::new("Import", Arc::new(Theme::default()))
                .on_activate_with_context(|ctx| ctx.read_clipboard_text())
                .on_clipboard_text(move |text| {
                    received_text.replace(Some(text.to_owned()));
                }),
        ),
        fixture_text_resources(),
    )
    .unwrap();
    prepare(&mut root, Size::new(120.0, 44.0));

    let result = root
        .dispatch(&UiEvent::PointerDown {
            position: Point::new(20.0, 20.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(result.clipboard.is_empty());
    let result = root
        .dispatch(&UiEvent::PointerUp {
            position: Point::new(20.0, 20.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    let [ClipboardRequest::ReadText { target }] = result.clipboard.as_slice() else {
        panic!("button activation should request one targeted clipboard read")
    };
    root.dispatch(&UiEvent::ClipboardText {
        target: *target,
        text: "portable preset".to_owned(),
    })
    .unwrap();
    assert_eq!(received.borrow().as_deref(), Some("portable preset"));
}

#[test]
fn text_rendering_components_reject_a_root_without_text_resources() {
    let mut root = UiRoot::new(Element::new(Button::new(
        "Apply",
        Arc::new(Theme::default()),
    )))
    .unwrap();
    assert_eq!(
        root.layout(Size::new(120.0, 44.0)),
        Err(nkdhr_ui::UiError::TextResourcesRequired)
    );
}

#[test]
fn toggle_updates_bound_value_and_reduced_motion_settles_without_spatial_frames() {
    let clock = ManualClock::default();
    let value = Reactive::new(false);
    let mut theme = Theme::default();
    theme.motion.mode = MotionMode::Reduced;
    let mut root = UiRoot::with_clock(
        Element::new(Toggle::new("Blur", value.clone(), Arc::new(theme))),
        clock,
    )
    .unwrap();
    let resting = prepare(&mut root, Size::new(44.0, 44.0)).len();

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(22.0, 22.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(22.0, 22.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert!(value.get());
    root.set_focus(None).unwrap();
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(100.0, 100.0),
    })
    .unwrap();

    let list = prepare(&mut root, Size::new(44.0, 44.0));
    assert_eq!(list.len(), resting, "reduced motion adds no spatial bridge");
    assert_eq!(
        root.semantic_tree()[0].semantics.value.as_deref(),
        Some("on")
    );
}

#[test]
fn standard_toggle_emits_temporary_fluid_bridge_only_during_transfer() {
    let clock = ManualClock::default();
    let value = Reactive::new(false);
    let mut root = UiRoot::with_clock(
        Element::new(Toggle::new(
            "Blur",
            value.clone(),
            Arc::new(Theme::default()),
        )),
        clock.clone(),
    )
    .unwrap();
    let resting = prepare(&mut root, Size::new(44.0, 44.0)).len();
    value.set(true);
    prepare(&mut root, Size::new(44.0, 44.0));
    clock.advance(std::time::Duration::from_millis(110));
    assert!(root.tick());
    let transferring = prepare(&mut root, Size::new(44.0, 44.0)).len();
    assert!(transferring > resting);
    clock.advance(std::time::Duration::from_millis(220));
    assert!(root.tick());
    let settled = prepare(&mut root, Size::new(44.0, 44.0)).len();
    assert_eq!(settled, resting);
}

#[test]
fn pending_controls_keep_requested_geometry_visible_and_block_duplicate_input() {
    let clock = ManualClock::default();
    let activations = Rc::new(Cell::new(0));
    let callback_count = Rc::clone(&activations);
    let mut button = UiRoot::with_clock_and_text(
        Element::new(
            Button::new("Apply", Arc::new(Theme::default()))
                .pending(true)
                .on_activate(move || callback_count.set(callback_count.get() + 1)),
        ),
        clock.clone(),
        fixture_text_resources(),
    )
    .unwrap();
    prepare(&mut button, Size::new(120.0, 44.0));
    assert!(button.frame_requested());
    let blocked = button
        .dispatch(&UiEvent::PointerDown {
            position: Point::new(60.0, 22.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(blocked.handled);
    let blocked = button
        .dispatch(&UiEvent::PointerUp {
            position: Point::new(60.0, 22.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(blocked.handled);
    assert_eq!(activations.get(), 0);
    assert_eq!(
        button.semantic_tree()[0].semantics.value.as_deref(),
        Some("pending")
    );

    let requested_toggle = Reactive::new(true);
    let effective_toggle = Reactive::new(false);
    let mut toggle = UiRoot::with_clock(
        Element::new(
            Toggle::new("Blur", requested_toggle.clone(), Arc::new(Theme::default()))
                .effective_value(effective_toggle),
        ),
        clock.clone(),
    )
    .unwrap();
    prepare(&mut toggle, Size::new(44.0, 44.0));
    assert!(toggle.frame_requested());
    primary_click(&mut toggle, Point::new(22.0, 22.0), Modifiers::default());
    assert!(
        requested_toggle.get(),
        "pending Toggle ignores duplicate input"
    );
    assert_eq!(
        toggle.semantic_tree()[0].semantics.value.as_deref(),
        Some("on; pending; effective off")
    );

    let requested_slider = Reactive::new(70.0);
    let effective_slider = Reactive::new(60.0);
    let slider = Slider::new(
        "Opacity",
        requested_slider.clone(),
        0.0,
        100.0,
        Arc::new(Theme::default()),
    )
    .unwrap()
    .effective_value(effective_slider);
    let mut slider = UiRoot::with_clock(Element::new(slider), clock).unwrap();
    prepare(&mut slider, Size::new(200.0, 44.0));
    assert!(slider.frame_requested());
    primary_click(&mut slider, Point::new(180.0, 22.0), Modifiers::default());
    assert_eq!(requested_slider.get(), 70.0);
    assert_eq!(
        slider.semantic_tree()[0].semantics.value.as_deref(),
        Some("70; pending; effective 60")
    );

    let requested_slider = Reactive::new(70.0);
    let effective_slider = Reactive::new(70.0);
    let slider = Slider::new(
        "Live opacity",
        requested_slider.clone(),
        0.0,
        100.0,
        Arc::new(Theme::default()),
    )
    .unwrap()
    .effective_value(effective_slider);
    let mut slider = UiRoot::new(Element::new(slider)).unwrap();
    prepare(&mut slider, Size::new(200.0, 44.0));
    slider
        .dispatch(&UiEvent::PointerDown {
            position: Point::new(180.0, 22.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(slider.pointer_capture().is_some());
    assert_ne!(requested_slider.get(), 70.0);
    let release = slider
        .dispatch(&UiEvent::PointerUp {
            position: Point::new(180.0, 22.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(release.handled);
    assert!(slider.pointer_capture().is_none());
}

#[test]
fn slider_keeps_exact_value_node_and_supports_normal_coarse_and_fine_steps() {
    let value = Reactive::new(0.0);
    let slider = Slider::new(
        "Opacity",
        value.clone(),
        0.0,
        100.0,
        Arc::new(Theme::default()),
    )
    .unwrap()
    .step(1.0)
    .unwrap()
    .ideal_width(200.0)
    .unwrap();
    let mut root = UiRoot::new(Element::new(slider)).unwrap();
    prepare(&mut root, Size::new(200.0, 44.0));

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(100.0, 22.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(100.0, 22.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert_eq!(value.get(), 50.0);

    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowRight,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(value.get(), 51.0);
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowRight,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    assert_eq!(value.get(), 61.0);
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowLeft,
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    assert_eq!(
        value.get(),
        61.0,
        "a 0.1 delta quantizes back to the 1.0 step"
    );
}

#[derive(Debug, Clone, Copy)]
struct LabelBlock;

impl Widget for LabelBlock {
    fn measure(
        &self,
        _ctx: &mut nkdhr_ui::MeasureCtx<'_>,
        constraints: Constraints,
    ) -> Result<Size, nkdhr_ui::UiError> {
        Ok(constraints.constrain(Size::new(180.0, 20.0)))
    }

    fn paint(&self, ctx: &mut nkdhr_ui::PaintCtx<'_>) -> Result<(), nkdhr_ui::UiError> {
        let rect = ctx.rect();
        ctx.builder().rounded_rect(
            rect,
            nkdhr_render::CornerRadii::all(4.0),
            Color::from_srgba8(192, 202, 245, 180),
        )?;
        Ok(())
    }
}

fn setting_row(label: &str, control: Element, theme: &Theme) -> Element {
    Element::new(Flex {
        axis: Axis::Horizontal,
        gap: 16.0,
        main_alignment: MainAxisAlignment::SpaceBetween,
        cross_alignment: CrossAxisAlignment::Center,
    })
    .child(
        Element::new(Text::new(
            label,
            theme.text_style(TextRole::Body),
            theme.palette.text_primary,
        ))
        .flex(1.0),
    )
    .child(control)
}

fn settings_scene(theme: Arc<Theme>) -> Element {
    let blur = Reactive::new(true);
    let opacity = Reactive::new(86.0);
    let content = Element::new(Flex {
        axis: Axis::Vertical,
        gap: 12.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Stretch,
    })
    .child(setting_row(
        "Background blur 模糊",
        Element::new(Toggle::new("Background blur", blur, Arc::clone(&theme))),
        &theme,
    ))
    .child(setting_row(
        "Content opacity",
        Element::new(
            Slider::new("Content opacity", opacity, 60.0, 98.0, Arc::clone(&theme)).unwrap(),
        ),
        &theme,
    ))
    .child(setting_row(
        "Motion curve 🚀",
        Element::new(
            Button::new("Open curve editor", Arc::clone(&theme)).variant(ButtonVariant::Primary),
        ),
        &theme,
    ));

    Element::new(
        GlassSurface::new(theme, MaterialTier::ContentSurface)
            .radius(28.0)
            .padding(Insets::all(16.0)),
    )
    .child(content)
}

#[test]
fn settings_like_scene_composes_only_public_ui_api_at_two_widths() {
    let theme = Arc::new(Theme::default());
    let mut root =
        UiRoot::with_text(settings_scene(Arc::clone(&theme)), fixture_text_resources()).unwrap();
    let wide = prepare(&mut root, Size::new(720.0, 260.0));
    assert!(
        wide.primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::Shape(_)))
    );
    let mut renderer = SoftwareRenderer::new(720, 260).unwrap();
    renderer.clear(theme.palette.backdrop);
    renderer
        .render(&wide, root.texture_store().unwrap(), 1.0)
        .unwrap();
    assert_eq!(fnv1a(&renderer.rgba8()), 16_050_547_116_187_463_765);

    let mut compact_theme = (*theme).clone();
    compact_theme.density = Density::Compact;
    root.reconcile(settings_scene(Arc::new(compact_theme)))
        .unwrap();
    let narrow = prepare(&mut root, Size::new(420.0, 220.0));
    assert!(!narrow.is_empty());
}

#[test]
fn list_items_share_selection_and_navigation_activation() {
    let selection = Reactive::new(Some(100_u64));
    let theme = Arc::new(Theme::default());
    let activated = Rc::new(Cell::new(0));
    let mut rows = Vec::new();
    for index in 0..3 {
        let activated = Rc::clone(&activated);
        rows.push(
            Element::new(
                ListItem::new(
                    100 + index as u64,
                    format!("Item {index}"),
                    selection.clone(),
                    Arc::clone(&theme),
                )
                .on_activate(move || activated.set(index + 1)),
            )
            .keyed(100 + index as u64)
            .child(Element::new(LabelBlock)),
        );
    }
    let list = List::new(
        "Pages",
        selection.clone(),
        [100, 101, 102],
        Arc::clone(&theme),
    )
    .unwrap();
    let mut root = UiRoot::new(Element::new(list).children(rows)).unwrap();
    prepare(&mut root, Size::new(240.0, 144.0));

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(80.0, 72.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(80.0, 72.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert_eq!(selection.get(), Some(101));
    assert_eq!(activated.get(), 2);
    assert_eq!(root.semantic_tree()[0].semantics.role, SemanticRole::List);

    let reordered = [102_u64, 100, 101].into_iter().map(|identity| {
        Element::new(ListItem::new(
            identity,
            format!("Item {}", identity - 100),
            selection.clone(),
            Arc::clone(&theme),
        ))
        .keyed(identity)
        .child(Element::new(LabelBlock))
    });
    root.reconcile(
        Element::new(
            List::new(
                "Pages",
                selection.clone(),
                [102, 100, 101],
                Arc::clone(&theme),
            )
            .unwrap(),
        )
        .children(reordered),
    )
    .unwrap();
    prepare(&mut root, Size::new(240.0, 144.0));
    let semantics = root.semantic_tree();
    let selected = semantics
        .iter()
        .find(|node| {
            node.semantics.role == SemanticRole::ListItem
                && node.semantics.value.as_deref() == Some("selected")
        })
        .unwrap();
    assert_eq!(selected.semantics.label.as_deref(), Some("Item 1"));
}

#[test]
fn multi_list_pointer_selection_supports_ranges_and_discontiguous_toggles() {
    let selection = Reactive::new(ListMultiSelection::default());
    let theme = Arc::new(Theme::default());
    let entries = (10..15)
        .map(|identity| ListEntry::new(identity, format!("Item {identity}")))
        .collect::<Vec<_>>();
    let rows = entries
        .iter()
        .map(|entry| {
            Element::new(ListItem::with_selection(
                entry.identity(),
                entry.label(),
                selection.clone(),
                Arc::clone(&theme),
            ))
            .keyed(entry.identity())
            .child(Element::new(LabelBlock))
        })
        .collect::<Vec<_>>();
    let list =
        List::from_entries("Objects", selection.clone(), entries, Arc::clone(&theme)).unwrap();
    let mut root = UiRoot::new(Element::new(list).children(rows)).unwrap();
    prepare(&mut root, Size::new(240.0, 240.0));

    primary_click(&mut root, Point::new(80.0, 72.0), Modifiers::default());
    assert_eq!(selection.get().selected().collect::<Vec<_>>(), vec![11]);

    primary_click(
        &mut root,
        Point::new(80.0, 168.0),
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(selection.get().selected().collect::<Vec<_>>(), vec![11, 13]);

    primary_click(
        &mut root,
        Point::new(80.0, 216.0),
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(selection.get().selected().collect::<Vec<_>>(), vec![13, 14]);

    primary_click(
        &mut root,
        Point::new(80.0, 24.0),
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(
        selection.get().selected().collect::<Vec<_>>(),
        vec![10, 13, 14]
    );
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Space,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(selection.get().selected().collect::<Vec<_>>(), vec![13, 14]);
}

#[test]
fn list_keyboard_navigation_moves_real_focus_and_typeahead_uses_labels() {
    let selection = Reactive::new(ListMultiSelection::default());
    let theme = Arc::new(Theme::default());
    let activated = Rc::new(Cell::new(None));
    let entries = [(1, "Alpha"), (2, "Beta"), (3, "Gamma")]
        .into_iter()
        .map(|(identity, label)| ListEntry::new(identity, label))
        .collect::<Vec<_>>();
    let rows = entries
        .iter()
        .map(|entry| {
            let activated = Rc::clone(&activated);
            let identity = entry.identity();
            Element::new(
                ListItem::with_selection(
                    identity,
                    entry.label(),
                    selection.clone(),
                    Arc::clone(&theme),
                )
                .behavior(ListItemBehavior::Object)
                .on_activate(move || activated.set(Some(identity))),
            )
            .keyed(identity)
            .child(Element::new(LabelBlock))
        })
        .collect::<Vec<_>>();
    let list =
        List::from_entries("Objects", selection.clone(), entries, Arc::clone(&theme)).unwrap();
    let mut root = UiRoot::new(Element::new(list).children(rows)).unwrap();
    prepare(&mut root, Size::new(240.0, 144.0));

    root.dispatch(&UiEvent::KeyDown {
        key: Key::Tab,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowDown,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(selection.get().cursor(), Some(2));
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Enter,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(activated.get(), Some(2), "Enter follows the moved focus");

    root.dispatch(&UiEvent::TextInput("g".to_owned())).unwrap();
    assert_eq!(selection.get().cursor(), Some(3));
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Enter,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(activated.get(), Some(3));

    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowUp,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    assert_eq!(selection.get().selected().collect::<Vec<_>>(), vec![2, 3]);
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowUp,
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    assert_eq!(selection.get().cursor(), Some(1));
    assert_eq!(selection.get().selected().collect::<Vec<_>>(), vec![2, 3]);
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Space,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(
        selection.get().selected().collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn tree_list_disclosure_and_left_right_navigation_use_stable_entries() {
    let selection = Reactive::new(Some(1_u64));
    let theme = Arc::new(Theme::default());
    let toggle = Rc::new(Cell::new(None));
    let entries = vec![
        ListEntry::new(1, "Root").tree(0, true),
        ListEntry::new(2, "Child").depth(1),
        ListEntry::new(3, "Sibling"),
    ];
    let rows = [
        (1, "Root", 0, true),
        (2, "Child", 1, false),
        (3, "Sibling", 0, false),
    ]
    .into_iter()
    .map(|(identity, label, depth, disclosure)| {
        Element::new(
            ListItem::new(identity, label, selection.clone(), Arc::clone(&theme))
                .tree(depth, disclosure),
        )
        .keyed(identity)
        .child(Element::new(LabelBlock))
    });
    let toggle_callback = Rc::clone(&toggle);
    let list = List::from_entries("Tree", selection.clone(), entries, Arc::clone(&theme))
        .unwrap()
        .on_tree_toggle(move |event| toggle_callback.set(Some(event)));
    let mut root = UiRoot::new(Element::new(list).children(rows)).unwrap();
    prepare(&mut root, Size::new(240.0, 144.0));

    root.dispatch(&UiEvent::KeyDown {
        key: Key::Tab,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowRight,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(selection.get(), Some(2));
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowLeft,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(selection.get(), Some(1));
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowLeft,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(
        toggle.get(),
        Some(ListTreeToggle {
            identity: 1,
            expanded: false
        })
    );

    toggle.set(None);
    primary_click(&mut root, Point::new(18.0, 24.0), Modifiers::default());
    assert_eq!(
        toggle.get(),
        Some(ListTreeToggle {
            identity: 1,
            expanded: false
        })
    );
}

#[test]
fn list_reorder_drag_uses_placeholder_motion_and_stable_before_identity() {
    let selection = Reactive::new(Some(2_u64));
    let theme = Arc::new(Theme::default());
    let reorder = Rc::new(Cell::new(None));
    let entries = [1_u64, 2, 3]
        .into_iter()
        .map(|identity| ListEntry::new(identity, format!("Item {identity}")))
        .collect::<Vec<_>>();
    let rows = entries
        .iter()
        .map(|entry| {
            Element::new(ListItem::new(
                entry.identity(),
                entry.label(),
                selection.clone(),
                Arc::clone(&theme),
            ))
            .keyed(entry.identity())
            .child(Element::new(LabelBlock))
        })
        .collect::<Vec<_>>();
    let reorder_callback = Rc::clone(&reorder);
    let list = List::from_entries("Reorder", selection.clone(), entries, Arc::clone(&theme))
        .unwrap()
        .on_reorder(move |event| reorder_callback.set(Some(event)));
    let mut root = UiRoot::new(Element::new(list).children(rows)).unwrap();
    prepare(&mut root, Size::new(240.0, 144.0));

    let down = root
        .dispatch(&UiEvent::PointerDown {
            position: Point::new(222.0, 72.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(down.pointer_capture.is_some());
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(222.0, 140.0),
    })
    .unwrap();
    let dragging = prepare(&mut root, Size::new(240.0, 144.0));
    assert!(
        dragging
            .primitives()
            .iter()
            .any(|primitive| match primitive {
                Primitive::Shape(shape) => shape.transform != nkdhr_render::Transform::IDENTITY,
                Primitive::Texture(texture) =>
                    texture.transform != nkdhr_render::Transform::IDENTITY,
                Primitive::BackdropBlur(blur) =>
                    blur.transform != nkdhr_render::Transform::IDENTITY,
            })
    );
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(222.0, 140.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert_eq!(
        reorder.get(),
        Some(ListReorder {
            identity: 2,
            before: None
        })
    );

    reorder.set(None);
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowUp,
        modifiers: Modifiers {
            control: true,
            shift: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    assert_eq!(
        reorder.get(),
        Some(ListReorder {
            identity: 2,
            before: Some(1)
        })
    );
}

#[test]
fn virtual_list_preserves_extent_hidden_selection_and_loading_row_geometry() {
    let selection = Reactive::new(ListMultiSelection::new([99]));
    let theme = Arc::new(Theme::default());
    let entries = vec![
        ListEntry::new(1, "Visible"),
        ListEntry::new(2, "Loading").loading(true),
    ];
    let list = List::from_entries("Virtual", selection.clone(), entries, Arc::clone(&theme))
        .unwrap()
        .virtual_window(ListVirtualWindow::new(100.0, 200.0).unwrap());
    let rows = [
        Element::new(ListItem::with_selection(
            1,
            "Visible",
            selection.clone(),
            Arc::clone(&theme),
        ))
        .keyed(1)
        .child(Element::new(LabelBlock)),
        Element::new(
            ListItem::with_selection(2, "Loading", selection.clone(), Arc::clone(&theme))
                .loading(true),
        )
        .keyed(2),
    ];
    let mut root = UiRoot::new(Element::new(list).children(rows)).unwrap();
    prepare(&mut root, Size::new(240.0, 396.0));

    let items = root
        .semantic_tree()
        .into_iter()
        .filter(|node| node.semantics.role == SemanticRole::ListItem)
        .collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].bounds.y, 100.0);
    assert_eq!(items[1].bounds.y, 148.0);
    assert_eq!(items[1].semantics.value.as_deref(), Some("loading"));
    assert_eq!(selection.get().selected().collect::<Vec<_>>(), vec![99]);
}

#[test]
fn object_list_double_clicks_to_activate_and_exposes_context_action() {
    let selection = Reactive::new(None);
    let theme = Arc::new(Theme::default());
    let activations = Rc::new(Cell::new(0));
    let context = Rc::new(Cell::new(None));
    let activation_callback = Rc::clone(&activations);
    let context_callback = Rc::clone(&context);
    let item = ListItem::new(1, "Object", selection.clone(), Arc::clone(&theme))
        .behavior(ListItemBehavior::Object)
        .on_activate(move || activation_callback.set(activation_callback.get() + 1))
        .on_context_menu(move |point| context_callback.set(Some(point)));
    let list = List::from_entries(
        "Objects",
        selection.clone(),
        [ListEntry::new(1, "Object")],
        theme,
    )
    .unwrap();
    let mut root =
        UiRoot::new(Element::new(list).child(Element::new(item).child(Element::new(LabelBlock))))
            .unwrap();
    prepare(&mut root, Size::new(240.0, 48.0));

    primary_click(&mut root, Point::new(80.0, 24.0), Modifiers::default());
    assert_eq!(activations.get(), 0);
    primary_click_count(&mut root, Point::new(80.0, 24.0), Modifiers::default(), 2);
    assert_eq!(activations.get(), 1);

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(90.0, 24.0),
        button: PointerButton::Secondary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert_eq!(context.get(), Some(Point::new(90.0, 24.0)));

    context.set(None);
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Named("ContextMenu".to_owned()),
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(context.get(), Some(Point::new(120.0, 24.0)));
}

#[test]
fn scroll_clamps_pointer_and_keyboard_updates_to_content_extent() {
    let offset = Reactive::new(ScrollOffset::ZERO);
    let scroll = Scroll::new(
        "Settings page",
        Size::new(100.0, 400.0),
        offset.clone(),
        Arc::new(Theme::default()),
    )
    .unwrap()
    .horizontal(false);
    let mut root = UiRoot::new(Element::new(scroll).child(Element::new(LabelBlock))).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    root.dispatch(&UiEvent::PointerScroll {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 72.0,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    assert_eq!(offset.get().y, 72.0);
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Tab,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));
    root.dispatch(&UiEvent::KeyDown {
        key: Key::End,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(offset.get().y, 300.0);
}

#[test]
fn scrollbar_overlay_supports_exact_thumb_drag_and_track_paging() {
    let offset = Reactive::new(ScrollOffset::ZERO);
    let scroll = Scroll::new(
        "Settings page",
        Size::new(100.0, 400.0),
        offset.clone(),
        Arc::new(Theme::default()),
    )
    .unwrap()
    .horizontal(false);
    let mut root = UiRoot::new(Element::new(scroll).child(Element::new(LabelBlock))).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    let down = root
        .dispatch(&UiEvent::PointerDown {
            position: Point::new(95.0, 12.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(down.handled);
    assert!(down.pointer_capture.is_some());
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(95.0, 72.0),
    })
    .unwrap();
    assert!((offset.get().y - 240.0).abs() < 0.01);
    let up = root
        .dispatch(&UiEvent::PointerUp {
            position: Point::new(95.0, 72.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(up.pointer_capture.is_none());

    offset.set(ScrollOffset::ZERO);
    prepare(&mut root, Size::new(100.0, 100.0));
    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(95.0, 80.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    assert_eq!(offset.get().y, 100.0);
}

#[test]
fn nested_scroll_hands_only_the_exact_boundary_remainder_outward() {
    let outer_offset = Reactive::new(ScrollOffset::ZERO);
    let inner_offset = Reactive::new(ScrollOffset::new(0.0, 150.0));
    let theme = Arc::new(Theme::default());
    let inner = Scroll::new(
        "Inner",
        Size::new(100.0, 500.0),
        inner_offset.clone(),
        Arc::clone(&theme),
    )
    .unwrap()
    .horizontal(false);
    let outer = Scroll::new(
        "Outer",
        Size::new(100.0, 300.0),
        outer_offset.clone(),
        theme,
    )
    .unwrap()
    .horizontal(false);
    let scene = Element::new(outer).child(Element::new(inner).child(Element::new(LabelBlock)));
    let mut root = UiRoot::new(scene).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    let result = root
        .dispatch(&UiEvent::PointerScroll {
            position: Point::new(50.0, 50.0),
            delta_x: 0.0,
            delta_y: 100.0,
            modifiers: Modifiers::default(),
        })
        .unwrap();
    assert!(result.handled);
    assert_eq!(inner_offset.get().y, 200.0);
    assert_eq!(outer_offset.get().y, 50.0);
}

#[test]
fn nested_scroll_gesture_lifecycle_reaches_the_container_that_consumed_remainder() {
    let clock = ManualClock::default();
    let outer_offset = Reactive::new(ScrollOffset::ZERO);
    let inner_offset = Reactive::new(ScrollOffset::new(0.0, 200.0));
    let theme = Arc::new(Theme::default());
    let inner = Scroll::new(
        "Inner",
        Size::new(100.0, 500.0),
        inner_offset.clone(),
        Arc::clone(&theme),
    )
    .unwrap()
    .horizontal(false);
    let outer = Scroll::new(
        "Outer",
        Size::new(100.0, 300.0),
        outer_offset.clone(),
        theme,
    )
    .unwrap()
    .horizontal(false);
    let mut root = UiRoot::with_clock(
        Element::new(outer).child(Element::new(inner).child(Element::new(LabelBlock))),
        clock.clone(),
    )
    .unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    for (advance, delta_y, phase) in [
        (0, 0.0, ScrollPhase::Begin),
        (16, 60.0, ScrollPhase::Update),
        (16, 0.0, ScrollPhase::End),
    ] {
        clock.advance(Duration::from_millis(advance));
        root.dispatch(&UiEvent::ScrollGesture {
            position: Point::new(50.0, 50.0),
            delta_x: 0.0,
            delta_y,
            phase,
            modifiers: Modifiers::default(),
        })
        .unwrap();
    }
    assert_eq!(inner_offset.get().y, 200.0);
    assert_eq!(outer_offset.get().y, 60.0);
    clock.advance(Duration::from_millis(32));
    assert!(root.tick());
    prepare(&mut root, Size::new(100.0, 100.0));
    assert!(outer_offset.get().y > 60.0);
}

#[test]
fn captured_thumb_drag_never_transfers_scroll_to_an_ancestor() {
    let outer_offset = Reactive::new(ScrollOffset::ZERO);
    let inner_offset = Reactive::new(ScrollOffset::ZERO);
    let theme = Arc::new(Theme::default());
    let inner = Scroll::new(
        "Inner",
        Size::new(80.0, 500.0),
        inner_offset.clone(),
        Arc::clone(&theme),
    )
    .unwrap()
    .horizontal(false);
    let outer = Scroll::new(
        "Outer",
        Size::new(100.0, 300.0),
        outer_offset.clone(),
        theme,
    )
    .unwrap()
    .horizontal(false);
    let inset_inner = Element::new(Padding {
        insets: Insets::new(0.0, 0.0, 20.0, 0.0),
    })
    .child(Element::new(inner).child(Element::new(LabelBlock)));
    let mut root = UiRoot::new(Element::new(outer).child(inset_inner)).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    let down = root
        .dispatch(&UiEvent::PointerDown {
            position: Point::new(75.0, 20.0),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        })
        .unwrap();
    assert!(down.pointer_capture.is_some());
    root.dispatch(&UiEvent::PointerScroll {
        position: Point::new(75.0, 20.0),
        delta_x: 0.0,
        delta_y: 100.0,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    assert_eq!(inner_offset.get(), ScrollOffset::ZERO);
    assert_eq!(outer_offset.get(), ScrollOffset::ZERO);
}

#[test]
fn scroll_gesture_inertia_is_host_clocked_and_interruptible() {
    let clock = ManualClock::default();
    let offset = Reactive::new(ScrollOffset::ZERO);
    let scroll = Scroll::new(
        "Canvas",
        Size::new(100.0, 1_000.0),
        offset.clone(),
        Arc::new(Theme::default()),
    )
    .unwrap()
    .horizontal(false);
    let mut root = UiRoot::with_clock(
        Element::new(scroll).child(Element::new(LabelBlock)),
        clock.clone(),
    )
    .unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    root.dispatch(&UiEvent::ScrollGesture {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 0.0,
        phase: ScrollPhase::Begin,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    clock.advance(Duration::from_millis(16));
    root.dispatch(&UiEvent::ScrollGesture {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 60.0,
        phase: ScrollPhase::Update,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    clock.advance(Duration::from_millis(16));
    root.dispatch(&UiEvent::ScrollGesture {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 0.0,
        phase: ScrollPhase::End,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    let released = offset.get().y;
    clock.advance(Duration::from_millis(32));
    assert!(root.tick());
    prepare(&mut root, Size::new(100.0, 100.0));
    assert!(offset.get().y > released);

    root.dispatch(&UiEvent::ScrollGesture {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 0.0,
        phase: ScrollPhase::Begin,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    let interrupted = offset.get().y;
    clock.advance(Duration::from_millis(48));
    root.tick();
    prepare(&mut root, Size::new(100.0, 100.0));
    assert_eq!(offset.get().y, interrupted);
}

#[test]
fn scroll_anchor_reveal_and_conditional_tail_follow_preserve_visual_context() {
    let offset = Reactive::new(ScrollOffset::ZERO);
    let theme = Arc::new(Theme::default());
    let anchor = ScrollAnchor::new(1, Point::new(0.0, 300.0), Point::new(0.0, 40.0)).unwrap();
    let scene =
        |content_height: f32, anchor: Option<ScrollAnchor>, reveal: Option<ScrollReveal>| {
            let scroll = Scroll::new(
                "History",
                Size::new(100.0, content_height),
                offset.clone(),
                Arc::clone(&theme),
            )
            .unwrap()
            .horizontal(false)
            .follow_tail(true)
            .anchor(anchor)
            .reveal(reveal);
            Element::new(scroll).child(Element::new(LabelBlock))
        };
    let mut root = UiRoot::new(scene(500.0, Some(anchor), None)).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));
    assert_eq!(offset.get().y, 260.0);

    offset.set(ScrollOffset::new(0.0, 100.0));
    root.reconcile(scene(500.0, Some(anchor), None)).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));
    assert_eq!(
        offset.get().y,
        100.0,
        "the same anchor revision must not fight user scrolling"
    );

    let reveal = ScrollReveal::new(1, nkdhr_render::Rect::new(0.0, 330.0, 20.0, 20.0)).unwrap();
    root.reconcile(scene(500.0, Some(anchor), Some(reveal)))
        .unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));
    assert_eq!(offset.get().y, 250.0);

    offset.set(ScrollOffset::new(0.0, 400.0));
    root.reconcile(scene(500.0, Some(anchor), Some(reveal)))
        .unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));
    root.reconcile(scene(560.0, Some(anchor), Some(reveal)))
        .unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));
    assert_eq!(
        offset.get().y,
        460.0,
        "content growth follows only when already near the old tail"
    );
}

#[test]
fn opt_in_snap_points_settle_on_the_host_clock() {
    let clock = ManualClock::default();
    let offset = Reactive::new(ScrollOffset::ZERO);
    let scroll = Scroll::new(
        "Pages",
        Size::new(100.0, 400.0),
        offset.clone(),
        Arc::new(Theme::default()),
    )
    .unwrap()
    .horizontal(false)
    .snap_points(ScrollAxis::Vertical, [0.0, 100.0, 200.0, 300.0])
    .unwrap();
    let mut root = UiRoot::with_clock(
        Element::new(scroll).child(Element::new(LabelBlock)),
        clock.clone(),
    )
    .unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    root.dispatch(&UiEvent::ScrollGesture {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 0.0,
        phase: ScrollPhase::Begin,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    root.dispatch(&UiEvent::ScrollGesture {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 70.0,
        phase: ScrollPhase::End,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    assert_eq!(offset.get().y, 70.0);
    clock.advance(Duration::from_millis(300));
    assert!(root.tick());
    prepare(&mut root, Size::new(100.0, 100.0));
    assert_eq!(offset.get().y, 100.0);
}

#[test]
fn elastic_boundary_motion_translates_content_but_reduced_motion_does_not() {
    fn boundary_display_list(motion_mode: MotionMode) -> nkdhr_render::DisplayList {
        let mut theme = Theme::default();
        theme.motion.mode = motion_mode;
        let scroll = Scroll::new(
            "Boundary",
            Size::new(100.0, 400.0),
            Reactive::new(ScrollOffset::ZERO),
            Arc::new(theme),
        )
        .unwrap()
        .horizontal(false);
        let mut root = UiRoot::new(Element::new(scroll).child(Element::new(LabelBlock))).unwrap();
        prepare(&mut root, Size::new(100.0, 100.0));
        root.dispatch(&UiEvent::PointerScroll {
            position: Point::new(50.0, 50.0),
            delta_x: 0.0,
            delta_y: -40.0,
            modifiers: Modifiers::default(),
        })
        .unwrap();
        prepare(&mut root, Size::new(100.0, 100.0))
    }

    let standard = boundary_display_list(MotionMode::Standard);
    assert!(
        standard
            .primitives()
            .iter()
            .any(|primitive| match primitive {
                Primitive::Shape(shape) => shape.transform != nkdhr_render::Transform::IDENTITY,
                Primitive::Texture(texture) =>
                    texture.transform != nkdhr_render::Transform::IDENTITY,
                Primitive::BackdropBlur(blur) =>
                    blur.transform != nkdhr_render::Transform::IDENTITY,
            })
    );
    let reduced = boundary_display_list(MotionMode::Reduced);
    assert!(
        reduced
            .primitives()
            .iter()
            .all(|primitive| match primitive {
                Primitive::Shape(shape) => shape.transform == nkdhr_render::Transform::IDENTITY,
                Primitive::Texture(texture) =>
                    texture.transform == nkdhr_render::Transform::IDENTITY,
                Primitive::BackdropBlur(blur) =>
                    blur.transform == nkdhr_render::Transform::IDENTITY,
            })
    );
}

#[test]
fn switching_to_reduced_motion_settles_active_scroll_spatial_state_immediately() {
    let offset = Reactive::new(ScrollOffset::ZERO);
    let scene = |motion_mode: MotionMode| {
        let mut theme = Theme::default();
        theme.motion.mode = motion_mode;
        Element::new(
            Scroll::new(
                "Boundary",
                Size::new(100.0, 400.0),
                offset.clone(),
                Arc::new(theme),
            )
            .unwrap()
            .horizontal(false),
        )
        .child(Element::new(LabelBlock))
    };
    let mut root = UiRoot::new(scene(MotionMode::Standard)).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));
    root.dispatch(&UiEvent::PointerScroll {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: -40.0,
        modifiers: Modifiers::default(),
    })
    .unwrap();
    let moving = prepare(&mut root, Size::new(100.0, 100.0));
    assert!(moving.primitives().iter().any(|primitive| match primitive {
        Primitive::Shape(shape) => shape.transform != nkdhr_render::Transform::IDENTITY,
        Primitive::Texture(texture) => texture.transform != nkdhr_render::Transform::IDENTITY,
        Primitive::BackdropBlur(blur) => blur.transform != nkdhr_render::Transform::IDENTITY,
    }));

    root.reconcile(scene(MotionMode::Reduced)).unwrap();
    let reduced = prepare(&mut root, Size::new(100.0, 100.0));
    assert!(
        reduced
            .primitives()
            .iter()
            .all(|primitive| match primitive {
                Primitive::Shape(shape) => shape.transform == nkdhr_render::Transform::IDENTITY,
                Primitive::Texture(texture) =>
                    texture.transform == nkdhr_render::Transform::IDENTITY,
                Primitive::BackdropBlur(blur) =>
                    blur.transform == nkdhr_render::Transform::IDENTITY,
            })
    );
}

#[test]
fn shift_wheel_and_vim_keys_share_scroll_direction_semantics() {
    let offset = Reactive::new(ScrollOffset::ZERO);
    let scroll = Scroll::new(
        "Timeline",
        Size::new(400.0, 400.0),
        offset.clone(),
        Arc::new(Theme::default()),
    )
    .unwrap();
    let mut root = UiRoot::new(Element::new(scroll).child(Element::new(LabelBlock))).unwrap();
    prepare(&mut root, Size::new(100.0, 100.0));

    root.dispatch(&UiEvent::PointerScroll {
        position: Point::new(50.0, 50.0),
        delta_x: 0.0,
        delta_y: 50.0,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    })
    .unwrap();
    assert_eq!(offset.get(), ScrollOffset::new(50.0, 0.0));
    for key in ["l", "j", "h", "k"] {
        root.dispatch(&UiEvent::KeyDown {
            key: Key::Character(key.to_owned()),
            modifiers: Modifiers::default(),
            repeat: false,
        })
        .unwrap();
    }
    assert_eq!(offset.get(), ScrollOffset::new(50.0, 0.0));
}

#[test]
fn text_input_edits_graphemes_and_password_semantics_never_expose_content() {
    let value = Reactive::new(String::new());
    let input = TextInput::new("Secret", value.clone(), Arc::new(Theme::default())).password(true);
    let mut root = UiRoot::with_text(Element::new(input), fixture_text_resources()).unwrap();
    prepare(&mut root, Size::new(200.0, 44.0));
    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(40.0, 22.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(40.0, 22.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::TextInput("a\u{301}🙂".to_owned()))
        .unwrap();
    assert_eq!(value.get(), "a\u{301}🙂");
    let semantic_value = root.semantic_tree()[0].semantics.value.clone().unwrap();
    assert_eq!(semantic_value, "••");
    assert!(!semantic_value.contains('🙂'));

    root.dispatch(&UiEvent::KeyDown {
        key: Key::Backspace,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert_eq!(value.get(), "a\u{301}");
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Backspace,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    assert!(value.get().is_empty());
}

#[test]
fn retained_text_and_text_input_use_shared_glyph_geometry() {
    let theme = Arc::new(Theme::default());
    let value = Reactive::new("abc🙂".to_owned());
    let scene = Element::new(Flex {
        axis: Axis::Vertical,
        gap: 8.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Stretch,
    })
    .child(Element::new(Text::new(
        "nkdhr UI 你好 🚀",
        theme.text_style(TextRole::Body),
        theme.palette.text_primary,
    )))
    .child(Element::new(TextInput::new(
        "Command",
        value.clone(),
        Arc::clone(&theme),
    )));
    let mut root = UiRoot::with_text(scene, fixture_text_resources()).unwrap();
    let display_list = prepare(&mut root, Size::new(260.0, 90.0));
    assert!(
        display_list
            .primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::Texture(_)))
    );
    assert!(root.texture_store().is_some());

    // The input begins on the second row. A click at its left text inset must
    // place the caret at the first glyph, rather than the legacy shell-end fallback.
    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(12.0, 50.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(12.0, 50.0),
        button: PointerButton::Primary,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
    .unwrap();
    root.dispatch(&UiEvent::TextInput("X".to_owned())).unwrap();
    assert_eq!(value.get(), "Xabc🙂");
}

#[test]
fn clipboard_reads_are_targeted_and_copy_cut_follow_password_policy() {
    let value = Reactive::new("alpha".to_owned());
    let input = TextInput::new("Clipboard", value.clone(), Arc::new(Theme::default()));
    let mut root = UiRoot::with_text(Element::new(input), fixture_text_resources()).unwrap();
    prepare(&mut root, Size::new(220.0, 44.0));
    primary_click(&mut root, Point::new(30.0, 22.0), Modifiers::default());
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Character("a".to_owned()),
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();

    let copied = root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("c".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    assert_eq!(
        copied.clipboard,
        vec![ClipboardRequest::WriteText("alpha".to_owned())]
    );

    let paste = root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("v".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    let target = match paste.clipboard.as_slice() {
        [ClipboardRequest::ReadText { target }] => *target,
        other => panic!("unexpected clipboard requests: {other:?}"),
    };
    root.set_focus(None).unwrap();
    root.dispatch(&UiEvent::ClipboardText {
        target,
        text: "粘贴🙂".to_owned(),
    })
    .unwrap();
    assert_eq!(value.get(), "粘贴🙂");
    root.set_focus(Some(target)).unwrap();
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Character("a".to_owned()),
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    let cut = root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("x".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    assert_eq!(
        cut.clipboard,
        vec![ClipboardRequest::WriteText("粘贴🙂".to_owned())]
    );
    assert!(value.get().is_empty());

    let secret = Reactive::new("secret".to_owned());
    let password = TextInput::new("Password", secret.clone(), Arc::new(Theme::default()))
        .password(true)
        .password_copy_policy(PasswordCopyPolicy::Deny);
    let mut password_root =
        UiRoot::with_text(Element::new(password), fixture_text_resources()).unwrap();
    prepare(&mut password_root, Size::new(220.0, 44.0));
    primary_click(
        &mut password_root,
        Point::new(30.0, 22.0),
        Modifiers::default(),
    );
    password_root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("a".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    let denied = password_root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("c".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    assert!(denied.clipboard.is_empty());
}

#[test]
fn undo_redo_and_formatter_preserve_the_explicit_caret() {
    let value = Reactive::new(String::new());
    let input = TextInput::new("Formatted", value.clone(), Arc::new(Theme::default()))
        .formatter(|edit| TextInputEdit::new(edit.value.to_uppercase(), edit.selection));
    let mut root = UiRoot::with_text(Element::new(input), fixture_text_resources()).unwrap();
    prepare(&mut root, Size::new(220.0, 44.0));
    primary_click(&mut root, Point::new(12.0, 22.0), Modifiers::default());
    root.dispatch(&UiEvent::TextInput("ab".to_owned())).unwrap();
    root.dispatch(&UiEvent::KeyDown {
        key: Key::ArrowLeft,
        modifiers: Modifiers::default(),
        repeat: false,
    })
    .unwrap();
    root.dispatch(&UiEvent::TextInput("x".to_owned())).unwrap();
    assert_eq!(value.get(), "AXB");

    root.dispatch(&UiEvent::KeyDown {
        key: Key::Character("z".to_owned()),
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    assert_eq!(value.get(), "AB");
    root.dispatch(&UiEvent::KeyDown {
        key: Key::Character("z".to_owned()),
        modifiers: Modifiers {
            control: true,
            shift: true,
            ..Modifiers::default()
        },
        repeat: false,
    })
    .unwrap();
    assert_eq!(value.get(), "AXB");

    let secret = Reactive::new(String::new());
    let password =
        TextInput::new("Secret", secret.clone(), Arc::new(Theme::default())).password(true);
    let mut password_root =
        UiRoot::with_text(Element::new(password), fixture_text_resources()).unwrap();
    prepare(&mut password_root, Size::new(220.0, 44.0));
    primary_click(
        &mut password_root,
        Point::new(12.0, 22.0),
        Modifiers::default(),
    );
    password_root
        .dispatch(&UiEvent::TextInput("sensitive".to_owned()))
        .unwrap();
    password_root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("z".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    assert_eq!(secret.get(), "sensitive");
}

#[test]
fn pointer_double_and_triple_click_select_unicode_word_and_line_units() {
    let value = Reactive::new("alpha beta\nlast".to_owned());
    let input = TextInput::new("Multiline", value, Arc::new(Theme::default()))
        .multiline(3)
        .unwrap();
    let mut root = UiRoot::with_text(Element::new(input), fixture_text_resources()).unwrap();
    prepare(&mut root, Size::new(240.0, 80.0));

    primary_click_count(&mut root, Point::new(20.0, 18.0), Modifiers::default(), 2);
    let word = root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("c".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    assert_eq!(
        word.clipboard,
        vec![ClipboardRequest::WriteText("alpha".to_owned())]
    );

    primary_click_count(&mut root, Point::new(20.0, 38.0), Modifiers::default(), 3);
    let line = root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Character("c".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            repeat: false,
        })
        .unwrap();
    assert_eq!(
        line.clipboard,
        vec![ClipboardRequest::WriteText("last".to_owned())]
    );
}

#[test]
fn debounced_validation_ignores_stale_generations_and_retains_backend_text() {
    let clock = ManualClock::default();
    let value = Reactive::new(String::new());
    let result = Reactive::new(None);
    let requests = Rc::new(RefCell::<Vec<TextInputValidationRequest>>::new(Vec::new()));
    let callback_requests = Rc::clone(&requests);
    let input = TextInput::new("Validated", value.clone(), Arc::new(Theme::default()))
        .validation(
            TextInputValidationTrigger::OnChange {
                debounce: Duration::from_millis(100),
            },
            result.clone(),
            move |request| callback_requests.borrow_mut().push(request),
        )
        .unwrap();
    let mut root =
        UiRoot::with_clock_and_text(Element::new(input), clock.clone(), fixture_text_resources())
            .unwrap();
    prepare(&mut root, Size::new(240.0, 44.0));
    primary_click(&mut root, Point::new(12.0, 22.0), Modifiers::default());
    root.dispatch(&UiEvent::TextInput("a".to_owned())).unwrap();
    clock.advance(Duration::from_millis(50));
    assert!(root.tick());
    root.dispatch(&UiEvent::TextInput("b".to_owned())).unwrap();
    clock.advance(Duration::from_millis(99));
    assert!(root.tick());
    assert!(requests.borrow().is_empty());
    clock.advance(Duration::from_millis(1));
    assert!(root.tick());
    let first = requests.borrow()[0].clone();
    assert_eq!(first.value, "ab");

    result.set(Some(TextInputValidationResult::invalid(
        first.generation.saturating_sub(1),
        "stale",
    )));
    prepare(&mut root, Size::new(240.0, 44.0));
    assert!(
        !root.semantic_tree()[0]
            .semantics
            .value
            .as_deref()
            .unwrap()
            .contains("stale")
    );

    result.set(Some(TextInputValidationResult::invalid(
        first.generation,
        "not available",
    )));
    prepare(&mut root, Size::new(240.0, 44.0));
    assert!(
        root.semantic_tree()[0]
            .semantics
            .value
            .as_deref()
            .unwrap()
            .contains("invalid: not available")
    );
    assert_eq!(value.get(), "ab");

    root.dispatch(&UiEvent::TextInput("c".to_owned())).unwrap();
    prepare(&mut root, Size::new(240.0, 44.0));
    assert!(
        !root.semantic_tree()[0]
            .semantics
            .value
            .as_deref()
            .unwrap()
            .contains("not available")
    );
    clock.advance(Duration::from_millis(100));
    assert!(root.tick());
    let second = requests.borrow()[1].clone();
    result.set(Some(TextInputValidationResult::backend_error(
        second.generation,
        "offline",
    )));
    prepare(&mut root, Size::new(240.0, 44.0));
    let semantic = root.semantic_tree()[0].semantics.value.clone().unwrap();
    assert!(semantic.contains("validation unavailable: offline"));
    assert!(semantic.starts_with("abc"));

    root.dispatch(&UiEvent::TextInput("d".to_owned())).unwrap();
    clock.advance(Duration::from_millis(100));
    assert!(root.tick());
    let third = requests.borrow()[2].clone();
    result.set(Some(TextInputValidationResult::valid(third.generation)));
    prepare(&mut root, Size::new(240.0, 44.0));
    assert!(
        root.semantic_tree()[0]
            .semantics
            .value
            .as_deref()
            .unwrap()
            .contains("valid")
    );
    clock.advance(Duration::from_millis(220));
    assert!(root.tick());
    prepare(&mut root, Size::new(240.0, 44.0));
    assert!(
        !root.semantic_tree()[0]
            .semantics
            .value
            .as_deref()
            .unwrap()
            .contains("valid")
    );
}

#[test]
fn blur_and_submit_validation_fire_only_at_their_declared_boundaries() {
    let blur_requests = Rc::new(RefCell::<Vec<TextInputValidationRequest>>::new(Vec::new()));
    let blur_callback = Rc::clone(&blur_requests);
    let blur_input = TextInput::new(
        "Blur validation",
        Reactive::new("draft".to_owned()),
        Arc::new(Theme::default()),
    )
    .validation(
        TextInputValidationTrigger::OnBlur,
        Reactive::new(None),
        move |request| blur_callback.borrow_mut().push(request),
    )
    .unwrap();
    let mut blur_root =
        UiRoot::with_text(Element::new(blur_input), fixture_text_resources()).unwrap();
    prepare(&mut blur_root, Size::new(240.0, 44.0));
    primary_click(&mut blur_root, Point::new(20.0, 22.0), Modifiers::default());
    blur_root
        .dispatch(&UiEvent::TextInput("!".to_owned()))
        .unwrap();
    assert!(blur_requests.borrow().is_empty());
    blur_root.set_focus(None).unwrap();
    assert_eq!(blur_requests.borrow().len(), 1);

    let submit_requests = Rc::new(RefCell::<Vec<TextInputValidationRequest>>::new(Vec::new()));
    let submit_callback = Rc::clone(&submit_requests);
    let submit_input = TextInput::new(
        "Submit validation",
        Reactive::new("ready".to_owned()),
        Arc::new(Theme::default()),
    )
    .validation(
        TextInputValidationTrigger::OnSubmit,
        Reactive::new(None),
        move |request| submit_callback.borrow_mut().push(request),
    )
    .unwrap();
    let mut submit_root =
        UiRoot::with_text(Element::new(submit_input), fixture_text_resources()).unwrap();
    prepare(&mut submit_root, Size::new(240.0, 44.0));
    primary_click(
        &mut submit_root,
        Point::new(20.0, 22.0),
        Modifiers::default(),
    );
    submit_root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Enter,
            modifiers: Modifiers::default(),
            repeat: false,
        })
        .unwrap();
    assert_eq!(submit_requests.borrow().len(), 1);
    assert_eq!(submit_requests.borrow()[0].value, "ready");
}

#[test]
fn multiline_enter_and_explicit_tab_completion_follow_declared_form_policy() {
    let multiline_value = Reactive::new("one".to_owned());
    let multiline = TextInput::new("Notes", multiline_value.clone(), Arc::new(Theme::default()))
        .multiline(2)
        .unwrap();
    let mut multiline_root =
        UiRoot::with_text(Element::new(multiline), fixture_text_resources()).unwrap();
    prepare(&mut multiline_root, Size::new(220.0, 60.0));
    primary_click(
        &mut multiline_root,
        Point::new(200.0, 18.0),
        Modifiers::default(),
    );
    multiline_root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Enter,
            modifiers: Modifiers::default(),
            repeat: false,
        })
        .unwrap();
    assert_eq!(multiline_value.get(), "one\n");

    let completions = Rc::new(Cell::new(0));
    let completion_count = Rc::clone(&completions);
    let command = TextInput::new(
        "Command",
        Reactive::new("/audio".to_owned()),
        Arc::new(Theme::default()),
    )
    .tab_behavior(TextInputTabBehavior::Complete)
    .on_complete(move |_| completion_count.set(completion_count.get() + 1));
    let mut command_root =
        UiRoot::with_text(Element::new(command), fixture_text_resources()).unwrap();
    prepare(&mut command_root, Size::new(220.0, 44.0));
    primary_click(
        &mut command_root,
        Point::new(20.0, 22.0),
        Modifiers::default(),
    );
    let tab = command_root
        .dispatch(&UiEvent::KeyDown {
            key: Key::Tab,
            modifiers: Modifiers::default(),
            repeat: false,
        })
        .unwrap();
    assert!(tab.handled);
    assert_eq!(completions.get(), 1);
}

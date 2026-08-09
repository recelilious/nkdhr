use std::{cell::Cell, rc::Rc, sync::Arc};

use cosmic_text::{FontSystem, fontdb};
use nkdhr_render::{
    Color, DisplayListBuilder, Point, Primitive, TextureStore, software::SoftwareRenderer,
};
use nkdhr_ui::text::{TextConfig, TextResources, TextSystem};
use nkdhr_ui::{
    Axis, Button, ButtonVariant, Constraints, CrossAxisAlignment, Density, Element, Flex,
    GlassSurface, Insets, Key, List, ListItem, MainAxisAlignment, ManualClock, MaterialTier,
    Modifiers, MotionMode, PointerButton, Reactive, Scroll, ScrollOffset, SemanticRole, Size,
    Slider, Text, TextInput, TextRole, Theme, Toggle, UiEvent, UiRoot, Widget,
};

fn prepare(root: &mut UiRoot, size: Size) -> nkdhr_render::DisplayList {
    root.layout(size).unwrap();
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    builder.finish()
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
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerMoved {
        position: Point::new(200.0, 20.0),
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(200.0, 20.0),
        button: PointerButton::Primary,
    })
    .unwrap();
    assert_eq!(activations.get(), 0);

    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(20.0, 20.0),
        button: PointerButton::Primary,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(20.0, 20.0),
        button: PointerButton::Primary,
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
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(22.0, 22.0),
        button: PointerButton::Primary,
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
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(100.0, 22.0),
        button: PointerButton::Primary,
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
    assert_eq!(fnv1a(&renderer.rgba8()), 17_414_112_130_380_794_923);

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
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(80.0, 72.0),
        button: PointerButton::Primary,
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
    })
    .unwrap();
    assert_eq!(offset.get().y, 72.0);
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
fn text_input_edits_graphemes_and_password_semantics_never_expose_content() {
    let value = Reactive::new(String::new());
    let input = TextInput::new("Secret", value.clone(), Arc::new(Theme::default())).password(true);
    let mut root = UiRoot::with_text(Element::new(input), fixture_text_resources()).unwrap();
    prepare(&mut root, Size::new(200.0, 44.0));
    root.dispatch(&UiEvent::PointerDown {
        position: Point::new(40.0, 22.0),
        button: PointerButton::Primary,
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(40.0, 22.0),
        button: PointerButton::Primary,
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
    })
    .unwrap();
    root.dispatch(&UiEvent::PointerUp {
        position: Point::new(12.0, 50.0),
        button: PointerButton::Primary,
    })
    .unwrap();
    root.dispatch(&UiEvent::TextInput("X".to_owned())).unwrap();
    assert_eq!(value.get(), "Xabc🙂");
}

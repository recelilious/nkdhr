use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use cosmic_text::{FontSystem, fontdb};
use nkdhr_render::{
    Color, DisplayList, DisplayListBuilder, Point, Primitive, Rect, TextureStore,
    software::SoftwareRenderer,
};
use nkdhr_settings::{
    AppearanceSetting, AppearanceSettings, INSPECTOR_WIDTH, LAYOUT_INSET, MotionPreference,
    SettingsAssets, SettingsFeedbackKind, SettingsLayoutMode,
};
use nkdhr_ui::{
    CompiledMotionCurve, ManualClock, MaterialCapabilities, Modifiers, PointerButton, ScrollPhase,
    SemanticRole, Size, Theme, UiEvent, UiRoot,
    text::{TextConfig, TextResources, TextSystem},
};

const GOLDEN_WIDTH: u32 = 1_160;
const GOLDEN_HEIGHT: u32 = 760;

fn fixture_text_resources() -> TextResources {
    let mut database = fontdb::Database::new();
    for bytes in [
        include_bytes!("fonts/MapleMonoNF-CN.appearance.subset.ttf").as_slice(),
        include_bytes!("fonts/MapleMonoNF-CN-Italic.appearance.subset.ttf").as_slice(),
        include_bytes!("../../nkdhr-ui/tests/fonts/NotoSansLatin.subset.ttf").as_slice(),
        include_bytes!("fonts/NotoSansCJKsc.appearance.subset.otf").as_slice(),
        include_bytes!("../../nkdhr-ui/tests/fonts/NotoColorEmoji.subset.ttf").as_slice(),
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

fn capabilities() -> MaterialCapabilities {
    MaterialCapabilities {
        backdrop_blur: true,
        reduced_transparency: false,
        high_contrast: false,
    }
}

fn settings_list(
    model: &AppearanceSettings,
    size: Size,
    capabilities: MaterialCapabilities,
) -> (UiRoot, DisplayList) {
    let mut text_resources = fixture_text_resources();
    let assets = SettingsAssets::load(text_resources.textures_mut()).unwrap();
    let element = model
        .element(size, Arc::new(Theme::default()), &assets, capabilities)
        .unwrap();
    let mut root = UiRoot::with_text(element, text_resources).unwrap();
    root.layout(size).unwrap();
    let mut builder = DisplayListBuilder::new();
    wallpaper(&mut builder, size);
    root.paint(&mut builder).unwrap();
    (root, builder.finish())
}

fn body_children(root: &UiRoot) -> [nkdhr_ui::WidgetId; 3] {
    let shell = root.children(root.root_id()).unwrap()[0];
    let body_padding = root.children(shell).unwrap()[1];
    let body_layout = root.children(body_padding).unwrap()[0];
    root.children(body_layout).unwrap().try_into().unwrap()
}

fn wallpaper(builder: &mut DisplayListBuilder, size: Size) {
    builder
        .rect(
            Rect::new(0.0, 0.0, size.width, size.height),
            Color::from_srgba8(14, 19, 35, 255),
        )
        .unwrap();
    let stripe = size.width / 8.0;
    for index in 0..8 {
        let color = if index % 2 == 0 {
            Color::from_srgba8(63, 74, 120, 255)
        } else {
            Color::from_srgba8(94, 54, 116, 255)
        };
        builder
            .rect(
                Rect::new(index as f32 * stripe, 0.0, stripe, size.height),
                color,
            )
            .unwrap();
    }
    builder
        .rounded_rect(
            Rect::new(size.width * 0.12, size.height * 0.10, 330.0, 260.0),
            nkdhr_render::CornerRadii::all(96.0),
            Color::from_srgba8(80, 162, 180, 220),
        )
        .unwrap();
    builder
        .rounded_rect(
            Rect::new(size.width * 0.62, size.height * 0.52, 360.0, 260.0),
            nkdhr_render::CornerRadii::all(112.0),
            Color::from_srgba8(187, 104, 158, 220),
        )
        .unwrap();
}

fn dump_transition_frame(root: &mut UiRoot, size: Size, name: &str) {
    let Some(directory) = std::env::var_os("DUMP_SETTINGS_TRANSITION") else {
        return;
    };
    fs::create_dir_all(&directory).unwrap();
    let mut builder = DisplayListBuilder::new();
    wallpaper(&mut builder, size);
    root.paint(&mut builder).unwrap();
    let mut renderer = SoftwareRenderer::new(size.width as u32, size.height as u32).unwrap();
    renderer.clear(Color::from_srgba8(14, 19, 35, 255));
    renderer
        .render(&builder.finish(), root.texture_store().unwrap(), 1.0)
        .unwrap();
    fs::write(PathBuf::from(directory).join(name), renderer.ppm()).unwrap();
}

#[test]
fn production_view_lays_out_at_every_accepted_width_mode() {
    let model = AppearanceSettings::new();
    model.set_professional_mode(true);
    for (width, mode) in [
        (1_160.0, SettingsLayoutMode::ThreeColumn),
        (1_000.0, SettingsLayoutMode::NavigationAndContent),
        (760.0, SettingsLayoutMode::CompactNavigation),
        (640.0, SettingsLayoutMode::SingleColumn),
    ] {
        let size = Size::new(width, 760.0);
        let (mut root, list) = settings_list(&model, size, capabilities());
        assert_eq!(SettingsLayoutMode::for_width(width), mode);
        assert_eq!(
            list.primitives()
                .iter()
                .filter(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
                .count(),
            1,
            "only the outer Settings glass may blur at {width}px"
        );
        if let Some(directory) = std::env::var_os("DUMP_SETTINGS_ORACLES") {
            let mut renderer = SoftwareRenderer::new(width as u32, size.height as u32).unwrap();
            renderer.clear(Color::from_srgba8(14, 19, 35, 255));
            renderer
                .render(&list, root.texture_store().unwrap(), 1.0)
                .unwrap();
            fs::write(
                PathBuf::from(directory).join(format!("appearance-{width:.0}.ppm")),
                renderer.ppm(),
            )
            .unwrap();
        }
        let semantics = root.semantic_tree();
        assert!(semantics.iter().any(|node| {
            node.semantics.role == SemanticRole::Group
                && node.semantics.label.as_deref() == Some("nkdhr 设置")
        }));
        assert!(semantics.iter().any(|node| {
            node.semantics.role == SemanticRole::List
                && node.semantics.label.as_deref() == Some("设置分类")
        }));
        assert!(semantics.iter().any(|node| {
            node.semantics.role == SemanticRole::Text
                && node.semantics.label.as_deref() == Some("外观与交互")
        }));
        if mode != SettingsLayoutMode::ThreeColumn {
            let shell = root.children(root.root_id()).unwrap()[0];
            let body_padding = root.children(shell).unwrap()[1];
            let body_layout = root.children(body_padding).unwrap()[0];
            let drawer_barrier = root.children(body_layout).unwrap()[2];
            let drawer_left =
                width - LAYOUT_INSET - INSPECTOR_WIDTH.min(width - LAYOUT_INSET * 2.0);
            assert_eq!(
                root.hit_test(Point::new(drawer_left + 20.0, 650.0)),
                Some(drawer_barrier),
                "blank drawer material must block pointer-through at {width}px"
            );
        }
    }
}

#[test]
fn blur_config_and_accessibility_capability_reach_the_outer_surface() {
    let model = AppearanceSettings::new();
    let size = Size::new(1_160.0, 760.0);
    let (_, capable) = settings_list(&model, size, capabilities());
    assert!(
        capable
            .primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
    );

    let (_, reduced) = settings_list(
        &model,
        size,
        MaterialCapabilities {
            reduced_transparency: true,
            ..capabilities()
        },
    );
    assert!(
        !reduced
            .primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
    );
}

#[test]
fn backend_feedback_is_local_visible_and_generation_ordered() {
    let model = AppearanceSettings::new();
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let stale = model.begin_apply(AppearanceSetting::BackgroundBlur, "正在应用背景模糊");
    let latest_blur = model.begin_apply(AppearanceSetting::BackgroundBlur, "正在重试背景模糊");
    let wallpaper = model.begin_apply(AppearanceSetting::WallpaperAdaptive, "正在更新壁纸配色");
    assert!(!model.complete_apply(stale, Ok("不应显示的旧结果".to_owned())));
    let snapshot = model.snapshot();
    assert_eq!(snapshot.feedback, SettingsFeedbackKind::Pending);
    assert_eq!(
        snapshot.feedback_setting,
        Some(AppearanceSetting::WallpaperAdaptive)
    );
    assert_eq!(
        snapshot.pending_settings,
        vec![
            AppearanceSetting::WallpaperAdaptive,
            AppearanceSetting::BackgroundBlur,
        ]
    );

    let (mut root, _) = settings_list(&model, size, capabilities());
    assert!(root.frame_requested(), "pending edge requests host frames");
    let pending = root.semantic_tree().into_iter().find(|node| {
        node.semantics.role == SemanticRole::Toggle
            && node.semantics.label.as_deref() == Some("壁纸自适应")
    });
    assert_eq!(
        pending.unwrap().semantics.value.as_deref(),
        Some("on; pending; effective on")
    );

    assert!(model.complete_apply(latest_blur, Ok("背景模糊已生效".to_owned())));
    assert_eq!(
        model.snapshot().pending_settings,
        vec![AppearanceSetting::WallpaperAdaptive]
    );
    assert!(model.complete_apply(wallpaper, Err("配色服务暂时不可用".to_owned())));
    let snapshot = model.snapshot();
    assert_eq!(snapshot.feedback, SettingsFeedbackKind::Error);
    assert_eq!(snapshot.status, "配色服务暂时不可用");
}

#[test]
fn accepted_wide_composition_matches_the_committed_software_golden() {
    let model = AppearanceSettings::new();
    model.set_professional_mode(true);
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let (root, list) = settings_list(&model, size, capabilities());
    let mut renderer = SoftwareRenderer::new(GOLDEN_WIDTH, GOLDEN_HEIGHT).unwrap();
    renderer.clear(Color::from_srgba8(14, 19, 35, 255));
    renderer
        .render(&list, root.texture_store().unwrap(), 1.0)
        .unwrap();
    let actual = renderer.ppm();
    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        fs::write(&path, &actual).unwrap();
    }
    let expected = fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "failed to read {}: {error}; run UPDATE_GOLDENS=1 cargo test -p nkdhr-settings --test settings_view",
            path.display()
        )
    });
    assert_eq!(actual, expected, "Settings composition golden changed");
}

#[test]
fn professional_motion_workspace_uses_the_owner_approved_p1_allocation() {
    let model = AppearanceSettings::new();
    model.open_motion_editor();
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let (mut root, list) = settings_list(&model, size, capabilities());
    let [navigation, graph_workspace, inspector] = body_children(&root);
    assert_eq!(root.rect(navigation).unwrap().width, 64.0);
    let navigation_rect = root.rect(navigation).unwrap();
    let mut stack = vec![navigation];
    let mut compact_icons = Vec::new();
    while let Some(widget) = stack.pop() {
        let children = root.children(widget).unwrap();
        if children.is_empty() {
            let rect = root.rect(widget).unwrap();
            if (rect.width - 18.0).abs() < 0.001 && (rect.height - 18.0).abs() < 0.001 {
                compact_icons.push(rect);
            }
        } else {
            stack.extend_from_slice(children);
        }
    }
    assert_eq!(compact_icons.len(), 9);
    for icon in compact_icons {
        assert!((icon.x + icon.width * 0.5 - (navigation_rect.x + 32.0)).abs() < 0.001);
    }
    assert_eq!(root.rect(graph_workspace).unwrap().width, 744.0);
    assert_eq!(root.rect(inspector).unwrap().width, 288.0);
    assert_eq!(root.rect(inspector).unwrap().x, 856.0);
    let [scope_rail, graph, preview] = root.children(graph_workspace).unwrap().try_into().unwrap();
    assert_eq!(root.rect(scope_rail).unwrap().height, 88.0);
    assert_eq!(root.rect(graph).unwrap().height, 356.0);
    assert_eq!(root.rect(preview).unwrap().height, 176.0);
    assert_eq!(
        list.primitives()
            .iter()
            .filter(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
            .count(),
        1,
        "the continuous Settings workface owns the only backdrop blur"
    );
    let semantics = root.semantic_tree();
    assert!(semantics.iter().any(|node| {
        node.semantics.role == SemanticRole::Group
            && node.semantics.label.as_deref() == Some("专业动画工作室")
    }));
    assert!(semantics.iter().any(|node| {
        node.semantics.role == SemanticRole::Group
            && node.semantics.label.as_deref() == Some("真实设置面板动画预览")
    }));

    if let Some(directory) = std::env::var_os("DUMP_MOTION_P1") {
        fs::create_dir_all(&directory).unwrap();
        let mut renderer = SoftwareRenderer::new(GOLDEN_WIDTH, GOLDEN_HEIGHT).unwrap();
        renderer.clear(Color::from_srgba8(14, 19, 35, 255));
        renderer
            .render(&list, root.texture_store().unwrap(), 1.0)
            .unwrap();
        fs::write(
            PathBuf::from(directory).join("motion-workspace-p1.ppm"),
            renderer.ppm(),
        )
        .unwrap();
    }
}

#[test]
fn professional_motion_graph_edits_the_persistent_editor_session() {
    let model = AppearanceSettings::new();
    model.open_motion_editor();
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let (mut root, _) = settings_list(&model, size, capabilities());
    let graph = root
        .semantic_tree()
        .into_iter()
        .find(|node| {
            node.semantics
                .label
                .as_deref()
                .is_some_and(|label| label.starts_with("动画曲线图："))
        })
        .expect("the professional graph exposes its semantic node");
    let plot = graph.bounds.inset(18.0);
    let before = model.motion_editor_snapshot();
    let compiled = CompiledMotionCurve::compile(&before.curve).unwrap();
    let viewport = before.viewport;
    let time = 0.5;
    let progress = compiled.sample(time);
    let position = Point::new(
        plot.x
            + plot.width
                * ((time - viewport.time_start()) / (viewport.time_end() - viewport.time_start()))
                    as f32,
        plot.bottom()
            - plot.height
                * ((progress - viewport.progress_start())
                    / (viewport.progress_end() - viewport.progress_start()))
                    as f32,
    );

    let result = root
        .dispatch(&UiEvent::PointerDown {
            position,
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 2,
        })
        .unwrap();
    assert!(result.handled);
    assert_eq!(result.focused, Some(graph.id));
    assert_eq!(result.pointer_capture, None);

    let after = model.motion_editor_snapshot();
    assert_eq!(after.curve.anchors.len(), before.curve.anchors.len() + 1);
    assert!(after.document_generation > before.document_generation);
    assert!(after.can_undo);

    let (mut rebuilt, _) = settings_list(&model, size, capabilities());
    assert!(rebuilt.semantic_tree().iter().any(|node| {
        node.semantics
            .label
            .as_deref()
            .is_some_and(|label| label.starts_with("动画曲线图："))
    }));
    assert_eq!(model.motion_editor_snapshot().curve.anchors.len(), 3);
}

#[test]
fn professional_motion_graph_viewport_zoom_reset_and_fit_are_live() {
    let model = AppearanceSettings::new();
    model.open_motion_editor();
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let (mut root, _) = settings_list(&model, size, capabilities());
    let semantics = root.semantic_tree();
    let graph = semantics
        .iter()
        .find(|node| {
            node.semantics
                .label
                .as_deref()
                .is_some_and(|label| label.starts_with("动画曲线图："))
        })
        .expect("the professional graph exposes its semantic node");
    let one_to_one = semantics
        .iter()
        .find(|node| {
            node.semantics.role == SemanticRole::Button
                && node.semantics.label.as_deref() == Some("100%")
        })
        .expect("the canonical viewport control is present");
    let fit = semantics
        .iter()
        .find(|node| {
            node.semantics.role == SemanticRole::Button
                && node.semantics.label.as_deref() == Some("适应")
        })
        .expect("the fit viewport control is present");
    let initial = model.motion_editor_snapshot();
    assert_eq!(initial.viewport.time_start(), 0.0);
    assert_eq!(initial.viewport.time_end(), 1.0);
    assert!((initial.viewport.progress_end() - 1.2).abs() < 1.0e-9);
    let document_generation = initial.document_generation;

    let zoom = root
        .dispatch(&UiEvent::PointerScroll {
            position: Point::new(
                graph.bounds.x + graph.bounds.width * 0.5,
                graph.bounds.y + 80.0,
            ),
            delta_x: 0.0,
            delta_y: -24.0,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        })
        .unwrap();
    assert!(zoom.handled);
    let zoomed = model.motion_editor_snapshot();
    assert!(zoomed.viewport.time_end() - zoomed.viewport.time_start() < 1.0);
    assert!(zoomed.viewport.progress_end() - zoomed.viewport.progress_start() < 1.2);
    assert_eq!(zoomed.document_generation, document_generation);

    for event in [
        UiEvent::PointerDown {
            position: Point::new(
                one_to_one.bounds.x + one_to_one.bounds.width * 0.5,
                one_to_one.bounds.y + one_to_one.bounds.height * 0.5,
            ),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        },
        UiEvent::PointerUp {
            position: Point::new(
                one_to_one.bounds.x + one_to_one.bounds.width * 0.5,
                one_to_one.bounds.y + one_to_one.bounds.height * 0.5,
            ),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        },
    ] {
        root.dispatch(&event).unwrap();
    }
    let reset = model.motion_editor_snapshot();
    assert_eq!(reset.viewport.time_start(), 0.0);
    assert_eq!(reset.viewport.time_end(), 1.0);
    assert_eq!(reset.viewport.progress_start(), 0.0);
    assert_eq!(reset.viewport.progress_end(), 1.0);

    for event in [
        UiEvent::PointerDown {
            position: Point::new(
                fit.bounds.x + fit.bounds.width * 0.5,
                fit.bounds.y + fit.bounds.height * 0.5,
            ),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        },
        UiEvent::PointerUp {
            position: Point::new(
                fit.bounds.x + fit.bounds.width * 0.5,
                fit.bounds.y + fit.bounds.height * 0.5,
            ),
            button: PointerButton::Primary,
            modifiers: Modifiers::default(),
            click_count: 1,
        },
    ] {
        root.dispatch(&event).unwrap();
    }
    let fitted = model.motion_editor_snapshot();
    assert!(fitted.viewport.progress_end() - fitted.viewport.progress_start() > 1.0);
    assert_eq!(fitted.document_generation, document_generation);

    let gesture_position = Point::new(
        graph.bounds.x + graph.bounds.width * 0.5,
        graph.bounds.y + graph.bounds.height * 0.5,
    );
    let begin = root
        .dispatch(&UiEvent::ScrollGesture {
            position: gesture_position,
            delta_x: 0.0,
            delta_y: 0.0,
            phase: ScrollPhase::Begin,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        })
        .unwrap();
    assert!(begin.handled);
    assert_eq!(begin.pointer_capture, Some(graph.id));
    let update = root
        .dispatch(&UiEvent::ScrollGesture {
            position: gesture_position,
            delta_x: 0.0,
            delta_y: -20.0,
            phase: ScrollPhase::Update,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        })
        .unwrap();
    assert!(update.handled);
    let end = root
        .dispatch(&UiEvent::ScrollGesture {
            position: gesture_position,
            delta_x: 0.0,
            delta_y: 0.0,
            phase: ScrollPhase::End,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        })
        .unwrap();
    assert!(end.handled);
    assert_eq!(end.pointer_capture, None);
    let touchpad_zoomed = model.motion_editor_snapshot();
    assert!(
        touchpad_zoomed.viewport.time_end() - touchpad_zoomed.viewport.time_start()
            < fitted.viewport.time_end() - fitted.viewport.time_start()
    );
    assert_eq!(touchpad_zoomed.document_generation, document_generation);
}

#[test]
fn professional_inspector_uses_interruptible_host_clocked_layout_motion() {
    let model = AppearanceSettings::new();
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let mut text_resources = fixture_text_resources();
    let assets = SettingsAssets::load(text_resources.textures_mut()).unwrap();
    let theme = Arc::new(Theme::default());
    let clock = ManualClock::default();
    let initial = model
        .element(size, Arc::clone(&theme), &assets, capabilities())
        .unwrap();
    let mut root = UiRoot::with_clock_and_text(initial, clock.clone(), text_resources).unwrap();
    root.layout(size).unwrap();
    let [_, content, inspector] = body_children(&root);
    assert_eq!(root.rect(content).unwrap().width, 720.0);
    assert_eq!(root.rect(inspector).unwrap().x, 1_144.0);
    dump_transition_frame(&mut root, size, "wide-closed.ppm");

    model.set_professional_mode(true);
    root.reconcile(
        model
            .element(size, Arc::clone(&theme), &assets, capabilities())
            .unwrap(),
    )
    .unwrap();
    root.layout(size).unwrap();
    assert!(root.frame_requested());
    assert_eq!(root.rect(content).unwrap().width, 720.0);
    dump_transition_frame(&mut root, size, "wide-opening-000.ppm");

    clock.advance(Duration::from_millis(140));
    assert!(root.tick());
    root.layout(size).unwrap();
    let opening_content = root.rect(content).unwrap().width;
    let opening_inspector_x = root.rect(inspector).unwrap().x;
    assert!((592.0..720.0).contains(&opening_content));
    assert!((856.0..1_144.0).contains(&opening_inspector_x));
    dump_transition_frame(&mut root, size, "wide-opening-140.ppm");

    model.set_professional_mode(false);
    root.reconcile(
        model
            .element(size, Arc::clone(&theme), &assets, capabilities())
            .unwrap(),
    )
    .unwrap();
    root.layout(size).unwrap();
    assert!((root.rect(content).unwrap().width - opening_content).abs() < 0.001);
    assert!((root.rect(inspector).unwrap().x - opening_inspector_x).abs() < 0.001);
    dump_transition_frame(&mut root, size, "wide-reversed-000.ppm");

    clock.advance(Duration::from_millis(240));
    assert!(root.tick());
    root.layout(size).unwrap();
    assert!((root.rect(content).unwrap().width - 720.0).abs() < 0.001);
    assert!((root.rect(inspector).unwrap().x - 1_144.0).abs() < 0.001);
    dump_transition_frame(&mut root, size, "wide-closed-final.ppm");
}

#[test]
fn compact_inspector_is_a_clipped_pointer_blocking_drawer_during_entry() {
    let model = AppearanceSettings::new();
    let size = Size::new(760.0, 760.0);
    let mut text_resources = fixture_text_resources();
    let assets = SettingsAssets::load(text_resources.textures_mut()).unwrap();
    let theme = Arc::new(Theme::default());
    let clock = ManualClock::default();
    let initial = model
        .element(size, Arc::clone(&theme), &assets, capabilities())
        .unwrap();
    let mut root = UiRoot::with_clock_and_text(initial, clock.clone(), text_resources).unwrap();
    root.layout(size).unwrap();
    let [_, content, barrier] = body_children(&root);
    let content_width = root.rect(content).unwrap().width;

    model.set_professional_mode(true);
    root.reconcile(
        model
            .element(size, Arc::clone(&theme), &assets, capabilities())
            .unwrap(),
    )
    .unwrap();
    root.layout(size).unwrap();
    let [_, _, open_barrier] = body_children(&root);
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    assert_eq!(barrier, open_barrier);
    assert_ne!(root.hit_test(Point::new(740.0, 650.0)), Some(barrier));

    clock.advance(Duration::from_millis(160));
    assert!(root.tick());
    root.layout(size).unwrap();
    let drawer = root.rect(barrier).unwrap();
    assert!((456.0..744.0).contains(&drawer.x));
    assert_eq!(root.rect(content).unwrap().width, content_width);
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    assert_eq!(root.hit_test(Point::new(740.0, 650.0)), Some(barrier));

    model.set_professional_mode(false);
    root.reconcile(model.element(size, theme, &assets, capabilities()).unwrap())
        .unwrap();
    root.layout(size).unwrap();
    assert_eq!(body_children(&root)[2], barrier);
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    assert_eq!(root.hit_test(Point::new(740.0, 650.0)), Some(barrier));

    clock.advance(Duration::from_millis(240));
    assert!(root.tick());
    root.layout(size).unwrap();
    let mut builder = DisplayListBuilder::new();
    root.paint(&mut builder).unwrap();
    assert_ne!(root.hit_test(Point::new(740.0, 650.0)), Some(barrier));
}

#[test]
fn reduced_motion_opens_the_inspector_without_spatial_transition() {
    let model = AppearanceSettings::new();
    model.set_motion_preference(MotionPreference::Reduced);
    let size = Size::new(GOLDEN_WIDTH as f32, GOLDEN_HEIGHT as f32);
    let mut text_resources = fixture_text_resources();
    let assets = SettingsAssets::load(text_resources.textures_mut()).unwrap();
    let theme = Arc::new(Theme::default());
    let clock = ManualClock::default();
    let initial = model
        .element(size, Arc::clone(&theme), &assets, capabilities())
        .unwrap();
    let mut root = UiRoot::with_clock_and_text(initial, clock, text_resources).unwrap();
    root.layout(size).unwrap();
    let [_, content, inspector] = body_children(&root);

    model.set_professional_mode(true);
    root.reconcile(model.element(size, theme, &assets, capabilities()).unwrap())
        .unwrap();
    root.layout(size).unwrap();
    assert_eq!(root.rect(content).unwrap().width, 592.0);
    assert_eq!(root.rect(inspector).unwrap().x, 856.0);
    assert!(!root.frame_requested());
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/appearance-settings.ppm")
}

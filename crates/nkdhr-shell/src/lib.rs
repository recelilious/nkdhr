//! Output-local shell UI. Unlike canvas nodes, these surfaces remain fixed to
//! one physical display while its workspace viewport moves underneath them.

use std::any::Any;
use std::mem::MaybeUninit;

use nkdhr_render::{DisplayList, TextureStore};
use nkdhr_ui::text::{TextConfig, TextResources, TextStyle, TextWrap};
use nkdhr_ui::{
    Align, Alignment, Constraints, DispatchResult, Element, EventCtx, GlassSurface, Insets,
    MaterialCapabilities, MaterialTier, MeasureCtx, Padding, Reactive, Size, Stack, Text, TextRole,
    ThemeRuntime, UiError, UiEvent, UiHost, UiResult, UiRoot, UiSurface, Widget, WidgetId,
};

/// The eight shell attachment zones are stable semantic identities. A zone may
/// be hidden without removing its identity, so plugins and motion histories do
/// not depend on incidental layout nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeRegion {
    TopLeft,
    TopCenter,
    TopRight,
    LeftCenter,
    RightCenter,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// First production output-local surface. Only the already-defined calm clock
/// state is composed here; the other seven regions attach to this same host as
/// their product models become available.
pub struct ShellSurface {
    clock_text: Reactive<String>,
    theme_runtime: ThemeRuntime,
    seen_theme_generation: u64,
    viewport: Size,
    capabilities: MaterialCapabilities,
    host: UiHost,
}

impl ShellSurface {
    pub fn new(
        viewport: Size,
        output_scale: f32,
        capabilities: MaterialCapabilities,
        theme_runtime: ThemeRuntime,
    ) -> UiResult<Self> {
        let text = TextResources::from_config(TextConfig::default(), output_scale)
            .map_err(|error| UiError::Text(error.to_string()))?;
        Self::with_text_resources(viewport, output_scale, capabilities, theme_runtime, text)
    }

    fn with_text_resources(
        viewport: Size,
        output_scale: f32,
        capabilities: MaterialCapabilities,
        theme_runtime: ThemeRuntime,
        text: TextResources,
    ) -> UiResult<Self> {
        let clock_text = Reactive::new(local_clock_text());
        let snapshot = theme_runtime.snapshot();
        let element = shell_element(&snapshot.theme(), &clock_text, capabilities);
        let mut root = UiRoot::with_text(element, text)?;
        root.set_theme_runtime(theme_runtime.clone());
        let host = UiHost::new(root, viewport, output_scale)?;
        Ok(Self {
            clock_text,
            theme_runtime,
            seen_theme_generation: snapshot.generation(),
            viewport,
            capabilities,
            host,
        })
    }

    fn refresh(&mut self, viewport: Size) -> UiResult<()> {
        let clock = local_clock_text();
        if self.clock_text.get() != clock {
            self.clock_text.set(clock);
        }
        let snapshot = self.theme_runtime.snapshot();
        if self.viewport != viewport || self.seen_theme_generation != snapshot.generation() {
            self.host.reconcile(shell_element(
                &snapshot.theme(),
                &self.clock_text,
                self.capabilities,
            ))?;
            self.viewport = viewport;
            self.seen_theme_generation = snapshot.generation();
        }
        Ok(())
    }
}

impl UiSurface for ShellSurface {
    fn render(&mut self, logical_size: Size, output_scale: f32) -> UiResult<()> {
        self.refresh(logical_size)?;
        self.host.resize(logical_size, output_scale)?;
        self.host.render().map(|_| ())
    }

    fn display_list(&self) -> &DisplayList {
        self.host.display_list()
    }

    fn textures(&self) -> &TextureStore {
        self.host.textures()
    }

    fn commit(&self) -> u64 {
        self.host.commit()
    }

    fn dispatch(&mut self, event: &UiEvent) -> UiResult<DispatchResult> {
        self.host.dispatch(event)
    }

    fn pointer_capture(&self) -> Option<WidgetId> {
        self.host.pointer_capture()
    }

    fn keyboard_focus(&self) -> Option<WidgetId> {
        self.host.keyboard_focus()
    }

    fn frame_requested(&mut self) -> bool {
        let clock = local_clock_text();
        if self.clock_text.get() != clock {
            self.clock_text.set(clock);
        }
        self.theme_runtime.snapshot().generation() != self.seen_theme_generation
            || self.host.frame_requested()
    }
}

fn shell_element(
    theme: &std::sync::Arc<nkdhr_ui::Theme>,
    clock_text: &Reactive<String>,
    capabilities: MaterialCapabilities,
) -> Element {
    let token = theme.typography.token(TextRole::Mono);
    let style = TextStyle {
        families: theme.typography.families.mono.clone(),
        weight: token.weight,
        font_size: token.font_size,
        line_height: token.line_height,
        wrap: TextWrap::None,
        ..TextStyle::default()
    };
    let clock = Element::new(InputShield).keyed(3_u64).child(
        Element::new(
            GlassSurface::new(theme.clone(), MaterialTier::CompactNode)
                .capabilities(capabilities)
                .radius(14.0)
                .padding(Insets::symmetric(14.0, 6.0)),
        )
        .keyed(4_u64)
        .child(
            Element::new(Text::bound(
                clock_text.clone(),
                style,
                theme.palette.text_primary,
            ))
            .keyed(5_u64),
        ),
    );
    Element::new(Stack).keyed(1_u64).child(
        Element::new(Align {
            horizontal: Alignment::Center,
            vertical: Alignment::Start,
        })
        .keyed(2_u64)
        .child(
            Element::new(Padding {
                insets: Insets::new(0.0, 14.0, 0.0, 0.0),
            })
            .child(clock),
        ),
    )
}

/// A visible shell surface is one hit target even if its current child is
/// informational. This prevents clicks in glass from leaking to a client
/// visually behind it.
#[derive(Debug, Clone, Copy)]
struct InputShield;

impl Widget for InputShield {
    fn create_state(&self) -> Box<dyn Any> {
        Box::new(())
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> UiResult<Size> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        if ctx.child_count() == 0 {
            Ok(constraints.min())
        } else {
            ctx.measure_child(0, constraints)
        }
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> UiResult<()> {
        if event.is_pointer() {
            ctx.set_handled();
        }
        Ok(())
    }

    fn accepts_pointer(&self) -> bool {
        true
    }
}

fn local_clock_text() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let epoch = libc::time_t::try_from(seconds).unwrap_or(libc::time_t::MAX);
    let mut local = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: `local` points to writable storage for one `tm`, and `epoch`
    // remains alive for the entire call. `localtime_r` writes only that object.
    let result = unsafe { libc::localtime_r(&epoch, local.as_mut_ptr()) };
    if result.is_null() {
        return "--:--".to_owned();
    }
    // SAFETY: a non-null `localtime_r` return initialized the supplied `tm`.
    let local = unsafe { local.assume_init() };
    format_clock(local.tm_hour, local.tm_min)
}

fn format_clock(hour: i32, minute: i32) -> String {
    format!("{hour:02}:{minute:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::Arc};

    use cosmic_text::{FontSystem, fontdb};
    use nkdhr_render::{
        Color, DisplayListBuilder, Primitive, Rect, TextureStore, software::SoftwareRenderer,
    };
    use nkdhr_ui::text::TextSystem;

    fn fixture_text_resources() -> TextResources {
        let mut database = fontdb::Database::new();
        for bytes in [
            include_bytes!("../../nkdhr-settings/tests/fonts/MapleMonoNF-CN.appearance.subset.ttf")
                .as_slice(),
            include_bytes!(
                "../../nkdhr-settings/tests/fonts/MapleMonoNF-CN-Italic.appearance.subset.ttf"
            )
            .as_slice(),
        ] {
            database.load_font_source(fontdb::Source::Binary(Arc::new(bytes.to_vec())));
        }
        let system = TextSystem::with_font_system(
            FontSystem::new_with_locale_and_db("zh-CN".to_owned(), database),
            TextConfig::default(),
        )
        .expect("fixture text system should initialize");
        TextResources::new(system, TextureStore::new(), 1.0)
            .expect("fixture text resources should initialize")
    }

    #[test]
    fn clock_is_fixed_width_and_zero_padded() {
        assert_eq!(format_clock(0, 5), "00:05");
        assert_eq!(format_clock(23, 59), "23:59");
    }

    #[test]
    fn all_eight_regions_have_stable_distinct_identity() {
        let regions = [
            EdgeRegion::TopLeft,
            EdgeRegion::TopCenter,
            EdgeRegion::TopRight,
            EdgeRegion::LeftCenter,
            EdgeRegion::RightCenter,
            EdgeRegion::BottomLeft,
            EdgeRegion::BottomCenter,
            EdgeRegion::BottomRight,
        ];
        assert_eq!(
            regions
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            8
        );
    }

    #[test]
    fn calm_clock_records_glyphs_and_material() {
        let size = Size::new(1280.0, 800.0);
        let mut surface = ShellSurface::new(
            size,
            1.0,
            MaterialCapabilities {
                backdrop_blur: true,
                reduced_transparency: false,
                high_contrast: false,
            },
            ThemeRuntime::default(),
        )
        .expect("clock surface should initialize");
        surface.render(size, 1.0).expect("clock should render");

        let primitives = surface.display_list().primitives();
        assert!(
            primitives
                .iter()
                .any(|primitive| matches!(primitive, Primitive::BackdropBlur(_)))
        );
        assert!(
            primitives
                .iter()
                .any(|primitive| matches!(primitive, Primitive::Texture(_))),
            "clock text must be recorded as glyph textures"
        );
    }

    #[test]
    fn calm_clock_matches_the_committed_software_golden() {
        const WIDTH: u32 = 360;
        const HEIGHT: u32 = 140;
        let size = Size::new(WIDTH as f32, HEIGHT as f32);
        let mut surface = ShellSurface::with_text_resources(
            size,
            1.0,
            MaterialCapabilities {
                backdrop_blur: true,
                reduced_transparency: false,
                high_contrast: false,
            },
            ThemeRuntime::default(),
            fixture_text_resources(),
        )
        .expect("clock surface should initialize");
        surface.render(size, 1.0).expect("clock should render");
        surface.clock_text.set("21:50".to_owned());
        surface
            .host
            .render()
            .expect("fixed clock value should repaint");
        assert_eq!(
            surface
                .display_list()
                .primitives()
                .iter()
                .filter(|primitive| matches!(primitive, Primitive::Texture(_)))
                .count(),
            5,
            "all five clock glyphs must survive a reactive minute update"
        );
        for texture in surface
            .display_list()
            .primitives()
            .iter()
            .filter_map(|primitive| match primitive {
                Primitive::Texture(texture) => Some(texture),
                _ => None,
            })
        {
            let asset = surface.textures().get(texture.texture).unwrap();
            let source = texture.source.unwrap();
            let mut sum = 0_u64;
            for y in source.y as usize..source.bottom() as usize {
                for x in source.x as usize..source.right() as usize {
                    sum += u64::from(asset.pixels()[y * asset.width() as usize + x]);
                }
            }
            assert!(sum > 0, "every recorded clock glyph needs atlas coverage");
        }

        let mut wallpaper = DisplayListBuilder::new();
        wallpaper
            .rect(
                Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32),
                Color::from_srgba8(18, 23, 42, 255),
            )
            .unwrap();
        for index in 0..9 {
            wallpaper
                .rect(
                    Rect::new(index as f32 * 40.0, 0.0, 20.0, HEIGHT as f32),
                    if index % 2 == 0 {
                        Color::from_srgba8(62, 96, 156, 255)
                    } else {
                        Color::from_srgba8(126, 74, 146, 255)
                    },
                )
                .unwrap();
        }
        let mut renderer = SoftwareRenderer::new(WIDTH, HEIGHT).unwrap();
        renderer.clear(Color::from_srgba8(18, 23, 42, 255));
        renderer
            .render(&wallpaper.finish(), &TextureStore::new(), 1.0)
            .unwrap();
        renderer
            .render(surface.display_list(), surface.textures(), 1.0)
            .unwrap();
        let actual = renderer.ppm();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/calm-clock.ppm");
        if std::env::var_os("UPDATE_GOLDENS").is_some_and(|value| value != "0") {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &actual).unwrap();
        }
        let expected = fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {}: {error}; run UPDATE_GOLDENS=1 cargo test -p nkdhr-shell",
                path.display()
            )
        });
        assert_eq!(actual, expected, "calm clock golden changed");
    }
}

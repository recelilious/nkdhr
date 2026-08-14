# nkdhr UI Stack — User and Application Guide

> [中文版本 / Chinese version](USAGE_zh_CN.md)

The nkdhr UI stack is the shared rendering and interaction foundation for the
integrated shell and every in-project system application. It consists of two
crates:

- `nkdhr-render` records renderer-independent 2D primitives and draws them with
  a batched OpenGL ES backend.
- `nkdhr-ui` builds a retained widget tree, layout, input, animation, themes
  and typed actions on top of those primitives.

The same widget code can run inside `nkdhr-canvas` as a compositor-owned scene
node or in a standalone Wayland client. Applications do not choose a different
widget set or rendering API for the two modes.

## Coordinates and color

Application and widget geometry uses logical `f32` coordinates. A render
target supplies its output scale when a display list is drawn; pixel snapping
is therefore a backend concern rather than something every widget repeats.

Colors use normalized RGBA components. `Color::from_srgba8` is the normal
constructor for design-token and application colors. Blending is
premultiplied-alpha throughout the GLES pipeline. Invalid non-finite geometry,
negative sizes, negative radii and invalid opacity are rejected when commands
are recorded rather than reaching a shader.

## Drawing primitives (`nkdhr-render`)

A frame is described by a `DisplayList`. Use `DisplayListBuilder` to record
commands in painter's order:

```rust
use nkdhr_render::{
    Color, CornerRadii, DisplayListBuilder, Rect, Shadow, Transform,
};

let mut list = DisplayListBuilder::new();
list.shadow(
    Rect::new(24.0, 24.0, 320.0, 180.0),
    CornerRadii::all(18.0),
    Shadow::new(0.0, 10.0, 24.0, 0.0, Color::from_srgba8(0, 0, 0, 96)),
)?;
list.backdrop_blur(
    Rect::new(24.0, 24.0, 320.0, 180.0),
    CornerRadii::all(18.0),
    32.0,
)?;
list.rounded_rect(
    Rect::new(24.0, 24.0, 320.0, 180.0),
    CornerRadii::all(18.0),
    Color::from_srgba8(36, 40, 59, 255),
)?;
list.border(
    Rect::new(24.0, 24.0, 320.0, 180.0),
    CornerRadii::all(18.0),
    1.0,
    Color::from_srgba8(238, 242, 255, 48),
)?;

list.with_clip(Rect::new(32.0, 32.0, 304.0, 164.0), |list| {
    list.with_transform(Transform::translation(8.0, 0.0), |list| {
        list.rect(
            Rect::new(0.0, 0.0, 120.0, 32.0),
            Color::from_srgba8(91, 182, 255, 255),
        )
    })
})?;

let display_list = list.finish();
# Ok::<(), nkdhr_render::BuildError>(())
```

The primitive surface is deliberately small:

- rectangles and independently rounded corners;
- uniform borders following the same corner geometry;
- soft outer shadows with offset, spread and blur;
- painter-order backdrop blur through transformed rounded masks;
- RGBA textures with source cropping, opacity and nearest/linear sampling;
- nested axis-aligned clips;
- affine transforms for translated, scaled and rotated content.

Clips are target-axis-aligned. Translation and scale may be active when a clip
is pushed; a rotated or sheared clip is rejected. Content inside that clip may
still use arbitrary affine transforms. This explicit rule keeps clipping
deterministic and cheap without a hidden stencil fallback.

`TextureStore` owns immutable or revisioned RGBA assets and single-channel
alpha masks. Display lists refer to stable `TextureId` values; each GLES
context uploads and caches only the revisions it needs. RGBA assets may be
modulated by a tint, while one alpha mask can be reused at any text color.
Removing an asset invalidates it in every backend at the next prepare pass.

Backdrop blur reads the target behind the primitive, then later fill, border
and content commands draw sharply above it. A compositor doing partial redraws
must call `PreparedDisplayList::expand_damage` before repainting lower layers
and pass that expanded damage to `GlesBackend::draw`. Declaring
`MaterialCapabilities::backdrop_blur` without honoring this dependency contract
is invalid; leave the capability off to receive the compensated material fill.

## Text (`nkdhr-ui`, UI-2)

Text widgets accept a font family list, weight, style, logical size, line
height, wrapping mode and alignment. Shaping and fallback use `cosmic-text`.
Glyph masks and color emoji live in bounded atlases owned by the render
context. CJK, Latin, emoji and bidirectional text can be mixed in one run.

Text layout is cached by the tuple of content, font attributes, wrap width,
scale and locale. Scrolling a stable paragraph reuses shaping and atlas
entries; it does not rebuild glyphs every frame.

Create one `TextSystem` for a UI render context. The default `TextConfig`
retains 256 shaped paragraphs and bounds the atlas to four 1024-pixel mask
pages plus two color pages; applications with tighter budgets can lower those
limits explicitly. Call `layout` when content or layout-affecting style
changes, then call `begin_frame().draw(...)` to record the visible glyphs into
a `DisplayListBuilder`. The optional target-space clip both records the clip
and avoids visiting distant lines in a long paragraph. Keep the returned frame
guard alive for the whole text submission so pages referenced by that frame
cannot be evicted underneath it.

Text color is intentionally not part of the layout key. Retained paint caches
may reuse a shaped layout across color changes, but must compare
`atlas_generation()` before reusing previously recorded glyph texture
coordinates: the generation advances whenever page eviction invalidates them.

## Widgets and layout (`nkdhr-ui`, UI-3)

Widgets form a retained tree. Public application code composes standard
widgets instead of issuing primitive commands directly:

- `Row`, `Column`, `Stack`, `Padding` and `Align` for flex-style layout;
- `Button`, `Toggle`, `Slider`, `List`, `Scroll` and `TextInput`;
- focus scopes, keyboard traversal and pointer capture;
- declarative state bindings and time-based animation values.

Layout always runs measure then arrange. A widget receives finite constraints
from its parent and returns a finite size; overflow is explicit and clipped by
the widget that owns it. Hit testing uses the arranged tree in reverse paint
order, so visual and input stacking cannot diverge.

The style-neutral framework gate is implemented. `Element` describes the next
tree, while `UiRoot::reconcile` retains a node's generational `WidgetId` and
private state when its sibling key and widget type still match. `Flex`,
`Padding`, `Align`, `Stack` and `Clip` are structural algorithms only: callers
provide every gap, inset and alignment, and none chooses a color, radius,
typography rule or motion curve. A host runs explicit `layout`, `paint`,
`dispatch` and `tick` boundaries. An animation tick invokes a widget's
`AnimationCtx` before layout so host-clocked motion may update geometry; a
paint-only transition can still sample time in `PaintCtx`. `Reactive<T>`
mutations queue layout, paint or semantic invalidation for the next boundary
rather than re-entering a borrowed tree.

Pointer events use the last paint order for hit testing, bubble from target to
root, and keep hover independent from pointer capture. Keyboard focus follows
semantic tree order and stays within the nearest focus scope. Host-normalized
press/release events retain modifiers and a nonzero click sequence count, so
selection and system double-click timing are not guessed by components. Text
and IME events reject composition selections that are not UTF-8 boundaries.
Clipboard shortcuts emit `ClipboardRequest`; a read response returns as a
`ClipboardText` event explicitly addressed to the requesting `WidgetId`, so a
focus change while the host reads cannot paste into another field.
Animations read a host-provided monotonic `Clock`; the framework does not select
durations or easing. A flattened semantic tree is available to future host
accessibility adapters.

The owner-controlled component gate has now produced its first production
slice. `Theme` contains the approved density metrics, spacing, radii,
typography, semantic palette, seven glass tiers and motion profiles. A
`GlassSurface` resolves explicit backdrop-blur capability versus compensated
no-blur fill, reduced transparency and high contrast. `Button`, `Toggle`,
`Slider`, stable-ID `List`/`ListItem`, `Scroll`, `Text` and `TextInput` are public and
share focus, pointer/keyboard, semantic, material and motion contracts.
`Reactive<T>` is their UI-thread binding seam.

Pending presentation is layout-stable. `Button::pending` blocks duplicate
activation while a fine edge moves inside the existing bounds. Toggle and
Slider accept an optional backend-confirmed `effective_value`; when it differs
from the requested binding they keep the exact requested node visible, expose
both values in semantics, block duplicate input and animate the same bounded
edge. Reduced/Off keeps a static non-spatial pending marker. Hosts confirm or
return the requested binding rather than asking a component to guess success.

Text-rendering roots are constructed with `UiRoot::with_text` and one
`TextResources` owner. That owner keeps `TextSystem`, its bounded glyph atlases
and the shared `TextureStore` in one identifier namespace; after paint, the host
submits the display list with `UiRoot::texture_store()`. The public `Text` widget
accepts static or reactive strings and full `TextStyle`; Button and ListItem draw
their built-in labels when no custom child is supplied. TextInput draws value/
revealed-or-masked password glyphs and IME preedit through the same shaped
layout. `TextLayout::selection_rects` resolves one logical range into multiple
clipped visual fragments across wrapping, explicit lines and BiDi runs;
composition text, its internal selection and caret use the same display-
boundary map. Single drag, host-reported double-click word and triple-click line
selection coexist with grapheme, visual, Unicode-word, visual-line and document
keyboard movement.

Ctrl+A/C/X/V and non-sensitive Ctrl+Z/Y/Shift+Z use the public clipboard and
bounded in-memory history contracts. Password history is never populated,
semantics remain redacted during explicit reveal, and copying is denied unless
`PasswordCopyPolicy::Allow` is declared. Enter and Tab are form policies:
multiline inserts a line by default, single-line submits, and Tab navigates
unless insert/completion is explicit. A formatter receives and returns
`TextInputEdit`, including its selection, so caret mapping is deliberate.
Validation may trigger after a positive on-change debounce, blur or submit;
requests/results carry a generation, stale results are ignored, backend errors
retain the user's value and successful decoration expires after the validation
motion interval. A text-rendering component on a root without resources returns
`UiError::TextResourcesRequired` instead of silently dropping its label.

The accepted host-independent Appearance & Interaction view now lives in
`nkdhr-settings`. `AppearanceSettings::begin_apply` returns an opaque token and
selects that local pending control; different settings may remain pending
concurrently. `complete_apply` accepts only the latest generation for the same
setting and publishes either the downstream success or error message. A stale
same-setting service reply is ignored. The
professional inspector uses the selected Panel/Drawer motion family, retargets
from its currently visible geometry, retains its narrow-screen input barrier
through exit and settles immediately when spatial motion is disabled. The
global speed percentage scales every control/panel/fluid duration while
preserving curve shape. Standalone and in-compositor window hosts remain later
milestones; this view is not itself a Wayland application host.
Scroll now overlays inset proportional bars without changing layout. The fine
visible thumb has a wider pointer target, preserves the exact grab point during
captured drag, and its track pages toward the pointer. Wheel input remains in
the pointed region without stealing keyboard focus; Shift-wheel is horizontal,
and a focus reached by Tab supports arrows, standard Vim `HJKL`, Page, Space,
Home and End. Hosts use `ScrollGesture` phases for precision/touch scrolling:
velocity is sampled from the injected clock, inertia is interruptible, and
opt-in snap points settle on that same clock. Nested regions consume only what
they can move and bubble the exact remainder; lifecycle phases still reach an
outer region that consumed earlier remainder, while captured thumb drag never
transfers. Only the outer reached boundary owns bounded visual elasticity.
`ScrollAnchor` and `ScrollReveal` use caller revisions so insertion/removal and
focus reveal apply once without fighting later user movement; conditional tail
following occurs only near the previous tail. Persistent/high-contrast bars,
disabled axes and Reduced motion are explicit policies. Virtualized data still
uses the same scroll transaction model; `ListVirtualWindow` now supplies stable
leading/trailing extent for its materialized rows.

List retains the compatible `Reactive<Option<u64>>` single-selection form and
adds `ListMultiSelection` for stable selected identities plus a separate cursor
and range anchor. Shift extends ranges, Ctrl toggles discontiguous items or
moves only the cursor, and Space toggles the truly focused row. A bubbling List
moves real child focus with arrows/Home/End/Page and Unicode text/IME typeahead;
page size and typeahead timeout are validated descriptor values. `ListEntry`
supplies visible labels, enabled/loading state and tree depth/disclosure data.
Left/Right and a disclosure target emit stable `ListTreeToggle` transactions.
Opting into `on_reorder` paints an explicit trailing handle: captured pointer
drag keeps the source placeholder, moves the row visually, opens an insertion
seam and emits `ListReorder { identity, before }`; Ctrl+Shift+Up/Down emits the
same identity transaction. `ListVirtualWindow` reserves finite before/after
extent while only visible entries and children are retained, so selection may
remain on filtered/offscreen identities and loading rows keep final height.
Navigation rows activate on one click; object rows select on one and activate
from host-reported double click or Enter. Secondary click, the ContextMenu key
and Shift+F10 share the context callback. Child controls that handle their own
event never trigger the row. A capable `GlassSurface` now records the real
backdrop pass before its translucent material and children; an incapable or
reduced-transparency host paints the specified compensation. See the UI-3
checkpoint in `plan/ROADMAP.md` before treating any of these APIs as milestone-
complete.

## Themes (`nkdhr-ui`, UI-4)

The first UI-4 foundation is implemented. `nkdhr-theme` owns the portable,
non-executable schema-v1 document. A profile selects Tokyo Night, Nord or a
wallpaper base and stores explicit overrides separately. Wallpaper bases always
carry their complete resolved fallback palette, so a frozen/exported profile
does not need the image; live regeneration replaces the base palette while
leaving the override tree intact. JSON import/export is bounded to 1 MiB and
rejects unknown fields, invalid token types and invalid cross-token scales as
one unit.

The active document is the CTRL-5 scalar `theme.profile`; `theme.library` is a
separate schema-v1 collection of up to 256 validated saved profiles and is
bounded to 4 MiB. Use `nkdhrctl config get theme.profile`,
`nkdhrctl config get theme.library` and `nkdhrctl config watch theme` to inspect
commits. One scalar per operation is deliberate: the complete active profile or
library candidate validates, persists and signals atomically.

Appearance Settings now owns the host-independent editor transaction. A valid
complete profile previews through its shared `ThemeRuntime` immediately;
cancel restores the committed baseline. Save emits a generation-ordered
`theme.profile` request for the host to write asynchronously. Failure leaves
the preview intact for retry, confirmation advances the baseline, and an
external change is adopted when clean or reported as a conflict while local
work remains visible. Library save, explicit-ID copy, profile/library import
and profile/library export use the same validation and opaque async request
boundary. This is model/runtime behavior only; concrete file pickers and host
D-Bus execution remain with UI-5/SHELL-6.

Wallpaper image decoding also remains a host job. The host passes a borrowed
RGBA8 view (dimensions, row stride and bytes) to `WallpaperPaletteGenerator`
off the UI thread. At most 262,144 evenly distributed pixels enter a fixed
5-bit/channel histogram; fully transparent input and malformed views are
rejected. The generator returns a complete semantic `PaletteData`, never image
bytes, and provides Auto/Dark/Light appearance plus bounded colorfulness and
contrast inputs. Generated primary text is checked at 7:1 against its surface,
secondary text at 4.5:1, muted text at 3:1 and on-accent content at 4.5:1.

For a live profile, Appearance Settings issues a separate generation token
before the host decodes/extracts. Only the newest token may publish. A clean
result previews immediately and returns one atomic `theme.profile` persistence
request so the frozen portable fallback follows the wallpaper. If unrelated
local edits already exist, regeneration updates their wallpaper base and keeps
the whole preview visibly unsaved instead of committing those edits
implicitly. Frozen profiles reject automatic regeneration. Identity, name and
all explicit overrides survive a valid live update.

A valid theme update is published as one immutable generation. An invalid
update is rejected; running applications keep the last-known-good theme.
`ThemeRuntime::watch_ctrl5()` follows the leaf, and a `UiRoot` attached
with `set_theme_runtime` synchronizes immutable generations at its next explicit
local boundary. Different displays may therefore observe different generations
while only one is active, then safely jump directly across skipped generations.
Theme changes invalidate paint and only the layout values actually read by that
root/component. The existing default resolves exactly to the accepted UI-3
`Theme`; this mechanism does not itself change component design.

Trusted hosts may register third-party theme leaves before creating a runtime.
Groups use reverse-DNS names beneath `extension.*`; each leaf has a declared
type, default, validation range and paint/layout impact. Profile files contain
only sparse values. Missing values use registered defaults, while unknown or
invalid extension data rejects the whole candidate and leaves the last-good
generation visible. The registry is available to Settings/library transactions
as well as widgets. This does not yet load plugins or make extension values
persistable through the static daemon; that requires the future loader to give
daemon, Settings and shell the same trusted declarations.

## Integrated and standalone hosts (`nkdhr-ui`, UI-5)

`AppearanceSurface` is the real shared Settings application boundary. It owns
the accepted host-independent model, assets, theme generation and one
`UiHost`; neither presentation mode reconstructs the Settings behavior.

An integrated root uses `UiPinnedNode` and the COMP-7 pinned-node contract. It
submits the surface's display list directly into the compositor's current GLES
frame under a compositor-owned world transform. Pointer input arrives in
node-local logical coordinates, pointer capture continues outside the node,
and a focused toolkit control receives keyboard/text events until an outside
press clears that local focus. Developers can exercise the real path with
`NKDHR_CANVAS_DEMO_UI=1 nkdhr-canvas --nested`; it is off by default.

The `nkdhr-settings` binary is the standalone host. It requires Wayland, owns
its winit/EGL window and directly submits the same display list. Configure and
fractional scale update one host boundary; pointer, multi-click, keyboard,
repeat, text, focus and IME preedit/commit become the same `UiEvent` values.
Target-addressed plain-text clipboard requests use `wl-copy`/`wl-paste` at the
platform edge. Host-specific code stops at surface lifecycle, scale, frame
scheduling and clipboard/IME integration; widget behavior is shared.

The compositor fixture currently declares `backdrop_blur: false`, because its
Smithay scene adapter does not yet expand a UI filter's dependency into lower
scene-element damage. The normal compensated glass is therefore intentional.
The standalone full-frame adapter safely advertises and draws backdrop blur.

## Bindings and typed actions (`nkdhr-ui`, UI-6)

Every configurable compositor interaction is a registered typed action.
`ActionCatalog` exposes stable IDs, descriptions, instant/continuous kind,
closed scalar argument schemas and required host capabilities. Configuration
contains only an action ID plus data arguments; it cannot contain shell, Rust,
JavaScript or an expression to evaluate.

CTRL-5 stores the complete schema-v1 binding document in the bounded scalar
`canvas.bindings`. The empty value is the migration/default sentinel: the
compositor constructs the same full structured document while honoring the
three former key leaves. A non-empty document has this shape:

```json
{
  "version": 1,
  "bindings": [
    {
      "id": "window-close",
      "context": "window",
      "trigger": {
        "type": "key",
        "key": "Escape",
        "modifiers": ["logo"],
        "phase": "press"
      },
      "invocation": {
        "action": "canvas.window.close",
        "arguments": {}
      }
    }
  ]
}
```

Key, button and gesture triggers are normalized before publication. Modifier
array order is irrelevant. Gesture identity includes device class, finger
count, origin, optional direction, activation and context. Unknown actions,
wrong/missing arguments, malformed triggers, duplicate IDs and overlaps in
compatible contexts are errors. The complete candidate is rejected and the
previous immutable generation remains active. Missing device/capability is a
warning instead: the binding remains discoverable as unsupported but cannot
match input. This is how nested mode reports TTY-only VT/touchpad defaults
instead of pretending they work.

The default effective map includes the Phase 2 operations plus the already
approved standard-Vim keyboard variants: Super+Escape closes, Alt+Tab cycles,
Super+O toggles overview, Super+arrows/HJKL pans, Super+Shift+arrows/HJKL moves
the focused window, Super+Ctrl+arrows/HJKL resizes its right/bottom edges,
Super+digit jumps to a mark and Super+Shift+digit sets it. Super+primary drag
moves a window, Super+secondary drag resizes, primary drag on a frame moves,
and primary drag from empty canvas pans. TTY defaults own three-finger swipe
pan and three-finger pinch pan/zoom. Two-finger scroll is always client-owned;
touchscreen configuration is currently reported unsupported and complete touch
sequences pass to the focused client. The four-finger namespace remains
reserved for workspace/overview vocabulary; no directional default is guessed
before that workspace action is owner-approved.

Continuous actions receive `Begin`, zero or more `Update` phases, and exactly
one `End` or `Cancel`. Focus/output changes, target destruction, device removal,
session lock and a newly published binding generation all cancel centrally.
The rest of a consumed pointer/gesture sequence is suppressed after such a
cancel, so a client never receives an update/release without its begin/press.

`BindingSnapshot` carries the exact catalog Arc, generation, compiled rows and
diagnostics. `BindingSettingsModel` consumes that snapshot directly and turns
it into style-neutral discoverability rows; rejected candidate diagnostics do
not replace its effective rows. `ActionFeedback` is the shared
invoked/began/updated/ended/cancelled feedback value for later shell surfaces.

## Multi-segment motion curves (`nkdhr-theme`, `nkdhr-ui`, UI-7A)

UI-7A implements the portable curve value and runtime compiler; the approved
professional Settings editor is intentionally not constructed yet.
`MotionCurveData` stores 2–64 time-ordered anchors with fixed `(0,0)` and
`(1,1)` endpoints. Tangents use one of four explicit representations:
automatic, continuous with one direction and independent side lengths, broken
with independent vectors, or corner. Duration is not part of the curve, so one
normalized shape remains reusable at different speeds.

`CompiledMotionCurve::compile` validates the complete data, resolves the
versioned shape-preserving automatic tangent algorithm, rejects a control
polygon that turns backward in time, analytically finds progress extrema and
rejects undeclared overshoot or reverse motion. Sampling uses absolute
normalized time, a segment binary search and a fixed-iteration monotonic time
inversion. It allocates nothing, takes no lock and returns exact endpoint
values. `analysis`, `velocity` and a stable content fingerprint are available
to the later editor/runtime layers.

`split_motion_curve` performs an exact De Casteljau split. Adding an anchor
therefore cannot change the animation until the new point or handles are
actually moved. The existing `CubicBezier` API remains active and exposes
`to_motion_curve_data`/`compile_motion_curve` for lossless migration. A legacy
CSS cubic whose time-control polygon does not meet the stricter direct-editing
order is exactly divided into two legal segments in memory. Existing theme
JSON is not rewritten in UI-7A; versioned preset persistence and inherited
runtime snapshots belong to UI-7B.

## Inherited motion styles (`nkdhr-theme`, `nkdhr-ui`, UI-7B)

UI-7B adds `MotionStyleProfileData` as the optional `motion.style` member of a
theme's resolved motion data. Its absence is meaningful: the existing four
cubics and family durations are migrated exactly in memory, so old profile JSON
is not rewritten and every existing widget continues to use its accepted
`MotionProfile` path. An authored style pins either a built-in `(style,
revision)` or embeds a complete immutable preset snapshot, then stores a sparse
override tree.

The tree resolves in one fixed order: profile, semantic family, stable
component identifier, transition identifier. Each level may independently
provide a complete `MotionCurveData` and a duration. A curve is one atomic
field—anchor or tangent JSON is never inherited piecemeal—while a component
curve may, for example, coexist with a family duration. `resolve_scope` reports
separate exact provenance for both fields. Removing one field is therefore the
reset operation and reveals the precise parent value again.

`ThemeSnapshot::motion_style` exposes the immutable `CompiledMotionStyle`
compiled before the theme generation is published. Every curve in both the
base and override trees is compiled, even if another value currently shadows
it. Invalid candidates preserve the exact last-known-good theme snapshot.
`resolve` and `resolve_family` return an Arc-backed compiled curve, duration and
field provenance without reparsing configuration or recompiling curves.

Balanced revision 1 is the exact snapshot of the accepted legacy defaults.
Lively, Calm and Direct have stable reserved identities but deliberately have
no numeric revision yet; selecting an unavailable revision fails rather than
silently falling back. Their actual curves and timings require owner-guided
calibration. `snapshot_as_preset` freezes the effective same-scope overrides
into a portable user revision independent of its former built-in base.

`MotionPresetLibraryData` stores at most 256 immutable `(id, revision)`
snapshots. Reimporting identical data is a no-op; different content at an
existing identity is a conflict rather than an overwrite. Settings'
`MotionPresetLibraryEditor` fully parses and compiles an import before returning
one opaque `theme.motion_library` CTRL-5 write. Its durable local library only
changes after the host confirms that complete write. Appearance Settings can
now freeze the live professional-editor document as the next immutable
revision, import/export either one preset or the complete library, and queue
the scalar write on the same non-blocking host worker as `theme.profile`.
Valid libraries restore at host startup; either key can fall back independently
if its stored value is unavailable or invalid. These are nonvisual APIs: the
owner-reviewed preset controls and file-picker composition remain a later
UI-7E slice.

## Policy motion runtime (`nkdhr-ui`, UI-7C)

Use `ThemeSnapshot::motion_runtime`, not the authoring-only `motion_style`, for
component execution. Resolve a stable `MotionScopeData` together with a
`MotionPropertyDomain`; the returned opaque `MotionExecutionSpec` is already
governed by Expressive/Standard/Reduced/Off policy. Reduced makes spatial
motion immediate while retaining brief non-spatial feedback, and Off is fully
immediate. Direct pointer/keyboard manipulation remains functional in either
mode. Callers must consume begin/terminal outcomes from `KineticMotion` or
`SelectionMassMotion`; both keep only the latest target and never queue clips.

Fluid overrides may be authored independently at any style scope. Use
`resolve_fluid`, then sample the returned `ResolvedSemanticFluid`; its policy
mode cannot be supplied or overridden by the caller. Event variation requires
a stable event seed. Idle water requires a stable component seed and absolute
time. An authored zero oscillation is stationary; a non-zero oscillation stays
alive in Standard/Expressive and is forced to exact rest in Reduced/Off.

This stage supplies framework and execution behavior only. Existing widgets
still render their accepted appearance until each component's visual adoption
and numeric tuning are reviewed with the owner; no Settings editor or fluid
component composition is introduced here.

## Style-neutral motion editor (`nkdhr-ui`, UI-7D)

`MotionCurveEditor` owns the effective curve, inherited parent, independent
duration, field sources, selection, viewport, playhead and bounded undo/redo.
Construct it with the exact inherited values and a `MotionCurveConsumerSet`.
The set intersects every registered property's capability: overshoot or
reverse may be enabled only when every consumer is spatial or shape based;
opacity, color and bounded scalar consumers reject those permissions. An empty
set is deliberately conservative.

Editing an inherited curve or duration creates an explicit override;
`reset_curve` or `reset_duration` removes only that override and reveals the
exact current parent again. Double activation uses the same shape-preserving
De Casteljau split as UI-7A. Anchors, automatic/continuous/broken/corner
tangents and direct handle coordinates can be edited numerically. Multi-
selection drag uses one delta, clamps against unselected neighbors and
optionally snaps time/progress. Every candidate is fully compiled before
publication, so a rejected number, permission or clipboard payload leaves the
last-good document and history unchanged.

Use `begin_transaction`/`commit_transaction` around a drag; all intermediate
updates become one undo step. Cancellation restores the curve, selection,
playhead, viewport and playback state captured at begin. Copy/paste uses a
versioned, 64-KiB-bounded JSON keyframe payload. Explicit handles are preserved
when valid; detached handles that cannot fit their new neighbors are resolved
and constrained before the complete candidate is compiled. Duplicate times or
unsafe progress still fail atomically.

The graph stores normalized time internally. `MotionEditorAxis` converts it to
an independently editable real-time duration without changing curve geometry.
The viewport supports bounded pan/zoom, while playback advances only from the
host's absolute clock. `take_preview` coalesces any number of edits or playhead
changes into the newest immutable preview for the next host frame.

`MotionEditorInputController` is a style-free adapter contract, not a widget.
It accepts targeted mouse, pen and one-contact touch direct edits; targeted
two-contact touch or precision-touchpad viewport gestures; and keyboard
selection/editing. It never installs a compositor-global gesture. Arrow keys
and standard Vim `H/J/K/L` mean left/down/up/right, Shift uses the coarse step,
and the controller emits explicit text clipboard read/write requests. The first
UI-7E production binding now connects mouse direct editing, graph-local mouse/
precision-touchpad viewport gestures, keyboard/history/clipboard commands and
host-clock preview playback to the owner-reviewed Settings composition. Its
duration input accepts an integer from 1 through 60000 with an optional `ms`
suffix; invalid submissions preserve the last-good document. Visible Fluid
percentages map into sparse semantic overrides and survive reconciliation. The
Save action writes the current transition into `theme.profile` through the
shared host's non-blocking CTRL-5 worker. Pending, success and failure remain
visible in both the inspector and status bar. Preset snapshot/import/export and
startup recovery now have complete model and host APIs, while their visible
controls remain owner-guided work.

## Verification tools

`nkdhr-render` ships a deterministic primitive gallery. Its software reference
renderer produces committed PPM golden images for every primitive and their
important combinations. Tests compare exact bytes and print the update command
only when a deliberate visual change is intended.

The GLES gallery renders the same display lists offscreen and can dump a frame
for review. The benchmark scene contains approximately 1,000 mixed primitives;
the acceptance target is under 2 ms GPU rendering time on the reference Intel
Iris Xe. Software-renderer measurements are reported separately and never used
to claim that hardware target.

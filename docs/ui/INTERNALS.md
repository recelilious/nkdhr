# nkdhr UI Stack — Internals

> [中文版本 / Chinese version](INTERNALS_zh_CN.md)

## Scope

This document covers Phase 3 (`UI-1` through `UI-6`): the shared primitive
renderer, text system, retained widget toolkit, theme runtime, dual host
integration and typed interaction language. Shell product components remain
Phase 4 work. The Phase 3 demos and galleries exercise public APIs but are not
temporary shell implementations.

## Dependency direction

```text
nkdhr-canvas ─┬─> nkdhr-ui ──> nkdhr-render ──> Smithay GLES
              └─> COMP-7 pinned-node host

nkdhr-settings/files/tasks ──> nkdhr-ui ──> nkdhr-render

nkdhr-ui ──> nkdhr-ipc (themes, actions and configuration types)
```

`nkdhr-render` has no dependency on `nkdhr-ui`, the compositor world model,
Wayland widgets or CTRL-5 schemas. `nkdhr-ui` has no dependency on a concrete
canvas backend. Host adapters depend inward on the toolkit.

## Crate layout

```text
crates/nkdhr-render/src/
  lib.rs             public primitive API
  geometry.rs        Rect, radii, affine transforms and color
  display_list.rs    validated commands and state-stack recorder
  texture.rs         stable CPU-side texture assets and revisions
  gles.rs            Smithay GlesFrame adapter, programs, VBOs and cache
  software.rs        deterministic test oracle
  shaders/           checked-in GLSL sources

crates/nkdhr-ui/src/
  lib.rs             public prelude and root lifecycle
  text/              shaping, font fallback and glyph atlases
  tree/              widget identity, retained nodes and invalidation
  layout/            constraints, flex layout and arranged geometry
  input/             hit testing, focus, capture, IME and accessibility data
  animation/         clocks, easing and invalidation scheduling
  widgets/           standard public widget set
  theme/             typed token schema and live snapshots
  host/canvas.rs     COMP-7 scene-node adapter
  host/wayland.rs    standalone Wayland client adapter
  action/            registry, typed arguments, bindings and diagnostics
```

Modules are introduced with their milestone. Empty future module scaffolding
is not created early.

## UI-1: primitive layer

### Display lists

`DisplayList` is an immutable sequence in painter's order. Commands contain
only owned renderer-independent data and stable `TextureId` handles. A
`DisplayListBuilder` maintains transform and clip stacks, validates all input,
normalizes corner radii to fit their rectangles and flattens state into each
command. Finishing a list cannot fail because failures are reported at the
recording operation that introduced them.

Logical coordinates remain `f32` until submission. The backend multiplies them
by the target scale exactly once. Affine transforms use six finite components
and compose parent-to-child. Axis-aligned clipping is represented as one
intersected target-space rectangle per command; empty intersections drop the
command. Rotated or sheared clips return `BuildError::NonAxisAlignedClip`.

The command set is:

```rust
enum Primitive {
    Shape(ShapePrimitive),   // fill, rounded fill, border or shadow
    Texture(TexturePrimitive),
    BackdropBlur(BackdropBlurPrimitive),
}
```

Plain rectangles are rounded rectangles with zero radii. Borders share the
rounded-rectangle signed-distance function so inner and outer edges cannot
disagree. A shadow expands its draw bounds by spread plus three blur sigmas;
its shader evaluates coverage from the same signed distance as the source
shape. Texture source rectangles are normalized against the CPU asset during
the prepare pass. A backdrop blur is a painter-order barrier: its rounded rect,
transform and target-space clip mask a screen-space filter of pixels produced
by commands and compositor layers before it. It never filters its own material
fill or later child content.

### GLES backend and batching

`GlesBackend` is created for one Smithay `GlesRenderer` context. It owns four
GLSL programs, dynamic VBOs, lazily target-sized blur textures/FBO and a per-
context texture cache. `prepare` uploads
new texture revisions before a `GlesFrame` borrows the renderer. `draw` then
runs inside that frame using `GlesFrame::with_context`; the framebuffer and
Smithay projection are already active.

Shape vertices carry transformed target position, local shape position, size,
four corner radii, fill/border colors and signed-distance parameters. Six
vertices per shape quad are streamed into one dynamic VBO. Consecutive shapes
with the same clip form one draw call. Texture vertices carry target position,
UV, opacity, tint and alpha mode; the shader selects RGBA modulation or
single-channel alpha-mask tinting from the uploaded texture format. Four
vertices per texture quad use one shared index buffer. Consecutive commands
with the same texture, sampling mode and clip form one draw call. Painter's
order is never reordered across a pipeline, clip or texture boundary.

A backdrop batch copies only its radius-expanded dependency rectangle from the
active Smithay framebuffer, runs a nine-sample horizontal pass into a reusable
RGBA target, then runs the matching vertical samples while replacing pixels
through the transformed rounded mask. Sampling and replacement operate on
premultiplied pixels; the original snapshot is mixed at antialiased edges, so
transparent targets do not gain alpha through source-over feedback. Consecutive
ordinary batches cannot cross this barrier.

Incremental composition has an explicit host contract. Before repainting
layers below a prepared list, the host calls `PreparedDisplayList::expand_damage`
and uses the returned physical rectangles both for those lower layers and
`GlesBackend::draw`. Damage touching a blur dependency expands to its complete
source/output dependency, with fixed-point propagation across overlapping blur
nodes. This ensures the pass samples a freshly repainted backdrop rather than
glass pixels retained from the previous frame.

The backend restores every GL state it changes to Smithay's documented frame
baseline: framebuffer/viewport after each offscreen pass, array-buffer binding,
program, both touched texture units and bindings, enabled attributes, scissor,
blend enable and premultiplied blend function.
Resource destruction is explicit and context-bound; hosts call `destroy`
before dropping a render context. Texture removals are reclaimed during the
next `prepare` pass.

The shaders target GLES 2.0 syntax and do not require uniform buffers,
storage buffers or desktop GL. Iris Xe uses the same path as the supported
fallback GLES implementation; batching does not depend on instancing.

### Golden-image oracle

`software.rs` is a deterministic test backend, not a product rendering path.
It consumes the public flattened display list and implements the same
premultiplied blending, rounded signed distance, border, shadow, clipping,
transform, texture sampling and separable backdrop-filter rules. Each software
blur reads an immutable painter-order snapshot before replacing its masked
pixels, matching the GLES pass without becoming a product rendering path.
Gallery fixtures are rendered at a
fixed scale and encoded as binary PPM after compositing over an explicit
background. Exact committed bytes are the golden contract.

Focused unit tests separately cover geometry validation, transform
composition, clip intersection, radius normalization, batching boundaries,
texture revision handling and premultiplied blending. An offscreen GLES
gallery is compared visually and with a small per-channel tolerance where
hardware edge coverage differs from the scalar reference.

### Performance measurement

The benchmark builds one stable full-screen display list containing roughly
1,000 representative primitives, performs warm-up frames, then records CPU
submission and GPU completion time separately. GPU completion is fenced; a
mere command-enqueue duration is not reported as render time. The renderer,
driver, resolution, scale, primitive count, draw-call count and percentiles are
stored with the result. UI-1 acceptance requires the p95 GPU duration below
2 ms on the reference Iris Xe.

The accepted 2026-08-08 reference run used Mesa Iris Xe at native 2560×1600.
The 1,000-primitive scene compiled into one batch. Stable-list compilation and
VBO upload took 1.937 ms; warm GPU timing measured a 1.189 ms median and
1.228 ms p95. Full clear, UI draw and fence wall time had a 3.032 ms p95 and
is recorded separately rather than being presented as UI draw cost.

The 2026-08-09 backdrop checkpoint repeated the baseline at 1.220 ms GPU p95,
then added one representative 1160×760 ContentSurface filter at 36 logical px.
That 1,002-primitive/three-batch frame measured 3.101 ms GPU median, 3.212 ms
p95 and 5.426 ms full clear + UI + fence wall p95 on the same Iris Xe at
2560×1600. A dedicated high-frequency offscreen scene matches the scalar
oracle at maximum channel difference <=4 and p95 <=1; a transformed, clipped,
rounded glass case is also committed as an exact software PPM golden.

## UI-2: text

`cosmic-text` owns shaping, bidi resolution and font fallback. The toolkit
turns shaped glyphs into atlas keys containing font identity, glyph ID,
subpixel bin, scale and raster mode. Monochrome masks and color glyphs use
separate texture formats. Atlas pages use a skyline allocator, bounded memory
and least-recently-used page eviction; entries referenced by the current frame
are pinned until submission completes.

Text layout caching is independent from glyph raster caching. Changing color
repaints without reshaping. Changing width reshapes only wrapping-dependent
runs. Atlas eviction invalidates texture residency, never shaped layout.

`TextSystem` loads the system font database once, runs advanced shaping, and
stores immutable `TextLayout` values in a bounded paragraph LRU keyed by text,
family priority, weight, slant, metrics, wrapping, alignment, width, scale and
locale. Each layout also stores ordered per-line glyph ranges. A vertical clip
locates the nearby line span with binary searches before glyph iteration, so
scrolling cost depends on the viewport rather than total document length.

New glyphs are rasterized through Swash. COLR/CPAL glyphs are attempted first
through `oxitext-raster`, because Swash 0.2 renders the installed Noto
COLRv1 emoji as a monochrome outline instead of returning an RGBA image. The
result is packed into separate `Alpha8` mask and straight-alpha RGBA page sets;
mask pages are tinted at draw time and color pages remain white-modulated.
Dirty CPU pages update stable `TextureStore` IDs, while the renderer's normal
revision cache owns per-GLES-context upload. Page eviction removes every key on
the victim page and increments `atlas_generation`; retained paint data must
compare that generation before reuse. If every bounded page of the required
format is pinned by the current frame, drawing fails explicitly with
`AtlasFull` instead of exceeding its resource budget.

Deterministic mixed-script tests load checked-in SIL OFL subsets of Noto Sans,
Noto Sans CJK SC and Noto Color Emoji. They cover Latin/CJK/emoji fallback,
bidirectional shaping, width-sensitive wrapping, exact mixed-script golden
output, color-page selection, bounded-page eviction and artifact-free rebuild,
and line-range clipping. The renderer's real offscreen GLES/software
cross-check includes an `Alpha8` mask drawn through the production shader.

The accepted 2026-08-08 release benchmark shapes 5,000 mixed Latin/CJK/emoji
lines (265,000 glyphs) once, then records 300 clipped scrolling frames after
40 warm-ups. On the development machine, initial layout took 175.999 ms and a
cached lookup 1.539 ms. Each frame considered 2,055 nearby glyphs and recorded
1,744 primitives; CPU record time was 0.253 ms median, 0.262 ms p95 and
0.286 ms maximum. The stable atlas held 56 glyphs on one mask and one color
page with zero eviction during measurement.

## UI-3: toolkit core

Each widget has a stable generational `WidgetId`. The retained tree stores
widget state, children, layout results and dirty flags. Reconciliation reuses a
node only when its keyed or positional sibling identity and widget type match.
Removed nodes release pointer capture, focus and animation registrations before
destruction.

Invalidation has three levels: layout, paint and semantics. Layout runs
top-down constraints and bottom-up measurement, then top-down arrangement.
Paint walks front-to-back into a display list. Input hit testing walks the same
arranged stacking order in reverse and respects the same clips.

Reactive values enqueue one root-local update; they do not call application
callbacks while the tree is borrowed. Updates, input and animation ticks are
processed at explicit frame boundaries. Animation values use a host-provided
monotonic clock, making tests deterministic.

An animation registration now invokes `Widget::animation` with an
`AnimationCtx` before the next layout pass. The default callback remains
paint-only, preserving existing visual timelines, while geometry-changing
motion can update retained state, invalidate layout and explicitly re-register.
This keeps Scroll inertia and snapping on the host clock rather than deriving
layout from paint or installing an independent timer.

Keyboard focus is a tree path with focus scopes. Tab traversal follows
semantic order, not incidental allocation order. Text input routes through an
IME transaction and maintains selection/composition ranges in UTF-8-safe text
indices.

The implemented framework represents each declarative `Element` as a widget
descriptor, optional sibling `WidgetKey`, explicit flex factor and child list.
An arena assigns `(slot, generation)` identities. Same-key/same-type nodes
receive the new descriptor through `Widget::update` while retaining their
type-erased state; keyed sibling lookup is linear through a temporary map, and
unkeyed children reconcile positionally. Removing a subtree clears focus,
capture, hover, animation and reactive liveness before state is dropped. Slot
reuse increments the generation, so stale IDs and queued subscriptions cannot
address a replacement node.

`Constraints` require finite ordered minimum and maximum sizes. Every measured
result and arranged `Rect`, including derived flex/padding geometry, is checked
for finiteness and non-negative dimensions. The implemented structural widgets
are `Flex`, `Padding`, `Align`, `Stack` and `Clip`; they contain no theme or
visual defaults. Layout invalidation implies paint, and paint is rejected until
layout has completed. Fully clipped subtrees and children intentionally omitted
by their parent's paint pass have paint dirtiness consumed without recording
commands, preventing an invisible node from requesting frames forever.

Painting traverses the retained children in their declared stacking order and
records that exact traversal for reverse hit testing. Effective ancestor clips
are shared by paint and hit testing. Pointer events target the last painted hit
node unless capture is active, then bubble to the root. Hover still follows the
actual hit path during capture and produces enter/leave state transitions.
Focus changes produce explicit events; Tab traversal uses tree order within the
nearest focus scope. A host `PointerCancel` transaction is delivered to the
captured target and then forcibly releases capture, preventing a removed device
or surface-focus loss from stranding an interaction. IME preedit selections are
validated as ordered UTF-8 byte boundaries before dispatch. The same tree emits
flattened stable-ID semantic nodes for a later accessibility host adapter.
Pointer press/release transactions retain host-normalized modifiers and a
nonzero click sequence count. A child may mark one transaction handled while
explicitly continuing to a composite ancestor, and that ancestor may request
focus for one direct child after dispatch; ordinary handled events still stop
immediately. List uses these two narrow seams for range selection and for
keeping the actual focus target synchronized with its stable cursor.
Clipboard output is accumulated in `DispatchResult`. Reads retain their
originating generational `WidgetId`, and `ClipboardText` dispatch bypasses the
current focus only when that identity is still alive; removed or stale targets
receive nothing.
Containers may register a clipped pointer overlay after painting children, so
a fine scrollbar can own a wider hit strip without intercepting the content
area or shifting layout. A child can also be painted through a visual-only
translation while retaining rigid arranged/hit geometry; Scroll uses that seam
only for bounded elastic feedback.

`Reactive<T>` stores root-local subscriptions keyed by root and generation-safe
widget identity. A mutation only appends invalidation to the root queue; the
queue drains at the next reconcile/layout/paint/input boundary, so callbacks
never re-enter a borrowed tree. Erased subscription tokens are owned by each
retained node, deduplicated across repeated passes, cleared when a descriptor
is replaced and unsubscribed immediately when the node is removed; long-lived
reactive values therefore do not accumulate dead widget registrations.
Animation uses a host `Clock`, a normalized `Timeline` with caller-supplied
duration, and explicit per-widget frame registration. Production uses
`SystemClock`; tests advance `ManualClock` without sleeping. No duration,
easing or motion character is selected here.

Framework acceptance tests cover keyed reorder and state retention (including
2,000 reversed siblings), generational slot reuse, state destruction, finite
flex geometry and resize, alignment, clip/paint/hit agreement, event bubbling,
focus scopes, capture and hover, queued reactivity, semantic snapshots,
UTF-8-safe IME validation and deterministic animation sampling.

The owner-reviewed product layer is now partially implemented in `theme.rs`,
`motion.rs` and `widgets/`. `Theme` is currently an immutable descriptor-level
snapshot (UI-4 still owns atomic live publication/diff invalidation). It carries
the accepted density, spacing, radii, type, palette and glass material values.
Material capability resolution keeps requested logical blur explicit and adds
the approved opacity compensation when a host does not declare the real
backdrop pass or reduced transparency disables it. A capable `GlassSurface`
records the backdrop barrier after its shadow but before its translucent fill,
state overlays, protected edges and children.

`MotionProfile` stores curves, per-family durations, global policy and fluid
tuning. `ScalarMotion::retarget` samples the current visible value before
changing target, preventing queued-clip discontinuities. Shell-only procedural
variation is a deterministic function of a stable event seed and bounded
tuning; ordinary controls do not call it. `GlassSurface`, Button, Toggle,
Slider, List/ListItem, Scroll, Text and TextInput record through existing primitives.
`MotionProfile::with_speed_multiplier` scales control, panel and wallpaper
durations plus fluid base/per-distance/maximum/bud timing together. It leaves
curves and geometric amplitudes untouched and rejects non-finite/non-positive
multipliers.

Button, Toggle and Slider pending edges are paint-only host-clocked motion and
never alter measure/arrange geometry. Toggle and Slider compare their requested
binding with an optional effective binding, keep the requested node exact and
make the mismatch noninteractive until reconciliation. The Settings
presentation model keeps one opaque latest-generation token per setting over
those local states. Different settings may wait concurrently, while only the
latest token for one setting can publish success/error, so transport reordering
cannot regress visible feedback.
List owns selection decoration and reads arranged child geometry through
`PaintCtx::child_rect`. `ListSelection` is either the compatible single
`Reactive<Option<u64>>` or a `Reactive<ListMultiSelection>` whose stable-ID set,
cursor and range anchor remain independent. Visible `ListEntry` order is used
only to resolve range/typeahead/navigation geometry; filtering, sort and a
`ListVirtualWindow` never rewrite hidden selection. The virtual window adds
validated leading/trailing extent around measured materialized rows, while a
loading ListItem keeps the density row height and exposes loading semantics.

Tree depth and expanded state are declarative entry metadata. Disclosure and
Left/Right input emit `ListTreeToggle` and wait for ordinary application
reconciliation; the component does not keep a second private tree model.
Reorder behaves the same way: overlay handles capture the List, retain the
source layout rectangle as a placeholder, translate the source child only for
feedback and calculate an insertion seam, then emit stable
`ListReorder { identity, before }`. Keyboard reorder emits the identical
transaction. Typeahead buffers committed Unicode text on the root clock and
uses configurable timeout/page-step values. Pointer click counts come from the
host's system policy, so object-row double-click has no toolkit-local timer.

Scroll owns logical offset separately from visual elasticity. Each scroll
handler computes `next = clamp(current + delta)` and bubbles exactly
`delta - (next - current)` through `EventCtx::handoff_scroll`; ordinary handled
events keep their existing stop-propagation meaning. Begin/end/cancel lifecycle
events cross a zero remainder so every nested participant terminates cleanly,
and only the outermost scroll that still has a nonzero remainder receives the
`scroll_boundary` callback. Disabled axes are removed before elasticity. Thumb
capture stops transfer entirely. The retained state also holds injected-clock
velocity/inertia, material stretch, visibility/idle delay, opt-in snap motions,
last-applied anchor/reveal revisions and prior content/viewport extents for
conditional tail following. Layout clamps non-finite external offsets and
applies anchor/reveal revisions once; paint translates only the clipped child,
never its hit geometry or exact thumb center. Reduced motion bypasses inertia,
snap interpolation, stretch and elastic return.

`TextResources` is the retained resource boundary. One instance owns the
`TextSystem`, bounded glyph atlases, output scale and the `TextureStore` whose
stable IDs appear in the display list. `UiRoot::paint` advances one atlas frame
before traversing widgets; every page touched during that traversal is pinned by
the same frame number, so later text in the frame cannot invalidate earlier
recorded commands. Measure contexts shape/cache layouts; paint contexts resolve
and record glyphs. The host retrieves the same store from the root for renderer
submission, and may insert non-text assets through `texture_store_mut` so IDs
cannot cross owners.

`TextLayout` retains paragraph source offsets beside cosmic-text clusters and
exposes local hit testing, caret/visual-neighbor geometry, per-line targeting
and selection fragments. Selection walks selected grapheme cells in each shaped
cluster, reverses their visual cells for RTL levels, merges adjacent cells per
visual line and can therefore return several rectangles for one logical BiDi
range. All APIs preserve global grapheme-safe UTF-8 boundaries across explicit
lines and wrapping.

TextInput stores logical anchor/caret separately from the immutable layout and
invalidates stale geometry after every committed edit. Pointer granularity is
fixed at press (character/word/line), while keyboard movement chooses visual
graphemes/lines or Unicode logical words/document bounds. Display-boundary maps
cover masked/revealed source and preedit text; IME's internal selection and
caret are mapped independently, and formatting runs only after commit through
an explicit value+selection result. Bounded undo/redo snapshots are never
created for password descriptors. Clipboard reads are asynchronous targeted
transactions rather than synchronous platform calls from event handlers.

Validation retained state owns a monotonic generation, optional root-clock
debounce deadline, pending/transient status and last applied result. Every edit
invalidates the previous generation; only an equal result may change status.
Blur and submit issue immediately, backend errors never rewrite the value, and
valid decoration clears through `Widget::animation` after the theme validation
duration. Password source content remains absent from semantics regardless of
visible reveal state.

The host-independent accepted Appearance & Interaction composition is now a
deterministic structural/visual oracle with full-CJK fixture text, real icon
masks, exact four-width behavior and one outer backdrop pass. Its professional
panel/drawer owns `ScalarMotion` in retained layout state: animation ticks
invalidate layout, measure and arrange sample the same host time, rapid reversal
retargets the visible openness, and Reduced/Off settles directly. A drawer
identity depends on the responsive mode rather than open state, preserving its
`InputBarrier` throughout exit. `PaintCtx::paint_child_clipped` clips only that
moving overlay; sibling surfaces and their approved shadows remain untouched.
The committed static golden remains byte-identical. The owner accepted the
resting and exported transition frames on 2026-08-10, so UI-3 is complete;
standalone/in-compositor hosts remain their existing UI-5 work rather than an
UI-3 condition.

## UI-4: theme runtime

Implemented foundation: backend-neutral `nkdhr-theme` resolves a bounded
schema-v1 `ThemeProfile` into complete `ThemeData`. A base is immutable Tokyo
Night/Nord or a wallpaper source with its full frozen palette. Sparse JSON
overrides merge only over known fields, remain separately enumerable, and are
reused unchanged when a live wallpaper base is regenerated. Resolution
normalizes colors and rejects the whole candidate on syntax, version, unknown
field, type, range, ordered-scale, material or motion error.

`nkdhrd` registers `theme.profile` and `theme.library` as separate scalar
CTRL-5 leaves. `Namespace::validate` resolves the complete active profile and
validates every library member plus unique identities and collection bounds;
this keeps each persistence/`Changed` operation atomic despite nested
overrides/font arrays and preserves the prior cached string on rejection.
`ThemeRuntime` resolves and converts a candidate
before locking, then swaps one immutable shared `ThemeSnapshot` and generation.
It may subscribe to CTRL-5 or accept application-controlled publication
directly. A rejected candidate never mutates the current Arc or generation.

Theme diffs classify tokens by paint/layout impact. A color-only switch does
not rebuild layout. `ThemeToken<T>` plus `ThemeReadSet` records semantic leaf
reads. An attached `UiRoot` compares its last snapshot with the newest one at
reactive/frame boundaries (so skipped publications are safe), replaces themed
descriptors' immutable Arc, and dirties only intersecting readers. Standard
components declare their real dependencies; spacing/typography/density/motion
are layout-impacting while palette/material/radius changes remain paint-only.
The default portable profile is structurally equal to the accepted UI-3
runtime default.

`ThemeProfileEditor` is a cloneable, host-independent Settings transaction
owner around one `ThemeRuntime`. Preview validates and publishes before
recording a draft; cancellation republishes the committed profile. Active and
library commits return an opaque generation token, dotted key and validated
JSON string so the host performs D-Bus work off the UI thread. Completion is
latest-token ordered. Failure keeps the draft/library baseline usable, while
external `Changed` input either confirms the matching request, adopts a clean
value or preserves a local profile preview with an explicit conflict. The
versioned library validates candidates before replacing itself and implements
save/upsert, explicit-identity copy, removal and bounded JSON profile/library
round trips. `AppearanceSettings` owns this editor, bridges its feedback into
the existing status/pending model, and exposes the runtime and host request
methods without adding new accepted visuals. At that checkpoint, wallpaper
extraction and extension token registration remained open UI-4 slices.

`WallpaperPaletteGenerator` closes the extraction/regeneration slice while
keeping codecs and filesystem access outside `nkdhr-theme`. `WallpaperImage`
validates non-zero dimensions, RGBA row stride, overflow and backing length.
Analysis visits at most 262,144 deterministic evenly spaced pixels and
accumulates their alpha-weighted 5-bit RGB histogram in fixed-size memory.
Candidate and average colors are converted through OKLab/OKLCH; a weighted
median selects Auto light/dark appearance, population/chroma selects wallpaper
accent seeds, and bounded gamut mapping materializes all semantic roles.
Appearance, colorfulness and contrast are typed inputs rather than hard-wired
UI controls. Contrast tests cover extreme inputs and the generator never keeps
source pixels or executes a decoder.

`regenerate_live_wallpaper_profile` accepts only a valid live-linked profile
and source identity, replaces its frozen base palette, resolves the full
candidate and leaves profile identity/name/override JSON equivalent. Settings
wraps asynchronous extraction with a latest-generation token. A clean result
enters the existing active-profile persistence transaction; a dirty result
updates the local preview but deliberately emits no implicit write. Superseded
extraction results and results for a profile the user has since left are
ignored. Invalid/failed extraction preserves the last runtime generation. The
same review fixed an adjacent transaction edge: a successful older save now
leaves newer previews marked unsaved, and a divergent external signal no
longer discards a still-in-flight request.

Wallpaper extraction/regeneration is therefore implemented. The final UI-4
slice adds `ThemeExtensionRegistry`: a host registers bounded reverse-DNS
groups whose token descriptors contain type/range/default and paint/layout
impact. Resolution splits `overrides.extension` from built-ins, fills omitted
registered defaults and rejects the full candidate on unknown or invalid
extension data. `ResolvedTheme` carries the complete normalized map;
`diff_resolved` joins built-in and extension leaf changes. `ThemeRuntime`
immutably shares one registry, `ThemeReadSet` owns dynamic paths, and
`Widget::apply_theme_snapshot` lets an extension consume values without
changing the core widgets' existing typed `Theme` hook. Settings and library
transactions have matching registry-aware entry points.

Multi-root coverage attaches two retained roots to one runtime and advances
them at different boundaries. One root remains on generation 1 while the other
uses generation 2, then jumps directly to generation 3 on its next local
activity. A layout value changed and reverted only in skipped generation 2 is
absent from the direct 1→3 diff, while a generation-3 paint value still
invalidates its exact reader. Invalid candidates preserve the last-good Arc.
This completes and accepts UI-4; trusted registry distribution/plugin loading
remains later extension work rather than part of the theme runtime.

## UI-5: two hosts, one runtime

`UiHost` owns a `UiRoot`, logical size/output scale, layout/paint scheduling,
the last complete immutable `DisplayList`, its texture namespace and a commit
counter. A successful record is the only operation that advances the commit;
unchanged frames reuse the prior list. `UiSurface` is the object-safe
application boundary over render/list/textures/commit/input/focus/frame demand.
`AppearanceSurface` implements it once for both modes, rebuilding the
declarative element only when viewport, composition revision or theme
generation changes; value-only reactives continue through the mounted root.

The canvas adapter implements `PinnedNode` through `UiPinnedNode`. Its
`NkdhrUi` payload borrows the complete display list, exact texture store and
commit. `DisplayList::transformed` creates an immutable placement copy by
composing the viewport translation/zoom and already-target-space clips; the UI
root continues to render node-local coordinates. A distinct context-bound
`GlesBackend` and cached `PreparedDisplayList` live per `(node ID, erased GLES
context ID)`, preventing one node/output prepare generation from staling
another. The local `GlesTargetRenderer` bridge draws through direct nested
`GlesFrame` or the TTY multi-GPU frame's GLES render side. Element identity and
commit signatures include application commit, target, placement, zoom and
scale for Smithay damage tracking. No intermediate CPU image exists.

Pointer events carry node-local position, modifiers and host-normalized click
count. UI padding is part of the surface hit target; capture remains routed
beyond bounds; focus/leave are explicit. Canvas-local keyboard focus forwards
normalized key and printable text events before global bindings and is cleared
by any outside press. Integrated clipboard and compositor-owned IME protocol
bridging remain later shell integration work; the shared surface contract is
already capable of receiving both.

The standalone adapter owns a native Wayland winit window, `wl_egl_window`, EGL
display/context/surface, Smithay `GlesRenderer` and `GlesBackend`. Resize and
fractional scale update the EGL target and retained text scale at one frame
boundary. Winit pointer/keyboard/focus/text-input-v3 events terminate there as
`UiEvent`; clipboard requests retain their originating `WidgetId` and are
fulfilled by the installed Wayland clipboard tools. It renders full damage,
so its advertised backdrop blur satisfies the lower-layer dependency contract.

The acceptance root is the existing Appearance Settings composition, not a
new gallery. A deterministic twin-surface test proves identical list output;
live standalone and nested compositor first frames exercise both real EGL
paths. All-feature compilation covers the TTY `MultiRenderer` implementation.

## UI-6: typed interaction language

`nkdhr-ui::action` is backend-neutral. `ActionCatalog` bounds and validates up
to 512 stable lowercase IDs. Each `ActionDescriptor` carries human-readable
description, instant/continuous kind, at most 32 closed scalar arguments and a
set of declarative required capabilities. Boolean, bounded integer/number/
string and closed choice schemas accept data only. `ActionRegistry<C, P>` adds
one Send+Sync host adapter for each catalog entry; the configured document
never selects a function pointer or executable string.

The schema-v1 `BindingDocument` is capped at 1 MiB/2,048 entries. A trigger is
one key, button or gesture plus a `BindingContext`. Compilation resolves the
action, fills typed defaults, validates supplied arguments, lowercases key
identity, converts modifier arrays into `ModifierSet`, rejects client-reserved
two-finger touchpad gestures and non-empty/edge touchscreen ownership, and
compares every candidate overlap. Gesture conflicts include kind, device-class
overlap, finger count, origin overlap, optional direction and context; button
conflicts include device/origin/context as well. Duplicate IDs, invalid actions
or arguments and conflicts are structured errors. Unavailable action
capabilities and absent device classes are structured warnings with an
explicit non-effective row.

`BindingRuntime` publishes only a completely compiled candidate. Its
`Arc<BindingSnapshot>` contains the same catalog Arc, monotonically increasing
generation, compiled rows and accepted warnings. Rejection returns candidate
diagnostics while preserving the exact old Arc/generation. The canvas CTRL-5
watcher holds this runtime beside grid policy. `canvas.bindings` is one bounded
string because CTRL-5 still supports scalar leaves; empty means synthesize the
canonical structured document from the legacy key leaves. A non-empty JSON
document is authoritative. Changes publish under the same mutex and the input
thread clones one immutable snapshot per lookup.

`ActionDispatcher<App, CanvasActionPayload>` is the only adapter entry point
for configurable compositor actions. `input.rs` performs host normalization
and binding lookup but contains no per-shortcut action match; `actions.rs`
owns every action implementation. Instant dispatch calls `Invoke`.
Continuous begin allocates an `InteractionId`; only that ID may update/end.
The dispatcher clears ownership before a terminal adapter call and update
failure cancels, guaranteeing at most one terminal phase. Canvas additionally
cancels on active output/focus change, target death, device removal, lock or
binding-generation change. It suppresses the consumed stream remainder after
asynchronous cancellation.

Phase 2 pointer `Drag` remains the operational move/resize/pan state, now
created/updated/finished only by typed action phases. Client-requested xdg
move/resize enters the same dispatcher after protocol grab validation. TTY
three-finger swipe is `canvas.viewport.pan`; three-finger pinch keeps the
gesture's initial world point beneath the moving logical center while clamping
zoom to the supported work-view interval. Other gestures forward through
pointer-gestures. A real Smithay touch handle now forwards down/motion/up/frame/
cancel unchanged; no touchscreen action is advertised effective until an
empty-canvas/edge recognizer exists.

Session-lock VT switching is the deliberate exception to configured lookup:
the fail-closed lock path retains Linux's fixed Ctrl+Alt+Fn/XF86 emergency
chords. Normal-session VT switching is a typed, capability-gated action and is
reported unsupported in nested mode.

`BindingSettingsModel` accepts a `BindingSnapshot` rather than recompiling or
copying metadata. It formats style-neutral trigger rows from that snapshot's
catalog and retains effective rows when a rejected publication supplies new
diagnostics. `ActionFeedback` supplies a common result seam without designing
a Phase 4 notification component.

No binding value is executable code. Custom command execution, if later
implemented through CTRL-EXT, is a separately authorized typed action rather
than an escape hatch in the binding grammar.

## UI-7A: segmented motion-curve foundation

Portable authored data lives in `nkdhr-theme::motion_curve`; executable/runtime
precomputation lives in `nkdhr-ui::motion_curve`. This preserves the dependency
direction and keeps profile data non-executable. `MotionCurveData` is an atomic
curve field with a schema version, automatic-tangent algorithm version,
overshoot/reverse permissions and 2–64 anchors. Structural validation fixes
the endpoints, enforces a `1e-6` minimum normalized time gap, rejects non-finite
values and bounds hostile progress/handle data before compilation.

Automatic anchors resolve with version-one shape-preserving PCHIP derivatives.
Continuous anchors normalize one stored direction and apply independent input/
output lengths; broken anchors preserve both vectors; corner anchors resolve
to zero handles. Each adjacent pair becomes one `CompiledSegment` containing
four points and precomputed f64 power-basis polynomials for time and progress.
Control times must remain ordered inside the segment. The immutable compiled
object stores boxed segments behind one Arc, analytic range/reverse results and
a stable fingerprint; sampling performs no allocation, locking or tree access.

Progress extrema are roots of each segment's quadratic derivative. Derivative
sign intervals additionally distinguish true reverse motion inside the normal
range from the necessary return from an allowed above-one overshoot. Actual
overshoot/reverse requires the authored permission, and an absolute progress
safety range remains non-bypassable. Time inversion uses exactly 32 bisection
iterations after a segment binary search, so output is a function of curve and
absolute time rather than frame cadence. Zero and one have exact branches.

`split_motion_curve` compiles the source, solves the selected segment parameter
and applies De Casteljau subdivision. It converts resolved neighboring handles
to explicit broken tangents and recompiles the result; dense sampling verifies
the shape is unchanged. Legacy `[x1,y1,x2,y2]` data maps exactly. If `x1>x2`,
one exact half subdivision turns the CSS-valid legacy shape into two segments
whose control times satisfy the editor's stronger direct-manipulation rule.
The old scalar runtime and portable theme schema remain untouched during
UI-7A; UI-7B will own atomic inherited presets and persistence migration.

Tests cover portable bounds, fixed endpoints/order, all accepted legacy
defaults, exact insertion, hidden extrema/reverse, settling overshoot without a
false reverse, automatic tangent determinism, maximum anchor count, absolute-
time repeatability and 256 deterministically generated legal monotone curves.

## UI-7B: inheritance and immutable preset snapshots

`nkdhr-theme::motion_style` keeps executable compilation out of portable data.
An active `MotionStyleProfileData` pins a built-in revision or embeds a complete
`MotionStylePresetData`, plus a sparse `MotionStyleTreeData`. Each tree has root
values and maps semantic families to stable component IDs and transition IDs.
Documents are bounded to 4,096 nodes and 1 MiB; IDs are bounded lowercase
stable identifiers. The root of a preset must contain a curve and duration,
while descendants may contain either field independently.

Resolution interleaves base and profile values at each specificity level:
base root, override root, base family, override family, and likewise for
component and transition. Thus specificity outranks origin, but an explicit
profile value replaces its preset at the same scope. Curves are `Option` values
at the layer boundary and replace as one complete object. Curve and duration
carry separate `MotionValueProvenanceData`; reset deletes one option rather
than copying a parent value. `snapshot_as_preset` overlays same-scope fields
onto the pinned base and produces a new complete immutable revision.

Only Balanced revision 1 currently resolves. It is generated from the legacy
four cubics and all 23 family durations, and dense tests compare its compiled
result with every old family evaluator. Lively, Calm and Direct are stable enum
identities without fabricated revision payloads; unavailable revisions fail
closed. When `MotionData.style` is absent, `CompiledMotionStyle` embeds the
same exact legacy migration in memory. The optional serde field is skipped, so
old theme profiles retain their bytes until a user explicitly authors style
data.

`CompiledMotionStyle` mirrors both trees with precompiled Arc-backed curves.
Compilation visits every source curve, including shadowed nodes. `ThemeRuntime`
builds this object beside `Theme` before acquiring its publication mutex, and
`ThemeSnapshot` carries both under one generation. A data or curve error cannot
replace the previous Arc. Lookup only walks four bounded map levels and clones
the selected curve's Arc; it performs no JSON work or curve compilation.
Existing widgets still execute the old `Theme::motion` path in UI-7B, ensuring
no visible change before UI-7C's policy/runtime work.

`MotionPresetLibraryData` is a 4 MiB/256-preset collection keyed by immutable
`(id, revision)`. A different payload cannot overwrite the same identity.
`nkdhrd` validates it as the scalar `theme.motion_library` leaf and supplies an
empty default to older theme files. The Settings-side
`MotionPresetLibraryEditor` adds the stronger runtime validation pass: every
import is completely compiled in isolation before it can produce an opaque
persistence request; its durable model changes only after matching host or
CTRL-5 confirmation.

## Error and safety policy

- Public geometry and style constructors reject non-finite values.
- Allocation arithmetic is checked before texture or atlas upload.
- Shader compilation and GL submission errors propagate to the host; a frame
  is skipped rather than displaying partially updated UI state.
- Last-known-good snapshots protect themes and binding maps.
- Unsafe code is isolated in the GLES module, documents its GL-context and
  buffer-layout invariants, and is covered by the software oracle plus live
  offscreen rendering.
- No renderer, widget or action callback performs privileged system work
  directly; system operations go through `nkdhrd`.

# Canvas — Internals

> [中文版本 / Chinese version](INTERNALS_zh_CN.md)

Audience: nkdhr contributors working on `nkdhr-canvas`. This document covers
the Phase 2 implementation accepted on 2026-08-08. Ground truth for the APIs
cited here is Smithay 0.7.0's own source (inspected directly,
`examples/minimal.rs` in particular) — not assumed from memory of older
Smithay versions, which have changed this API substantially over time.

## Scope and non-goals for this document

This document covers the compositor's own architecture (COMP-1 … COMP-8).
It does **not** cover the 2D primitive layer (`nkdhr-render`, UI-1) or the
widget toolkit (`nkdhr-ui`, UI-3) in any depth — those are Phase 3, built
*after* Phase 2 is accepted (ROADMAP's stated ordering: the canvas
compositor first, UI after compositor acceptance). Where COMP-1 needs to
put pixels on screen before either of those exist, it draws directly with
Smithay's own `GlesRenderer` (a bare clear + swap) — not a placeholder to
be deleted later, just the correct amount of rendering for what COMP-1
alone needs to prove (nested window lifecycle, resize, no leaks). COMP-7
is the seam where the canvas hands scene-node hosting over to `nkdhr-ui`
once Phase 3 exists.

## Crate layout

```
crates/nkdhr-canvas/src/
  main.rs           entry point: picks winit (nested) or TTY backend, runs the event loop
  backends/
    winit.rs         COMP-1/2: nested backend (smithay::backend::winit)
    tty.rs            COMP-5: DRM/KMS + GBM + libinput + libseat backend
  canvas/
    world.rs          COMP-3: world-coordinate window model
    viewport.rs        COMP-4: pan/zoom, overview mode, position marks
    output_group.rs    COMP-4/5: output <-> canvas binding (ROADMAP §2.3)
  input.rs            COMP-1..4: input dispatch and interaction geometry
  settings.rs         COMP-3: hot-reloadable keybindings and grid policy
  protocols/          COMP-6: clipboard, DnD, screencopy, session-lock, xwayland, ...
  widget_host.rs      COMP-7: the scene-node interface `nkdhr-ui` renders into
```

Both backends (`winit`, `tty`) implement the same small internal trait
(the render loop, damage tracking and input delivery are backend-specific;
everything above `backends/` — the world model, protocol handlers, input
dispatch — is backend-agnostic and identical on both). This is what makes
COMP-5 (TTY) additive rather than a rewrite of COMP-1 (nested): the
`canvas/`, `protocols/`, and shell-facing code never know which backend is
running.

## COMP-1: nested skeleton

`smithay::backend::winit` provides the nested backend: it runs the
compositor as a client window inside whatever desktop the developer is
already running (X11 or Wayland; winit itself detects and picks one). With
`default-features = false, features = ["backend_winit"]` on the `smithay`
dependency, this pulls in exactly `winit`, `backend_egl`, `wayland-client`,
`wayland-cursor`, `wayland-egl`, and `renderer_gl` — no `wayland_frontend`,
no `desktop`, no DRM/GBM/libinput/libseat. COMP-1 genuinely doesn't need
those yet: it has no Wayland clients of its own (that's COMP-2) and isn't
running on bare TTY (that's COMP-5), so there's nothing for a `Display` or
a seat to serve.

Initialization and the render loop (verified against Smithay 0.7.0's own
`examples/minimal.rs`, stripped to just the windowing/rendering parts —
the example also wires up xdg-shell/seat, which is COMP-2's job, not
COMP-1's):

```rust
let (mut backend, mut winit) = smithay::backend::winit::init_from_attributes::<GlesRenderer>(
    WinitWindow::default_attributes().with_title("nkdhr-canvas"),
)?;

'main: loop {
    let mut should_exit = false;
    let status = winit.dispatch_new_events(|event| match event {
        WinitEvent::CloseRequested => should_exit = true,
        WinitEvent::Resized { size, .. } => { /* log; next frame just re-binds at the new size */ }
        WinitEvent::Input(event) => { /* log */ }
        WinitEvent::Focus(_) | WinitEvent::Redraw => {}
    });
    if should_exit || matches!(status, PumpStatus::Exit(_)) {
        break 'main;
    }

    let size = backend.window_size();
    let damage = Rectangle::from_size(size);
    let (renderer, mut framebuffer) = backend.bind()?;
    let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
    frame.clear(CANVAS_BACKGROUND, &[damage])?;
    frame.finish()?;
    backend.submit(Some(&[damage]))?;
}
```

Notes that aren't obvious from the example alone:

- **`WinitEvent::CloseRequested` does not exit the loop by itself.**
  Smithay's own `minimal.rs` example doesn't handle it at all (its `match`
  falls through to `_ => ()`), which would leave the process running after
  the window closes. nkdhr-canvas sets `should_exit` explicitly and breaks
  its own loop — `PumpStatus::Exit` is a *separate* signal (winit's own
  event loop terminating, which does not happen just because the window
  closed) and checking only that would miss the normal "user clicked the
  close button" case entirely. This is the mechanism behind COMP-1's
  "exits cleanly nested" verification criterion.
- **Resize needs no explicit handling beyond logging.** `backend.bind()`
  already re-queries `window_size()` and resizes the `EGLSurface` itself
  every frame if it changed (see `WinitGraphicsBackend::bind`'s own
  source) — nkdhr-canvas doesn't need to track the size itself or resize
  anything by hand for COMP-1's purposes (world-relative viewport layout
  reacting to resize is COMP-4's job).
- **`Transform::Flipped180`** is what `minimal.rs` uses and matches GL's
  bottom-left-origin framebuffer convention versus Smithay's top-left
  logical convention; kept as-is rather than re-derived.
- No `Seat`/`SeatState`/keyboard focus exists yet in COMP-1 — there is
  nothing to focus (no clients). Input events are logged directly from
  the `WinitEvent::Input` variant's `InputEvent<WinitInput>` payload.
  COMP-2 introduces a real `Seat` once there's a client surface to route
  focus to.
- 10-minute leak verification runs the loop above with `RUST_LOG` off and
  samples this process's `/proc/<pid>/status` `VmRSS` at intervals — no
  Smithay API is involved in that check, it's process-external.
- The render loop has no frame pacing (no `wl_surface.frame()` callback
  wait, `vsync: false` per `init_from_attributes`'s default `GlAttributes`)
  — it renders and swaps as fast as the host will accept, which is
  visible as high, constant CPU use while nested. This is deliberate for
  COMP-1's narrow scope (prove the window/render/input lifecycle, not
  frame pacing) and expected to be revisited once there's real frame
  content worth pacing around; it is not a regression to "fix" later so
  much as a placeholder policy to *replace* with a real one (COMP-4/5),
  which is different from a placeholder implementation left to rot —
  nothing here needs to be deleted, just a pacing policy layered on top.

**Verified live** (temporary weston `--backend=headless --renderer=pixman`
host, cleaned up after — same "spin up a temporary real environment,
verify, tear down" approach CTRL-1 used for its systemd unit): starts
cleanly nested, connects over Wayland, creates its window at the
requested 1280×800, and renders every frame without erroring for the
full verification run; `VmRSS` sampled every 30s over 10 minutes stayed
flat (see PROGRESS.md's COMP-1 entry for the actual numbers) — no leak.
**Not exercised**: a live, interactive resize (dragging an edge) and a
live `CloseRequested` from clicking the window's close button — both
require a real pointer/WM interaction this headless, display-less dev
environment has no way to generate, the same category of gap CTRL-3/4
left open for AC-plug and Wi-Fi-toggle testing. What *is* confirmed:
`WinitGraphicsBackend::bind()`'s own source unconditionally re-queries
and resizes on every frame regardless of *why* the size changed (read
directly, not assumed), and this run's own initial-configure resize
(the very first size negotiation with the host, which uses the identical
`WinitEvent::Resized` code path a live drag-resize would) was received
and logged correctly. The `CloseRequested` → `should_exit` fix (Smithay's
own `examples/minimal.rs` doesn't handle this variant at all, which would
leave nkdhr-canvas running forever after its window closes) is a 3-line,
self-evidently-correct match arm verified by inspection; a live trigger
is still the strictly stronger check and is left for whoever next has
interactive desktop access to this machine.
- **Software rendering, not the target Iris Xe hardware path.** EGL
  initialization logged `failed to get driver name for fd -1` /
  `MESA-LOADER: failed to retrieve device information` — Weston's headless
  backend used the `pixman` (software) renderer for its own output, so it
  never advertised a real DRM device to nkdhr-canvas over the Wayland
  connection, and Mesa fell back to software rendering (llvmpipe) for
  nkdhr-canvas's own EGL context rather than opening `/dev/dri/renderD128`
  directly. This is expected and fine for the *nested* backend, which is
  explicitly a development/testing mode layered on top of whatever host
  is available — real Iris Xe hardware acceleration is exercised by the
  TTY backend (COMP-5), which opens the DRM render node itself with no
  host compositor in between.

## COMP-2: Wayland clients

Adds a real `wayland_server::Display` and a `ListeningSocket::bind_auto`
socket (`wayland-0`, `wayland-1`, ... — whichever is first free; not the
`minimal.rs` example's hardcoded `"wayland-5"`), split into
`main.rs` (entry point, backend/event loop, rendering, socket accept
loop — the part that changes per COMP milestone) and a new `state.rs`
(the `App`/`ClientState` structs and every protocol handler trait impl —
the part that mostly just grows). This is the same "split once there's
enough real content to justify it" moment CTRL-1 → CTRL-2 went through,
not a structure imposed up front.

`App` owns the five pieces of `wayland_frontend` state COMP-2 needs:
`CompositorState` (`wl_compositor`/`wl_subcompositor`, buffer commit),
`ShmState` (`wl_shm`), `DmabufState` (`zwp_linux_dmabuf_v1`), `XdgShellState`
(`xdg_wm_base` — toplevels and popups), `SeatState` + one `Seat`
(`wl_seat` — keyboard, pointer and touch). UI-6 feeds complete touch
down/motion/up/frame/cancel sequences to Smithay; touchscreen compositor
gestures remain unavailable until an empty-canvas/edge recognizer exists.
`DataDeviceState` (clipboard/DnD) is also wired up already,
matching `minimal.rs`, even though COMP-2's own verification list doesn't
exercise it — skipping it would mean *removing* handler impls again in
COMP-6 rather than adding to them, and the four `SelectionHandler`/
`DataDeviceHandler`/`*DndGrabHandler` impls it needs are a few lines of
boilerplate, not a new subsystem.

**dmabuf, not just `wl_shm`**: GPU-accelerated clients need it to avoid a
CPU copy every frame. Registered via the simpler `DmabufState::
create_global` (protocol version 3, plain format list from
`renderer.dmabuf_formats()`) rather than `create_global_with_default_feedback`
(version 4, needs a `main_device: libc::dev_t` to build a `DmabufFeedback`)
— resolving a real DRM device node for the *nested* winit backend is
exactly the query that already failed during COMP-1's own verification
(`failed to get driver name for fd -1`), and feedback's whole purpose is a
multi-GPU hint, moot on this single-GPU target anyway. `DmabufHandler::
dmabuf_imported` just creates the `wl_buffer` object
(`notifier.successful::<App>()`) without eagerly importing into the
renderer — real GPU import happens lazily, the first time the buffer is
actually used in a render element, on whichever renderer is rendering
that frame. `App` has no renderer reference to import into eagerly even
if it wanted to (see below), and eager validation isn't worth threading
one through for what COMP-2 needs.

**No `desktop::Window`/`Space` yet — deliberately, not an oversight.**
The original sketch here cited Smithay's `desktop` module for its
XWayland-unifying `Window` type, but that module's whole value is
managing *positioned* windows, and COMP-2 has no position model at all
(COMP-3 is where world coordinates arrive). Using it now would mean
threading a `Space` through code that has nothing real to place in it yet.
COMP-2 instead follows `minimal.rs` directly: `XdgShellState::
toplevel_surfaces()` (a flat, unordered list) plus
`render_elements_from_surface_tree` per surface, each rendered at a fixed
`(0, 0)` local offset — every client's window fills the same spot, since
there's no placement to differ by yet. `desktop::Window`/`Space` becomes
worth adopting exactly when COMP-3 needs real per-window world positions,
not before.

**Focus is "the newest mapped toplevel wins", not real click-to-focus.**
`XdgShellHandler::new_toplevel` calls `keyboard.set_focus` on the surface
being mapped immediately. This is *not* the real design (COMP-3's section
above already settles that: click-based, not automatic) — it's the
smallest thing that makes "accept typing" true for COMP-2's own
verification scenario (one client open at a time), stated as a deliberate
placeholder in `state.rs`'s own doc comment so it reads as something to
*replace*, not as the intended behavior that quietly ships. Pointer
events (`PointerMotionAbsolute`/`PointerButton`, forwarded via
`PointerHandle::motion`/`button`) target whichever surface currently holds
keyboard focus, for the same reason — there's no other surface to route
to yet without real placement.

The renderer itself stays a `GlesRenderer` owned by `main.rs`'s
`WinitGraphicsBackend`, never moved into `App` — nothing in protocol
handling (`CompositorHandler::commit`, `DmabufHandler::dmabuf_imported`,
etc.) needs to touch pixels directly, only `main.rs`'s own render-loop
body does, and it already has the renderer in scope there. Keeping `App`
renderer-agnostic is also what makes the eventual TTY backend (COMP-5)
additive: `state.rs` doesn't know or care which backend supplied the
renderer it's rendering with.

**A `wl_output` global, found necessary by testing, not anticipated by
this document's first draft.** ROADMAP's COMP-2 bullet doesn't mention
`wl_output`, and neither did this section originally — but `foot` refused
to render at all against an early build with no output global
(`err: wayland.c:1827: no monitors available`), which is real evidence,
not a hypothetical: several real clients query `wl_output` before they'll
even create a surface, even though nothing about `wl_shm`/`xdg-shell`/
`wl_seat` strictly requires one to exist. One `Output`
(`smithay::output::Output`), sized to match the nested window and
advertised via `Output::create_global` (plus the required
`OutputHandler` impl, empty — its one method has a default), fixes this.
Real per-output modelling (resize propagation, multiple outputs, the
output-group binding from ROADMAP §2.3) is COMP-4/5's job; this is
deliberately just enough output to stop being a protocol-compliance
blocker for ordinary clients.

**Verified live**, nested inside a temporary
`weston --backend=headless --renderer=pixman` host (same pattern as
COMP-1, cleaned up after): `weston-simple-shm` (`wl_shm`),
`weston-simple-egl` (EGL/GL rendering), and `weston-simple-dmabuf-egl`
(explicit `zwp_linux_dmabuf_v1`, confirming the dmabuf global was
correctly advertised and negotiable) all connected and ran without
error. `foot` and `gtk4-demo` (substituting for ROADMAP's "SDL/OpenGL
app" — see PROGRESS.md's COMP-2 entry for why) both opened and rendered
real content once the `wl_output` fix above landed; all five clients,
including three running concurrently, disconnected cleanly (via
`timeout`/`kill`) without ever crashing or hanging the compositor.
**Not verified**: actually typing into a focused client and seeing it
echo back, and a live interactive click changing pointer focus — this
headless environment has no real keyboard/pointer input source at any
layer (not even weston's own host has one), so there is no way to
generate a real key or button event here, the same category of gap
COMP-1 left for interactive resize/close. What *is* confirmed: the
keyboard/pointer forwarding code (`KeyboardHandle::input`,
`PointerHandle::motion`/`button` in `input.rs`) compiles
against Smithay's real API and is a direct, unmodified application of
the handles' documented usage — not exercised end-to-end, but not novel
or hand-wavy either.

## COMP-3: canvas model

The core novel design of this project. A **canvas** is an unbounded 2D
plane in `f64` world coordinates (`Point<f64, World>`, a marker type
distinct from Smithay's own `Logical`/`Physical`, in the new
`nkdhr-canvas/src/world.rs`). `Canvas` (also `world.rs`) holds every
mapped window (`ManagedWindow { surface, position }`) in stacking order.
Placement is unbounded rather than tiled: two windows may overlap, sit at
negative coordinates, or be placed arbitrarily far apart. A configurable
`GridSettings` policy aligns interactive geometry to lines measured from
world `(0, 0)` by default (`snap_to_grid = true`, `grid_size = 32`). Turning
it off restores exact unconstrained placement. Grid snapping never changes
the world model itself and never relocates pinned nodes. It also aligns the
work-state viewport's canvas anchor after navigation, described under COMP-4;
the viewport remains continuous while a gesture is active. A
window's *size* is read on demand from Smithay's own committed
buffer-size tracking (`with_renderer_surface_state`), not stored
separately — there's exactly one source of truth for "how big is this
window" (what the client actually committed), so it can never drift out
of sync with what's rendered.

No `desktop::Window`/`Space` here either, same reasoning as COMP-2:
`Canvas` is nkdhr's own small, purpose-built model (map/unmap/hit-test/
raise/cycle), not a wrapper around Smithay's `desktop` module.

- **Placement default**: a newly mapped window cascades from the last
  10 windows' positions. With the default grid this begins at `(96,96)`
  and advances one grid interval per window, wrapping after 10; with
  snapping disabled the legacy `(100,100)`, `(140,140)`, ... cascade is
  retained. This is a sensible starting point, not a layout constraint.
- **Move**: `super+left-drag` anywhere on a window updates its world
  position continuously from the pointer's motion delta. When the button is
  released with the grid enabled, a short compositor-owned point animation
  eases the content's top-left corner to the nearest grid intersection. No
  other window is re-laid out because nothing else's position derives from
  it. Snapping during the drag was rejected after physical testing because
  it makes the window jump between grid cells and visually detach from the
  pointer.
- **Resize**: `super+right-drag` computes a new logical size from the
  drag delta, aligns only the actively dragged edge(s) to the grid while
  keeping the opposite edge fixed, and re-requests an `xdg_toplevel`
  configure at that size;
  the client's own next commit (picked up automatically by
  `ManagedWindow::size`'s live buffer-size read) supplies the actual new
  content — nkdhr-canvas never assumes the client honored the requested
  size exactly.
- **Focus**: exactly one window (or none) has keyboard focus; plain
  left-click on a window focuses **and** raises it (moves it to the top
  of `Canvas`'s stacking order — both hit-testing and rendering treat the
  last element as topmost) and the click still forwards to the client
  normally, so clicking a background window both switches to it and
  activates whatever was under the cursor, matching ordinary desktop
  behavior. `cycle_focus` (default Alt+Tab) is the other way to switch,
  independent of pointer position. Neither `super+drag` nor
  `super+right-drag` count as a real Wayland pointer grab
  (`smithay::input::pointer::PointerGrab`) — that trait exists for
  protocol-visible grabs (popup dismissal, client-requested interactive
  move/resize), but a WM-level modifier gesture is entirely the
  compositor's own business, observed in `main.rs`'s own input dispatch
  *before* anything is forwarded to the seat, with no protocol object
  involved at all. This sidesteps `PointerGrab`'s large trait surface
  (every gesture/axis/relative-motion method needs an implementation,
  even as a pass-through) for something that doesn't need it.
- **Interaction settings**: UI-6 adds the bounded scalar `canvas.bindings`, a
  schema-v1 JSON document compiled by `nkdhr-ui` into key/button/gesture
  triggers and typed action invocations. Empty selects the canonical complete
  document and reads the three old keys as migration inputs; non-empty is
  authoritative. `nkdhrd` owns only the 1 MiB scalar bound. Domain validation,
  conflict analysis, device/capability availability, last-known-good
  generation and structured diagnostics belong to the compositor/shared UI
  compiler. `canvas.snap_to_grid` and `canvas.grid_size` remain ordinary typed
  leaves. The watcher publishes the complete binding candidate atomically;
  `input.rs` only normalizes events and looks up a compiled trigger, while
  `actions.rs` centrally maps stable action IDs to canvas operations. No
  configuration value executes code.

**Verified live**, nested inside a temporary
`weston --backend=headless --renderer=pixman` host with `nkdhrd` also
running (cleaned up after, same pattern as COMP-1/2): 12 `weston-simple-shm`
clients launched simultaneously cascaded to 10 distinct world positions
(logged, then wrapping as designed) without ever crashing the compositor.
`nkdhrctl config set canvas.close_window w` while nkdhr-canvas was
running produced a `keybindings reloaded` log line with the new value
within about a second — real hot-reload, not just "the mechanism should
work". An empty value was rejected at the CTRL-5 layer
(`close_window must not be empty`, last-known-good kept, matching every
other CTRL-5 namespace); a syntactically valid but unrecognized key name
(`"NotARealKeyName123"`) passed CTRL-5's validation and was instead
caught by `nkdhr-canvas`'s own fallback, logging the warning and reverting
to the built-in default as designed. **Not verified**: actually holding
Super and dragging, clicking to focus, or pressing Alt+Tab/`super+q` for
real — this headless environment still has no real pointer/keyboard input
source at any layer, the same gap COMP-1's resize/close and COMP-2's
typing left open. What *is* confirmed: the move/resize/focus/keybinding
logic compiles and type-checks against the real Smithay API, including
the disjoint-borrow shape needed to mutate the active canvas from inside a
match on `app.drag` (resolved by cloning the small `Drag` value out first
rather than holding a borrow across the mutation, avoiding any doubt about
borrow-checker behavior there); the D-Bus/config half of the same feature
(keybindings hot-reload) received the full live verification described
above, since that path needs no pointer or keyboard at all.

## COMP-4: viewport

`Viewport` (`canvas/world.rs`) is a camera onto a canvas: a world-space
anchor point plus a zoom factor (`Viewport::WORK` = origin, 1:1). The anchor
is a group-local logical point supplied by COMP-5's output layout and defaults
to the primary display's center. Thus `Viewport::center` means "the world
point shown at the canvas anchor", not necessarily the center of a wide
multi-monitor bounding rectangle. Its
`to_group_logical`/`group_logical_to_world`/`to_world_delta` methods are
the only place world<->group conversion happens; a backend applies the
individual output's group offset and physical scale afterward. COMP-3's
fixed-origin, no-zoom placeholders are gone. COMP-4 initially stored one
view on `App`; COMP-5 moved the same state into one `GroupView` per output
group without changing the viewport mathematics.

Two states are tracked by each `GroupView::in_overview` plus
`pre_overview_viewport` (the work viewport to return to), rather than a
formal state-machine enum — two fields remain simpler than a third
"transitioning" state:

- **Work state** (default): zoom pinned at 1:1. Panning is a direct
  world-offset translation of `viewport.center` — three real input paths,
  matching ROADMAP's "keyboard / pointer / touchpad gestures" bullet
  exactly:
  - Pointer: a plain left-drag that starts on *empty* canvas (no window
    under the cursor) becomes `Drag::Pan` — the same `input.rs`-owned drag
    mechanism as COMP-3's move/resize, not a `PointerGrab`, for the same
    reason. Compositor-owned move/resize/pan drags still advance Smithay's
    `PointerHandle` location with `focus = None` on every motion. Relative
    backends calculate the next global point from that handle; updating the
    viewport without updating the pointer would make every event start from
    the original press and appear to freeze a touchpad drag.
  - Touchpad: the typed default map owns exactly-three-finger swipe as
    `canvas.viewport.pan` and exactly-three-finger pinch as
    `canvas.viewport.pinch`. Swipe translates delta into `viewport.center`;
    pinch preserves its initial world-space anchor beneath the moving logical
    center while changing zoom. Two-finger
    scrolling remains an ordinary `InputEvent::PointerAxis`; it is forwarded
    unchanged to the pointer-focused Wayland client (after a pinned node gets
    first refusal), exactly like a mouse wheel. Treating every axis event as
    canvas pan made application lists impossible to scroll and was rejected
    by the first real GTK/TTY test. Other unbound finger counts remain available
    through the standard pointer-gestures protocol. The nested winit backend
    types native gesture events as `UnusedEvent`, so three-finger canvas pan
    is a TTY-session feature; ordinary application scrolling still works on
    both backends.
  - Keyboard: `super+arrow` or standard Vim H/J/K/L, a fixed step per press (`PAN_STEP` world
    units) rendered through a short ease-out transition. A new press starts
    from the currently displayed viewport but adds its step to the previous
    animation's destination, so rapid/repeated input coalesces rather than
    losing distance or flashing through intermediate positions. Deliberately
    *not* bare arrow keys —
    any focused client (a text field, a terminal's readline) already
    needs those for its own cursor movement; gating on Super avoids the
    conflict. Smithay's normal keyboard repeat supplies repeated presses;
    no compositor-specific repeat timer is required.
  With `snap_to_grid`, pointer and three-finger pans remain continuous until
  release, then a short viewport animation aligns the anchor's world point to
  the nearest grid intersection. Keyboard destinations, marks and overview
  exits use the same aligned work-state target. Disabling the grid preserves
  exact viewport coordinates. The overview camera itself is transient and is
  not quantized while fitting or inspecting content.
  No windows are ever scaled in this state, per the project's sharpness
  policy (ROADMAP §2.3/§2.4: 1:1 always in work state, scaling blur only
  accepted in overview).
- **Overview state** (transient — entered/exited via `super+overview`,
  default key `o`, CTRL-5-backed like `close_window`/`cycle_focus`;
  `Esc` and clicking empty space also exit): `Viewport::fit_group` computes a
  zoom (never *in* past 1:1, only out or unchanged — `Canvas::bounding_rect`
  merged across every mapped window, with a fixed 1.25× margin) and
  animates there. Clicking a window animates the viewport to that
  window's center at 1:1, exits overview, and focuses+raises it — the
  same "animate, then focus" sequence a mark jump uses, which is what the
  COMP-4 verification bullet ("clicking a window in overview zooms to it
  1:1 with correct input routing afterwards") is checking. Pointer motion
  is not forwarded to any client while in overview (nothing to interact
  with until a window is picked or the overview is dismissed).

**Animated transitions**: `world::Animation` (`from`/`to` viewport, start
`Instant`, `Duration`, ease-out-cubic), advanced once per render-loop
iteration (`advance_animations`, before rendering) rather than via a
separate timer or animation engine. The function advances the
optional animation on every group, covering overview enter,
overview exit/cancel and mark jumps without building general-purpose
animation infrastructure ahead of a second real user.

**Position marks** (ROADMAP §2.3): `canvas::marks::Marks` is a plain
`HashMap<u8, Point<f64, World>>` in memory, but persisted to CTRL-5 as a
single **string** (`canvas.marks`, `nkdhrd/src/namespaces/canvas.rs`), not
a nested table — CTRL-5's
`Config1.Get`/`Set` only support scalar leaf values so far, and even
setting that aside, `Set` only ever overwrites an *already-existing* leaf
(this is how "unknown keys are rejected" applies to writes, not just to
hand-edited files), so a `HashMap`-shaped namespace field could never
create a mark's entry the first time it's set. Encoding the whole set as
one string, parsed/formatted entirely inside `nkdhr-canvas`
(`marks::parse`/`marks::format`, with real unit tests — the one piece of
COMP-4 fully testable without a live Wayland/D-Bus session), sidesteps
both limits without changing CTRL-5's engine for one namespace's needs.
COMP-5 namespaces these maps by canvas and writes
`v2;<hex-canvas>:<digit>:<x>,<y>;...`; hex only makes arbitrary UTF-8
canvas names unambiguous. The old `<digit>:<x>,<y>;...` form still loads
into the `default` canvas.
`super+shift+<digit>` sets a mark at the current viewport center and
saves immediately; `super+<digit>` jumps (animated) to one, also exiting
overview if active. Digits are matched on the key's *raw*, unshifted
level (`KeysymHandle::raw_syms()`, not `modified_sym()`) specifically so
`super+shift+3` still means "mark 3", not whatever Shift turns the "3"
key into on the active layout. This is intentionally the *only*
long-distance navigation primitive — there's no minimap, no window
switcher grid; overview mode plus marks is the complete navigation model
per the settled design.

Focused-window keyboard movement is deliberately not assigned a Phase 2
binding. It is recorded as an interaction candidate to design with the wider
shortcut, discoverability, settings and visual-feedback model once Phase 3's
toolkit exists, before Phase 4 commits the shell interaction language. The
canvas already exposes the required position and animation mechanisms, so
deferral does not require a later compositor rewrite.

**Verified live**, nested inside a temporary
`weston --backend=headless --renderer=pixman` host with `nkdhrd` also
running (cleaned up after, same pattern as COMP-1…3): 10 simultaneous
`weston-simple-shm` clients sustained **57.5-57.7 fps (~17.4ms/frame)**
over multiple 5-second sampling windows, via a new always-on frame-time
log (not one-off test instrumentation — genuinely useful for checking
compositor frame pacing later too) — close to, though this environment's
software (llvmpipe) rendering and nested nature mean it is *not*
representative of real Iris Xe hardware performance, the same caveat
carried since COMP-1. Marks were verified fully live and end to end,
persistence included: `nkdhrctl config set canvas.marks
"0:120.5,80.25;3:400.0,-200.5"` while nkdhr-canvas was running, then
killing and restarting the process, produced `loaded 2 saved mark(s)` on
the new run — real cross-restart persistence through CTRL-5, not just the
`marks::parse`/`format` unit tests (which also pass, covering round-trip
encoding, the empty-string case, and tolerating unparsable entries).
CTRL-5's new `canvas.overview` field round-trips correctly via
`nkdhrctl config get`. **Not verified**: any interaction that needs real
pointer or keyboard input — `super+drag` pan, three-finger swipe pan,
`super+arrow` pan, `super+o` entering/leaving overview, clicking a window
in overview, and setting/jumping to a mark via the actual keypress rather
than `nkdhrctl` directly. Same root cause as every prior COMP milestone's
gap: this headless environment has no real pointer or keyboard input
source at any layer. What *is* confirmed: all of this code compiles and
type-checks against the real Smithay API, uses the same input-dispatch
structure already live-verified for COMP-3's move/resize/focus, and the
non-input-driven half of every COMP-4 feature (frame pacing, mark
persistence) received full live verification.

## COMP-5: TTY backend

The executable has two permanent backends behind one small `Backend`
trait. `--nested` selects winit for development; `--tty` selects the real
backend. With no flag it chooses nested only when `WAYLAND_DISPLAY` or
`DISPLAY` exists, otherwise TTY. Cargo features keep deployments lean:
`nested` enables `backend_winit`; `tty` enables GBM, udev, libinput,
libseat, GLES and Smithay's multi-GPU renderer. Neither backend is
temporary scaffolding and the protocol/world/input layers are shared.

The TTY event loop is calloop-based. `LibSeatSession` owns KMS device
access and pause/resume; `UdevBackend` enumerates and hotplugs DRM cards;
`DrmScanner` maps connectors to CRTCs; `DrmOutputManager` owns atomic KMS
surfaces backed by GBM. The primary GPU's render node is opened directly
and registered with `GpuManager<GbmGlesBackend<...>>`; render nodes cannot
become DRM master and therefore cannot take over a display. KMS primary
nodes are opened separately and only when their connectors are eligible
for scanout. Rendering happens on the primary render node and copies
across GPUs when a scanout device has its own render node. A scanout-only
device (including VKMS) borrows the primary render-node allocator and
restricts formats to linear modifiers. No setuid helper or nkdhr-owned
privileged code is introduced.

The graphical VT does not receive kernel-console switching automatically.
On the TTY backend only, `Ctrl+Alt+F1` through `Ctrl+Alt+F12` are intercepted
before client delivery and converted into `LibSeatSession::change_vt`. XKB
may resolve that chord directly to an `XF86_Switch_VT_n` keysym, so dispatch
accepts that dedicated result and also checks the unmodified level-zero
function keys with Ctrl+Alt tracked separately; relying on `modified_sym()`
alone made the first physical test silently miss the binding.
`App` carries a one-shot backend-control request rather than a libseat handle,
so the shared input layer remains session-library-independent and the nested
backend leaves those combinations untouched. The same binding remains
available while ext-session-lock is active: switching to another authenticated
VT does not unlock or expose this compositor session. Before asking libseat to
switch an outgoing VT, the backend stops rendering, performs one device-wide
atomic reset that disables every connector and plane, then pauses every DRM
output manager and releases master. Requesting the switch only after that
sequence avoids a race in which logind can revoke device access before the
reset runs. If the switch request fails, the backend reactivates and rescans
the devices immediately; an asynchronous pause event repeats the pause
idempotently and suspends libinput. Activation resumes libinput, reactivates
DRM with connectors/planes reset, resets output buffers and rescans connectors
without discarding clients, canvas state or focus.

`canvas/output_group.rs` resolves the persisted `canvas.outputs` map
against the currently connected connector names. Configured coordinates
are logical coordinates and are normalized so negative placements work;
physical mode size divided by the positive fractional `scale` gives each
output's logical extent. Groups are then packed into a deterministic,
non-overlapping compositor-wide coordinate space solely for `wl_output`
and libinput routing — their canvases still have no spatial relationship.
With no configuration, all connected outputs form one horizontal
`default` group bound to the `default` canvas. Once explicit groups
exist, an unmentioned hotplugged connector becomes an `auto:<connector>`
single-output group/canvas so it is never blank.

Each resolved group also has one `canvas_anchor` in group-local logical
coordinates. It is the center of `CanvasOutputGroup::primary` when configured,
otherwise the center of the first connected member in stable connector-name
order; a single-output group therefore has no ambiguity. The output-group
schema validates that a non-empty `primary` names a member. Rendering and hit
testing use this anchor in every world/group transform, so world `(0,0)` is
shown at the primary display center in `Viewport::WORK`, and adding a monitor
does not silently redefine the canvas origin as the combined rectangle's
center.

`App` owns first-class canvas worlds by canvas name and group view state by
group name. A group view contains exactly one canvas binding, viewport,
overview state and animation. Every output render pass in a group reads
that same viewport, transforms world coordinates into the group's rigid
logical rectangle, subtracts the individual output's group position, and
finally applies that output's scale. Distinct groups therefore pan,
overview and receive newly mapped windows independently; two groups may
also deliberately bind the same canvas while retaining independent
views. Layout reconciliation preserves disconnected group/canvas state so
unplugging and reconnecting a monitor does not discard its world.
Position marks are stored per canvas in the existing scalar
`canvas.marks` setting with a versioned, canvas-name-safe encoding; the
old single-canvas encoding still loads into `default`.

Pointer coordinates remain compositor-global for Wayland seat delivery,
but hit testing first finds the physical output under the pointer, chooses
its group, subtracts the group's packed origin, then applies that group's
viewport to reach world coordinates. Crossing into another output group
activates that group; a drag remains attached to the group in which it
started. Keyboard actions target the active group.

`DrmOutput::render_frame` supplies Smithay's per-output damage history.
Only non-empty frames are queued; KMS vblank completes them through
`frame_submitted`, and Wayland frame callbacks are sent only for windows
on the canvas actually presented. This avoids a full redraw on unchanged
outputs while keeping hotplug/config changes on the same reconciliation
path. Each output permits at most one rendered KMS frame in flight. While
that frame awaits vblank, further input and scene changes are coalesced in
the compositor state rather than rendered into additional intermediate
buffers; the first render pass after `frame_submitted` presents the latest
state. This keeps pointer and dragged-object motion synchronized to display
presentation instead of accumulating stale positions under high-rate input.
The seat pause/activation path clears the compositor-side in-flight marker
to mirror the DRM output manager's presentation-state reset on activation.
After successful activation it also calls `DrmOutput::reset_buffers` for
every output. The fresh swapchain slots have buffer age zero, forcing the
first resumed frame to repaint the complete output; retaining pre-pause ages
made the first physical VT-resume test expose only a partially damaged block
until the next input event.
Physical connector removal explicitly clears that output's KMS surface (DPMS
off and every plane disabled) before dropping its `DrmOutput`; the remaining
outputs then restore explicit buffer modifiers where possible. Reconnecting
therefore starts from a clean CRTC rather than inheriting a stale framebuffer
or plane assignment. The current SDR compositor prefers 8-bit ABGR/ARGB
scanout formats and keeps 10-bit formats only as fallbacks. Choosing 10-bit
before the project has HDR/color-management policy adds unneeded output-side
dithering and driver-specific behavior, so the ordinary SDR path remains the
stable default.
The two-output VKMS diagnostic measured zero render-engine growth over a
10-second idle window and 0.10% of one CPU core. A later active-local-VT run
on the laptop panel exercised real libseat pause/resume by switching from
TTY1 to TTY2 and back: clients, focus and canvas state survived, input resumed,
and the buffer-reset fix repainted the whole panel immediately without an
extra input event. Final COMP-5 acceptance on a real eDP + HDMI setup then
passed one rigid group, two independent groups, live configuration changes,
client-preserving physical unplug/replug and a clean two-output VT handoff in
both directions. The long-session damage/idle observation belongs to COMP-8.

For safe development only, `NKDHR_DRM_DEVICE` overrides the primary render
node and `NKDHR_DRM_SCANOUT_DEVICE` forms a hard KMS-device boundary: when
set, no other primary DRM node is opened, not merely excluded from
connector scanning. The render override must name a render node and the
scanout override must name a primary node. These invariants exist so a
VKMS diagnostic can use the real GPU for rendering without acquiring DRM
master or modesetting the developer's real panel. Production discovers
the render GPU and opens seat KMS devices normally. A compositor must
normally run in a local seat session. `LIBSEAT_BACKEND=noop` is acceptable
for isolated VKMS diagnostics but is not a production launch mode.

### Reproducible two-output VKMS lab

`crates/nkdhr-canvas/tools/vkms-lab.sh` is permanent developer tooling for
the hardware-independent part of COMP-5 regression testing. It uses the
kernel's VKMS configfs ABI to build one virtual DRM device with two complete
pipelines (each connector has its own encoder, CRTC and primary plane), so
both outputs can scan out concurrently. This exercises the same udev,
connector scanner, atomic KMS, GBM, output-group and damage paths as a
physical display; it is not a compositor-side fake-output backend.

The commands are intentionally narrow and idempotence-safe:

- `setup` creates only `/sys/kernel/config/vkms/nkdhr-lab`, refuses to
  overwrite an existing instance, and rolls back that exact instance if
  construction fails;
- `connect <0|1>` and `disconnect <0|1>` change the selected connector's
  configfs status, which emits the kernel hotplug event consumed by udev;
- `show` reports the lab instance and exposed DRM connector state without
  requiring root;
- `audit <pid>` reads the compositor's `/proc/<pid>/fd` links and fails if
  it finds an open primary DRM node not owned by the VKMS lab;
- `teardown` disables and removes only `nkdhr-lab`, using explicit unlink
  and `rmdir` operations rather than recursive deletion. It deliberately
  leaves the `vkms` module loaded because another test instance may exist.

Mutating commands require root because configfs represents live kernel
objects. Production never calls this tool. A VKMS run proves compositor
logic and kernel API integration, but it still cannot prove physical link
training, EDID quirks, the local logind seat handoff, or the target panel's
real modes; those remain physical acceptance items.

Before exercising outputs, a VKMS diagnostic must audit the compositor's
open file descriptors. They may include the selected VKMS primary node and
the real GPU's render node, but must not include any excluded real-GPU
primary node. This is the executable safety check for the hard boundary;
connector logs alone are insufficient because opening a primary node can
acquire DRM master before a connector scan occurs.

## COMP-6: protocol long tail

`protocols/` owns the globals and the compositor-side policy that is shared
by both backends. Smithay supplies most protocol object lifecycles, but
registration alone is not sufficient: selection follows keyboard focus,
pointer constraints alter motion delivery, lock surfaces replace the normal
scene and screencopy must read back the composed output. Per protocol:

- **Clipboard + primary selection**: `wayland::selection::data_device`
  already exists for COMP-2 drag-and-drop, but COMP-6 makes clipboard
  focus follow keyboard focus and adds
  `wayland::selection::primary_selection`. Selection bytes continue to
  travel directly between the offering and receiving clients through the
  protocol pipe; the compositor stores metadata and brokers the file
  descriptor, not a second copy of clipboard contents.
- **Drag-and-drop**: the same `data_device` state and
  `ClientDndGrabHandler`/`ServerDndGrabHandler` are shared with the real
  pointer focus path. A DnD target is therefore the topmost surface under
  the canvas pointer, including across output-group transforms. The
  optional client DnD icon is rendered below the pointer and receives frame
  callbacks from the same backend-independent render path.
- **Server-side decoration policy**: `xdg-decoration-unstable-v1` always
  configures `ServerSide`, including when a client requests or unsets CSD.
  The compositor draws the matching border/titlebar in the same canvas
  render list as the client surface so damage, stacking and viewport
  transforms remain unified. Clients that do not bind the negotiation
  protocol continue to render their own CSD as required by xdg-shell.
- **Screencopy**: nkdhr implements the server side of
  `wlr-screencopy-unstable-v1` directly from Smithay's
  `wayland_protocols_wlr` re-export (Smithay 0.7 has bindings but no
  ready-made screencopy state). Requests are validated against the
  referenced `wl_output`, queued by output identity, fulfilled from the
  next fully composed frame and copied into the client's advertised
  XRGB8888 SHM buffer with stride/format/pool bounds checks. Requests with
  and without `overlay_cursor` are separated onto successive frames so a
  client gets exactly the cursor policy it requested. A locked output
  exposes only its black/lock-surface composition, never the obscured
  canvas. Nested GLES readback has one non-obvious repair: mapping a texture
  leaves EGL current without the winit window surface, so the backend does
  a no-op render bind before swap; without it the first real `grim` capture
  causes an EGL `BadAlloc`/compositor exit.
- **`ext-session-lock`**: `wayland::session_lock` enters a locking state
  immediately, stops all input to normal surfaces and renders only black
  or the per-output lock surface. The compositor sends `locked` only after
  every connected output has presented one protected frame; dropping or
  crashing the lock client keeps the session protected, while a valid
  `unlock_and_destroy` restores the previous group focus. The actual PAM
  UI remains SESS-3 territory.
- **Pointer constraints**: `wayland::pointer_constraints` constraints are
  activated only while their surface has pointer focus. Locked pointers
  receive relative motion without changing the compositor cursor;
  confined pointers clamp motion to the surface or requested region. The
  compositor renders client cursor surfaces (including their hotspot) or a
  built-in RGBA arrow on both backends; winit's host cursor is hidden so it
  cannot double-render over the compositor cursor. On TTY, normal frames
  permit only DRM cursor-plane scanout (not primary/overlay direct scanout),
  avoiding software-cursor trails while preserving the composed primary
  plane. A pending screencopy disables cursor-plane scanout for that frame so
  `overlay_cursor` captures still contain the cursor in framebuffer readback.
- **Idle inhibit**: `wayland::idle_inhibit` tracks live, visible inhibitor
  surfaces. `App::idle_inhibited()` is the single compositor policy query
  used by the eventual session idle/DPMS path; dead, unmapped or hidden
  surfaces do not keep the session awake.
- **Fractional/integer scale**: per-output scale is read from the same
  CTRL-5 output-arrangement config as COMP-5's group layout. The
  compositor sends `wp-fractional-scale-v1.preferred_scale` for the output
  currently presenting each toplevel and keeps `wl_output`'s rounded-up
  integer scale as the fallback for clients that do not bind the
  fractional protocol. Compositor-owned memory elements use their native
  image rect as the sample source and an output-scale-adjusted physical
  target size; in particular, the fallback pointer does not shrink to half
  its logical size on a 2x output.
- **XWayland — resolves ROADMAP §8's open question**: **in-process**,
  via Smithay's own `smithay::xwayland` module (`XWayland` +
  `X11Wm`/`XwmHandler`), not an external `xwayland-satellite`-style
  proxy. Smithay ships first-class support for this exact model (it's
  what `XwmHandler` exists for: nkdhr-canvas plays the X11 window
  manager role directly), it needs no extra moving process or IPC
  surface beyond the Xwayland server itself. COMP-6 refactors
  `ManagedWindow` from its original XDG-only `ToplevelSurface` field to
  Smithay's `desktop::Window`, which abstracts xdg-shell and X11 surfaces
  identically; COMP-3's canvas model (world position, focus, move/resize)
  then applies to XWayland windows with no parallel world model. The
  Wayland/X11 clipboard and primary selections bridge in both directions;
  after Smithay 0.7 queues a new X11 selection-owner request, a harmless
  RANDR reply round trip acts as the required flush barrier. Xwayland's
  scrubbed child environment explicitly retains `LD_LIBRARY_PATH`, which
  keeps non-system/Nix runtimes and unpacked diagnostics usable without
  weakening normal packaged launches.
  `xwayland-satellite`'s
  process-isolation benefit (crash containment, lazy startup without the
  main compositor knowing X11 details) isn't worth the added IPC surface
  for a project whose whole compositor is already a single from-scratch
  process audited as a unit.

Two compatibility pieces were found by real clients rather than named in
ROADMAP's shorthand list. `OutputManagerState::new_with_xdg_output` supplies
logical output geometry (`grim` otherwise inferred a zero-size nested
output), and `PopupManager` tracks, renders, hit-tests, grabs and repositions
nested xdg popups. Client-requested xdg/X11 move and all eight resize edges
reuse the canvas drag machinery after validating the active pointer grab.

**Verified live** under a temporary headless Weston host (all downloaded
RPMs and runtime directories removed afterward): real Wayland SHM, GTK4 and
pointer-constraints clients mapped; clipboard and primary selection copied
exact text both directions between Wayland (`wl-copy`/`wl-paste`) and X11
(`xsel`) through a real in-process Xwayland server; a GTK4 app forced through
X11 rendered in a `grim` capture; full 1280×800 and 256×256 region captures
succeeded; default and `grim -c` captures differed only by the compositor's
visible fallback arrow; and repeated readback did not terminate the nested
compositor. A real `swaylock` request reached protected presentation and was
acknowledged only afterward. Fedora had no `/etc/pam.d/swaylock`, so the PAM
worker then crashed; nkdhr remained fail-closed and a post-crash screenshot
was pure black, proving the old canvas was not exposed. On the real TTY
backend, Weston's constraints client then confirmed both confinement and
locked-pointer relative motion, and its standard data-source DnD client
confirmed drag-icon tracking plus a successful move to an empty target.
Weston's `--self-only` no-data-source compatibility mode removes the source
item but does not receive a drop under Smithay 0.7: Smithay marks every
source-less offer unvalidated before emitting `wl_data_device.drop`. This is
recorded as a non-blocking upstream compatibility limitation; ordinary
Wayland DnD is the accepted path and passed. Observable idle inhibition and
a valid PAM-backed unlock remain environment/integration checks. Xwayland
itself was unpacked temporarily for the nested test because the host package
is not installed; production X11 support requires an `Xwayland` executable
on `PATH`.

## COMP-7: canvas widget host

The seam Phase 3/4 (`nkdhr-ui`, then the shell) build on. A **pinned
node** is anything with a world-space position that isn't a client
window: a clock, a system monitor, eventually shell chrome. The host
interface (`widget_host.rs`) remains deliberately tiny and object-safe. UI-5
extended its renderer-independent payload and input hooks without putting a
concrete renderer or window-system event into the world model:

```rust
pub trait PinnedNode {
    fn id(&self) -> &str;
    fn world_rect(&self) -> Rectangle<f64, World>;
    fn layer(&self) -> PinnedLayer;
    fn render_data(&self) -> PinnedRenderData<'_>;
    fn pointer_event(&mut self, event: PinnedPointerEvent) -> InputHandled;
    fn prepare_frame(&mut self, output_scale: f32) -> Result<(), String>;
    fn keyboard_event(&mut self, event: &UiEvent) -> InputHandled;
}

pub enum PinnedRenderData<'a> {
    Memory {
        buffer: &'a MemoryRenderBuffer,
        source_size: Size<i32, Logical>,
    },
    NkdhrUi {
        display_list: &'a DisplayList,
        textures: &'a TextureStore,
        commit: u64,
    },
}

pub enum PinnedLayer { BehindWindows, AboveWindows }
```

The trait deliberately returns renderer-independent **data**, not a generic
`CanvasRenderElement<R>`. A generic render method makes the trait unusable
as `dyn PinnedNode`, while fixing it to `GlesRenderer` breaks the TTY
multi-GPU renderer. The canvas host translates `PinnedRenderData` into its
generic render-element list using the renderer supplied by either backend.
`UiPinnedNode` now adapts any object-safe `UiSurface`. Each `(node, GLES
context)` owns one context-bound `GlesBackend`; immutable lists receive the
viewport translation/zoom outside the retained tree and are prepared against
the complete output target. The same path works through direct `GlesRenderer`
and TTY `MultiRenderer` frames, so no intermediate CPU image is created.

Layering and hit-testing use the same two explicit bands. `AboveWindows`
nodes render and receive pointer input before windows; `BehindWindows`
nodes do so after windows. Cursor and DnD icons remain above both. Pointer
events carry local node coordinates plus normalized button/motion data —
never `InputEvent<WinitInput>`, which would quietly make the supposedly
shared host depend on the nested backend. Returning `Captured` prevents the
same event from reaching a client surface or starting a canvas pan.
Pointer capture remains routed beyond node bounds. A pressed retained control
may also claim canvas-local keyboard focus; clicking elsewhere clears it, so
toolkit keys/text and compositor/client bindings remain mutually exclusive.

COMP-7 ships exactly one permanent developer fixture to prove the interface
end to end: setting `NKDHR_CANVAS_DEMO_PINNED_IMAGE=1` registers a generated
RGBA image at a fixed world coordinate on the default canvas. It is not
enabled in normal sessions. It remains visually static, but accepts a
pointer press and logs its hit count, satisfying ROADMAP's rendering,
layering **and input** acceptance without adding a temporary toolkit or a
product widget before Phase 3/4.

The fixture was subsequently exercised on the real TTY backend: its world
position remained correct under pan and overview transitions, the intended
behind-window layer held, and its empty-area input target reported presses
while window content above it retained input priority.

UI-5 additionally live-tested the opt-in Appearance Settings node in the
nested compositor. It recorded and drew the real retained list under the
canvas world transform with no GLES error. The standalone Wayland/EGL binary
uses the identical `AppearanceSurface` model/root and display-list path.

## COMP-8: stabilization

Not a feature bucket: first close correctness issues found by an audit, then
run an extended active-TTY session, re-run every previous
milestone's verification checklist against the final configuration, and
exercise client-crash resilience under sustained load. Record start/end RSS,
CPU and DRM engine counters, every client crash injected, compositor liveness
after each, and the exact duration. A nested soak is useful regression
evidence but does not replace the active-VT, real-panel and external-output
checks required by COMP-5/8. Phase 2 may be called implementation-complete
with hardware gaps recorded, but is not **accepted** and its staging docs do
not graduate until those physical checks and the full eight-hour active-TTY
soak actually pass. The owner may choose an idle or interactive soak workload;
the separate physical milestone regressions carry interaction correctness.

Client disconnect cleanup is intentionally defensive. A graceful
`xdg_toplevel.destroy` or XWM unmap removes its window immediately, while the
once-per-frame maintenance pass also drops any `desktop::Window` for which
Smithay's `IsAlive` is false. The same pass clears dead keyboard focus, a
move/resize drag whose surface died, and a dead DnD icon. This second path is
required for abrupt process death: a COMP-8 SIGKILL stress run demonstrated
that relying on the protocol destroy callback alone left dead windows in the
stack even though the clients and their descriptors were gone. Neither
backend retains the `Client` handle returned by `insert_client`; the display
owns the connection and `ClientData`, so keeping another handle in the nested
loop would itself be an unbounded per-connection history.

The final bounded COMP-8 regression used an optimized build with the COMP-7
fixture under a temporary headless Weston/Pixman host. It force-killed 200
SHM/EGL clients; all mapped windows were reclaimed, the compositor stayed
live, the window count returned to zero and the FD count stayed at 30. The
following 20 samples at 30-second intervals (9.5 minutes between first and
last, approximately a ten-minute run) measured 130,676 KiB RSS initially,
131,704 KiB after allocator warm-up, then exactly 131,704 KiB for samples
3–20. This is useful crash/resource regression evidence only, not the
ROADMAP-required eight-hour active-TTY soak.

The physical acceptance run is collected by
`crates/nkdhr-canvas/tools/soak-test.sh`, not by keeping an interactive agent
or terminal watcher alive. `run` creates a timestamped state directory,
starts a transient user-systemd collector under a system sleep inhibitor and
then `exec`s the release compositor so the compositor preserves its local TTY
and libseat context. `start --pid` can attach the same collector to an
existing compositor. Only seconds during which the recorded logind session
is active count toward the target, so a VT excursion is observable without
being misreported as nkdhr use. A PID start-time identity check distinguishes
the original process from PID reuse.

The collector records process RSS/high-water memory, CPU ticks, threads and
descriptor count; deduplicated per-DRM-client i915 fdinfo engine/memory
counters; connector status/enabled signatures; session state; sampling gaps;
filtered kernel DRM failures; and compositor-owned failure/panic lines. A
report is frozen on completion, early process death or an explicit monitoring
stop, so output appended by a compositor left alive afterward does not mutate
the historical verdict. Automatic warnings are bounded screening criteria,
not a substitute for workload-aware review: real clients may legitimately
change compositor buffers, while monotonic RSS/FD growth, GPU-reset evidence
or the absence of any idle render-engine interval needs investigation.
Runtime data lives in the user's state directory and never in the Git
worktree.

The 2026-08-05 real-panel regression then re-passed the single-output input
and presentation paths with native `foot` and Weston clients. It found and
fixed two presentation/input defects rather than merely documenting them:
compositor-owned drags now keep Smithay's pointer location synchronized, and
each KMS output allows only one queued frame until vblank while ordinary
frames permit cursor-plane scanout. Together these removed touchpad drag
freezes and persistent pointer/window trails. Default-on grid placement,
continuous move followed by eased release snapping, edge-only resize
snapping, negative/free placement, smooth keyboard pan, overview, marks,
pinned-node routing, standard DnD and pointer constraints were all observed
on the real TTY. A GTK list exposed that the original axis policy swallowed
application scrolling; after the correction, real touchpad testing confirmed
that two-finger axis input scrolls the focused client while an exactly-three-
finger libinput swipe moves the canvas globally, including over a client.
Ctrl+Alt+Fn VT switching and immediate full repaint on resume also passed.
The 2026-08-07 external-display regression subsequently passed rigid and
independent output groups, physical hotplug with preserved clients and clean
two-output VT handoff. On 2026-08-08 the owner accepted an uninterrupted
eight-hour active-TTY idle soak as sufficient for this early compositor phase:
all 956 samples retained the original process, RSS grew by only 192 KiB, FDs
and threads were constant, average CPU use was 0.243% of one core, and no
kernel DRM failure was recorded. A post-soak VT log exposed one final ordering
race; the fix now clears and pauses DRM before the VT switch request. A final
one-minute physical TTY regression then recorded 32/32 matching samples,
correct inactive/active output transitions, zero kernel/compositor errors and
an immediate full repaint after returning. Phase 2 was accepted and these
documents graduated after that result.

## Rendering pipeline boundary (why Phase 3 is a hard line here)

COMP-1 through COMP-7 render everything as flat textured quads via
Smithay's own `GlesRenderer`/`Frame` API directly (rects, borders,
window/pinned-node content) — there is no rounded-rect/shadow/text
primitive layer in Phase 2 at all, because `nkdhr-render` (UI-1) is Phase
3. Window chrome in Phase 2 is therefore visually minimal (a flat border,
no shadow, no themed titlebar text) by design, not as an oversight — the
real chrome design is a Phase 3/4 concern (UI-4 theming, SHELL work) that
would be wasted effort to build twice if done now. `PinnedNode::render_data`
returning renderer-independent payloads for the host to adapt into a thin
`CanvasRenderElement` is the same boundary: Phase 3 does not need to expose
backend-specific input or renderer types to the node registry when it gives
`nkdhr-ui` a real primitive layer.

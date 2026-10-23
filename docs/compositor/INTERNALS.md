# Compositor — Internals

> [中文版本 / Chinese version](INTERNALS_zh_CN.md)

Audience: contributors working on `nkdhr-compositor`. Ground truth for every
Smithay API cited here is Smithay 0.7.0's own source as vendored under
`~/.cargo/registry` (its `examples/` and module documentation in
particular), not memory of older Smithay versions, which changed these APIs
substantially over time.

## Scope

This crate is nkdhr's display protocol layer: everything needed to serve
Wayland and X11 clients on real displays, plus one permanent, minimal window
behaviour so that the result is usable on its own. It contains no
interaction design: no workspaces, launcher, shell, keybinding catalog,
animation or UI rendering. A future interaction layer attaches through the
seams described in [EXTENDING.md](EXTENDING.md).

Until 2026-10-23 an interaction layer sat on top of this one, in a crate
then named `nkdhr-canvas`: the canvas model, viewport and overview (COMP-3,
COMP-4), the pinned-widget host (COMP-7), numbered workspaces, modal
placement, the typed binding language, the output-local shell and the UI
rendering adapter. It was removed whole when the interaction design was
withdrawn for a redesign. Its code and documents are preserved at the git
tag `archive/interaction-v1` (`git show archive/interaction-v1:<path>`).
The backend and protocol work of COMP-1, COMP-2, COMP-5, COMP-6 and COMP-8
carried over and is described below; [History](#history) keeps its
verification record.

## Crate layout

```
crates/nkdhr-compositor/
  Cargo.toml        features: nested (default), tty
  src/
    main.rs         backend selection
    state.rs        App: protocol state, seat, window store, focus policy, popup grabs,
                    Smithay handlers
    windows.rs      ManagedWindow, WindowStore, the initial-placement and keep-reachable rules
    outputs.rs      OutputLayout resolver and the `outputs` namespace watcher
    input.rs        the input router
    grabs.rs        MoveGrab and ResizeGrab (client-initiated move/resize)
    render.rs       render element list, per-output composition, output membership,
                    frame callbacks, refresh
    cursor.rs       cursor image state and the built-in fallback arrow
    backends/
      mod.rs        the Backend trait
      winit.rs      nested backend
      tty.rs        DRM/KMS + GBM + libinput + libseat backend
    protocols/
      mod.rs        ProtocolState: the protocol long tail and session lock
      xwayland.rs   in-process X11 window manager and selection bridge
      screencopy.rs wlr-screencopy server
  tools/
    vkms-lab.sh     two-output VKMS lab
    soak-test.sh    long-run stability collector
```

Cargo features keep deployments lean. `nested` enables Smithay's
`backend_winit`; `tty` enables `backend_gbm`, `backend_libinput`,
`backend_session_libseat`, `backend_udev`, `renderer_gl` and
`renderer_multi`, plus `smithay-drm-extras` and `thiserror`. The base
Smithay features are `desktop`, `wayland_frontend` and `xwayland`. The only
nkdhr dependency is `nkdhr-ipc`, used with `zbus` by the outputs watcher.

Everything outside `backends/` is shared by both backends and never knows
which one is running. A backend owns its event loop, renderer, Smithay
`Output` objects and presentation, and calls the shared layers at fixed
points: `input::handle` for every input event, `App::finish_client_dispatch`
right after each client dispatch, `render::refresh` once per loop
iteration, `render::output_elements` to compose each output, the
frame-callback and output-membership helpers after each output's render
pass, and `App::set_output_layout` whenever it re-resolves the layout.

## Backend selection

`main.rs` picks the backend from its single argument: `--nested` runs
winit, `--tty` runs the TTY backend, and with no argument it chooses nested
only when `WAYLAND_DISPLAY` or `DISPLAY` is set, otherwise TTY. Any other
argument is an error, and so is selecting a backend whose feature is not
compiled in. Both implement the one-method `Backend` trait;
neither is scaffolding for the other.

## `App`

`App` (`state.rs`) is the Smithay state type every handler is implemented
on. It holds protocol state and the window store, and nothing else:

- Smithay protocol state: `CompositorState`, `XdgShellState`, `ShmState`,
  `DmabufState`, `OutputManagerState` (created with xdg-output), `SeatState`
  with one `Seat` carrying a keyboard, a pointer and touch, the
  `PopupManager`, and `ProtocolState` (`protocols/mod.rs`, the protocol long
  tail and the session-lock runtime).
- `cursor` (`cursor.rs`) and the optional drag-and-drop icon.
- `touch_points`: the touch points that are down, with their last
  positions, recorded by the input router. Smithay keeps that list
  privately, and the session lock needs it to cancel every touch sequence
  in progress (see [Session lock](#session-lock)).
- `user_actions`: the latest key press, key release, button press and
  touch-down, each with its serial and the client it was delivered to,
  recorded by the input router. `xdg_popup.grab` is honoured only for one
  of these (see [Protocols](#protocols)).
- `popup_grab`: the `PopupGrab` granted most recently, kept until the
  compositor's side of its chain is released. It decides whether another
  client's menu may open, and it is what a touch outside the menu, the
  per-iteration settling of the grab and the session lock act on (see
  [Protocols](#protocols)).
- A flag marking a drag that a client other than the locker started during
  a session lock, which `App::finish_client_dispatch` cancels after the
  dispatch (see [Session lock](#session-lock)).
- `windows`: the `WindowStore`.
- `output_layout`: a copy of the backend's current `OutputLayout`, replaced
  through `App::set_output_layout`. Protocol handlers (the commit-time
  placement rule in particular) have no access to the backend, so `App`
  keeps its own copy.
- `focus_before_lock`: the keyboard focus to restore on unlock.
- The VT-switch seam: `vt_switching_enabled` plus a one-shot
  `pending_vt_switch`. Only the TTY backend calls `enable_vt_switching`; the
  input router calls `request_vt_switch`, and the backend drains
  `take_vt_switch_request` after each input event. The shared input layer
  therefore never touches libseat.

`KeyboardFocusTarget` is `Wayland(WlSurface)` or `X11(X11Surface)`; it
covers native windows, X11 windows and session-lock surfaces. Each Wayland
client carries `ClientState` (its `CompositorClientState`); the Xwayland
client carries Smithay's `XWaylandClientData`, and `client_compositor_state`
accepts either.

The renderer is never stored in `App`. Nothing in protocol handling touches
pixels: buffer import happens lazily at render time on whichever renderer
the backend passes in. Keeping `App` renderer-agnostic is what lets one
`App` serve the `GlesRenderer` of the nested backend and the multi-GPU
renderer of the TTY backend.

## Window store

`windows.rs` holds the only list of client windows.

`ManagedWindow` wraps Smithay's `desktop::Window`, which represents an xdg
toplevel and an X11 surface with one type, together with:

- a `WindowId`: a process-unique, monotonically increasing number that is
  never reused, so stale references cannot alias a newer window;
- `location: Option<Point<i32, Logical>>`, `None` while the window is
  unplaced;
- the in-progress resize state used by `ResizeGrab`.

Its accessors cover what the protocol layer needs: the root `wl_surface`
and the X11 surface, `matches_surface` (true for the root, any subsurface
and any popup, through `Window::with_surfaces`), `geometry`, `bbox` (popups
included) and `global_bbox`, `keyboard_target` and `owns_focus` for the
focus policy, `size_limits` (the client's minimum and maximum size hints)
and `request_size` (an xdg configure or an X11 configure). Nothing in the
protocol layer closes windows, so there is no close operation.

**Location convention.** A window's location is the position, in the
compositor-global logical space, of its root surface's origin: point
`(0, 0)` of the window in Smithay's terms. Rendering draws the window at
`location - output.global_location`, and hit testing passes
`point - location` to `Window::surface_under`. The window's visible frame
is `geometry()` offset by the location; for a client that draws shadows
outside its xdg window geometry, `geometry().loc` is the shadow inset. For
X11 windows `geometry().loc` is `(0, 0)`, so the location equals the X11
position.

**Stacking.** The store is a vector ordered bottom to top; the last element
is topmost. Rendering walks it top to bottom, and hit testing returns the
first match from the top. `raise` moves a window to the end.

**Operations.** Every change goes through the store: inserting an unplaced
toplevel, placing it (which raises it) and unplacing it, inserting an
already-placed X11 window, removing a window, raising it, setting its
location, looking a window up by id, by its root `wl_surface` or by its
`X11Surface`, finding the topmost focusable window (placed and not
override-redirect), iterating windows, and reaping windows whose `IsAlive`
is false. The surface lookup at a global point
returns the window, the exact surface under the point (root, subsurface or
popup, respecting input regions through `Window::surface_under` with
`WindowSurfaceType::ALL`) and that surface's global origin, which is what
pointer focus needs. Unplaced windows are found only by the surface
lookups protocol handlers use (by root `wl_surface` or by `X11Surface`);
hit testing, focus, rendering and frame callbacks skip them, and output
membership makes them leave every output.

### Window lifecycle

- **xdg toplevel.** `new_toplevel` inserts the window unplaced. The commit
  handler (`CompositorHandler::commit`) runs Smithay's
  `on_commit_buffer_handler`, `PopupManager::commit` and, for the window
  whose root surface the committed surface belongs to, `Window::on_commit`
  (which refreshes its bounding box); a synchronized subsurface is handled
  when its parent's commit applies it. It then sends the toplevel's initial configure whenever the
  toplevel has not received one yet: on its first commit, and again after
  a null-buffer unmap, which Smithay's buffer handler marks for
  reinitialization. The initial configure carries no size, so the client
  chooses its own, and no states. When an unplaced toplevel commits with a
  buffer attached, its size is known: it is placed by the initial-placement
  rule, raised and focused through the focus policy. A placed toplevel that
  commits a null buffer becomes unplaced again and is treated as unmapped
  (focus fallback); its next buffer commit places it like a new window.
  `toplevel_destroyed` removes it.
- **X11 managed window.** `map_window_request` calls `set_mapped(true)`.
  Once the X11 window has its `wl_surface` (immediately, or later through
  `XWaylandShellHandler::surface_associated`, tracked in
  `pending_x11_maps`), it is placed from its X11 geometry by the same rule,
  configured to that position, inserted on top and focused.
- **X11 override-redirect window** (menus, tooltips). It is inserted on top
  at its own X11 position and follows every `configure_notify`. It is never
  raised by a click and never focused.
- **Removal.** `unmapped_window`, `destroyed_window`, `toplevel_destroyed`
  and dead-client reaping remove the window; if it held keyboard focus, the
  focus fallback runs.

Every one of these changes can alter what lies under a pointer that has
not moved, and pointer focus otherwise follows only motion. Each is
therefore followed by `input::refresh_pointer_focus` (see
[Input routing](#input-routing)).

### The initial-placement rule

`windows::initial_location(geometry, &OutputLayout, pointer)` is the one
function that decides where a new window goes. Its inputs are the window's
geometry (size and the inset of its visible frame), the layout and the
current pointer position:

1. The target output is the one containing the pointer
   (`OutputLayout::output_at`). If the pointer is on no output, for example
   right after the output it was on was unplugged and before the next motion
   clamps it again, the first output in layout order is used. With no output
   at all, the visible frame's top-left is put at `(0, 0)`.
2. On each axis independently, if the frame is no larger than the output it
   is centred (`output_start + (output_extent - frame_extent) / 2`, integer
   division); if it is larger, the frame starts at the output's start, so the
   title bar and left edge stay on screen.
3. The returned location subtracts the geometry inset, so the visible frame
   rather than the shadow is what gets centred.

The rule never moves any other window. Its unit tests cover centring,
oversize on one axis and on both, a frame inset, the first-output fallback
and the empty layout.

### The keep-reachable rule

A window must never be stranded where no display shows it, with no way
for the user to bring it back. After every output layout change (hotplug,
a configuration change, a nested window resize),
`windows::location_to_keep_reachable(location, geometry, &OutputLayout,
pointer)` is applied to every placed, managed window: a window whose
visible frame no longer intersects any output is placed again by the
initial-placement rule (the output under the pointer, otherwise the first
output). A window that still intersects an output keeps its location, even
if most of it is now off-screen, so rearranging displays moves nothing that
is still reachable. A layout without any output moves nothing; the rule
runs again when outputs return. Override-redirect windows are left to their
clients. Relocation changes neither stacking nor focus. The unit tests
cover kept windows (fully and barely visible), stranded windows (including
one that only touches an output's far edge) and the empty layout.

## Output layout

`outputs.rs` turns the `outputs` namespace and the set of connected
connectors into one flat, deterministic layout.

`ConnectedOutput { name, physical_size }` is what a backend reports: the
DRM connector name with the mode size, or the nested window's output.
`OutputLayout::resolve(&OutputPlacements, &[ConnectedOutput])` produces
`ResolvedOutput { name, physical_size, logical_size, scale, global_location }`
for every connected connector:

1. `logical_size` is the physical size divided by the scale, rounded to
   the nearest whole logical pixel on each axis and at least 1×1. That is
   exactly how Smithay computes the size it advertises through xdg-output
   (`to_logical(scale).to_i32_round()`), which XWayland's RandR reports in
   turn, so the rectangles the compositor lays out, clamps the pointer to
   and sizes lock surfaces from are the ones clients are told about.
2. Connected connectors with an entry are placed at their configured
   `(x, y)` with their configured scale, in connector-name order. Entries
   for connectors that are not connected are ignored and reserve no space.
3. Connected connectors without an entry get scale `1.0` and are packed left
   to right in connector-name order. The first starts at the right edge of
   the configured outputs' bounding box, level with its top; each next one
   starts at the previous one's right edge. With no configured output the
   packing starts at `(0, 0)`.
4. The whole layout is translated so that its bounding box starts at
   `(0, 0)`. Relative positions are preserved; negative configured
   coordinates are therefore fine.

`nkdhrd` accepts any `i32` position and any positive scale, so every step
saturates instead of overflowing: the packing cursor, a logical size whose
quotient exceeds `i32::MAX`, the normalization and the centring of the
initial-placement rule (Smithay's own point and rectangle arithmetic
saturates the same way). An absurd configuration produces an absurd but
valid layout, never a panic.

The result is in layout order: configured outputs, then packed ones. It
answers `output(name)`, `output_at(point)` (the first output in layout order
whose half-open rectangle contains the point), the bounding size used to
map absolute input devices, and `clamp(point)`. Clamping keeps the pointer
inside the union of the output rectangles rather than their bounding box: a
point inside some output is unchanged; otherwise it is clamped into each
output rectangle and the nearest result wins. There are no dead zones
between outputs, and a motion that would leave the union slides along its
edge. A clamped point lies at most one logical pixel before an output's
far edges, which the half-open rectangle excludes. With no output at all,
points are left unchanged. Overlapping configured outputs are not
rejected; each simply shows its own rectangle of the global space. The
resolver's unit tests cover automatic packing, normalization of negative
positions, scale-dependent logical sizes and their rounding (including a
fraction below one half, as xdg-output rounds it), packing after a
configured area, saturation at the end of the coordinate range, ignored
disconnected entries, `output_at` across a gap, clamping into the union,
the empty layout and the decoding of flattened keys.

### The `outputs` watcher

`OutputsConfig::watch()` connects to the session bus and reads the namespace
once with `Config1.GetAll("outputs")`. A background thread then subscribes
to `Config1.Changed`; for any key under `outputs.` it re-reads the whole
namespace, replaces the shared snapshot and bumps an atomic generation
counter. Backends compare the generation once per loop iteration and
re-resolve when it moved. Without a session bus, without `nkdhrd`, or with
an `nkdhrd` that does not know the namespace, the snapshot is empty and
every output is packed automatically.

Keys arrive flattened as `outputs.<connector>.<field>`. The `nkdhrd` schema
forbids `.` in connector names, so the key splits unambiguously. `x` and `y`
accept any integer variant that fits `i32`; `scale` arrives as a double (a
whole number sent as an integer is accepted too). Missing fields, values of
an unexpected type and scales that are not finite and positive take the
`OutputPlacement` defaults, so a malformed value can never produce a
degenerate output. `nkdhrd` emits `Changed` only for keys that exist after
an edit, so deleting a whole entry produces no signal; the compositor picks
the deletion up with the next signalled change or at its next start.

### Applying a layout

A backend that has resolved a new layout sets every Smithay `Output`'s
current state to its mode, `Scale::Fractional(scale)` and
`global_location`, which `wl_output` and xdg-output advertise (the
`wl_output` integer scale is the fractional scale rounded up). It then
calls `App::set_output_layout`, which stores the copy, reconciles the
protocol bookkeeping keyed by output name (session-lock coverage and the
lock's keyboard focus, and pending screencopy requests; requests for a
vanished output fail), applies
[the keep-reachable rule](#the-keep-reachable-rule) and finally refreshes
pointer focus. Otherwise windows are not moved: a window's location is
global and does not follow its output.

## Focus

The focus policy lives in `state.rs`, in a few named functions the rest of
the crate calls:

- `App::focus_window(id)` raises the window and gives it keyboard focus.
  Mapping a window and clicking one both use it. Raising an X11 window also
  restacks it in the X server (`X11Wm::raise_window`), whose stacking order
  X11 clients rely on, for example to find the target of a drag between
  X11 windows. A popup grab in place is dismissed first
  (`App::dismiss_popup_grab`), as a click outside its menu would: Smithay's
  `PopupKeyboardGrab` keeps keyboard focus on the menu and silently drops
  any other focus, so a window opening while a menu is open would
  otherwise be raised above the menu's window yet stay unfocused.
- `App::focus_topmost()` is the fallback: when the window holding keyboard
  focus is removed, focus moves to the topmost focusable window, or to
  nothing.
- While the session is locked, live keyboard focus belongs to the lock
  surface. Both functions then update `focus_before_lock` instead: a window
  mapped during the lock becomes the focus restored on unlock, and if the
  remembered window goes away, the remembered focus falls back to the
  topmost remaining window. When the lock begins, what is remembered is
  the window that owns the focus, never a popup holding the keyboard
  through its grab, since the lock dismisses that popup: the popup's
  chain stands for its root window, by the same rule that returns focus
  there when a chain closes (below), even when the popup was destroyed a
  moment before the lock and its chain not yet settled.
- `App::keep_keyboard_on_lock_surface()` keeps the live focus on a lock
  surface while locked (see [Session lock](#session-lock)).
- A granted popup grab gives the popup keyboard focus, since xdg-shell
  gives it to the topmost grabbing popup, also when its client did not
  hold the focus (a menu opened by touch). It also raises the popup's root
  window: popups are drawn and hit-tested at their window's stacking
  level, so a menu of a window that is not on top would otherwise be
  partly hidden under other windows, and touching the hidden part would
  dismiss it. While the chain is open, focus
  stays on its topmost grabbing popup; when a nested popup closes, it
  moves to the parent popup.
- When a popup-grab chain that held keyboard focus closes, whether its
  client destroyed the popups (an item chosen, Escape), a click or touch
  outside dismissed them or the root window went away, focus returns to
  the chain's root window, not to the topmost window, and the window is
  not raised again (the grant raised it). Smithay's popup grabs do this
  themselves when the chain ends through pointer input or a key.
  Otherwise `App::settle_popup_grab` or `App::dismiss_popup_grab` does it
  through `App::focus_popup_root`, which runs the focus fallback instead
  when the root window is gone or unmapped. Settling runs on the next
  loop iteration, before dead-client reaping could apply the fallback to
  the focus the destroyed popup held (see [Protocols](#protocols)).

`focus_window` raises, which can change what lies under the pointer; its
callers refresh pointer focus once their whole change is done, so a
single motion describes the outcome.

Override-redirect windows and unplaced toplevels are never focus targets.
Live focus is set through `App::set_keyboard_focus`, which goes through the
seat's `KeyboardHandle`; `SeatHandler::focus_changed` then keeps protocol
state consistent with the result: exactly the focused window carries xdg
`Activated` (or X11 activation), a configure is sent only to toplevels
whose state actually changed (and never before their initial configure),
and clipboard and primary-selection focus follow the client of the focused
surface, cleared while locked. Smithay 0.7 calls `focus_changed` only for
a new focus, not when focus is cleared, so `set_keyboard_focus` applies the
same bookkeeping itself when it clears focus; otherwise the last focused
window would stay activated and keep the clipboard focus.

## Input routing

`input::handle(app, &OutputLayout, event)` is the single entry point for
input. The TTY backend passes every libinput event except device
add/remove; the nested backend passes winit's input events. The router
consults nothing but the seat, the window store, the layout, the lock
state and the open popup-grab chain (which a touch outside it dismisses),
records nothing but which touch points are down and the latest user
action of each kind (`App::user_actions`), and has exactly one compositor
shortcut: VT switching.

1. **Locked session.** If a lock is in force, the event goes to the
   fail-closed lock path (below) and nothing else runs.
2. **Keyboard.** Every key passes through Smithay's `KeyboardHandle::input`
   filter. When `vt_switching_enabled`, a VT key is intercepted on press
   and on release and the press records `request_vt_switch(n)`; everything
   else is forwarded to the focused surface. Each key event is recorded in
   `App::user_actions` as the latest key press or release, with the client
   of the surface that received it, or none when it was intercepted.
3. **Pointer motion.** A relative event targets `current + delta`, an
   absolute event maps onto the layout's bounding size; either target is
   clamped into the union of outputs. The surface under the target becomes
   pointer focus. Active pointer constraints are honoured as described under
   [Protocols](#protocols). Relative events also produce
   `relative_motion` (with the unaccelerated delta) for
   `zwp_relative_pointer_v1`. Each event ends with `pointer.frame`.
4. **Buttons.** On a press, when the pointer has no active grab, the router
   looks up the window under the pointer; if it is a placed,
   non-override-redirect window, `focus_window` focuses and raises it. A
   press over no window leaves focus alone. Pointer focus is then
   recomputed where the pointer is (below) and the button is forwarded
   unchanged, so the press reaches exactly the surface the focus rule
   hit-tested even when the scene changed under a pointer that has not
   moved since (a client resized, a popup appeared). Skipping the focus
   step during a grab means a click that dismisses a popup (through
   Smithay's `PopupPointerGrab`) or ends a move does not also switch
   windows. A menu that closed otherwise (an item chosen by touch or
   key, Escape) has its `PopupPointerGrab` released by the next loop
   iteration (see [Protocols](#protocols)), so the next click focuses as
   usual. Each press is recorded in `App::user_actions` as the latest
   button press, with the client of the surface that received it: the
   pointer focus once Smithay has delivered it, none during a move or
   resize.
5. **Axis.** Every axis event is forwarded, including zero-delta stop
   frames, which kinetic scrolling in clients depends on. The frame carries
   the source, the relative direction, the continuous value, the v120
   discrete value and, for finger sources, `stop` on a zero value. A device
   that reports only discrete steps (a mouse wheel through the nested
   backend) gets a continuous value derived from them, 15 logical pixels
   per notch as libinput reports for wheels: Smithay sends `wl_pointer.axis`
   only for a non-zero value, and `axis_value120` without it is not a valid
   scroll for clients.
6. **Gestures.** Swipe, pinch and hold events are forwarded to the seat
   unchanged and reach clients through the `zwp_pointer_gestures_v1`
   global; touchpad scrolling is axis input and unaffected.
7. **Touch.** Positions map onto the layout's bounding size. Down and
   motion events are forwarded with the surface under the touch point, and
   up, frame and cancel events are forwarded as they are. Smithay keeps
   each touch sequence on the surface it began on: motion goes to the
   surface the point went down on, and while one finger is down further
   fingers go to the first finger's surface. The router records which
   points are down and where (`App::touch_points`), and each touch-down in
   `App::user_actions` as the latest touch-down, with the client of the
   surface that received it (the start focus of Smithay's `TouchDownGrab`;
   none when a touch drag-and-drop swallowed it). Touch never focuses or
   raises a window. A touch-down that starts a new touch sequence outside
   the client of the open popup-grab chain, on another client's surface or
   on no surface, first dismisses the chain through
   `App::dismiss_popup_grab`, as Smithay's `PopupPointerGrab` does for a
   click outside, and is then delivered as usual; Smithay has no touch
   counterpart of that grab. Further fingers of a sequence go to the first
   finger's surface whatever is under them, so they never dismiss.

Absolute devices (touchscreens, tablets) map across the bounding box of the
whole layout, not onto a single output.

**Refreshing pointer focus.** Smithay changes pointer focus only on
motion, so `input::refresh_pointer_focus(app)` re-runs the motion step at
the pointer's current location, with no relative motion and a synthetic
timestamp: a zero-distance motion towards the surface now under the
pointer, with the same constraint handling as real motion. It runs after
every change to what lies under a stationary pointer: placing an xdg
toplevel or mapping an X11 window (managed or override-redirect), moving an
override-redirect window, unplacing or removing a window, reaping dead
windows, adopting a new output layout (which covers keep-reachable
relocations), releasing a popup grab and unlocking, plus before a press is
forwarded. It does
nothing while the session is locked, where the lock path owns pointer
focus, or while a pointer grab is active, which decides focus itself. It
must never run inside a pointer grab's own callbacks (`MoveGrab` and
`ResizeGrab` move windows from there), because Smithay holds the pointer
while it calls them; `App::move_window` therefore leaves pointer focus
alone.

**The locked path** routes to lock surfaces only. Keyboard input still goes
through the VT-switch intercept, then to the focused surface, which can
only be a lock surface. Pointer motion is clamped exactly like the unlocked
path; pointer focus is the lock surface of the output under the pointer,
positioned at that output's global location. A button press moves keyboard
focus to that lock surface, if it is alive, before forwarding. Axis, gesture and touch
events target lock surfaces in the same way; touch sequences that began
before the lock were cancelled when it began, so none of them still
reaches a window. The window store is never consulted.

**VT keys.** XKB may resolve `Ctrl+Alt+Fn` directly to an `XF86Switch_VT_n`
keysym. The intercept therefore accepts that keysym from the modified
symbol, and separately accepts `F1`…`F12` from the raw, unmodified level-zero
symbols while Ctrl and Alt are held. Relying on `modified_sym()` alone made
the first physical test silently miss the binding.

## Grabs

`grabs.rs` implements client-initiated interactive move and resize as
Smithay `PointerGrab`s, in the style of Smithay's own examples. Requests
are accepted only for placed windows, never while locked, and only when
the pointer's active grab is Smithay's implicit `ClickGrab`, the grab of a
button held since a press, whose start focus belongs to the window (its
root surface, a subsurface or a popup):

- `xdg_toplevel.move`/`resize`: the seat must be the compositor's seat and
  `PointerHandle::has_grab(serial)` must hold for the request's serial.
- X11 `_NET_WM_MOVERESIZE` (`XwmHandler::move_request`/`resize_request`):
  the X11 message names no serial, so the new grab takes over the serial of
  the pointer's current grab.

Only the implicit grab carries the press a move or resize continues. A
request repeated while a `MoveGrab` or `ResizeGrab` already runs shares
that grab's serial, but its start data would pair the already-moved window
with the original press location and apply the offset twice; Smithay's
`PopupPointerGrab` reports the global origin as its start location. Both
are refused, and so is a request started by touch, which has no pointer
grab at all. The grab is installed with `Focus::Clear`, so no surface has
pointer focus while it runs, and its start data is the click grab's.

- **`MoveGrab`** sets the window's location to its initial location plus
  the pointer's offset from the grab's start, rounded to logical pixels. X11
  windows are reconfigured at the new position so the X server's idea of
  their position stays in sync (see [XWayland](#xwayland)).
- **`ResizeGrab`** computes a new size from the pointer offset for the
  requested edges, clamped to the client's minimum and maximum size hints
  and to at least 1×1. An xdg toplevel receives a configure with that size
  and the `Resizing` state. When the grab ends, for whatever reason, it
  receives a final configure without `Resizing`. While resizing, and until
  the client commits its response to that final configure (tracked through
  `XdgShellHandler::ack_configure`), the commit handler moves the window so
  that the edge opposite a top or left resize stays where it was when the
  grab began: `location.x = initial.x + (initial_width - current_width)`,
  and the same for `y`, where the widths are those of the window geometry.
  An X11 window is configured with the new size and the adjusted position
  together, since X11 geometry is set by the window manager. Moving the
  window meanwhile (the keep-reachable rule relocating it after an output
  change, for example) cancels the correction for good: X11 windows keep
  their moved position from then on, and the end of the grab arms the
  correction for the final configure only if the grab's own resize is
  still in progress (`ResizeState::grab_ended`), so the window is never put
  back relative to where the resize began.

Both grabs end when the last button is released, and then give pointer
focus to the surface under the pointer at that moment. Smithay's own focus
restore would replay the focus computed for the last motion before the
grab moved or resized the window, which can be the surface that was behind
the window's old position. Every other `PointerGrab` method passes the
event through. A grab whose window has died ends itself on its next event,
and locking the session ends any grab (see [Session lock](#session-lock)).
Grab arithmetic saturates, like the layout's.

## Rendering

`render.rs` defines the element type with Smithay's `render_elements!`
macro:

```rust
OutputRenderElement<R>:
    Surface = WaylandSurfaceRenderElement<R>,        // client surfaces, cursor surfaces, DnD icon
    Memory  = MemoryRenderBufferRenderElement<R>,    // the built-in fallback cursor
```

There is nothing else to draw: the background is the frame's clear colour,
a neutral dark constant, and black while locked.

`render::output_elements(renderer, app, &ResolvedOutput, include_cursor)`
returns one output's elements front to back:

1. the cursor, unless a pending screencopy without cursor overlay excludes
   it: the client's cursor surface at `pointer - hotspot`, or the built-in
   24×24 arrow for named cursors and the background;
2. when unlocked, the drag-and-drop icon at the pointer plus its offset;
3. when locked, the output's lock surface at the output origin, if the
   locker has provided one (otherwise only black);
4. when unlocked, every placed window whose bounding box (popups
   included) overlaps the output, top to bottom, through
   `Window::render_elements` at `(location - output.global_location)`
   converted to physical pixels with the output scale. Popups are part of
   their parent's elements.

Both backends draw this same list; only presentation differs.

**Output membership and fractional scale.** After composing an output, the
backend calls `render::update_output_membership`. Each placed window whose
bounding box (popups included) overlaps the output's rectangle enters it,
with its subsurfaces and popups each judged by their own position
(Smithay's `output_update` sends `wl_surface.enter` and `leave`); every
other window, unplaced ones included, leaves it. A window's preferred
fractional scale is the highest scale among the outputs its bounding box
overlaps; a window that overlaps none keeps its last preference. While the
session is locked, the output's lock surface enters that output and
prefers its scale. A surface that asks for its preferred scale before it
is shown anywhere, typically a new window before its first buffer, starts
with the scale of the output a new window is placed on (the one under the
pointer, otherwise the first output), so its first frame is drawn at the
right resolution (`FractionalScaleHandler::new_fractional_scale`).

**Frame callbacks.** After each output render pass,
`render::send_frame_callbacks` gives every placed window overlapping that
output its frame callbacks, popups and subsurfaces included
(`Window::send_frame`, with that output as the surfaces' primary output),
and the DnD icon its own; while locked, only the output's lock surface
receives them, matching what is drawn. The cursor surface receives its
callbacks on every pass.

**`render::refresh(app)`** runs once per loop iteration before rendering
and does only popup bookkeeping and dead-client reaping, in this order:
`PopupManager::cleanup`, which notes the popups clients destroyed and can
end a popup-grab chain; `App::settle_popup_grab`, which releases a chain
that is over and returns the keyboard focus it held to the chain's root
window (see [Protocols](#protocols)); and reaping, whose focus fallback
therefore never takes focus away from the window a closed menu belonged
to. Reaping drops windows whose `IsAlive` is false, dead idle-inhibitor
surfaces, a dead DnD icon and a dead remembered lock focus, runs the focus
fallback when keyboard focus died, keeps the live keyboard focus on a live
lock surface while locked, and refreshes pointer focus when it reclaimed a
window. A graceful
`xdg_toplevel.destroy` or XWM unmap removes its window immediately; this
pass exists for abrupt process death, where a SIGKILL stress run showed
that protocol destroy callbacks alone left dead windows behind.

## Backends

### Nested (`backends/winit.rs`)

The nested backend runs the compositor as a 1280×800 client window titled
`nkdhr-compositor` inside whatever X11 or Wayland desktop the developer is
using, through `smithay::backend::winit::init_from_attributes` with a
`GlesRenderer`. The host cursor is hidden over the window.

It owns one `Output` named `nkdhr-compositor-nested-0` (make `nkdhr`,
model `nested`) whose mode is the window size at 60 Hz. A window resize or
a new outputs generation re-resolves the layout. The dmabuf global is
created with `DmabufState::create_global` (protocol version 3) from the
renderer's format list. The listening socket comes from
`ListeningSocket::bind_auto("wayland", 0..32)` and is accepted once per loop
iteration. XWayland runs on its own calloop loop, dispatched with a zero
timeout every iteration.

Each iteration dispatches winit events (input goes to `input::handle`),
re-resolves the layout if needed, calls `render::refresh`, renders the
whole window, services screencopy, submits, updates output membership,
sends frame callbacks, accepts a pending client, dispatches XWayland and
the Wayland clients, calls `App::finish_client_dispatch`, and flushes. The
nested output's `wl_output` global lives as long as the process.
There is no damage tracking or frame pacing of its own: every iteration
redraws the full window and swaps as fast as the host accepts, which shows
as steady CPU use. That is acceptable for a development backend. Every five
seconds it logs the frame rate and window count.

Details that are not obvious from Smithay's examples:

- **`WinitEvent::CloseRequested` must end the loop explicitly.** Smithay's
  `examples/minimal.rs` ignores it, which would leave the process running
  after its window closes. `PumpStatus::Exit` is a separate signal (winit's
  own loop terminating) and does not fire just because the window closed.
- **Resize needs no handling of its own.** `WinitGraphicsBackend::bind()`
  re-queries the window size and resizes its `EGLSurface` every frame.
- **`Transform::Flipped180`** matches GL's bottom-left framebuffer origin
  against Smithay's top-left logical convention, as `minimal.rs` does.
- **Screencopy readback rebinding.** `GlesRenderer::map_texture` leaves the
  EGL context current without the window surface. A no-op render pass
  rebinds it before `submit`; without it the first real `grim` capture made
  EGL report `BadSurface`, and its attempted recovery failed with
  `BadAlloc` and took the compositor down.
- Under a headless, software-rendering host, Mesa falls back to llvmpipe
  for the nested EGL context (`failed to get driver name for fd -1`). That
  is expected for the nested backend; real GPU acceleration is exercised by
  the TTY backend.

### TTY (`backends/tty.rs`)

The TTY backend is calloop-based. `LibSeatSession` owns device access and
pause/resume; `UdevBackend` enumerates and hotplugs DRM devices;
`DrmScanner` maps connectors to CRTCs; `DrmOutputManager` owns atomic KMS
surfaces backed by GBM. No setuid helper or nkdhr-owned privileged code is
involved. The session is opened before anything else; XWayland is started
before the DRM devices are initialized; the dmabuf (version 3) and SHM
format lists come from the primary GPU's renderer.

**GPUs.** The primary render GPU is `NKDHR_DRM_DEVICE` if set, otherwise
the render node of the seat's primary GPU, otherwise the first render node
on the seat. It is opened directly and registered with
`GpuManager<GbmGlesBackend<…>>` at high context priority. Render-node
descriptors are kept in a distinct `RenderDeviceFd` type with no DRM-control
traits, because constructing a `DrmDeviceFd` attempts to acquire DRM master;
render nodes can never take over a display. KMS primary nodes are opened
through libseat, separately, and only for devices eligible for scanout.
Rendering happens on the primary GPU and is copied across GPUs when a
scanout device has its own render node; a scanout-only device (VKMS, for
example) borrows the primary render node's allocator and restricts formats
to linear modifiers.

**Development safety boundary.** `NKDHR_DRM_DEVICE` must name a render node
and `NKDHR_DRM_SCANOUT_DEVICE` a primary node; anything else is refused at
startup. With the scanout override set, no other primary DRM node is opened
at all, at startup or on hotplug, not merely excluded from connector
scanning. This lets a VKMS run render on the real GPU without acquiring DRM
master on, or modesetting, the developer's real panel. Production discovers
the render GPU and opens the seat's KMS devices normally. A compositor
normally runs in a local seat session; `LIBSEAT_BACKEND=noop` is acceptable
for isolated VKMS diagnostics only.

**Outputs.** A connected connector becomes an `Output` named
`<interface>-<interface id>` (`eDP-1`, `HDMI-A-1`, `Virtual-1`), driven at
its preferred mode (or its first listed mode), with its own `wl_output`
global. The global is created only once the output's KMS surface is
initialized, so a connector that cannot be driven is never advertised.
When the connector or its whole device goes away, dropping the output's
`SurfaceData` retires the global: it is disabled at once, which announces
its removal and keeps it bindable, and removed ten seconds later by a
calloop timer. Removing it at once would free it while a client's bind may
already be on its way (the client saw it announced a moment earlier, or is
in its first registry round trip), and wayland-server kills such a client
with a protocol error; wayland-server recommends exactly this disable,
then remove order. The scanout
formats prefer 8-bit ABGR/ARGB and keep 10-bit formats only as fallbacks:
choosing 10-bit before nkdhr has an HDR/colour-management policy adds
output-side dithering and driver-specific behaviour for no benefit. Any
connector change re-resolves the layout.

**Event loop.** Sources are the Wayland socket, the display (a `Generic`
source dispatching clients, then calling `App::finish_client_dispatch`),
udev, libinput, the session notifier, one DRM notifier per device for
vblank, and XWayland. Each iteration dispatches
with a 250 ms bound, re-resolves the layout if the outputs generation
moved, calls `render::refresh`, renders every output and flushes clients.
Client commits, input, vblank and hotplug wake the loop immediately; the
bound exists only so a configuration change published by the watcher
thread is applied within a quarter second on an otherwise idle session.

**Presentation and damage.** `DrmOutput::render_frame` supplies Smithay's
per-output damage tracking, so an unchanged output produces an empty frame
that is not queued, and idle GPU use stays near zero. Each output allows at
most one rendered frame in flight: while a frame awaits vblank, further
input and client changes accumulate in compositor state instead of being
rendered into intermediate buffers, and the first pass after
`frame_submitted` presents the latest state. This keeps pointer motion
synchronized with presentation instead of queuing stale positions under
high-rate input. Ordinary frames allow only cursor-plane scanout
(`FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT`), never primary or overlay direct
scanout, which removes software-cursor trails while keeping the composed
primary plane readable for screencopy.

**Hotplug.** A removed connector's KMS surface is cleared explicitly (DPMS
off, every plane disabled) before its `DrmOutput` is dropped, and the
device's remaining outputs then try to restore explicit buffer modifiers.
Reconnecting therefore starts from a clean CRTC rather than a stale
framebuffer or plane assignment. Removing a whole device unregisters its
DRM notifier and, for a secondary GPU, its render node. Clients and windows
survive every hotplug, including a client that binds a `wl_output` just
as its connector disappears.

**VT switching and handoff.** After each input event the backend drains
`take_vt_switch_request`; a request for the VT the compositor already
occupies (read from `/proc/self/fd/0`) does nothing. Otherwise, before
asking libseat to switch, the backend stops rendering, performs one
device-wide atomic reset that disables every connector and plane, pauses
every DRM output manager and releases master, and only then calls
`LibSeatSession::change_vt`. Requesting the switch only after that sequence
avoids a race in which logind revokes device access before the reset runs.
If the reset or the switch fails, the backend reactivates and rescans the
devices immediately, and stops if even that fails. The asynchronous pause
event repeats the pause idempotently and suspends libinput. Activation
resumes libinput, reactivates DRM with connectors and planes reset, calls
`DrmOutput::reset_buffers` on every output, clears the in-flight markers and
rescans connectors, without touching clients, windows or focus. The buffer
reset matters: fresh swapchain slots have buffer age zero and force a full
repaint, whereas keeping pre-pause ages made the first physical VT-resume
test repaint only a damaged block until the next input event.

## Protocols

`ProtocolState` (`protocols/mod.rs`) owns the globals and the policy shared
by both backends. Registration alone is rarely enough: selection follows
keyboard focus, constraints alter motion delivery, lock surfaces replace the
scene and screencopy reads back the composition.

- **Core.** `wl_compositor`/`wl_subcompositor`, `wl_shm`,
  `zwp_linux_dmabuf_v1` (version 3), `xdg_wm_base`, one `wl_seat`,
  `wl_output` per output and `zxdg_output_manager_v1`. dmabuf buffers are
  accepted without eager import (`notifier.successful`); real import
  happens the first time a render element uses the buffer, and an
  incompatible buffer surfaces as a render-time import error.
- **Popups.** `PopupManager` tracks, renders, hit-tests and grabs xdg
  popups. `xdg_popup.grab` is honoured only on the compositor's seat and
  never while the session is locked (a popup is not a lock surface). Its
  serial must be that of the latest key press, key release, button press
  or touch-down, and that action must have been delivered to the popup's
  client (`App::user_actions`, recorded by the input router). The action
  may be over by then, and the press keeps counting after its release, as
  in Mutter: GTK opens a menu button's popover when the button is released
  or the finger lifted and names the press or touch-down, a slow client's
  grab can arrive after the release even for a menu opened on the press,
  and a menu opened from the keyboard names the latest key press or
  release. Smithay's `has_grab(serial)` only matches the grab active now,
  which the release has already ended, so it is not consulted, and neither
  is keyboard focus, which touch never moves. Each kind keeps only its
  latest action, and the record is cleared when a session lock begins, so
  a stale serial cannot open a menu. The grab must also be free to start
  (`may_start_popup_grab` in `state.rs`). No other client's popup-grab
  chain may be open: Smithay keeps one chain per seat and answers a grab
  whose parent is not the chain's topmost popup with a protocol error
  (`not_the_topmost_popup`) that would disconnect the client asking. And
  the grab must be free to take the pointer: the pointer has no grab, or
  its grab is a menu's `PopupPointerGrab` (the client's own enclosing
  menu, or a chain that is over and not yet released), or Smithay's
  implicit `ClickGrab` that started on no surface, a dead one or one of
  the popup's client. Taking over the implicit grab of the press that
  opens a menu, or an enclosing menu's grab, is how menus open, nest and
  replace one another. A move, a resize or a drag keeps the pointer, and
  so does another client's held button: one application's menu must not
  take a held button away from another. A granted grab installs
  `PopupPointerGrab` and `PopupKeyboardGrab`, moves keyboard focus to the
  popup, raises the popup's root window (see [Focus](#focus)) and is kept
  in `App::popup_grab`. A grab
  that is not granted, for any of these reasons or because Smithay's
  `grab_popup` refuses it, dismisses the popup at once (through its popup
  tree, which also stops it from being drawn), as xdg-shell requires;
  otherwise the client would show a menu that nothing closes.

  A chain is open while one of its popups still holds the grab, and over
  once every grabbing popup has been destroyed or dismissed, or its root
  is gone. That is sooner than Smithay's `PopupGrab::has_ended`, which
  also waits for the client to destroy the popups that were dismissed:
  after a click outside one menu, the next menu often opens before the
  first client has destroyed its popups, and Smithay already accepts it
  then. A chain closes in one of these ways:

  - Its client destroys the popups (an item chosen, Escape). Smithay's
    `PopupPointerGrab` and `PopupKeyboardGrab` notice that only with the
    next pointer event or key, and until then keep pointer focus on that
    client and keep the next click from focusing a window, which on a
    touchscreen or with the keyboard alone can last indefinitely.
    `App::settle_popup_grab` therefore runs on every loop iteration, from
    `render::refresh` after `PopupManager::cleanup`. Once the chain is
    over, it ends the keyboard grab, then the pointer grab with pointer
    focus restored, returns keyboard focus to the chain's root window (see
    [Focus](#focus)) and drops `App::popup_grab`. While the chain is open,
    it keeps keyboard focus on the topmost grabbing popup, which Smithay's
    keyboard grab would only update at the next key after a nested popup
    closed.
  - A click outside the chain's client. Smithay's `PopupPointerGrab`
    dismisses every grabbing popup (`PopupGrab::ungrab` with
    `PopupUngrabStrategy::All`), ends itself and the keyboard grab, returns
    keyboard focus to the root and delivers the press. The click does not
    also focus the window it lands on (see [Input routing](#input-routing)).
  - A touch outside the chain's client. The input router does the same
    through `App::dismiss_popup_grab`, then delivers the touch.
  - A session lock, which dismisses the chain the same way (see
    [Session lock](#session-lock)).

  Dismissing through `PopupGrab::ungrab`, rather than by sending
  `popup_done` alone, also makes Smithay's per-seat chain forget the
  popups, so a client that never destroys them (one that is hung or
  stopped) cannot leave a topmost popup behind that would turn the next
  client's menu into a protocol error. A popup with a dead parent is
  dismissed. `reposition` requests apply the new positioner. Popups are
  placed exactly where their positioner says; the compositor does not
  slide or flip them to keep them on an output.
- **Window state requests.** `xdg_wm_base` is created with an empty
  window-management capability set, so `xdg_toplevel.wm_capabilities`
  tells clients that no maximize, fullscreen, minimize or window menu is
  available, and clients that honour it hide those controls. Smithay's
  default handlers answer the requests anyway: maximize and fullscreen
  requests receive a configure that leaves the state unchanged;
  unmaximize, unfullscreen, minimize and window-menu requests are ignored.
  The same requests from X11 clients are ignored.
- **Clipboard and primary selection** (`wl_data_device`,
  `zwp_primary_selection_v1`). Selection focus follows keyboard focus.
  Selection bytes travel directly between clients through the protocol
  pipe; the compositor only brokers metadata and file descriptors.
- **Drag-and-drop.** Shares the data-device state. The DnD target is the
  topmost surface under the pointer; the optional icon is drawn at the
  pointer, offset by the cursor hotspot recorded at drag start, and is
  cleared on drop. During a session lock only the locker may drag (see
  [Session lock](#session-lock)).
- **Decorations** (`zxdg_decoration_manager_v1`). Every `new_decoration`,
  `request_mode` and `unset_mode` is answered with `ClientSide`: before the
  toplevel's initial configure the mode rides on that configure, afterwards
  a configure is sent right away. The compositor draws no decorations of
  any kind. Clients that never bind the protocol draw their own, as
  xdg-shell requires.
- **Pointer constraints** (`zwp_pointer_constraints_v1`). A constraint is
  activated only while its surface holds pointer focus, is the surface
  under the pointer, and the pointer is inside the constraint's region (a
  constraint without a region covers the whole surface):
  `input::activate_pointer_constraint` checks this after every motion has
  updated pointer focus, and once when the constraint is created. Checking
  the actual pointer focus keeps constraints inactive on surfaces a grab
  drags the pointer across: a compositor move or resize clears pointer
  focus, and Smithay's implicit click grab keeps it on the pressed
  surface. Checking the region keeps a confinement from starting outside
  it, where every motion would be refused and the cursor would freeze. A
  locked pointer receives relative motion while the compositor cursor
  stays put; Smithay retains the cursor position hint. A confined pointer
  refuses motion that would leave the surface or region. A constraint is
  deactivated when pointer focus moves to another surface, or when its
  surface is no longer under the pointer (it moved, unmapped or was
  covered). The locked path never activates a constraint.
- **Relative pointer** (`zwp_relative_pointer_manager_v1`): fed from
  relative motion, including while a pointer lock is active.
- **Pointer gestures** (`zwp_pointer_gestures_v1`): touchpad swipe, pinch
  and hold gestures, forwarded by the input router, reach the client with
  pointer focus.
- **Idle inhibit** (`zwp_idle_inhibit_manager_v1`). Inhibitor surfaces are
  tracked once per inhibitor (`IdleInhibitors`): the protocol allows
  several inhibitors on one surface, and Smithay reports each creation and
  destruction separately, so a surface keeps inhibiting until its last
  inhibitor is destroyed. `App::idle_inhibited()` is true when any live
  inhibitor surface belongs to a placed window that overlaps a connected
  output. It is the
  single query the session's idle and display-power policy will consume;
  that consumer does not exist yet, so the method carries a documented
  `dead_code` allowance.
- **Fractional scale** (`wp_fractional_scale_manager_v1`): the preferred
  scale is maintained by the output-membership pass described under
  [Rendering](#rendering), which also gives a new surface its initial
  preference.
- **Cursor.** Client cursor surfaces are drawn with their hotspot; named
  cursors and the background use the built-in 24×24 arrow, scaled to the
  output so it keeps its logical size.
- **XWayland shell** (`xwayland_shell_v1`): associates X11 windows with
  their `wl_surface`; see [XWayland](#xwayland).
- **Session lock** (`ext_session_lock_manager_v1`) and **screencopy**
  (`zwlr_screencopy_manager_v1`) have their own sections.

## XWayland

X11 support is **in-process**: Smithay's `XWayland` spawns the Xwayland
server and the compositor itself is the X11 window manager through
`X11Wm`/`XwmHandler`. An external proxy in the style of
`xwayland-satellite` would add crash containment and lazy startup, but also
a second process and IPC surface; for a compositor audited as one process,
the in-process model is the simpler trust boundary, and it is the model
Smithay supports first-class.

- **Startup.** XWayland is spawned at startup on both backends with an
  automatically chosen display number and Smithay's scrubbed environment,
  which deliberately keeps `LD_LIBRARY_PATH` so non-system runtimes (a Nix
  profile, an unpacked diagnostic package) still work. When the server
  reports `Ready`, `X11Wm::start_wm` runs and the compositor logs
  `XWayland ready on DISPLAY=:<n>`. A missing `Xwayland` executable is
  logged and otherwise ignored.
- **Coordinates.** The X11 root coordinate space is the compositor-global
  logical space, one to one. Managed windows are therefore configured with
  their store location whenever it changes (placement, move, resize), so
  the X server's idea of each window's position is always the compositor's;
  X11 clients position their override-redirect menus from it.
- **Configure requests.** For a window that is not mapped yet, the request
  is applied as asked (the map-time placement then decides the position).
  For a mapped managed window, the requested size is applied and the
  position stays the compositor's. Override-redirect windows configure
  themselves; `configure_notify` updates their location.
- **Stacking.** Raising a managed window in the store also raises it in the
  X server (`X11Wm::raise_window`), so the X11 stacking order matches what
  is shown.
- **Selections.** The clipboard and primary selection are bridged in both
  directions. Smithay 0.7 queues a new X11 selection-owner request without
  flushing it, so after publishing a Wayland selection to X11 the
  compositor issues a harmless RANDR request
  (`get_randr_primary_output`) as a flush barrier; otherwise an X11 client
  can read a stale selection. X11 clients cannot access selections while
  the session is locked.

## Session lock

`ext-session-lock-v1` is fail-closed from the first request:

1. On `lock`, the session is protected immediately. The current keyboard
   focus is saved in `focus_before_lock` first, as the window that owns
   it; while a popup holds the keyboard through its grab, that is the
   popup chain's root window (see [Focus](#focus)). The popup-grab chain
   in `App::popup_grab` is then dismissed through
   `PopupGrab::ungrab(PopupUngrabStrategy::All)`, as a click outside it
   would be, and the root window's other popups, which hold no grab, with
   it: xdg-shell requires the popups to be dismissed when a popup grab
   ends, otherwise a menu would stay open with nothing left to close it,
   and `ungrab` makes Smithay's per-seat chain forget them, so a client
   that is hung and never destroys them cannot make the next menu opened
   after the unlock a protocol error (see [Protocols](#protocols)). The
   keyboard grab ends next, before anything can move keyboard focus:
   Smithay's `PopupKeyboardGrab` ignores focus changes while it is active,
   so an open popup would otherwise keep receiving keys during the lock,
   and a `PopupPointerGrab` that ends while it is active ends it too and
   hands keyboard focus to the menu's window for a moment. Every surface
   then loses pointer focus, so a drag-and-drop in progress leaves its
   target and is cancelled rather than dropped, and the compositor ends
   any active pointer grab (move, resize, popup, drag-and-drop), cancels
   every touch sequence in progress, clears the DnD icon, clears keyboard
   focus and sends a pointer motion with no focus. An interrupted resize
   sends its final configure as usual. Last, the record of user actions
   (`App::user_actions`) is cleared, so no serial from before the lock can
   take a popup grab once it ends.

   Touch needs its own step because Smithay keeps a touch sequence on the
   surface it began on: a finger held on a window would keep sending that
   window its motion, and while it is down Smithay sends every new finger
   there too. `input::cancel_touch_sequences` first moves a touch
   drag-and-drop off its target (so it is cancelled, not dropped), ends
   the touch grab and sends `wl_touch.cancel` to every surface with touch
   points; those points reach nothing until they are lifted. Smithay's
   cancel only reaches points with an event newer than the last frame, so
   each recorded point first receives a motion to where it already is.
2. From then on, input takes the locked path of the router, no new popup
   grab, move or resize starts, rendering draws only black and the lock
   surfaces, frame callbacks go only to lock surfaces, screencopy exposes
   only that composition and X11 selection access is refused. Windows
   mapped meanwhile are placed but receive no input and are not drawn.
3. Lock surfaces are accepted only from the client that holds the lock in
   force. Smithay hands over `get_lock_surface` from any lock object,
   including a refused one, and checks the output only against that
   client's own `wl_output` bindings, so a surface from another client
   would otherwise replace the locker's on screen and receive the user's
   typing. A surface from any other client, or for an output that is not
   in the layout, is ignored: never configured, drawn or focused. An
   accepted lock surface is fitted to its output: it is told the output's
   scale as its preferred fractional scale, then configured to the
   output's logical size from the layout (the size xdg-output advertises),
   so the frame it draws for the configure already matches the output.
   After every output layout change, every lock surface is fitted again
   (`ProtocolState::fit_lock_surfaces`): a scale or position change applied
   from the `outputs` namespace, or a resized nested window, changes an
   output's logical size while the session stays locked, and a surface
   left at the old size would leave part of the output black or put part
   of the locker off-screen. Smithay sends the scale only when it changed
   and the configure only when the size differs from the last one sent or
   acknowledged. After each render a lock surface enters its output.

   Keyboard focus stays on a live lock surface
   (`App::keep_keyboard_on_lock_surface`): whenever the focus is dead,
   missing or not one of the current lock surfaces, it moves to the lock
   surface of the output under the pointer, otherwise to that of the first
   output in layout order that has one. This runs for every new lock
   surface, after dead-client reaping and after every output layout
   change. Smithay keeps a destroyed surface as the keyboard focus until
   the focus is set again, so without it a locker that replaces the lock
   surface holding focus (its output slept or was unplugged) could not be
   typed into until a click. A click moves keyboard focus to the live lock
   surface under the pointer.

   Smithay starts a drag-and-drop for any client that names the serial of
   the pointer's or touch's current grab, which during the lock is the
   locker's own press, and it offers no way to refuse one. A drag started
   during the lock by any client but the locker (identified by its data
   source or icon; a drag with neither counts as foreign) is therefore
   cancelled by `App::finish_client_dispatch` right after the dispatch
   that started it, which ends the grab Smithay installed; its source
   receives `cancelled`, its icon is never shown, and pointer focus returns
   to the lock surface.
4. The compositor sends `locked` only after every connected output has
   presented one protected frame: on the TTY, when such a frame completes
   vblank; nested, after the protected frame is submitted. Outputs that
   disconnect meanwhile stop counting.
5. Only `unlock_and_destroy` on the confirmed lock in force ends the lock.
   Smithay 0.7 accepts the request on any lock object, including one whose
   `lock` was refused: it posts `invalid_unlock` but then still calls
   `SessionLockHandler::unlock`, which cannot tell which lock asked. The
   compositor therefore implements `Dispatch<ExtSessionLockV1, _>` itself
   instead of using `delegate_session_lock!` for that interface: an
   `unlock_and_destroy` on any other lock object, or on the lock in force
   before `locked` was sent, receives `invalid_unlock` and changes nothing
   (`may_unlock`); every other request goes to Smithay unchanged, and the
   remaining session-lock interfaces are delegated as the macro would. A
   valid unlock resets the lock runtime and restores `focus_before_lock` if
   that surface is still alive; if it died, the focus fallback picks the
   topmost window. Pointer focus is then refreshed, so the surface under
   the pointer receives the first click after unlocking without a motion.

A second lock request while a lock is in force is refused (dropping it
sends `finished`), even after the first locker has crashed: the session
then stays protected, black and input-dead, while VT switching keeps
working so it can be recovered from another VT. Another client's refused
lock can neither unlock the session nor show a lock surface. The lock
manager global is offered to every client. The PAM-backed unlock UI
belongs to the session work.

## Screencopy

nkdhr implements the server side of `wlr-screencopy-unstable-v1`
(version 3) directly on Smithay's `wayland_protocols_wlr` bindings; Smithay
0.7 has no ready-made screencopy state.

- `capture_output` and `capture_output_region` resolve the `wl_output` and
  convert a region from logical to physical pixels with the output's scale,
  intersected with the mode. A dead or modeless output, or an empty region,
  fails the frame. The client is offered one `wl_shm` XRGB8888 buffer of
  exactly that size (`buffer_done` follows for version 3).
- `copy`/`copy_with_damage` validate the buffer (format, size, stride,
  offset and pool bounds), reject reuse of a frame object and queue the
  request under the output's name.
- The backend fulfils queued requests from the next fully composed frame of
  that output. Requests with and without `overlay_cursor` are served on
  successive frames, so each client gets exactly the cursor policy it asked
  for. `copy_with_damage` reports the whole region as damaged, and the
  `YInvert` flag is set when the readback is flipped.
- On the TTY, a frame with pending requests disables cursor-plane scanout so
  the cursor is in the framebuffer, then exports the primary swapchain
  buffer as a dmabuf, imports it and copies the region. Nested, the region is
  copied straight from the window framebuffer (with the EGL rebind noted
  above).
- Requests for an output that disconnects fail.

## Verification

- **Gates.** `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-features`, and builds of this crate with the
  default (`nested`) and `tty` features.
- **Unit tests** cover the layout resolver (including xdg-output rounding
  and saturation) and the decoding of the `outputs` namespace, the
  initial-placement rule, the keep-reachable rule, the resize geometry
  (edges, the fixed opposite edge, size hints, saturation) and the resize
  state across the final configure and across a move during the grab, the
  rule that only the confirmed lock in force may unlock, idle-inhibitor
  counting, the user-action serials kept for popup grabs, the rule that
  decides whether a popup grab may start, the rule that decides whether a
  touch dismisses the open popup-grab chain, the VT key
  tables, the VT chord rule and the VT path parser, the SDR-first scanout
  format order and the fallback cursor image. The VT path parser and
  scanout format tests live in the TTY backend and run only in a build
  with the `tty` feature, which links against the system's libseat,
  libudev, libinput, GBM and libdrm.
  Protocol behaviour that needs real clients (focus, the session lock,
  grabs, membership) is verified by the smoke runs below.
- **Nested smoke runs** use a temporary
  `weston --backend=headless --renderer=pixman` host, removed afterwards.
  Real clients (`weston-simple-shm`, `weston-simple-egl`,
  `weston-simple-dmabuf-egl`, `foot`, GTK 4 applications, `grim`,
  `wl-copy`/`wl-paste`, X11 clients through XWayland) connect to the nested
  socket. Such a host has no real keyboard or pointer, so interactive input
  is verified on the TTY.
- **VKMS lab** (`tools/vkms-lab.sh`). It builds one virtual DRM device
  through the kernel's VKMS configfs ABI with two complete pipelines (each
  connector with its own encoder, CRTC and primary plane), so both outputs
  scan out concurrently and the real udev, connector-scanner, atomic KMS,
  GBM and damage paths are exercised. Commands are deliberately narrow:
  `setup` creates only `/sys/kernel/config/vkms/nkdhr-lab`, refuses to
  overwrite an existing instance and rolls back a failed construction;
  `connect`/`disconnect <0|1>` change one connector's configfs status, which
  emits the kernel hotplug event udev delivers; `show` needs no root;
  `audit <pid>` reads `/proc/<pid>/fd` and fails if any open primary DRM
  node is not the lab's; `teardown` disables and removes only `nkdhr-lab`
  with explicit `unlink`/`rmdir` (never recursive deletion) and leaves the
  module loaded. Audit before exercising outputs: connector logs alone are
  insufficient, because opening a primary node can acquire DRM master
  before any connector scan. VKMS proves compositor logic and kernel API
  integration, not link training, EDID quirks, the local logind handoff or a
  real panel's modes; those stay physical acceptance items.
- **Soak runner** (`tools/soak-test.sh`). `run` creates a timestamped state
  directory, starts a transient user-systemd collector under a system sleep
  inhibitor and then `exec`s the release compositor so it keeps its local
  TTY and libseat context; `start --pid` attaches to a running compositor.
  Only seconds during which the recorded logind session is active count
  toward the target, and a PID start-time check distinguishes the original
  process from PID reuse. The collector records RSS and high-water memory,
  CPU ticks, threads and descriptors, deduplicated per-DRM-client i915
  fdinfo engine and memory counters, connector status, session state,
  sampling gaps, filtered kernel DRM failures and compositor-owned failure
  or panic lines (`nkdhr-compositor: failed…`, `panicked at`). A report is
  frozen on completion, early process death or an explicit stop. The
  automatic warnings are screening criteria, not a substitute for
  workload-aware review. Runtime data lives in the user's state directory,
  never in the Git worktree.

## History

The verification record of the milestones whose code this crate carries.
Results before 2026-10-23 were recorded under the previous crate name with
the interaction layer present; the backend and protocol paths they
exercised are the ones described above.

- **COMP-1 (nested skeleton, 2026-08-03).** Started cleanly nested inside a
  headless Weston host, created its window at the requested 1280×800 and
  rendered every frame for the whole run; `VmRSS` sampled every 30 seconds
  over ten minutes stayed flat. The `CloseRequested` handling and the
  `bind()` resize behaviour were confirmed from Smithay's source.
- **COMP-2 (Wayland clients, 2026-08-03).** A `wl_output` global turned
  out to be necessary rather than optional: `foot` refused to start without
  one (`no monitors available`), even though nothing in `wl_shm`,
  xdg-shell or `wl_seat` requires it. The dmabuf global uses version 3
  without default feedback because feedback needs a resolvable main device,
  which the nested backend cannot provide. `weston-simple-shm`,
  `weston-simple-egl`, `weston-simple-dmabuf-egl`, `foot` and `gtk4-demo`
  connected, rendered and disconnected cleanly, three of them concurrently.
- **COMP-5 (TTY backend, 2026-08-07).** The two-output VKMS diagnostic
  measured zero render-engine growth over a ten-second idle window and
  0.10% of one CPU core. On the laptop panel, real libseat pause/resume
  (TTY1 to TTY2 and back) kept clients and focus, resumed input, and after
  the buffer-reset fix repainted the whole panel immediately. On a real
  eDP + HDMI setup, live arrangement changes, client-preserving physical
  unplug and replug, and a clean two-output VT handoff in both directions
  passed.
- **COMP-6 (protocol long tail, 2026-08-08).** Under a headless Weston host:
  SHM, GTK 4 and pointer-constraint clients mapped; clipboard and primary
  selection copied exact text in both directions between Wayland
  (`wl-copy`/`wl-paste`) and X11 (`xsel`) through a real in-process
  Xwayland; an X11-forced GTK 4 application rendered in a `grim` capture;
  full 1280×800 and 256×256 region captures succeeded; default and `grim -c`
  captures differed only by the fallback arrow; repeated readback did not
  stop the compositor. A real `swaylock` lock reached protected presentation
  and was confirmed only afterwards; when its PAM worker crashed (the host
  had no `/etc/pam.d/swaylock`), the session stayed protected and a
  screenshot was pure black. On the TTY, Weston's constraints client
  confirmed confinement and locked-pointer relative motion, and its
  data-source DnD client confirmed icon tracking and a successful drop.
  Weston's `--self-only` mode (DnD without a data source) does not receive a
  drop under Smithay 0.7, which marks every source-less offer unvalidated
  before `wl_data_device.drop`; this is an upstream limitation, and ordinary
  DnD is the supported path. Observable idle inhibition, a PAM-backed
  unlock and an installed (rather than unpacked) Xwayland remained
  integration checks.
- **COMP-8 (stabilization, 2026-08-08).** A SIGKILL stress run showed that
  protocol destroy callbacks alone leave dead windows in the stack after
  abrupt client death, which is why reaping exists; neither backend retains
  the `Client` handle returned by `insert_client`, because the display owns
  the connection and an extra handle would be unbounded per-connection
  history. An optimized nested run force-killed 200 SHM/EGL clients: every
  window was reclaimed, the window count returned to zero, the descriptor
  count stayed at 30, and RSS settled at 131,704 KiB from the third of 20
  samples onward. The eight-hour active-TTY idle soak kept the original
  process in all 956 samples, grew RSS by 192 KiB, held descriptors and
  threads constant, averaged 0.243% of one core and recorded no kernel DRM
  failure. A VT log from that soak exposed the handoff race that led to the
  reset-before-switch ordering; a final one-minute physical regression
  after the fix recorded 32/32 matching samples, correct output
  transitions, no errors and an immediate full repaint on return.

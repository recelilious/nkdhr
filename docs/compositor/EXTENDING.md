# Compositor — Extension Seams

> [中文版本 / Chinese version](EXTENDING_zh_CN.md)

Status: **seams only**. nkdhr's interaction design is being redesigned, and
no interaction layer exists today. This document fixes where a future
interaction layer attaches to `nkdhr-compositor` and which invariants it
must keep, so that the protocol layer does not have to be rewritten when it
arrives. It deliberately says nothing about what that layer will do.

The protocol layer ships one permanent default behaviour: centred
placement, focus on map and on click, client-initiated move and resize,
client-side decorations and VT switching (see [USAGE.md](USAGE.md#window-behaviour)).
Each part of it sits behind one of the seams below. A future layer replaces
a part by replacing what stands at that seam; until it does, the default
applies. The previous interaction layer, removed on 2026-10-23, is
preserved at the git tag `archive/interaction-v1` for reference only;
nothing in it is a commitment.

## Seams

### Initial placement

`windows::initial_location(geometry, &OutputLayout, pointer)` is the only
place that decides where a new window goes (see
[INTERNALS.md](INTERNALS.md#the-initial-placement-rule)). It is called
exactly once per mapping: when an xdg toplevel commits its first buffer
(again after a null-buffer unmap), and when an X11 managed window maps.
The keep-reachable rule also calls it for a window that an output layout
change left on no output (see
[INTERNALS.md](INTERNALS.md#the-keep-reachable-rule)), so a replacement
decides those relocations too. A replacement:

- returns a location in compositor-global logical coordinates, for the
  window's root surface origin;
- may consult the window, the layout, the pointer and its own state, but
  moves no other window as a side effect; rearranging other windows is a
  separate operation through the window store;
- must not assume an output exists: the layout may be empty.

Override-redirect X11 windows are not placed by this function; they keep
the position their client gives them.

### Focus policy

The focus policy consists of the functions in `state.rs` that every focus
change goes through: `App::focus_window` (focus and raise, used on map and
on click), `App::focus_topmost` (the fallback when the focused window
goes away) and `App::focus_popup_root` (where focus goes when a menu's
popup-grab chain that held it closes: its root window, else the
fallback), together with the click rule in the input router
(`input::click_to_focus`). A future layer
may change when windows are focused or raised, add other ways to move focus,
or change the fallback, by changing or replacing these functions. It keeps
their protocol obligations:

- live keyboard focus on windows is only ever set through
  `App::set_keyboard_focus`, which goes through the seat's
  `KeyboardHandle` so that `SeatHandler::focus_changed` keeps xdg
  activation, X11 activation and selection focus consistent, and applies
  the same bookkeeping itself when focus is cleared (Smithay 0.7 does not
  report a cleared focus);
- while the session is locked, the policy writes `focus_before_lock`, never
  the live focus;
- override-redirect windows and unplaced toplevels are never focus targets;
- raising a window can change what lies under the pointer, so a change
  that raises refreshes pointer focus once it is complete (see the window
  store below).

### Input router

`input::handle(app, &OutputLayout, event)` is the single entry point for
input. Its order is fixed:

1. the locked path, which returns before anything else runs;
2. the VT-switch intercept, inside the keyboard filter;
3. **the hook point**: an interaction layer's own handling of an event;
4. the default routing: forwarding to the seat, plus the click-to-focus
   rule.

There is no registration API. A future layer is written into the router at
step 3. It may consume an event (for example to drive its own pointer
grab), alter it, or let it continue to step 4. Compositor-owned pointer
interactions are implemented as Smithay `PointerGrab`s, as `grabs.rs` does
for move and resize, so that grab start, cancellation and the lock path
behave uniformly.

Forwarding also records the latest key press, key release, button press
and touch-down with the client each reached (`App::user_actions`), and
`xdg_popup.grab` is honoured only for one of these. A layer that consumes
such an event records it as reaching no client, so that an older serial
cannot open a menu while the user is doing something else.

The default routing also dismisses the open popup-grab chain when a touch
starts a new touch sequence outside the chain's client
(`App::dismiss_popup_grab`), as Smithay's `PopupPointerGrab` does for a
click outside it. A layer that consumes such a touch-down dismisses the
chain the same way, so that a menu never stays open under a touch
elsewhere.

### Render element list

`render::output_elements` builds each output's element list, and the
`render_elements!` enum in `render.rs` lists every element kind. A future
layer adds its own variants to that enum and inserts its elements at a
defined position in the list (above windows and below the cursor, or below
windows). Both backends draw the same list, so a new element kind works on
both without backend changes, as long as it is renderer-independent: no
element type may name a concrete renderer.

### Window store

`WindowStore` (`windows.rs`) is the one list of client windows: their ids,
locations, stacking order and placed state, with operations to place,
raise, move and look up windows and to hit-test a global point. A future
layer reads and changes windows only through it. It may keep its own data
keyed by `WindowId` (ids are never reused), and it may add store operations,
but it never keeps a second list of windows, positions or stacking order
that the protocol layer would not see.

Pointer focus follows only motion in Smithay. A change that alters what
lies under a stationary pointer (placing, raising, moving, unmapping or
removing a window) is therefore followed by
`input::refresh_pointer_focus`, once the whole change is done, as the
protocol layer's own changes are (see
[INTERNALS.md](INTERNALS.md#input-routing)). It must not be called from
inside a pointer grab's own callbacks: a grab that moves windows leaves
focus to the end of the grab, as `grabs.rs` does.

### Output layout

`OutputLayout` (`outputs.rs`) is the resolved arrangement of the connected
outputs in the compositor-global logical space, rebuilt from the `outputs`
namespace whenever outputs or configuration change. A future layer reads it
and may define its own coordinate spaces on top of it, but converts to
global logical coordinates before it touches the window store, the seat or
any protocol object. Output arrangement itself stays in the `outputs`
namespace.

Every new layout reaches the protocol layer through
`App::set_output_layout`, which also applies the keep-reachable rule
(`windows::location_to_keep_reachable`). A layer that changes what happens
to windows on a layout change does so there, and keeps the guarantee the
rule gives users: no window is ever left where no output shows it.

New persisted settings go into their own CTRL-5 namespace. `Config1.Set`
only overwrites values that already exist, so a setting whose keys are
created at runtime (one entry per output, per window class and so on) is
created by writing the namespace's file and adjusted through `Set`
afterwards, as `outputs` does.

## Invariants

Every extension keeps these, whatever it does:

- **The protocol layer owns every protocol fact.** Surface roles, the
  configure/acknowledge cycle, initial configures, keyboard and pointer
  focus delivery through the seat, `wl_surface` output membership,
  preferred scales, frame callbacks, selections, XWayland window-manager
  duties, screencopy and the lock state are maintained by the protocol
  layer. A policy asks for an outcome (place this window here, focus that
  one, request this size) and the protocol layer turns it into protocol
  traffic. No policy sends protocol events of its own.
- **Policies never bypass the window store.** Every window a client sees on
  screen is in the store, at the location the store records, in the order
  the store records. Rendering, hit testing, output membership and frame
  callbacks all read the store, so anything that bypasses it desynchronizes
  what clients are told from what users see.
- **Session-lock routing stays fail-closed.** The locked path runs before
  the hook point and is not itself a hook point. While locked, no policy
  element is rendered, no policy grab survives the start of the lock or
  starts during it, no policy receives input, and screencopy exposes only
  the lock composition. A policy that holds keyboard, pointer or touch
  grabs must let the lock end them, as the lock already does for Smithay's
  grabs and for touch sequences in progress.
- **VT switching stays reachable.** The VT-switch intercept runs before the
  hook point, in both the locked and the unlocked path, and no policy may
  consume `Ctrl+Alt+F1`…`F12` or `XF86Switch_VT_1`…`12`. It is the user's
  way out of a stuck session.

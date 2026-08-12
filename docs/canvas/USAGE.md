# Canvas — User Guide

> [中文版本 / Chinese version](USAGE_zh_CN.md)

This guide covers `nkdhr-canvas`, the compositor and integrated shell runtime
implemented and accepted in Phase 2 (COMP-1 … COMP-8).

`nkdhr-canvas` is nkdhr's Wayland compositor: an infinite, pannable,
zoomable 2D world ("the canvas") that windows and pinned widgets live on,
instead of a fixed desktop or a tiling grid.

## Running it

**Nested (development)**: `nkdhr-canvas` run from inside an existing
X11 or Wayland session opens as an ordinary window on that desktop —
useful for developing nkdhr itself without a dedicated machine or a TTY
switch:

```
nkdhr-canvas
```

**On the TTY (real use)**: a full nkdhr install launches `nkdhr-canvas`
directly from a `systemd` target after login (SESS-3, Phase 5) — it takes
over the display via DRM/KMS itself, the same way any other Wayland
compositor does; you should not normally start it by hand outside
development.

On this backend, `Ctrl+Alt+F1` through `Ctrl+Alt+F12` switch Linux virtual
terminals through libseat. Returning to nkdhr's VT resumes the existing
session and clients. The nested development backend does not claim these
host-desktop shortcuts.

**Two-output development without a spare monitor**: the repository ships a
VKMS configfs lab that creates two real kernel DRM connectors rather than
faking outputs inside the compositor:

```sh
sudo crates/nkdhr-canvas/tools/vkms-lab.sh setup
crates/nkdhr-canvas/tools/vkms-lab.sh show
# Run nkdhr-canvas with NKDHR_DRM_DEVICE set to the real GPU's render node
# and NKDHR_DRM_SCANOUT_DEVICE set to the shown VKMS primary card.
crates/nkdhr-canvas/tools/vkms-lab.sh audit <canvas-pid>
sudo crates/nkdhr-canvas/tools/vkms-lab.sh disconnect 1
sudo crates/nkdhr-canvas/tools/vkms-lab.sh connect 1
sudo crates/nkdhr-canvas/tools/vkms-lab.sh teardown
```

Use `disconnect`/`connect` while the compositor is running to verify real
udev hotplug handling. `teardown` removes only the named lab instance and
does not unload the VKMS module. This is a developer regression environment,
not a substitute for the final local-VT and physical-display acceptance.
The scanout filter is a hard safety boundary: the compositor refuses a
non-render-node renderer override and does not open unselected KMS cards.
Stop immediately if the laptop's physical display changes during a VKMS
run; a black physical panel is never expected test behaviour.

**Eight-hour acceptance soak**: the repository's COMP-8 runner keeps the
measurement independent of an interactive development shell:

```sh
cargo build --workspace --all-features --release
# Run nkdhrd separately, then launch this from nkdhr's local TTY:
crates/nkdhr-canvas/tools/soak-test.sh run --duration 8h

# These may be called later from any terminal in the same user session:
crates/nkdhr-canvas/tools/soak-test.sh status
crates/nkdhr-canvas/tools/soak-test.sh stop
crates/nkdhr-canvas/tools/soak-test.sh report
```

`run` starts a transient user-systemd collector, then replaces itself with
the release TTY compositor so the compositor retains the local controlling
TTY. The collector survives the terminal command and does not depend on an
open Codex conversation. It samples every 30 seconds by default and stores
CSV data, event/compositor/kernel logs, metadata and a Markdown report below
`$XDG_STATE_HOME/nkdhr/soak/` (or `~/.local/state/nkdhr/soak/`). It holds a
sleep inhibitor for the measurement, counts only time for which the launch
session remains active, records VT/output transitions, and leaves the
compositor running when collection completes. `stop` stops collection only;
exit the compositor separately when desired.

For an already-running compositor, attach without relaunching it:

```sh
crates/nkdhr-canvas/tools/soak-test.sh start --pid <canvas-pid> \
    --session <login-session-id> --duration 8h
```

The generated automatic verdict detects an early process exit or PID reuse,
an incomplete active-time duration, DRM/kernel failures, compositor-owned
failure/panic log lines, large RSS growth, file-descriptor growth and the
absence of any observable idle GPU interval. A terminal report is frozen at
collection completion, so later output from a deliberately still-running
compositor cannot retroactively change that run's result.
Real-work memory changes remain workload-dependent, so COMP-8 acceptance
still requires reviewing `report.md` and the raw samples rather than treating
one threshold as a proof of leak freedom.

**Pinned-node host smoke test**: developers can opt the permanent COMP-7
fixture into either backend with
`NKDHR_CANVAS_DEMO_PINNED_IMAGE=1`. It adds one generated image to the
default canvas at a fixed world position; the image pans/zooms with the
world, sits behind windows, captures pointer input and logs each press. The
environment variable is deliberately a diagnostic switch, not user widget
configuration, and is unset in normal sessions.

UI-5 adds a second opt-in diagnostic, `NKDHR_CANVAS_DEMO_UI=1`. It mounts the
real retained Appearance Settings application as an above-window world node,
using its display list directly rather than a screenshot. This exercises the
same application surface used by the standalone `nkdhr-settings` Wayland
window; it is likewise disabled in normal sessions.

## The canvas model

There is one infinite 2D plane per canvas. Windows may overlap and may be
placed at any positive or negative world position; there are no tiling slots
or canvas boundaries. By default, new-window placement and interactive
move/resize align to a 32-unit world grid so spatial plans remain regular.
The grid is a placement aid, not a layout manager: it never moves another
window, pinned node or an in-progress pan. In normal work view, the canvas
also has a screen-space anchor: by default the primary display's center.
That anchor initially shows world `(0,0)` and, when a pointer or three-finger
pan ends, eases to the nearest grid intersection just as a moved window does.
Overview remains a temporary free camera and returns to an aligned work view.
Both the toggle and interval are hot-reloadable:

```
nkdhrctl config set canvas.snap_to_grid false
nkdhrctl config set canvas.grid_size 64
```

`grid_size` must be between 1 and 4096 logical world units. Re-enable with
`nkdhrctl config set canvas.snap_to_grid true`. A session has one canvas by
default; see "Multiple canvases and multi-monitor" below for when a second
one is useful.

## Application compatibility and session safety

Native Wayland applications get the regular clipboard, middle-click
primary selection, drag-and-drop, server-side decoration negotiation,
fractional-scale hints, pointer lock/confinement and idle inhibition.
Legacy X11 applications run through the compositor's integrated XWayland
server and participate in the same canvas placement, focus and clipboard
model as native windows. The `Xwayland` executable must be installed; on
Fedora the package is `xorg-x11-server-Xwayland`. If it is absent, the
Wayland session still starts and logs that only X11 compatibility is
disabled.

Screenshot clients use `wlr-screencopy-unstable-v1` and capture one output
at a time. The captured image is the final composed output, including its
configured scale. While the session is locked, screenshots contain only
the protected black/lock-screen scene. Cursor overlay is opt-in (`grim -c`);
ordinary captures omit it.

An `ext-session-lock-v1` client protects every connected output before the
compositor acknowledges that locking succeeded. From the first lock
request until a valid unlock, ordinary canvas surfaces receive no keyboard
or pointer input and are never rendered or exposed through screencopy.

- **Pan**: click-drag on empty canvas, a three-finger touchpad swipe, or
  `super+arrow keys`. Two-finger touchpad scrolling and mouse wheels remain
  ordinary application scroll input; they never move the canvas. Each
  keyboard step uses a short eased transition. With grid snapping enabled,
  free pointer/three-finger motion follows the hand continuously and then
  eases the display anchor to its nearest grid intersection on release;
  repeated presses extend the pending destination instead of flashing
  through disconnected positions. Windows never change size or scale while panning —
  panning only moves *your view*, at a fixed 1:1 zoom, which is the state
  you spend nearly all your time in.
- **Overview**: `super+o` zooms out to see every window on the current
  canvas at once. Click a window to zoom back in on it at 1:1; click empty
  space, press `super+o` again, or `Esc` to cancel and return to where you
  were. This — not a minimap, not a window-switcher list — is how you get
  to a window that's far away on the canvas.
- **Marks**: `super+shift+<0-9>` records the current view position under
  that digit; `super+<0-9>` jumps back to it, animated. Marks persist
  across restarts. This is the fast way back to a spot you use often (a
  "workspace" in spirit, without the fixed-slot rigidity of one).
- **Move / resize a window**: `super+drag` anywhere on a window moves
  it; `super+right-drag` resizes it. With the default grid enabled, a moved
  window follows the pointer continuously and eases to the nearest grid
  intersection on release. Resizing aligns the actively dragged edge or
  corner while the opposite edge stays fixed. Apps using
  server-side decoration also
  get a minimal compositor title area that can start a normal drag; the
  modifier remains the precise canvas-native path from any point in a
  window. Clients using their own decoration can issue the standard
  xdg-shell/X11 move and eight-edge resize requests too.
- **Focus**: click a window to focus it (this also raises it above other
  windows and still delivers the click itself to whatever you clicked on);
  `alt+tab` cycles focus among windows currently mapped, independent of
  where the pointer is. Focus does not follow the mouse.
- **Close a window**: `super+escape` (without needing to find a close button —
  there isn't one yet, same reasoning as move/resize above).

## Multiple canvases and multi-monitor

Outputs (monitors) are arranged into **output groups** in
`canvas.outputs` under `~/.config/nkdhr/canvas.toml` (the graphical
settings surface arrives in the shell phase; see the control-plane
USAGE.md for external-edit validation). Each group is bound to exactly
one canvas:

- **One group with all your monitors**: every screen shows the same
  canvas, panning and zooming together as one wide viewport — the default
  for most multi-monitor setups.
- **Multiple groups (one monitor each, typically)**: each screen is its
  own fully independent canvas, panned and zoomed separately — closer to
  what "separate workspaces per monitor" means in other desktops, without
  actually being workspaces.

A monitor not mentioned in any configured group is treated as its own
one-output group automatically, so plugging in an unconfigured display
never leaves it blank.

With no `outputs` tables, every connected monitor is placed left-to-right
in one `default` group bound to the `default` canvas. A two-monitor rigid
group can be written as:

```toml
[outputs.desk]
canvas = "main"
primary = "eDP-1"

[outputs.desk.members.eDP-1]
x = 0
y = 0
scale = 1.0

[outputs.desk.members.DP-1]
x = 1920
y = 0
scale = 1.0
```

The connector names are the DRM names printed by nkdhr-canvas when an
output connects. Coordinates are logical pixels within the group and may
be negative; nkdhr normalizes the arrangement without changing the
relative positions. `scale` must be finite and greater than zero. An
output may belong to only one group. `primary` is optional but, when set,
must name one of the group's members; its logical center becomes the canvas
anchor. A one-output group naturally uses its only display. Without an
explicit primary in a multi-output group, the first connected member in
stable connector-name order is used. Valid file edits reload live; an
invalid edit is rejected by `nkdhrd` and the last-known-good layout stays
active.

## Keybindings

UI-6 replaces the former fixed-modifier leaves with one typed schema-v1
document. Every keyboard, mouse and touchpad binding maps a normalized trigger
to a registered action and validated scalar arguments. The document is a JSON
string because CTRL-5 currently exposes scalar leaves:

```
nkdhrctl config set canvas.bindings '<schema-v1-json>'
nkdhrctl config set canvas.snap_to_grid <bool>   # default true
nkdhrctl config set canvas.grid_size <integer>   # default 32
```

The default empty value selects the complete built-in structured map while
still honoring the three legacy key leaves during migration. A non-empty
document is authoritative. Changes publish immediately without restart only
when the complete candidate is valid. Unknown actions, wrong arguments,
duplicate IDs and trigger conflicts reject the candidate while the exact last
effective generation remains active; absent TTY/touchpad capabilities remain
visible as unsupported diagnostics rather than silently dead shortcuts.

Defaults are Super+Escape close, Alt+Tab focus cycle, Super+O overview,
Super+arrows or standard Vim H/J/K/L canvas pan, Super+Shift+direction focused
window move, Super+Ctrl+direction focused window resize, and the existing mark
bindings. Pointer move/resize/empty-canvas pan and TTY three-finger swipe/pinch
are in the same document. Client two-finger scroll and complete touchscreen
sequences are not captured. See the UI stack guide for the exact JSON grammar.

## Troubleshooting

- Nothing renders / crashes on start on real hardware: check
  `journalctl` for the session — a GPU/driver issue will show as an EGL
  or DRM error there. nkdhr-canvas targets Intel Iris Xe as the only
  supported GPU until the project reaches feature completeness (ROADMAP
  §2.1); other GPUs are a known post-completion gap, not a bug to report
  yet.
- A different desktop session blanks or suspends the machine during a TTY
  soak: stop that session's idle manager first. The runner blocks sleep, but
  it cannot safely rewrite or disable another compositor's DPMS policy.
- Starting `--tty` over SSH reports that no seat/session can be opened:
  start it from the local VT. libseat/logind grants DRM and input access
  to the active local seat, not to an unrelated remote login. The
  `LIBSEAT_BACKEND=noop` mode is only for isolated VKMS development and
  must not be used for a production session.
- X11 applications do not start: install `xorg-x11-server-Xwayland` and
  restart the compositor. Missing Xwayland is intentionally non-fatal and
  does not affect native Wayland clients.
- A window won't move/resize/focus: check `nkdhrctl watch session` — if
  the session reports `locked: true`, canvas input is intentionally
  routed to the lock screen only.
- Marks or keybindings didn't survive a restart: check `nkdhrctl config
  get canvas.marks` and `canvas.bindings`. A compositor-rejected binding
  candidate keeps its previous effective generation; an external namespace
  edit that failed validation keeps the last-known-good value. See the control-plane
  USAGE.md's troubleshooting section for how to find the rejection
  reason.

# Compositor — User Guide

> [中文版本 / Chinese version](USAGE_zh_CN.md)

`nkdhr-compositor` is nkdhr's Wayland compositor: the display protocol
layer, with a DRM/KMS backend for real sessions on a Linux virtual terminal
and a nested backend that runs as a window inside an existing desktop for
development. It serves native Wayland clients and X11 applications (through
an integrated XWayland server), drives every connected display, and keeps
the session safe while it is locked.

It deliberately has a small, fixed window behaviour: windows open centred,
a click focuses and raises, and applications move and resize themselves
through their own title bars. nkdhr's interaction design is being
redesigned. The previous interaction layer (the infinite canvas,
workspaces and the integrated shell) was removed on 2026-10-23 and is
preserved at the git tag `archive/interaction-v1`.

## Building

```sh
cargo build -p nkdhr-compositor                    # nested backend only (default)
cargo build -p nkdhr-compositor --features tty     # nested and TTY backends
cargo build --workspace --all-features --release   # everything, optimized
```

The `tty` feature links against libseat, libudev, libinput, GBM and EGL.
The binary is `target/<profile>/nkdhr-compositor`.

## Running it

### Nested (development)

Run from inside an existing X11 or Wayland session:

```sh
cargo run -p nkdhr-compositor
```

It opens a 1280×800 window titled `nkdhr-compositor` on your current
desktop. The window is one output; resizing it resizes that output. The
host's own cursor is hidden over the window and the compositor draws its
own.

### On a TTY

Log in on a local virtual terminal (for example `Ctrl+Alt+F3`), then:

```sh
target/release/nkdhr-compositor --tty
```

The compositor takes over every connected display through DRM/KMS, the same
way any other Wayland compositor does, using the seat that logind grants to
your local login. It must be started from the local VT; see
[Troubleshooting](#troubleshooting) for SSH. Starting nkdhr from a login
manager and systemd belongs to the session work that has not been built yet,
so today the TTY backend is started by hand.

Start `nkdhrd` first, in the same login session: an installed `nkdhrd` runs
as a `systemd --user` service, and from a source checkout you run it
yourself (for example `target/debug/nkdhrd &` before starting the
compositor); [Starting the daemon](../control-plane/USAGE.md#starting-the-daemon)
in the control-plane guide covers both. The compositor reads its output
arrangement from `nkdhrd` once at startup and then follows its change
notifications; without `nkdhrd` it still runs and arranges every display
automatically (see [Arranging outputs](#arranging-outputs)).

### Choosing the backend

| Invocation | Backend |
|---|---|
| `nkdhr-compositor --nested` | nested |
| `nkdhr-compositor --tty` | TTY |
| `nkdhr-compositor` | nested when `WAYLAND_DISPLAY` or `DISPLAY` is set, otherwise TTY |

Any other argument is an error. Asking for a backend the binary was built
without is an error too; the TTY backend needs `--features tty`.

### Launching applications

The compositor has no launcher, panel or application shortcuts. It prints
where clients connect:

```
nkdhr-compositor: listening on WAYLAND_DISPLAY=wayland-1
nkdhr-compositor: XWayland ready on DISPLAY=:1
```

Start a terminal with those values and launch everything else from it; its
children inherit both variables:

```sh
WAYLAND_DISPLAY=wayland-1 DISPLAY=:1 foot
```

When nested, always set `WAYLAND_DISPLAY` explicitly: without it, a client
started from a host terminal opens on the host desktop instead. In the same
way, the host's `DISPLAY` points at the host's X server, not at the
compositor's XWayland. On a TTY, start the first terminal from another VT
or from an SSH login of the same user; the socket lives in the same
`$XDG_RUNTIME_DIR`.

### Leaving the session

- **Nested**: close the window, or press `Ctrl+C` in the terminal that
  started the compositor. Closing that terminal stops it too.
- **TTY**: there is no exit key. Switch to another VT (`Ctrl+Alt+F3`), log
  in, and stop the process: `kill $(pidof nkdhr-compositor)`. An SSH login
  of the same user works as well, and so does ending the login session
  with `loginctl terminate-session`. The compositor does not have to be on
  the VT in front to stop.

Either way the compositor shuts down cleanly. It stops on `SIGTERM` (what
`kill` sends by default), `SIGINT` (`Ctrl+C`) and `SIGHUP` (the starting
terminal closing), and logs which one it received:

```
nkdhr-compositor: received SIGTERM, shutting down
```

It then removes its Wayland socket (`wayland-N` and `wayland-N.lock` in
`$XDG_RUNTIME_DIR`) and XWayland's X11 socket and lock file
(`/tmp/.X11-unix/X<n>` and `/tmp/.X<n>-lock`), stops XWayland and, on a
TTY, hands the displays back in the state it found them in and releases
the seat, so the VT it ran on returns to its text console. Clients lose
their connection when the compositor exits.

Ending a login session sends `SIGTERM` and right behind it `SIGHUP`; the
line then names `SIGTERM`. A signal that was already ignored when the
compositor started stays ignored: started with `nohup`, it keeps running
when its terminal closes, and when a script starts it in the background,
`Ctrl+C` does not stop it.

A nested compositor whose window the host desktop hides (minimized on
GNOME or KDE, or on a workspace that is not shown on sway or Hyprland)
may only stop once its window is shown again, because the host stops
letting it draw while hidden.

If shutting down ever hangs, a second `SIGTERM` or `SIGINT` (run `kill`
again, or press `Ctrl+C` again) ends the process at once, without any of
that cleanup. `SIGHUP` never counts towards that, neither as the first nor
as the second signal, because session managers and terminals often send
it right behind another signal. `kill -9` (`SIGKILL`) and a crash skip the
cleanup as well and leave the socket and lock files behind. They are
harmless: the next start takes a stale Wayland socket over, and reuses a
stale X11 display number unless its lock file names a process that is
still running.

## Window behaviour

This is the protocol layer's permanent default behaviour. It is
intentionally minimal and has no configuration.

- **Opening**: a new window appears once it has drawn its first frame,
  centred on the display that contains the pointer. A window larger than
  that display in either direction is aligned to the display's left or top
  edge in that direction instead, so its title bar and left edge stay
  reachable. Windows may overlap; nothing ever tiles or rearranges them.
- **Focus**: a newly opened window receives keyboard focus and is raised
  above the others. Clicking a window with any button focuses and raises it,
  and the click itself still reaches the application. Clicking where no
  window is leaves keyboard focus where it was. Focus does not follow the
  pointer. Opening a menu raises the window it belongs to, and the open
  menu has keyboard focus; when it closes, focus returns to that window. A
  click outside an open menu closes the menu without focusing or raising
  the window clicked, though the click itself still reaches it. A window
  that opens while a menu is open closes the menu and takes focus. When the
  focused window closes, focus moves to the topmost remaining window, or
  to nothing when none is left. Whatever window is under the pointer
  receives pointer input even if the pointer has not moved since it
  appeared there, for example a window that just opened under it.
- **Moving and resizing**: drag a window's own title bar or edges. The
  compositor carries out these client-initiated move and resize requests for
  both Wayland and X11 windows; while resizing from the top or left edge,
  the opposite edge stays put. A move or resize always continues the
  button press that started it; requests started from a touchscreen, and
  repeated requests while one is already running, are ignored. There is no
  compositor shortcut for moving, resizing or closing windows.
- **Decorations**: client-side. The compositor tells every client that
  negotiates decorations to draw its own and never draws a title bar or
  border itself. A window whose application draws no decorations therefore
  has no title bar and cannot be moved interactively.
- **Maximize, fullscreen and minimize**: not provided. The compositor tells
  applications so, and applications that respect it hide their maximize,
  minimize and window-menu controls. A request to maximize or go fullscreen
  is still answered, with an unchanged state; minimize and window-menu
  requests are ignored.
- **X11 windows**: ordinary X11 windows are centred and focused the same way
  as Wayland windows. Menus, tooltips and other override-redirect windows
  appear exactly where the application puts them and never take keyboard
  focus.
- **Touch**: a touch reaches the window under the finger that starts it,
  and stays with that window until every finger is lifted, as further
  fingers join the same touch. A touch does not focus or raise windows,
  except that a menu it opens raises its window, as any menu does.
  Touching anywhere outside an open menu's application closes the menu,
  as clicking outside it does, and the touch still reaches what is under
  the finger.
- **Keyboard layout**: there is no layout setting yet. The compositor uses
  libxkbcommon's defaults, which honour `XKB_DEFAULT_LAYOUT`,
  `XKB_DEFAULT_VARIANT`, `XKB_DEFAULT_MODEL` and `XKB_DEFAULT_OPTIONS` in
  the compositor's environment:

  ```sh
  XKB_DEFAULT_LAYOUT=de target/release/nkdhr-compositor --tty
  ```

- **Display changes**: windows keep their position in the shared output
  space when displays are added, removed or rearranged; they do not travel
  with a display. A window left on no display at all, for example because
  the display it was on was unplugged, is moved to the display under the
  pointer and centred there like a newly opened window, so no window is
  ever stranded out of sight. A window that is still partly on some display
  stays where it is.

## Switching virtual terminals

On the TTY backend, `Ctrl+Alt+F1` through `Ctrl+Alt+F12` switch Linux
virtual terminals, and so do the dedicated `XF86Switch_VT_1` …
`XF86Switch_VT_12` keysyms that some keyboard layouts produce for those
chords. These keys are reserved
for VT switching and never reach applications. They work while the session
is locked too: switching to another VT does not unlock or reveal this
session. Returning to nkdhr's VT resumes the session with every client,
window and focus intact and repaints every display immediately. Selecting
the VT the compositor is already on does nothing.

The nested backend leaves these combinations to the host desktop.

## Arranging outputs

Displays are arranged in one shared logical coordinate space, configured
through `nkdhrd`'s `outputs` namespace in `~/.config/nkdhr/outputs.toml`.
Each entry is one table named after a connector:

```toml
[eDP-1]
x = 0
y = 0
scale = 1.25

[HDMI-A-1]
x = 1536
y = 0
scale = 1.0
```

- **Connector names** are the kernel's (`eDP-1`, `DP-2`, `HDMI-A-1`, …).
  The compositor prints each one as it connects:
  `nkdhr-compositor: connected output HDMI-A-1 at …`. The nested backend's
  single output is named `nkdhr-compositor-nested-0`. Names may not
  contain `.`. A table whose name matches no connected connector is
  accepted and has no effect, so a misspelt name leaves its display placed
  automatically.
- **`x`, `y`** are the display's top-left corner in logical pixels and may
  be negative. Only the arrangement matters: after placing every display,
  the compositor shifts the whole layout so that its top-left corner is
  `(0, 0)`, keeping the relative positions. Both default to `0`.
- **`scale`** is physical pixels per logical pixel, a finite number greater
  than zero; fractional values such as `1.25` are fine. A display's logical
  size, and therefore the room it takes up in the layout, is its mode size
  divided by its scale, rounded to the nearest whole logical pixel, which
  is also the size applications are told: the 1920×1080 panel above at
  `1.25` is 1536×864 logical pixels, which is why the monitor sits at
  `x = 1536`, and a 2256×1504 panel at `1.75` is 1289×859. Defaults to
  `1.0`.
- **Unconfigured displays** are never left blank. A connected connector
  without an entry is placed automatically at scale `1.0`, to the right of
  the configured displays, in connector-name order. With an empty file,
  every display is placed left to right this way.

The pointer moves only across the displays themselves: gaps between
displays in the layout are not reachable, and the pointer slides along
their edges. Displays may overlap; overlapping areas show the same part of
the layout. Each display runs at its preferred mode; this namespace sets
position and scale only. There is no output-management protocol, so tools
such as `wlr-randr` and `kanshi` cannot change displays.

### Changing the arrangement

`nkdhrd` re-validates the file whenever it changes and the compositor
applies a valid arrangement immediately, without restarting. An invalid
edit (an unknown field, a bad scale, a name with `.`) is rejected and the
last valid arrangement stays active; `nkdhrd`'s log
(`journalctl --user -u nkdhrd`, or its terminal when run from a checkout)
shows why.

Adding a connector means editing the file, because `nkdhrctl config set`
only changes values that already exist. Once the entry is there, adjust it
from the command line:

```sh
printf '\n[DP-2]\n' >> ~/.config/nkdhr/outputs.toml   # a bare table is a complete entry
nkdhrctl config set outputs.DP-2.x 3456
nkdhrctl config set outputs.eDP-1.scale 1.5
nkdhrctl config get outputs.HDMI-A-1.x
nkdhrctl config watch outputs
```

Deleting a connector's table returns that display to automatic placement,
but `nkdhrd` reports only values that still exist after an edit, so a
deletion on its own sends the compositor no change notification. The
compositor applies it together with the next change to `outputs`, or when
it next starts.

Files that earlier nkdhr versions wrote into `~/.config/nkdhr/`, such as
`canvas.toml`, are ignored; an arrangement stored there has to be entered
again in `outputs.toml`.

## Application compatibility and session safety

Native Wayland applications get the regular clipboard, middle-click
primary selection, drag-and-drop, client-side decoration negotiation,
popups and menus, fractional scaling, surface cropping and scaling
(`wp_viewporter`, which video players and browsers use as well), pointer
lock and confinement, relative pointer motion, touch and idle-inhibit
requests. Touchpad two-finger scrolling arrives as ordinary scroll input,
and touchpad swipe, pinch and hold gestures reach applications that use
the pointer-gestures protocol.

On a display with a fractional scale such as `1.25`, applications that
support fractional scaling draw at exactly that scale and stay sharp.
Applications that support only whole scales draw at the next whole scale
(`2` for `1.25`) and are scaled down to fit, which looks slightly softer;
applications without any scaling support draw at scale 1 and are
enlarged.

Legacy X11 applications run through the compositor's integrated XWayland
server and share the clipboard and primary selection with Wayland
applications in both directions. The `Xwayland` executable must be
installed; on Fedora the package is `xorg-x11-server-Xwayland`. Without it
the session still starts and logs that X11 compatibility is disabled. X11
applications draw at scale 1 and are enlarged on scaled displays, so they
look soft there. X11 applications that rely on the window manager for
their title bar get none (see Decorations above).

Screenshot tools use `wlr-screencopy-unstable-v1` and capture one display
(or a region of one) at a time. The captured image is the final composed
display at its configured scale. The cursor is included only on request
(`grim -c`).

An `ext-session-lock-v1` client (a screen locker) protects every connected
display before the compositor confirms that the session is locked. From
the first lock request until the locker itself unlocks, no ordinary window
receives keyboard, pointer or touch input, nothing but the lock screen (or
black) is drawn, and screenshots contain only that protected image. Only
the locker's own surfaces are shown and typed into, only its own unlock
ends the lock, and a drag that another application tries to start during
the lock is cancelled at once. Whatever was in progress when the lock
began is cancelled rather than finished: an open menu closes, a drag is
not dropped, a move or resize stops where it is and a finger held on a
window no longer reaches it. When a display's scale or size changes during
the lock (a scale edited in `outputs.toml`, or the nested window resized),
the lock screen is resized to fit it. If the locker crashes, the session
stays locked and black; a second locker is refused while the first lock is
in force. VT switching keeps working, so the session can be stopped from
another VT.

The compositor records idle-inhibit requests from visible windows, but nkdhr
has no idle or display-power policy yet: nothing blanks or suspends the
displays on its own.

## Development tooling

### Two outputs without a spare monitor: the VKMS lab

The repository ships a VKMS configfs lab that creates two real kernel DRM
connectors (named `Virtual-1` and `Virtual-2`) rather than faking outputs
inside the compositor:

```sh
sudo crates/nkdhr-compositor/tools/vkms-lab.sh setup
crates/nkdhr-compositor/tools/vkms-lab.sh show
# Run nkdhr-compositor --tty with NKDHR_DRM_DEVICE set to the real GPU's
# render node and NKDHR_DRM_SCANOUT_DEVICE set to the VKMS card `show` lists.
crates/nkdhr-compositor/tools/vkms-lab.sh audit <compositor-pid>
sudo crates/nkdhr-compositor/tools/vkms-lab.sh disconnect 1
sudo crates/nkdhr-compositor/tools/vkms-lab.sh connect 1
sudo crates/nkdhr-compositor/tools/vkms-lab.sh teardown
```

Use `disconnect`/`connect` while the compositor runs to exercise real udev
hotplug handling. `teardown` removes only the lab's own instance and leaves
the VKMS module loaded. Run `audit` before exercising outputs: it fails if
the compositor holds any primary DRM node other than the lab's.

The two environment variables are development overrides with a hard safety
boundary: `NKDHR_DRM_DEVICE` must name a render node and
`NKDHR_DRM_SCANOUT_DEVICE` a primary node, and with the scanout override
set the compositor opens no other display device at all. Stop immediately
if the laptop's physical display changes during a VKMS run; a black
physical panel is never expected test behaviour. `LIBSEAT_BACKEND=noop` is
acceptable for an isolated VKMS run but never for a real session. The lab
is a regression environment, not a substitute for testing on the local VT
with physical displays.

### Long-run stability: the soak runner

The soak runner measures an extended TTY session independently of any
interactive shell:

```sh
cargo build --workspace --all-features --release
# Start nkdhrd separately, then run this from nkdhr's local VT:
crates/nkdhr-compositor/tools/soak-test.sh run --duration 8h

# From any terminal in the same user session, later:
crates/nkdhr-compositor/tools/soak-test.sh status
crates/nkdhr-compositor/tools/soak-test.sh stop
crates/nkdhr-compositor/tools/soak-test.sh report
```

`run` starts a transient user-systemd collector and then replaces itself
with `target/release/nkdhr-compositor --tty` (or the command given after
`--`), so the compositor keeps the local controlling TTY. The collector
survives the terminal that started it. It samples every 30 seconds by
default (`--interval`) and stores CSV samples, event/compositor/kernel
logs, metadata and a Markdown report below `$XDG_STATE_HOME/nkdhr/soak/`
(or `~/.local/state/nkdhr/soak/`). It holds a sleep inhibitor during the
measurement, counts only time during which the launching login session is
active, records VT and output transitions, and leaves the compositor
running when collection completes. `stop` ends collection only; stop the
compositor separately.

To measure an already-running compositor without restarting it:

```sh
crates/nkdhr-compositor/tools/soak-test.sh start --pid <compositor-pid> \
    --session <login-session-id> --duration 8h
```

The automatic verdict flags an early exit or PID reuse, an incomplete
active duration, DRM or kernel failures, compositor failure or panic lines,
large memory growth, file-descriptor growth and the absence of any idle GPU
interval. A report is frozen when collection completes, so later output
from a compositor left running cannot change it. Memory behaviour depends
on the workload, so read `report.md` and the raw samples rather than
treating one threshold as proof that nothing leaks.

## Troubleshooting

- **Nothing renders, or the compositor exits at start on real hardware**:
  the journal of the session shows the EGL or DRM error. nkdhr targets
  Intel Iris Xe as its only supported GPU until the project is
  feature-complete; other GPUs are a known gap, not yet a bug to report.
- **`--tty` over SSH reports that no seat or session can be opened**: start
  it from the local VT. logind grants DRM and input access to the active
  local seat, not to a remote login.
- **An application opens on the host desktop instead of the nested
  compositor**: set `WAYLAND_DISPLAY` (and `DISPLAY` for X11 applications)
  to the values the compositor printed.
- **X11 applications do not start**: install `xorg-x11-server-Xwayland` and
  restart the compositor. Missing Xwayland does not affect Wayland clients.
- **A window has no title bar and cannot be moved**: its application draws
  no decorations of its own; the compositor deliberately draws none.
- **A window moved to the middle of a display after unplugging or
  rearranging displays**: it was left on no display, so the compositor
  moved it to the display under the pointer. A window that is still partly
  on a display stays where it is; drag it back by its title bar.
- **Every window ignores input and the screen is black**: a screen locker is
  active, or crashed while locked. The compositor logs
  `protecting session while lock client prepares surfaces` when a lock
  begins and `session unlocked` when it ends. After a locker crash, switch
  VT, stop the compositor (see [Leaving the session](#leaving-the-session))
  and start it again.
- **An edit to `outputs.toml` has no effect**: `journalctl --user -u nkdhrd`
  shows the validation error, and `nkdhrctl config get outputs.<connector>.x`
  shows the value in force. A deleted table takes effect only with the next
  change or restart (see above).
- **`nkdhrctl config set outputs.<connector>.x …` reports an unknown key**:
  that connector has no entry yet; add its table to the file first.
- **`pkill -x nkdhr-compositor` finds nothing**: process names are truncated
  to 15 characters. Use `pidof nkdhr-compositor`, or `pkill -f`.
- **The compositor logged `shutting down` but does not exit**: run
  `kill $(pidof nkdhr-compositor)` again (or press `Ctrl+C` again); the
  second `SIGTERM` or `SIGINT` it receives ends the process at once. If it
  logged `received SIGHUP`, that hang-up does not count, so `kill` it
  twice.
- **`wayland-N`, `wayland-N.lock`, `/tmp/.X11-unix/X<n>` or
  `/tmp/.X<n>-lock` is left over**: the compositor was killed with
  `SIGKILL`, ended by a second `SIGTERM` or `SIGINT`, or crashed. The
  files are harmless; the next start reuses or skips them.
- **Another desktop session blanks or suspends the machine during a soak**:
  stop that session's idle manager first. The runner blocks sleep, but it
  cannot safely change another compositor's display-power policy.

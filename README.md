# nkdhr

*A self-contained Linux desktop environment built on its own Wayland
compositor.*

> [中文版本 / Chinese version](README_zh_CN.md)

nkdhr is a self-contained desktop environment for Linux. It runs on its own
Wayland compositor, and a control plane brokers every system-level action
(power, network, audio, brightness, session and nkdhr's own configuration)
between the desktop and the underlying system services.

The project is in early development. What exists today is the control plane
and the compositor's display protocol layer; the interaction design that
sits on top of them is being redesigned (see [Status](#status)).
Documentation lives in `docs/` in English and Simplified Chinese.

## Scope

- **Lower boundary**: a stock minimal Fedora Linux installation (Fedora 44
  "Everything", minimal) with nothing added except a working network
  configuration and `git`. nkdhr is meant to take a machine from that state
  to a complete desktop.
- **Upper boundary**: applications at or above the level of a text editor
  are not developed in this project. nkdhr only adapts them: the user
  selects which one to use, and nkdhr generates its configuration once (or
  again on explicit request), never forcing it over the user's own
  customization.
- **In between**, everything is developed in this project: the login
  screen, bar, launcher, notifications, settings, file manager, task
  manager and lock screen are nkdhr's own rather than third-party
  components such as waybar or greetd.

## Status

### Control plane

`nkdhr-ipc`, `nkdhrd` and `nkdhrctl` make up the control plane (Phase 1,
complete). `nkdhrd` is an unprivileged `systemd --user` daemon on the
session D-Bus (`org.nkdhr.*`). It reads battery, network, audio, brightness
and session state from UPower, NetworkManager, PipeWire, the sysfs
backlight and logind; performs mutating actions (brightness, volume and
mute, Wi-Fi connection, power off, reboot, suspend) behind polkit; signals
real changes without polling; and owns nkdhr's configuration: a
schema-validated TOML store under `~/.config/nkdhr/` with get, set and
watch over D-Bus, in which hand edits are re-validated and, when valid,
applied immediately. `nkdhrctl` is its command-line front end. The one
configuration namespace registered today is `outputs`, the compositor's
display arrangement.

### Compositor

`nkdhr-compositor` is nkdhr's Wayland compositor, reduced to the display
protocol layer:

- **Backends**: a nested backend that runs as a window inside an existing
  X11 or Wayland desktop for development, and a TTY backend that drives
  every connected display through DRM/KMS from a local virtual terminal,
  with libseat session handling, udev output hotplug, multi-GPU setups
  (rendering on one GPU, scanout on another) and Ctrl+Alt+F1…F12
  virtual-terminal switching.
- **Clients**: native Wayland clients, and X11 applications through an
  integrated XWayland server.
- **Protocols**: clipboard and primary selection (shared between Wayland
  and X11 in both directions), drag-and-drop, client-side decoration
  negotiation, session locking (`ext-session-lock-v1`; the session stays
  protected if the locker crashes), screen capture
  (`wlr-screencopy-unstable-v1`), fractional scaling (with
  `wp_viewporter`, so applications draw sharp buffers at scales such as
  1.5), pointer constraints, relative pointer motion, touchpad gestures,
  touch input and idle-inhibit requests.
- **Stopping**: `SIGTERM`, `SIGINT` and `SIGHUP` shut it down cleanly,
  removing its Wayland and X11 sockets and lock files and, on a TTY,
  handing the displays and the seat back.
- **Displays**: several displays arranged in one shared logical space
  through `nkdhrd`'s `outputs` namespace (position and scale per
  connector, applied live); displays without an entry are placed side by
  side automatically.
- **Window behaviour**: a deliberately minimal, permanent default. New
  windows open centred on the display under the pointer and take keyboard
  focus; a click focuses and raises a window; applications move and resize
  themselves through their own title bars, and the compositor carries out
  those requests; decorations are always client-side. There is no
  launcher, panel or workspace, and no compositor shortcut besides
  virtual-terminal switching.

The backend and protocol work was verified when the compositor milestones
were accepted on 2026-08-08, including real-TTY operation, physical output
hotplug and an eight-hour stability run; the
[compositor internals](docs/compositor/INTERNALS.md#history) keep the
record.

### Interaction design

The first interaction design was withdrawn on 2026-10-23 and is being
redesigned. It comprised the infinite canvas window model (pannable,
zoomable world coordinates, overview, output groups, pinned widgets,
numbered workspaces and position marks), the shared rendering and UI
toolkit, theming, the motion system, the integrated shell and the settings
application. Its code, documentation and product acceptance matrix were
removed from the main line and are preserved at the git tag
`archive/interaction-v1`:

```sh
git checkout archive/interaction-v1      # the complete previous state
git show archive/interaction-v1:<path>   # one file from it
```

Until a new design is settled, the compositor provides only the minimal
window behaviour described above.

### Crates

| Crate | Role |
|---|---|
| `nkdhr-ipc` | D-Bus interface definitions and configuration types shared by every crate that talks to `nkdhrd` |
| `nkdhrd` | Control-plane daemon |
| `nkdhrctl` | Command-line front end to `nkdhrd` |
| `nkdhr-compositor` | Wayland compositor: the display protocol layer, with nested and DRM/KMS backends |
| `nkdhr-sessiond`, `nkdhr-greet`, `nkdhr-files`, `nkdhr-tasks` | Reserved for the login daemon, login screen, file manager and task manager; not implemented yet |

## Building

nkdhr is developed on Fedora Linux with a stable Rust toolchain (for
example from [rustup](https://rustup.rs)). The workspace links against the
following system libraries; install their development packages first:

```sh
sudo dnf install pipewire-devel clang-devel libinput-devel systemd-devel \
    libseat-devel mesa-libgbm-devel libdrm-devel libxkbcommon-devel
```

Running X11 applications also needs Xwayland at runtime
(`xorg-x11-server-Xwayland`).

```sh
cargo build --workspace                          # every crate; the compositor with its nested backend only
cargo build -p nkdhr-compositor                  # the compositor, nested backend (default)
cargo build -p nkdhr-compositor --features tty   # the compositor, nested and TTY backends
```

Run the compositor nested inside your current desktop, or with the TTY
backend from a local virtual terminal:

```sh
cargo run -p nkdhr-compositor          # nested, inside an X11 or Wayland desktop
target/debug/nkdhr-compositor --tty    # TTY, from a local virtual terminal (built with --features tty)
```

The compositor prints the `WAYLAND_DISPLAY` and `DISPLAY` values that
applications connect to. The [compositor user guide](docs/compositor/USAGE.md)
covers launching applications, arranging displays and leaving a TTY
session. `nkdhrd` runs as a `systemd --user` service; its unit and polkit
policy are in `crates/nkdhrd/resources/`, and there is no installer yet.
The compositor also runs without `nkdhrd` and then arranges displays
automatically.

Before sending a change, run the checks every change has to pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## Documentation

- [Control plane user guide](docs/control-plane/USAGE.md) ·
  [中文](docs/control-plane/USAGE_zh_CN.md)
- [Control plane internals](docs/control-plane/INTERNALS.md) ·
  [中文](docs/control-plane/INTERNALS_zh_CN.md)
- [Compositor user guide](docs/compositor/USAGE.md) ·
  [中文](docs/compositor/USAGE_zh_CN.md)
- [Compositor internals](docs/compositor/INTERNALS.md) ·
  [中文](docs/compositor/INTERNALS_zh_CN.md)
- [Compositor extension seams](docs/compositor/EXTENDING.md) ·
  [中文](docs/compositor/EXTENDING_zh_CN.md)

## License

[PolyForm Noncommercial License 1.0.0](LICENSE.md): use, modification and
redistribution are free for any noncommercial purpose; commercial use is
not permitted.

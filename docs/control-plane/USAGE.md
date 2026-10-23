# Control Plane — User Guide

*[中文版](USAGE_zh_CN.md)*

Covers the `nkdhr-ipc`, `nkdhrd` and `nkdhrctl` crates, referred to
together as "the control plane" (ROADMAP.md Phase 1, CTRL-1 … CTRL-5).

`nkdhrd` and `nkdhrctl` together form the nkdhr control plane: the layer
that brokers every system-level action (power, network, audio, brightness,
session, and nkdhr's own configuration) between the desktop and the
underlying system services. Everything else in nkdhr — the compositor, the
bar, settings, the OSD — talks to the system exclusively through this
layer; nothing in nkdhr reads `/sys` or shells out to `systemctl`,
`nmcli`, etc. directly.

## Starting the daemon

`nkdhrd` runs as a `systemd --user` service, one instance per logged-in
session:

```
systemctl --user status nkdhrd
systemctl --user restart nkdhrd
journalctl --user -u nkdhrd
```

A full nkdhr install enables `nkdhrd` as part of session startup; you
should not normally need to manage it by hand.

There is no installer yet: the unit in `crates/nkdhrd/resources/systemd/`
expects the binary at `/usr/libexec/nkdhrd`. From a source checkout, run
the daemon directly in the login session it should serve instead, with
`cargo run -p nkdhrd` or, after a build, `target/<profile>/nkdhrd`. It then
logs to the terminal it runs in rather than to the journal, and stops with
`Ctrl+C`.

## `nkdhrctl`

`nkdhrctl` is the command-line front end to `nkdhrd`. Every subcommand
talks to the running daemon over D-Bus and exits non-zero with a message on
`stderr` if the daemon is unreachable or the action is rejected.

### Status

```
nkdhrctl ping                # prints "pong" if the daemon is alive
nkdhrctl status              # daemon version, uptime, loaded modules
```

### Reading system state

```
nkdhrctl battery             # charge %, charging state, time remaining
nkdhrctl network             # active connection, signal strength, IP
nkdhrctl audio               # volume, mute state, default sink and source
nkdhrctl brightness          # current brightness %
nkdhrctl session             # session id, seat, idle state, locked state
```

Each command prints human-readable text by default; add `--json` for
machine-readable output (this is what the bar and OSD use internally).

### Mutating the system

```
nkdhrctl brightness set 60          # 0-100
nkdhrctl audio set-volume 45
nkdhrctl audio mute | unmute
nkdhrctl network connect <ssid> --password <pw>   # omit --password for an open network
nkdhrctl power off | reboot | suspend
```

Every mutating command is checked against the corresponding
`org.nkdhr.policy.*` polkit action. If your session is not authorized,
`nkdhrctl` reports the polkit denial and exits non-zero — nothing happens
silently.

### Watching for changes

```
nkdhrctl watch battery       # streams one JSON line per real change, e.g.
                              # unplugging/plugging AC or crossing a charge
                              # threshold
nkdhrctl watch network
nkdhrctl watch audio
nkdhrctl watch brightness
nkdhrctl watch session
```

`watch` never polls: it prints only when `nkdhrd` receives a change signal
from the underlying service, so it is safe to leave running for hours.

### Configuration

nkdhr's own settings (distinct from any individual application's own
settings) live in a single schema-validated store owned by `nkdhrd`:

```
nkdhrctl config get <key>
nkdhrctl config set <key> <value>
nkdhrctl config watch <prefix>
```

Keys are dotted paths: the namespace, then one segment per nested table
down to the value (for example `outputs.eDP-1.scale`). `get` and `set`
address a single value; `watch` takes a prefix — a namespace, a deeper
dotted path, or nothing for every change. The backing files are plain TOML
under `~/.config/nkdhr/`, one per namespace; you may edit them directly
with a text editor — `nkdhrd` detects the change, re-validates it, and
either applies it or rejects it (keeping the last-known-good value active)
with a diagnostic in the journal.

`nkdhrctl config set` only changes values that already exist: an unknown
key is rejected rather than created, so a typo can never add a setting.

#### Output arrangement (`outputs`)

The one namespace registered today is `outputs`, which tells the
compositor where each monitor sits and at what scale.
`~/.config/nkdhr/outputs.toml` holds one table per output, named after its
connector as the kernel reports it (`eDP-1`, `DP-2`, `HDMI-A-1`, ...):

```toml
[eDP-1]
x = 0
y = 0
scale = 1.25

[DP-1]
x = 1536
y = 0
scale = 1.0
```

| Field | Default | Meaning |
|---|---|---|
| `x`, `y` | `0` | The output's top-left corner in the compositor's global logical coordinate space, the space that output positions and the pointer share. Integers; negative values are fine, since only the arrangement relative to the other outputs matters. |
| `scale` | `1.0` | Logical scale: physical pixels per logical pixel. Must be a finite number greater than 0; fractional values such as `1.25` are advertised to applications through the fractional-scale protocol. |

Positions are logical, so an output takes up its mode size divided by its
scale: the 1920×1200 panel above is 1536 logical pixels wide at scale
1.25, which is why `DP-1` starts at `x = 1536`. Any field may be left out
and takes its default, so a bare `[DP-1]` table is a valid entry. An
unknown field, an empty connector name, a name containing `.` or an invalid
scale makes `nkdhrd` reject the whole file. Connector names are not checked
against the connected outputs, so a misspelt table name (`[HDMI-A1]`) is
accepted and simply matches no output; compare it with the name the
compositor prints when the output connects.

Outputs without a table need no configuration: the compositor packs them
automatically, left to right after the configured outputs in
connector-name order, at scale 1.0. A missing or empty `outputs.toml`
therefore puts every output side by side. The compositor's user guide
([`../compositor/USAGE.md`](../compositor/USAGE.md)) describes how it
resolves the layout.

Because `config set` never creates keys, add an output's table by editing
the file; `nkdhrd` picks it up immediately. From then on, each of its
fields can also be changed from the command line:

```
nkdhrctl config set outputs.eDP-1.scale 1.5
nkdhrctl config set outputs.DP-1.x -1920   # a 1920-wide DP-1 left of eDP-1
nkdhrctl config watch outputs
```

`nkdhrd` ignores TOML files in `~/.config/nkdhr/` that belong to no
registered namespace, such as the `canvas.toml` and `theme.toml` written by
earlier nkdhr versions; they can be deleted.

## Troubleshooting

- `nkdhrctl` says "daemon not running": check
  `systemctl --user status nkdhrd` and the journal.
- A mutating command is denied: check
  `pkaction --verbose org.nkdhr.policy.<action>` for the current
  authorization rule; administrators can change these rules in
  `/etc/polkit-1/rules.d/`.
- An edited config file is ignored: `journalctl --user -u nkdhrd` shows the
  validation error that caused the rejection.

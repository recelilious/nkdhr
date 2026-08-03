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

Keys are dotted paths (e.g. `theme.accent-color`, `canvas.pan-speed`, once
those components register their settings — see the note below). The
backing files are plain TOML under `~/.config/nkdhr/`; you may edit them
directly with a text editor — `nkdhrd` detects the change, re-validates it,
and either applies it or rejects it (keeping the last-known-good value
active) with a diagnostic in the journal.

> As of CTRL-5, this store has no settings registered yet: no other
> component has landed a real one to persist. `nkdhrctl config get/set`
> against any key fails with "unknown config namespace" until a later
> milestone (theming, canvas keybindings, ...) adds its own. The mechanism
> itself — validation, rejection, hot-reload — is fully working; try it
> with `nkdhrctl status`, which lists `Config` among the loaded modules.

## Troubleshooting

- `nkdhrctl` says "daemon not running": check
  `systemctl --user status nkdhrd` and the journal.
- A mutating command is denied: check
  `pkaction --verbose org.nkdhr.policy.<action>` for the current
  authorization rule; administrators can change these rules in
  `/etc/polkit-1/rules.d/`.
- An edited config file is ignored: `journalctl --user -u nkdhrd` shows the
  validation error that caused the rejection.

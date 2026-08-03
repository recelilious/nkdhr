# Control Plane — Internals

*[中文版](INTERNALS_zh_CN.md)*

Audience: nkdhr contributors working on `nkdhr-ipc`, `nkdhrd`, or
`nkdhrctl` (ROADMAP.md Phase 1, CTRL-1 … CTRL-5).

## Crates

| Crate | Contents |
|---|---|
| `nkdhr-ipc` | D-Bus interface traits (via `zbus`'s `#[interface]`/`#[proxy]` macros) and the wire types shared between daemon and clients. No behavior lives here — it is the contract, imported by every crate that speaks to `nkdhrd`. |
| `nkdhrd` | The daemon binary: one `zbus::Connection` on the session bus, one Rust module per system service it aggregates, the config store, and the polkit authorization check. |
| `nkdhrctl` | A thin CLI: parses arguments (`clap`), builds the matching `nkdhr-ipc` proxy, makes one call, formats the result (text or `--json`), maps D-Bus errors to exit codes. |

## Bus placement

`nkdhrd` owns `org.nkdhr.Daemon1` on the **session** D-Bus, not the system
bus. Every backend it aggregates is either inherently session-scoped
(PipeWire) or already exposes session-safe, polkit-mediated methods on the
system bus that an unprivileged session can call directly (UPower,
NetworkManager, `logind`). Keeping `nkdhrd` itself unprivileged means it
needs no setuid helper and no root service — the only privileged process in
the whole stack is `nkdhr-sessiond` (SESS-1), which `nkdhrd` never talks
to.

## Object tree

```
/org/nkdhr/Daemon1       org.nkdhr.Daemon1       Ping, GetStatus, GetVersion
/org/nkdhr/Power1        org.nkdhr.Power1        battery + power actions
/org/nkdhr/Network1      org.nkdhr.Network1      NetworkManager wrapper
/org/nkdhr/Audio1        org.nkdhr.Audio1        PipeWire wrapper
/org/nkdhr/Brightness1   org.nkdhr.Brightness1   logind brightness wrapper
/org/nkdhr/Session1      org.nkdhr.Session1      logind session wrapper
/org/nkdhr/Config1       org.nkdhr.Config1       CTRL-5 config store
```

`/org/nkdhr/Commands1` is reserved and stays unclaimed — see EXTENDING.md
(still a staging document: CTRL-EXT itself hasn't been built).

Each module interface exposes, uniformly:

- **`GetStatus()`**, returning that module's whole status struct in one
  call. CTRL-2 shipped this instead of the granular per-field D-Bus
  properties originally sketched here: several modules (Network, Audio)
  only have an answer after chaining multiple backend calls together
  (e.g. active connection → device → access point for Network), so
  "current status" is inherently a compound read — one method matches that
  better than a `GetAll` over independent properties, and it is what
  `org.nkdhr.Daemon1.GetStatus` (CTRL-1) already established.
- **Methods** for mutating actions where the module has any (CTRL-3):
  `Power1.PowerOff()`/`Reboot()`/`Suspend()`, `Brightness1.Set(percent: u8)`,
  `Audio1.SetVolume(percent: u8)`/`SetMute(muted: bool)` (default sink
  only — the default source isn't mutable), `Network1.Connect(ssid: str,
  password: str)` (empty `password` for an open network).
- A `Changed` **signal** per module carrying the new status struct, fired
  only when the underlying backend reports a real change (CTRL-4) — never
  on a timer. `nkdhrctl watch <module>` is a thin loop over the matching
  signal. See "Change watchers (CTRL-4)" below for how each module detects
  "a real change" without polling.

## Backend modules

Each module wraps exactly one system service (or, for Brightness, one
sysfs path) and never touches hardware it doesn't own:

| Module | Backend | Notes |
|---|---|---|
| Power | `org.freedesktop.UPower`'s `DisplayDevice` (battery/AC aggregate) for reads; `org.freedesktop.login1` (`PowerOff`/`Reboot`/`Suspend`) for CTRL-3 actions | |
| Network | `org.freedesktop.NetworkManager`, primary active connection only | Wi-Fi scan/connect only for the base feature; wired/VPN management is out of scope until a later milestone |
| Audio | PipeWire, via the `pipewire` Rust bindings (no D-Bus API exists) — a dedicated worker thread owns PipeWire's main loop (its types are `Rc`-based, not `Send`) and keeps a plain-data cache the module reads synchronously. Tracks both the default sink (playback) and default source (capture) via the same generic per-`DeviceKind` logic: a media-class filter (`Audio/Sink`/`Audio/Source`) plus the matching `default.audio.*` metadata key. | |
| Brightness | `/sys/class/backlight/<device>/{brightness,max_brightness}`, first device found, for reads | CTRL-3's `Set` goes through `logind`'s `SetBrightness` rather than writing sysfs directly, so `nkdhrd` never needs elevated file permissions — reads don't need that broker since sysfs backlight files are world-readable |
| Session | The primary seat's active session — `Manager.GetSeat("seat0")` → `Seat.ActiveSession` → that `Session`'s `Id`/`Seat`/`Active`/`IdleHint`/`LockedHint` | Deliberately *not* `Manager.GetSessionByPID` on `nkdhrd`'s own PID: `nkdhrd` runs as a generic `systemd --user` service and isn't itself a member of any login session, so that lookup would always fail. Multi-seat hardware is out of scope; `seat0` is assumed. |

## Authorization

Every mutating method checks a dedicated polkit action under
`org.nkdhr.policy.*` (installed as
`/usr/share/polkit-1/actions/org.nkdhr.policy.policy`,
`crates/nkdhrd/resources/polkit/org.nkdhr.policy.policy` in the repo)
**before** calling the backend, via
`org.freedesktop.PolicyKit1.Authority.CheckAuthorization` on the system
bus. `nkdhrd` defines its own action for each mutating capability
(`org.nkdhr.policy.power-off`, `...reboot`, `...suspend`,
`...network-connect`, `...brightness-set`, `...audio-set-volume`,
`...audio-set-mute`) rather than relying on each backend's own default
rules, so an administrator has one place (`/etc/polkit-1/rules.d/`) to
control the whole nkdhr surface consistently, independent of what each
backend would allow a caller to do on its own. Defaults mirror logind's own
convention for equivalent actions: the seat's active session may act
without a password (`allow_active: yes`); anyone else needs to
authenticate as an administrator (`auth_admin_keep`).

The polkit **subject** passed to `CheckAuthorization` is a `unix-process`
(PID + start-time from `/proc/<pid>/stat`), resolved from the caller's
D-Bus unique name via `GetConnectionUnixProcessID` **on the session bus
the call itself arrived on** — not a `system-bus-name` subject built
straight from that unique name. A session-bus unique name means nothing
on the system bus (where polkit's `Authority` lives): the two buses assign
unique names independently, so passing one through as if it were valid on
the other resolves against whatever unrelated connection happens to hold
that name on the system bus at the time — sometimes a `NameHasNoOwner`
error, sometimes a real but wrong identity, silently. This was an actual
bug during CTRL-3 development, not a hypothetical: it manifested as
mutating calls succeeding when they should have been denied (and, once
the "wrong identity" happened not to be trusted, as an `Only trusted
callers... can use CheckAuthorization() for subjects belonging to other
identities` error). See `nkdhrd/src/backends/polkit.rs`'s
`check_authorization` doc comment for the fix.

No `AllowUserInteraction` flag is ever set — nkdhr's authorization is
entirely seat-activity-based (see the `.policy` file's `allow_active`),
never an interactive prompt, so the check never blocks on a polkit agent.

## Change watchers (CTRL-4)

Every module's `Changed` signal is emitted by a small background watcher
started right after the daemon's session-bus connection finishes building
(`nkdhrd/src/main.rs`, after `request_name_with_flags`) — not from inside
any `#[interface]` method, since nothing about "the backend changed" is
triggered by an incoming D-Bus call. Each watcher recomputes the module's
whole status and emits `Changed` only when it differs from the last one
emitted, so a backend signal that doesn't actually change anything nkdhr
surfaces (e.g. a UPower property nkdhr doesn't read) produces no D-Bus
traffic.

Three different underlying mechanisms are used, by backend shape:

- **Power, Network, Session** — `org.freedesktop.DBus.Properties.PropertiesChanged`.
  `backends/dbus_properties.rs` is a small, permanent, shared helper: it
  opens a raw `zbus::blocking::Proxy` for the `Properties` interface at a
  given destination/path and returns a blocking iterator over
  `PropertiesChanged`. Callers don't decode the signal's payload (which
  properties changed, to what) — receipt just means "recompute", which is
  simpler than tracking individual properties and, for a whole-status-per-
  module model, exactly as precise. Each of the three watchers picks a
  *stable* object to subscribe to, since the subscription is set up once
  at daemon startup and is not re-established:
  - Power watches UPower's `DisplayDevice` directly — the same object
    `GetStatus()` reads, and its path never changes.
  - Network watches NetworkManager's **root** object only, not the primary
    active connection's own sub-objects (which come and go as connections
    change). This is a deliberate scope decision: it misses a change
    confined entirely to a sub-object (e.g. Wi-Fi signal strength drifting
    with no accompanying state transition), but reliably catches every
    connect/disconnect, since `PrimaryConnection`/`ActiveConnections`/
    `State` all live on the root object and change on every such
    transition.
  - Session watches whichever session was active on the primary seat *when
    the watcher started* (daemon startup). A full session hand-off
    afterwards — fast user switching, or a fresh login on the same seat —
    is not observed until `nkdhrd` restarts, since that would require
    noticing the seat's own `ActiveSession` pointer change and
    re-subscribing to the new session object, which this project's
    single-seat, single-user target does not yet need. `GetStatus()` is
    unaffected: it always re-resolves the active session fresh.
- **Brightness** — `inotify`, watching the backlight device's `brightness`
  file for `IN_MODIFY` directly, since sysfs has no D-Bus signal of its
  own. The kernel backlight driver writes this file (via `sysfs_notify()`)
  on every change regardless of who caused it — `nkdhrd` itself (through
  `logind`'s `SetBrightness`), a hotkey, or another process — so this
  catches every source, not just nkdhr's own writes.
- **Audio** — no separate watcher thread at all. `nkdhrd`'s PipeWire
  connection (`backends/pipewire_client.rs`) is already fully event-driven:
  its `Tracker::reconcile()` runs on every relevant PipeWire event. CTRL-4
  attaches a callback there (`PipeWireHandle::on_change`) that the `Audio`
  module registers once, after the connection is built
  (`modules::audio::attach_watcher`); `reconcile()` invokes it after every
  update, and the callback does the status-diff-and-emit itself. Piggy-
  backing on the existing worker avoids opening a second PipeWire
  connection purely to watch for changes.

**Emitting from outside a dispatched call.** Power/Network/Session/
Brightness's `Changed` emissions above happen from a context zbus's
`#[interface]` macro doesn't hand you a `SignalEmitter` for — a background
thread, not a method call being dispatched. The pattern (documented
directly in `zbus::blocking::ObjectServer::interface`'s own doc example,
not reverse-engineered) is:

```rust
let iface = session.object_server().interface::<_, Power>(POWER_OBJECT_PATH)?;
zbus::block_on(Power::changed(iface.signal_emitter(), status))?;
```

`session.object_server().interface::<_, T>(path)` looks up the *already
registered* object's `InterfaceRef<T>`, whose `signal_emitter()` gives a
`&SignalEmitter` usable outside any dispatch. `T::changed(...)` is the
function the interface's `#[zbus(signal)] async fn changed(signal_emitter:
&SignalEmitter<'_>, status: T) -> zbus::Result<()>;` stub expands to —
using it instead of hand-building the D-Bus call (`emit_signal` with
`"org.nkdhr.Power1"`/`"Changed"` as literal strings) means the interface
and member names can never drift out of sync with the actual interface
definition, which matters given this project has twice already been bitten
by exactly this kind of string/name mismatch (see "zbus proxy pitfalls"
below). `zbus::block_on` bridges the (necessarily async, per zbus's signal
codegen) `changed` function into these watchers' plain blocking-thread
style; it carries none of the "async sandwich" deadlock risk documented
below, since none of these watcher threads are themselves being driven by
the connection they call `block_on` against.

`Config1.Set` (CTRL-5) is the one signal emission that does **not** need
this pattern: `Set` is itself the dispatched call, so its interface method
takes a `#[zbus(signal_emitter)] emitter: SignalEmitter<'_>` parameter
directly and calls `emitter.changed(key, value).await?` — no
`object_server().interface()` lookup needed. See "Config store (CTRL-5)"
below.

**What's been exercised for real vs. only started cleanly.** Brightness's
`inotify` path was verified end to end against the real backlight device
(see PROGRESS.md's CTRL-4 entry). Power, Network, and Session's watchers
were confirmed to subscribe without error and keep the daemon healthy for
an extended period, but triggering a *real* backend change to observe the
resulting `Changed` signal requires either physical hardware access
(AC plug/unplug for Power), is too disruptive to safely attempt from an
SSH-based dev session connected over the very Wi-Fi link that would be
toggled (Network), or is gated by `logind` itself refusing explicit
idle/lock control for `Type=tty` sessions, whose idle state instead comes
from real console input this session has no way to generate remotely
(Session). Whoever next has physical/console access should close this gap.

## Config store (CTRL-5)

**Ships with zero namespaces registered.** None of CTRL-1 … CTRL-4's
built-in modules ended up with a setting that actually needs persisting,
and the namespaces sketched in USAGE.md (`theme`, `canvas`) belong to
phases (UI-4, COMP-3) that haven't designed their real schemas yet —
defining them now would have been speculative, ahead-of-need design. What
CTRL-5 delivers is the generic engine, proven by a throwaway test-only
namespace in `nkdhrd/src/backends/config_store.rs`'s own unit tests (not
shipped) and, during development, by a temporary scratch namespace
registered and exercised live against a running daemon, then removed
before commit. A later phase registers its own namespace by implementing
the `Namespace` trait (`backends::config_store::Namespace` — **in
`nkdhrd`, not `nkdhr-ipc`**: no client has ever needed typed Rust access
to a namespace's shape, only the generic dotted-key `Config1` IPC below,
so the schema trait lives next to the engine that enforces it) on a
`serde`-derived struct and adding a `NamespaceSchema::of::<T>()` entry to
the `static NAMESPACES: &[NamespaceSchema]` list in `nkdhrd/src/main.rs`.

- On disk: TOML files under `~/.config/nkdhr/`, one logical namespace per
  file (e.g. `theme.toml`, `canvas.toml`); `nkdhrd` is the only writer other
  than the user's own editor. A namespace's file always holds its full
  *materialized* state (every field present, defaults filled in) — never a
  sparse diff — because every write, whether from `Config1.Set` or a
  re-validated external edit, goes back through a full deserialize of the
  concrete Rust struct before being re-serialized to disk.
- Schema: each namespace has a versioned schema (a `serde`-derived struct
  with `#[serde(deny_unknown_fields, default)]` plus a `Namespace::validate`
  method for cross-field checks `deny_unknown_fields` can't express);
  unknown keys are rejected, not silently dropped, so typos surface
  immediately, both from `Config1.Set` and from a hand-edited file.
- Watching: `nkdhrd` watches the whole config directory via a single
  `inotify` watch (`IN_CLOSE_WRITE | IN_MOVED_TO`, catching both in-place
  saves and the atomic write-then-rename pattern most editors and
  `nkdhrd`'s own writes use), dispatching on the changed file's name. An
  external edit triggers re-validation of just that namespace; a rejected
  file keeps the daemon's in-memory (last-known-good) value active and
  logs the diagnostic (see USAGE.md's troubleshooting section). `Config1.Set`
  emits `Changed` directly (see "Emitting from outside a dispatched call"
  above), so the watcher reloading its own write afterwards is a
  same-value no-op, not a duplicate signal.
- IPC: `Config1.Get(key) -> Variant`, `Config1.Set(key, Variant)`,
  `Config1.GetAll(prefix) -> {key: Variant}` for bulk reads (used by the
  settings UI in Phase 4), and a `Changed(key, Variant)` signal.
- Value types over IPC: booleans, integers, floats and strings. Arrays and
  nested tables are supported as on-disk TOML structure (a schema can
  nest freely) but not yet as a `Get`/`Set` leaf value or `GetAll` entry —
  no namespace has needed one yet, and extending the conversion in
  `config_store::json_to_variant`/`variant_to_json` is a matter of adding a
  match arm, not a redesign.
- Verification: four unit tests in `config_store.rs` (missing-file
  defaults, set-persists-and-rejects-invalid, external-edit reload with
  before/after diffing, `get_all` flattening) against a test-only
  namespace, plus a live run against the real daemon with a temporary
  scratch namespace exercising the same paths end to end over D-Bus
  (`nkdhrctl config get/set/watch`) — see PROGRESS.md's CTRL-5 entry.

## Single-instance enforcement

`nkdhrd` requests `org.nkdhr.Daemon1` on the session bus at startup with
`RequestName`/`DO_NOT_QUEUE`. A second instance's request fails
immediately; it logs the conflict and exits non-zero rather than queuing
behind the first (this is the CTRL-1 verification criterion).

**Implementation pitfall (zbus 5.18):** `connection::Builder::name()`'s own
doc comment claims `DoNotQueue` is always set, but the shipped code never
adds that flag — a second instance built via `.name(BUS_NAME)` just sits
queued forever instead of failing. `nkdhrd` therefore does not use
`Builder::name()`; it builds the connection with `.serve_at()` only, then
calls `Connection::request_name_with_flags(BUS_NAME,
RequestNameFlags::DoNotQueue.into())` explicitly. Re-check this against the
zbus changelog before any upgrade in case it gets fixed upstream.

## zbus proxy pitfalls hit so far

Real gotchas surfaced while wiring the CTRL-2/CTRL-3/CTRL-4 backend proxies
(`nkdhrd/src/backends/*.rs`), worth checking for again in any new proxy:

- **Acronyms in method names.** zbus's snake_case→PascalCase conversion
  capitalizes each underscore-separated word's first letter only, so Rust's
  `get_session_by_pid` becomes `GetSessionByPid` — not the real D-Bus
  method `GetSessionByPID`. A wrong member name doesn't error clearly: it
  just doesn't match any policy `send_member=` rule, so dbus-broker's
  default-deny swallows it as a generic `AccessDenied`, which looks like a
  permissions problem rather than a typo. Hit **twice** so far
  (`GetSessionByPID` in CTRL-2, `GetConnectionUnixProcessID` in CTRL-3's
  polkit subject resolution — the latter surfaced as `UnknownMethod:
  Invalid method call` instead, since that name genuinely doesn't exist
  under the wrong casing rather than colliding with a policy rule). Any
  method/property whose real D-Bus name has an all-caps run (`PID`,
  `SSID`, `URL`, …) needs an explicit `#[zbus(name = "...")]` override —
  when introducing a call to an unfamiliar interface, check its real
  member names (`busctl introspect`) rather than trusting the conversion.
- **`Optional<bool>` panics.** `zvariant::Optional<T>` encodes absence as
  `T`'s default value, and `bool::default()` (`false`) can't be told apart
  from a real "false" — encoding or decoding `Optional<bool>` panics by
  design (see `zvariant::Optional`'s own doc comment). Any wire field that
  is a nullable boolean needs a different representation (a plain `bool`
  with `false` doubling as the "unknown" sentinel, as in `Audio1`'s
  `muted`, or a small tri-state enum if `false` needs to be told apart from
  "unknown").
- **The "async sandwich" is easy to hit by accident, not just in theory.**
  zbus's own docs warn against calling a *blocking* proxy method against a
  connection from *inside* a call that connection's own object server is
  currently dispatching (the blocking wrapper's internal `block_on` waits
  on that same connection's executor to make progress — which is the
  thread now blocked waiting for it). This is exactly what an early
  CTRL-3 draft did: a non-async `Brightness1::set` used
  `#[zbus(connection)]` to fetch the caller's PID via a *blocking* proxy
  call on that same connection, which wedged the whole daemon (every
  subsequent call on the connection hung, including unrelated `Ping`)
  until restarted. The fix was making the interface method itself `async
  fn` and using the *async* proxy variant (`.await`, not
  `ProxyBlocking::new(...)`) specifically for calls against the connection
  the method arrived on. Calls against a genuinely separate connection
  (like `nkdhrd`'s own `system` bus connection, used throughout via
  `zbus::blocking::Connection`) carry no such risk and can stay blocking
  even from inside an `async fn` — only *same-connection* reentrancy is
  dangerous.

## systemd unit

`nkdhrd.service` is a `systemd --user` unit (`Type=dbus`,
`BusName=org.nkdhr.Daemon1`), started by D-Bus activation or eagerly by the
session-startup target the installer configures. Logs go to the journal
under the `nkdhrd` syslog identifier; there are no separate log files.

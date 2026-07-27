//! Minimal proxy for `org.freedesktop.PolicyKit1.Authority`, used to gate
//! every mutating action `nkdhrd` exposes. See
//! `resources/polkit/org.nkdhr.policy.policy` for the action definitions
//! and their default authorization levels.

use std::collections::HashMap;
use std::fs;

use zbus::blocking::Connection;
use zbus::message::Header;
use zbus::zvariant::Value;

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    async fn check_authorization(
        &self,
        subject: (&str, HashMap<&str, Value<'_>>),
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
}

/// The bus driver interface (`org.freedesktop.DBus`), present identically
/// on every bus. Used here on the **session** bus specifically, to
/// resolve a caller's unique name (meaningful only on that bus) into a
/// PID (meaningful everywhere) before asking the system bus's polkit
/// Authority about it.
#[zbus::proxy(
    interface = "org.freedesktop.DBus",
    default_service = "org.freedesktop.DBus",
    default_path = "/org/freedesktop/DBus"
)]
trait BusDriver {
    // zbus's snake_case-to-PascalCase conversion doesn't capitalize
    // acronyms, so this would otherwise be sent as
    // `GetConnectionUnixProcessId` — not the real
    // `GetConnectionUnixProcessID` — and get refused as `UnknownMethod`.
    // See `nkdhrd/src/backends/logind.rs`'s `GetSessionByPID` for the same
    // pitfall hit earlier.
    #[zbus(name = "GetConnectionUnixProcessID")]
    async fn get_connection_unix_process_id(&self, bus_name: &str) -> zbus::Result<u32>;
}

/// Checks whether the caller identified by `header` is authorized for
/// `action_id`, per the policy installed from
/// `resources/polkit/org.nkdhr.policy.policy`.
///
/// `session_connection` is the (async) connection the call itself arrived
/// on — nkdhrd's own session-bus connection, injected into interface
/// methods via `#[zbus(connection)]`. `header.sender()` is a unique name
/// that only means something on *that* bus. Resolving it into a PID via
/// `session_connection` before asking `system`'s polkit `Authority` about
/// it as a `unix-process` subject is required: an earlier version of this
/// code passed the session-bus unique name straight through as a
/// `system-bus-name` subject, which polkit resolves against the *system*
/// bus's own (unrelated) connections — the two buses assign unique names
/// independently, so this either silently authorized against the wrong
/// identity or failed with `NameHasNoOwner`, depending on what that name
/// happened to belong to on the system bus at the time.
///
/// No `AllowUserInteraction` flag is set: every nkdhr action's implicit
/// authorization is decided by seat activity alone (see the `.policy`
/// file), never by an interactive prompt, so this call never blocks
/// waiting on a polkit agent.
///
/// This is `async` — and specifically uses the *async* proxy, not
/// `BusDriverProxyBlocking` — only for the `session_connection` half of
/// the check. `session_connection` is the same connection nkdhrd's own
/// object server is dispatching this very call on; making a *blocking*
/// call against it from inside a call it is currently dispatching is the
/// textbook "async sandwich" deadlock (zbus's blocking wrappers each run
/// their own `block_on`, which then waits on this same connection's
/// executor to make progress — but that executor is the thread now
/// blocked waiting for it). `system` is a wholly separate connection with
/// its own executor, so blocking on it here carries no such risk.
pub async fn check_authorization(
    system: &Connection,
    session_connection: &zbus::Connection,
    header: &Header<'_>,
    action_id: &str,
) -> zbus::fdo::Result<()> {
    let Some(sender) = header.sender() else {
        return Err(zbus::fdo::Error::AccessDenied(
            "request has no D-Bus sender to authorize".to_owned(),
        ));
    };

    let bus_driver = BusDriverProxy::new(session_connection).await?;
    let pid = bus_driver
        .get_connection_unix_process_id(sender.as_str())
        .await?;
    let start_time = process_start_time(pid).unwrap_or(0);

    let authority = AuthorityProxyBlocking::new(system)?;
    let mut subject_details = HashMap::new();
    subject_details.insert("pid", Value::U32(pid));
    subject_details.insert("start-time", Value::U64(start_time));
    let subject = ("unix-process", subject_details);

    let (authorized, _is_challenge, _details) =
        authority.check_authorization(subject, action_id, HashMap::new(), 0, "")?;

    if authorized {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "not authorized for {action_id}"
        )))
    }
}

/// Reads a process's start time (in clock ticks since boot) from
/// `/proc/<pid>/stat`, field 22. Polkit uses this alongside the PID to
/// guard against a PID being reused by a different process between the
/// caller resolving it and polkit checking it; `0` (this function's
/// fallback) tells polkit that guard can't be applied, rather than
/// silently pinning to the wrong process.
fn process_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The process name (2nd field) is parenthesized and may itself
    // contain spaces or parens, so split on the *last* ')' rather than
    // whitespace to find where the remaining, plain space-separated
    // fields (starting from field 3, "state") begin.
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // `fields[0]` is field 3 ("state"), so field 22 ("starttime") is
    // index 22 - 3 = 19.
    fields.get(19)?.parse().ok()
}

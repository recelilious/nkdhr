//! Minimal proxies for the parts of `org.freedesktop.login1` the Session
//! and Brightness modules need. Not a general logind binding — only what
//! nkdhr reads and the mutating calls it makes.

use zbus::blocking::Connection;
use zbus::zvariant::OwnedObjectPath;

/// The primary physical seat. nkdhr targets single-seat hardware for now;
/// multi-seat support is out of scope until a real need for it appears.
pub const PRIMARY_SEAT: &str = "seat0";

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
pub trait Manager {
    async fn get_seat(&self, seat_id: &str) -> zbus::Result<OwnedObjectPath>;

    /// The `interactive` argument lets `logind` fall back to an
    /// interactive polkit prompt if the caller isn't otherwise
    /// authorized; nkdhr always passes `false` since authorization is
    /// nkdhr's own `org.nkdhr.policy.*` check, decided before this call is
    /// ever made.
    async fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    async fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    async fn suspend(&self, interactive: bool) -> zbus::Result<()>;
}

/// A seat's object path is looked up at runtime via [`ManagerProxyBlocking::get_seat`],
/// so unlike [`ManagerProxyBlocking`] this proxy has no `default_path` and
/// must be built with [`zbus::blocking::proxy::Builder::path`].
#[zbus::proxy(
    interface = "org.freedesktop.login1.Seat",
    default_service = "org.freedesktop.login1"
)]
pub trait Seat {
    #[zbus(property)]
    fn active_session(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

/// A session's object path varies per session, so unlike [`ManagerProxyBlocking`]
/// this proxy has no `default_path` and must be built with
/// [`zbus::blocking::proxy::Builder::path`].
#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
pub trait Session {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn seat(&self) -> zbus::Result<(String, OwnedObjectPath)>;

    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn idle_hint(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;

    fn set_brightness(&self, subsystem: &str, name: &str, value: u32) -> zbus::Result<()>;
}

/// Looks up the primary seat's active session's object path — the shared
/// first step for both the Session module's reads and the Brightness
/// module's `SetBrightness` call.
pub fn active_session_path(system: &Connection) -> zbus::Result<OwnedObjectPath> {
    let manager = ManagerProxyBlocking::new(system)?;
    let seat_path = manager.get_seat(PRIMARY_SEAT)?;

    let seat = SeatProxyBlocking::builder(system)
        .path(seat_path)?
        .build()?;
    let (_, session_path) = seat.active_session()?;

    Ok(session_path)
}

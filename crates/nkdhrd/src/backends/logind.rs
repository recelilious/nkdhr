//! Minimal proxies for the parts of `org.freedesktop.login1` the Session
//! module needs. Not a general logind binding — only what nkdhr reads.

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
}

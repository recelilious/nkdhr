//! Concrete CTRL-5 namespace schemas. Each submodule is one namespace,
//! owned here (not by the component it conceptually belongs to) because
//! the `Namespace` trait it implements lives next to the config-store
//! engine that enforces it (`backends::config_store`) — see that module's
//! doc comment for why. `canvas` is the first real entry; expect siblings
//! (`theme`, ...) as later phases land their own settings.

pub mod canvas;

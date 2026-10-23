//! Concrete CTRL-5 namespace schemas. Each submodule is one namespace,
//! owned here rather than by the component that reads it, because
//! validation is the daemon's job: clients only ever speak the generic
//! dotted-key `Config1` interface, and the `Namespace` trait a schema
//! implements lives next to the engine that enforces it
//! (`backends::config_store`). When a client also has to decode a
//! namespace's values, their types live in `nkdhr-ipc` and the schema here
//! wraps them, as `outputs` does with `nkdhr_ipc::OutputPlacements`.
//!
//! `outputs`, the compositor's multi-output arrangement, is the one
//! namespace today; a component that needs persisted settings adds a
//! sibling module and registers it in `main.rs`'s `NAMESPACES`.

pub mod outputs;

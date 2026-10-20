//! The contract between a host (Electron or Tauri) and the sandbox helper.
//!
//! A host sends a [`PolicyInput`] and a [`RequestInput`] as JSON. They are validated exactly once, into
//! a [`ResolvedPolicy`] and a [`ResolvedRequest`], and backends accept only the resolved forms. So no
//! backend can be handed a relative path, a path with an alternate data stream, an expired grant or an
//! environment variable the policy did not allow: those states cannot be represented after resolving.
//!
//! Resolving checks the paths as they are now. It is not the enforcement. Enforcement is the backend's
//! job, and a backend must report `Unavailable` rather than pretend when it cannot hold the policy.

mod path;
mod policy;
mod request;
mod result;

pub use path::{Access, windows_path_problem};
pub use policy::{NetworkMode, PolicyError, PolicyInput, ResolvedPolicy, SCHEMA_VERSION};
pub use request::{RequestInput, ResolvedRequest};
pub use result::{Denial, Enforcement, ExecutionResult};

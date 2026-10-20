//! Behavioural probes for a sandbox backend. A backend may call itself enforced only when these pass on
//! the host it runs on: expected-allow cases catch a sandbox that blocks too much, expected-deny cases
//! catch one that fails open, and the unsandboxed control proves the probes can see an open door.

pub mod catalog;
pub mod fixture;
pub mod launcher;
pub mod runner;

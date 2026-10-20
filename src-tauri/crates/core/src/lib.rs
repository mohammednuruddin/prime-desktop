//! The desktop app's logic. The Tauri host in `src-tauri` is a thin layer over this crate.

pub mod agent;
pub mod files;
pub mod git;
pub mod kernel_env;
pub mod platform;
pub mod primefiles;
pub mod product;
pub mod runtime;
pub mod sandbox;
pub mod store;
pub mod terminal;
pub mod updater;
pub mod validate;

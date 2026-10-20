//! The renderer's commands, one file per group. Each is thin: check the arguments, call desktop-core, shape the answer.

use serde_json::Value;

pub mod agent;
pub mod files;
pub mod git;
pub mod shell;
pub mod store;
pub mod terminal;
pub mod updater;

pub type CmdResult = Result<Value, String>;

/// A failure as the renderer shows it: the whole cause chain on one line.
pub fn err(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

pub fn ok<T: serde::Serialize>(value: T) -> CmdResult {
    serde_json::to_value(value).map_err(err)
}

/// File and process work that blocks goes off the async runtime.
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work).await.map_err(err)?.map_err(err)
}

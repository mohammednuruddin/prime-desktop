//! The update panel.

use tauri::State;

use super::{CmdResult, ok};
use crate::state::AppState;

/// `window.prime.updateState`
#[tauri::command]
pub async fn update_state(state: State<'_, AppState>) -> CmdResult {
    ok(state.updater.state())
}

/// `window.prime.updateCheck`
#[tauri::command]
pub async fn update_check(state: State<'_, AppState>) -> CmdResult {
    ok(state.updater.check())
}

/// `window.prime.updateInstall`
#[tauri::command]
pub async fn update_install(state: State<'_, AppState>) -> CmdResult {
    state.updater.install()?;
    Ok(serde_json::Value::Null)
}

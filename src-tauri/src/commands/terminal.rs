//! The terminal panel.

use std::path::PathBuf;

use serde_json::Value;
use tauri::State;

use super::{CmdResult, err, ok};
use crate::state::AppState;

fn project_path(state: &AppState, agent_id: &str) -> Result<PathBuf, String> {
    state
        .manager
        .project_path(agent_id)
        .ok_or_else(|| "Project is not available".to_string())
}

fn dimension(value: Option<u32>) -> Option<f64> {
    value.map(f64::from)
}

/// `window.prime.terminalStart`
#[tauri::command]
pub async fn terminal_start(
    state: State<'_, AppState>,
    agent_id: String,
    cols: Option<u32>,
    rows: Option<u32>,
) -> CmdResult {
    let (terminals, path) = (state.terminals.clone(), project_path(&state, &agent_id)?);
    ok(
        tokio::task::spawn_blocking(move || terminals.start(&agent_id, &path, dimension(cols), dimension(rows)))
            .await
            .map_err(err)?
            .map_err(err)?,
    )
}

/// `window.prime.terminalWrite`
#[tauri::command]
pub async fn terminal_write(state: State<'_, AppState>, agent_id: String, data: String) -> CmdResult {
    state.terminals.write(&agent_id, &data);
    Ok(Value::Null)
}

/// `window.prime.terminalResize`
#[tauri::command]
pub async fn terminal_resize(state: State<'_, AppState>, agent_id: String, cols: u32, rows: u32) -> CmdResult {
    state.terminals.resize(&agent_id, f64::from(cols), f64::from(rows));
    Ok(Value::Null)
}

/// `window.prime.terminalRestart`
#[tauri::command]
pub async fn terminal_restart(
    state: State<'_, AppState>,
    agent_id: String,
    cols: Option<u32>,
    rows: Option<u32>,
) -> CmdResult {
    let (terminals, path) = (state.terminals.clone(), project_path(&state, &agent_id)?);
    ok(
        tokio::task::spawn_blocking(move || terminals.restart(&agent_id, &path, dimension(cols), dimension(rows)))
            .await
            .map_err(err)?
            .map_err(err)?,
    )
}

/// `window.prime.terminalClear`
#[tauri::command]
pub async fn terminal_clear(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    state.terminals.clear(&agent_id);
    Ok(Value::Null)
}

/// `window.prime.terminalClose`: closing waits briefly for the shell to die, so it does not run on the async runtime.
#[tauri::command]
pub async fn terminal_close(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    let terminals = state.terminals.clone();
    tokio::task::spawn_blocking(move || terminals.close(&agent_id))
        .await
        .map_err(err)?;
    Ok(Value::Null)
}

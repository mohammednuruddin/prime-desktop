//! The Git panel and the checkpoints.

use std::path::PathBuf;

use desktop_core::git;
use tauri::State;

use super::{CmdResult, err, ok};
use crate::state::AppState;

fn project(state: &AppState, agent_id: &str) -> Result<PathBuf, String> {
    state
        .manager
        .project_path(agent_id)
        .ok_or_else(|| "Project is not available".to_string())
}

/// `window.prime.gitList`
#[tauri::command]
pub async fn git_list(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    ok(state.manager.list_checkpoints(&agent_id).await)
}

/// `window.prime.gitRestore`
#[tauri::command]
pub async fn git_restore(state: State<'_, AppState>, agent_id: String, sha: String) -> CmdResult {
    state.manager.restore_checkpoint(&agent_id, &sha).await.map_err(err)?;
    ok(true)
}

/// `window.prime.gitDiffFiles`
#[tauri::command]
pub async fn git_diff_files(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    ok(git::diff_files(&project(&state, &agent_id)?).await.map_err(err)?)
}

/// `window.prime.gitStatus`
#[tauri::command]
pub async fn git_status(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    ok(git::git_status(&project(&state, &agent_id)?).await.map_err(err)?)
}

/// `window.prime.gitFileDiff`
#[tauri::command]
pub async fn git_file_diff(state: State<'_, AppState>, agent_id: String, path: String, staged: bool) -> CmdResult {
    ok(git::git_file_diff(&project(&state, &agent_id)?, &path, staged)
        .await
        .map_err(err)?)
}

/// `window.prime.gitStage`
#[tauri::command]
pub async fn git_stage(state: State<'_, AppState>, agent_id: String, paths: Vec<String>) -> CmdResult {
    ok(git::stage_files(&project(&state, &agent_id)?, &paths)
        .await
        .map_err(err)?)
}

/// `window.prime.gitUnstage`
#[tauri::command]
pub async fn git_unstage(state: State<'_, AppState>, agent_id: String, paths: Vec<String>) -> CmdResult {
    ok(git::unstage_files(&project(&state, &agent_id)?, &paths)
        .await
        .map_err(err)?)
}

/// `window.prime.gitStageAll`
#[tauri::command]
pub async fn git_stage_all(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    ok(git::stage_all(&project(&state, &agent_id)?).await.map_err(err)?)
}

/// `window.prime.gitUnstageAll`
#[tauri::command]
pub async fn git_unstage_all(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    ok(git::unstage_all(&project(&state, &agent_id)?).await.map_err(err)?)
}

/// `window.prime.gitCommit`
#[tauri::command]
pub async fn git_commit(state: State<'_, AppState>, agent_id: String, message: String) -> CmdResult {
    ok(git::commit_staged(&project(&state, &agent_id)?, &message)
        .await
        .map_err(err)?)
}

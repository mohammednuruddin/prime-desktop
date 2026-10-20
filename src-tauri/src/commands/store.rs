//! Open projects, pinned chats and settings.

use desktop_core::agent::manager::primary_agent_id;
use desktop_core::store::ProjectTab;
use desktop_core::validate;
use serde_json::{Value, json};
use tauri::State;

use super::{CmdResult, blocking, err, ok};
use crate::state::AppState;

/// `window.prime.initial`: everything the window needs to draw itself the first time.
#[tauri::command]
pub async fn state_initial(state: State<'_, AppState>) -> CmdResult {
    let (store, prime) = (state.store.clone(), state.prime.clone());
    // Prime Agent's own settings file is the truth for the sub-agent depth: another Prime tool may have changed it.
    let stored = blocking(move || {
        let current = store
            .state()
            .settings
            .get("rlmMaxDepth")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let depth = prime.read_rlm_max_depth(current);
        if depth != current {
            store.set_settings(&json!({"rlmMaxDepth": depth}))?;
        }
        Ok(store.state())
    })
    .await?;
    state.binary.check().await;
    let mut initial = serde_json::to_value(stored).map_err(err)?;
    initial["binary"] = serde_json::to_value(state.binary.snapshot()).map_err(err)?;
    Ok(initial)
}

/// `window.prime.tabsList`
#[tauri::command]
pub async fn tabs_list(state: State<'_, AppState>) -> CmdResult {
    let current = state.store.state();
    ok(json!({"tabs": current.tabs, "activeTabId": current.active_tab_id}))
}

/// `window.prime.tabAdd`: open a folder as a project and start its agent.
#[tauri::command]
pub async fn tab_add(state: State<'_, AppState>, path: String) -> CmdResult {
    let path = validate::existing_dir("project path", &path).map_err(err)?;
    let wanted = path.to_string_lossy().into_owned();
    let fresh = ProjectTab {
        id: uuid::Uuid::new_v4().to_string(),
        name: desktop_core::agent::shared::base_name(&wanted),
        path: wanted,
    };
    // A folder that is already open is brought to the front, not opened a second time. The change is one step, so
    // another tab command at the same moment cannot be lost.
    let store = state.store.clone();
    let (tab, tabs) = blocking(move || {
        store.update(|stored| {
            let tab = stored
                .tabs
                .iter()
                .find(|tab| tab.path == fresh.path)
                .cloned()
                .unwrap_or_else(|| {
                    stored.tabs.push(fresh.clone());
                    fresh
                });
            stored.active_tab_id = Some(tab.id.clone());
            (tab, stored.tabs.clone())
        })
    })
    .await?;
    state.until_ready().await;
    state.manager.open_tab(&tab.id, path).await.map_err(err)?;
    ok(json!({"tab": tab, "tabs": tabs, "activeTabId": tab.id}))
}

/// `window.prime.tabRemove`
#[tauri::command]
pub async fn tab_remove(state: State<'_, AppState>, tab_id: String) -> CmdResult {
    let tab_id = validate::non_empty("tab id", &tab_id, 200).map_err(err)?;
    let current = state.store.state();
    let tabs: Vec<ProjectTab> = current.tabs.into_iter().filter(|tab| tab.id != tab_id).collect();
    let active = if current.active_tab_id.as_deref() == Some(tab_id.as_str()) {
        tabs.first().map(|tab| tab.id.clone())
    } else {
        current.active_tab_id
    };
    state.manager.close_tab(&tab_id).await;
    let (terminals, closing) = (state.terminals.clone(), primary_agent_id(&tab_id));
    let _ = tokio::task::spawn_blocking(move || terminals.close(&closing)).await;
    let (store, saved, saved_active) = (state.store.clone(), tabs.clone(), active.clone());
    blocking(move || store.set_tabs(saved, saved_active)).await?;
    ok(json!({"tabs": tabs, "activeTabId": active}))
}

/// `window.prime.tabSelect`
#[tauri::command]
pub async fn tab_select(state: State<'_, AppState>, tab_id: String) -> CmdResult {
    let current = state.store.state();
    if !current.tabs.iter().any(|tab| tab.id == tab_id) {
        return Err("Unknown tab".into());
    }
    let (store, selected) = (state.store.clone(), tab_id.clone());
    blocking(move || store.set_tabs(current.tabs, Some(selected))).await?;
    ok(json!({"activeTabId": tab_id}))
}

/// `window.prime.sessionPinsGet`
#[tauri::command]
pub async fn session_pins_get(state: State<'_, AppState>) -> CmdResult {
    ok(state.store.state().pinned_session_files)
}

/// `window.prime.sessionPinSet`
#[tauri::command]
pub async fn session_pin_set(state: State<'_, AppState>, path: String, pinned: bool) -> CmdResult {
    let path = validate::text("session path", &path, 4_096).map_err(err)?.to_string();
    let store = state.store.clone();
    ok(blocking(move || store.set_session_pinned(&path, pinned)).await?)
}

/// `window.prime.settingsGet`
#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> CmdResult {
    ok(state.store.state().settings)
}

/// `window.prime.settingsSet`: saves the patch, then tells every connected chat about the settings it should apply now.
#[tauri::command]
pub async fn settings_set(state: State<'_, AppState>, patch: Value) -> CmdResult {
    // The sandbox mode only changes through `sandbox_set`, which prepares the new mode and restarts the agent with it.
    validate::settings_patch(&patch).map_err(err)?;
    let (store, to_save) = (state.store.clone(), patch.clone());
    let settings = blocking(move || store.set_settings(&to_save)).await?;
    for info in state.manager.all_infos() {
        let Some(id) = info["id"].as_str() else { continue };
        let manager = state.manager.clone();
        let id = id.to_string();
        let patch = patch.clone();
        tokio::spawn(async move {
            if let Some(value) = patch.get("autoCompaction") {
                let _ = manager
                    .run_command(
                        &id,
                        json!({"type": "set_auto_compaction", "enabled": value.as_bool().unwrap_or(false)}),
                    )
                    .await;
            }
            if let Some(value) = patch.get("autoRetry") {
                let _ = manager
                    .run_command(
                        &id,
                        json!({"type": "set_auto_retry", "enabled": value.as_bool().unwrap_or(false)}),
                    )
                    .await;
            }
            if let Some(value) = patch.get("thinkingLevel").and_then(Value::as_str) {
                let _ = manager
                    .run_command(&id, json!({"type": "set_thinking_level", "level": value}))
                    .await;
            }
            if let Some(depth) = patch.get("rlmMaxDepth").and_then(Value::as_u64) {
                let _ = manager
                    .run_command(
                        &id,
                        json!({"type": "set_rlm_max_depth", "maxDepth": depth, "global": true}),
                    )
                    .await;
            }
        });
    }
    ok(settings)
}

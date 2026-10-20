//! Project files, previews and the file context menu.

use std::time::Duration;

use desktop_core::files;
use desktop_core::validate;
use serde_json::{Value, json};
use tauri::menu::{ContextMenu, Menu, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, State, WebviewWindow};
use tokio::sync::oneshot;

use super::shell::pick_file;
use super::{CmdResult, blocking, err, ok};
use crate::state::{AppState, FileMenuTargets, readable_roots};

fn project(state: &AppState, agent_id: &str) -> Result<std::path::PathBuf, String> {
    state
        .manager
        .project_path(agent_id)
        .ok_or_else(|| "Project is not available".to_string())
}

/// `window.prime.artifactChoose`: a file the user picks becomes one the renderer may read.
#[tauri::command]
pub async fn artifact_choose(app: AppHandle, state: State<'_, AppState>) -> CmdResult {
    let Some(picked) = pick_file(&app, None).await? else {
        return Ok(Value::Null);
    };
    let (store, file) = (state.store.clone(), picked.clone());
    let resolved = blocking(move || {
        let resolved = validate::existing_file("file", &file.to_string_lossy())?;
        store.add_preview_file(&resolved)?;
        Ok(resolved)
    })
    .await?;
    ok(resolved.to_string_lossy())
}

/// `window.prime.artifactReadFile`: a file in an open project or one the user picked.
#[tauri::command]
pub async fn artifact_read_file(state: State<'_, AppState>, path: String) -> CmdResult {
    let (projects, picked) = readable_roots(&state.store);
    ok(blocking(move || files::read_preview(&files::previewable_file(&projects, &picked, &path)?)).await?)
}

/// `window.prime.artifactRead`: a file inside the agent's project.
#[tauri::command]
pub async fn artifact_read(state: State<'_, AppState>, agent_id: String, path: String) -> CmdResult {
    let root = project(&state, &agent_id)?;
    ok(blocking(move || files::read_preview(&files::project_file(&root, &path)?)).await?)
}

/// `window.prime.artifactReveal`
#[tauri::command]
pub async fn artifact_reveal(state: State<'_, AppState>, agent_id: String, path: String) -> CmdResult {
    let root = project(&state, &agent_id)?;
    blocking(move || files::reveal(&files::project_file(&root, &path)?)).await?;
    ok(true)
}

/// `window.prime.projectFiles`
#[tauri::command]
pub async fn project_files(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    let Some(root) = state.manager.project_path(&agent_id) else {
        return Ok(json!({"files": [], "truncated": false}));
    };
    ok(tokio::task::spawn_blocking(move || files::project_files(&root))
        .await
        .map_err(err)?)
}

/// `window.prime.fileMenu`: the right-click menu for a file named in the chat or listed in Files. It answers `"open"` when
/// the user wants a preview; the other items act here.
#[tauri::command]
pub async fn file_menu(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    agent_id: String,
    path: String,
) -> CmdResult {
    let root = state.manager.project_path(&agent_id);
    let description = blocking(move || files::file_menu(root.as_deref(), &path)).await?;
    let build = |app: &AppHandle| -> tauri::Result<Menu<tauri::Wry>> {
        let open = MenuItem::with_id(app, "fm-open", "Open in Preview", true, None::<&str>)?;
        let reveal = MenuItem::with_id(
            app,
            "fm-reveal",
            description.reveal_label,
            description.revealable,
            None::<&str>,
        )?;
        let copy = MenuItem::with_id(app, "fm-copy", "Copy Path", true, None::<&str>)?;
        let separator = PredefinedMenuItem::separator(app)?;
        let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&open, &reveal, &separator, &copy];
        let relative = description
            .relative_path
            .is_some()
            .then(|| MenuItem::with_id(app, "fm-copy-rel", "Copy Relative Path", true, None::<&str>))
            .transpose()?;
        if let Some(relative) = &relative {
            items.push(relative);
        }
        Menu::with_items(app, &items)
    };
    let menu = build(&app).map_err(err)?;
    let (choice_tx, choice_rx) = oneshot::channel::<String>();
    let (closed_tx, closed_rx) = oneshot::channel::<()>();
    *state.file_menu.lock().map_err(err)? = Some((
        choice_tx,
        FileMenuTargets {
            full_path: description.full_path.clone(),
            relative_path: description.relative_path.clone(),
        },
    ));
    let owner = window.as_ref().window();
    window
        .run_on_main_thread(move || {
            let _ = menu.popup(owner);
            let _ = closed_tx.send(());
        })
        .map_err(err)?;
    // The popup returns before the chosen item's event does, so give that event a moment to arrive.
    let _ = closed_rx.await;
    let chosen = tokio::time::timeout(Duration::from_millis(400), choice_rx)
        .await
        .ok()
        .and_then(Result::ok);
    state.file_menu.lock().map_err(err)?.take();
    Ok(if chosen.as_deref() == Some("open") {
        json!("open")
    } else {
        Value::Null
    })
}

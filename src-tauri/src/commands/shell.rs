//! Windows, dialogs, links and the few things that belong to the desktop rather than to a project.

use desktop_core::validate;
use serde_json::{Value, json};
use tauri::menu::ContextMenu;
use tauri::{AppHandle, LogicalPosition, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use super::{CmdResult, blocking, err, ok};
use crate::state::AppState;

/// The folder the user picks, or `None` if they cancel.
pub async fn pick_folder(app: &AppHandle) -> Result<Option<std::path::PathBuf>, String> {
    let app = app.clone();
    let picked = tokio::task::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
        .await
        .map_err(err)?;
    picked.map(|path| path.into_path().map_err(err)).transpose()
}

pub async fn pick_file(
    app: &AppHandle,
    filter: Option<(&'static str, &'static [&'static str])>,
) -> Result<Option<std::path::PathBuf>, String> {
    let app = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        let mut dialog = app.dialog().file();
        if let Some((name, extensions)) = filter {
            dialog = dialog.add_filter(name, extensions);
        }
        dialog.blocking_pick_file()
    })
    .await
    .map_err(err)?;
    picked.map(|path| path.into_path().map_err(err)).transpose()
}

/// `window.prime.menuPopup`: the named menu, popped up under the button that asked for it. Only names from the template
/// are accepted.
#[tauri::command]
pub async fn menu_popup(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    label: String,
    x: f64,
    y: f64,
) -> CmdResult {
    let name = validate::non_empty("menu name", &label, 40).map_err(err)?;
    let (x, y) = (
        validate::number("menu x", x, None, None).map_err(err)?,
        validate::number("menu y", y, None, None).map_err(err)?,
    );
    let zoom = *state.zoom.lock().map_err(err)?;
    let target = crate::menu::menus(&app)
        .map_err(err)?
        .into_iter()
        .find(|menu| menu.text().ok().as_deref() == Some(name.as_str()));
    let Some(menu) = target else { return Ok(Value::Null) };
    // The page measures CSS pixels and the popup takes window pixels, which differ once the page is zoomed.
    let position = LogicalPosition::new((x * zoom).round(), (y * zoom).round());
    let owner = window.as_ref().window();
    let popup = owner.clone();
    window
        .run_on_main_thread(move || {
            let _ = menu.popup_at(popup, position);
        })
        .map_err(err)?;
    Ok(Value::Null)
}

/// `window.prime.chooseFolder`
#[tauri::command]
pub async fn choose_folder(app: AppHandle) -> CmdResult {
    Ok(pick_folder(&app)
        .await?
        .map_or(Value::Null, |path| json!(validate::simplify(path))))
}

/// `window.prime.chooseFile`
#[tauri::command]
pub async fn choose_file(app: AppHandle, state: State<'_, AppState>) -> CmdResult {
    let picked = pick_file(&app, Some(("Sessions", &["jsonl", "html"]))).await?;
    Ok(picked.map_or(Value::Null, |path| {
        let path = validate::simplify(path);
        state.picks.add(&path.to_string_lossy());
        json!(path)
    }))
}

/// `window.prime.chooseSave`
#[tauri::command]
pub async fn choose_save(app: AppHandle, state: State<'_, AppState>, default_name: Option<String>) -> CmdResult {
    let name = default_name.unwrap_or_else(|| "session.html".into());
    let name = validate::non_empty("file name", &name, 260).map_err(err)?;
    let picked = tokio::task::spawn_blocking(move || app.dialog().file().set_file_name(name).blocking_save_file())
        .await
        .map_err(err)?;
    Ok(picked
        .map(|path| path.into_path().map_err(err))
        .transpose()?
        .map_or(Value::Null, |path| {
            let path = validate::simplify(path);
            state.picks.add(&path.to_string_lossy());
            json!(path)
        }))
}

/// `window.prime.revealInFinder`: a file in an open project, or one the user picked.
#[tauri::command]
pub async fn reveal(state: State<'_, AppState>, path: String) -> CmdResult {
    let (projects, picked) = crate::state::readable_roots(&state.store);
    blocking(move || {
        let file = desktop_core::files::previewable_file(&projects, &picked, &path)?;
        desktop_core::files::reveal(&file)
    })
    .await?;
    Ok(Value::Null)
}

/// `window.prime.openExternal`: web links only.
#[tauri::command]
pub async fn open_external(app: AppHandle, url: String) -> CmdResult {
    let url = validate::safe_external_url(&url).map_err(err)?;
    app.opener().open_url(url, None::<&str>).map_err(err)?;
    Ok(Value::Null)
}

/// `window.prime.quit`
#[tauri::command]
pub async fn app_quit(app: AppHandle) -> CmdResult {
    app.exit(0);
    Ok(Value::Null)
}

/// `window.prime.changelog`
#[tauri::command]
pub async fn app_changelog(state: State<'_, AppState>) -> CmdResult {
    let dir = state.resource_dir.clone();
    ok(desktop_core::primefiles::changelog_text(&[dir.as_path()]))
}

/// `window.prime.logsReveal`: shows the agent log in the file manager and says where the folder is.
#[tauri::command]
pub async fn app_logs(state: State<'_, AppState>) -> CmdResult {
    let logs = state.prime.logs_dir();
    let target = logs.join("agent.jsonl");
    let shown = logs.clone();
    blocking(move || {
        // The file may not exist yet; the folder is the next best thing to show.
        desktop_core::files::reveal(if target.exists() { &target } else { &shown })
    })
    .await
    .ok();
    ok(logs.to_string_lossy())
}

/// `window.prime.windowFullscreen`: `on`, `off`, or toggle.
#[tauri::command]
pub async fn window_fullscreen(window: WebviewWindow, mode: Option<String>) -> CmdResult {
    let now = window.is_fullscreen().map_err(err)?;
    let want = match mode.as_deref() {
        Some("on") => true,
        Some("off") => false,
        _ => !now,
    };
    window.set_fullscreen(want).map_err(err)?;
    ok(want)
}

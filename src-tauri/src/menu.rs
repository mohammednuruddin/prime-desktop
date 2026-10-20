//! The application menus. Windows draws its own top bar, which asks for a menu by name and gets it popped up under the
//! button; the other systems show the menus in the system menu bar. Both come from the one template here.

use serde_json::Value;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager as _, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::state::{AppState, emit};
use crate::window;

type Result<T> = tauri::Result<T>;

fn item(app: &AppHandle, id: &str, label: &str, accelerator: Option<&str>) -> Result<MenuItem<Wry>> {
    MenuItem::with_id(app, id, label, true, accelerator)
}

fn edit(app: &AppHandle) -> Result<Submenu<Wry>> {
    Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )
}

fn view(app: &AppHandle) -> Result<Submenu<Wry>> {
    let reload = item(app, "view-reload", "Reload", Some("CmdOrCtrl+R"))?;
    let devtools = item(
        app,
        "view-devtools",
        "Toggle Developer Tools",
        Some("CmdOrCtrl+Shift+I"),
    )?;
    let reset = item(app, "zoom-reset", "Actual Size", Some("CmdOrCtrl+0"))?;
    let zoom_in = item(app, "zoom-in", "Zoom In", Some("CmdOrCtrl+="))?;
    let zoom_out = item(app, "zoom-out", "Zoom Out", Some("CmdOrCtrl+-"))?;
    let fullscreen = item(app, "view-fullscreen", "Toggle Full Screen", Some("F11"))?;
    let separator = PredefinedMenuItem::separator(app)?;
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![&reload];
    // Developer tools are a development aid; a release build has none to open.
    if cfg!(debug_assertions) {
        items.push(&devtools);
    }
    items.extend([
        &separator as &dyn tauri::menu::IsMenuItem<Wry>,
        &reset,
        &zoom_in,
        &zoom_out,
        &separator,
        &fullscreen,
    ]);
    Submenu::with_items(app, "View", true, &items)
}

/// The menus by name: File, Edit, View and Help on Windows; the app menu, Edit, View and Window elsewhere.
pub fn menus(app: &AppHandle) -> Result<Vec<Submenu<Wry>>> {
    let open_folder = item(app, "open-folder", "Open Folder…", Some("CmdOrCtrl+O"))?;
    let updates = item(app, "check-updates", "Check for Updates…", None)?;
    let separator = PredefinedMenuItem::separator(app)?;
    if cfg!(windows) {
        return Ok(vec![
            Submenu::with_items(
                app,
                "File",
                true,
                &[&open_folder, &separator, &PredefinedMenuItem::quit(app, None)?],
            )?,
            edit(app)?,
            view(app)?,
            Submenu::with_items(
                app,
                "Help",
                true,
                &[&PredefinedMenuItem::about(app, None, None)?, &updates],
            )?,
        ]);
    }
    let name = app.package_info().name.clone();
    Ok(vec![
        Submenu::with_items(
            app,
            name,
            true,
            &[
                &PredefinedMenuItem::about(app, None, None)?,
                &updates,
                &separator,
                &open_folder,
                &separator,
                &PredefinedMenuItem::hide(app, None)?,
                &PredefinedMenuItem::hide_others(app, None)?,
                &PredefinedMenuItem::show_all(app, None)?,
                &separator,
                &PredefinedMenuItem::quit(app, None)?,
            ],
        )?,
        edit(app)?,
        view(app)?,
        Submenu::with_items(
            app,
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(app, None)?,
                &PredefinedMenuItem::maximize(app, None)?,
            ],
        )?,
    ])
}

/// The whole bar, for the systems that show one.
pub fn bar(app: &AppHandle) -> Result<Menu<Wry>> {
    let menus = menus(app)?;
    let refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = menus
        .iter()
        .map(|menu| menu as &dyn tauri::menu::IsMenuItem<Wry>)
        .collect();
    Menu::with_items(app, &refs)
}

const ZOOM_STEP: f64 = 1.1;
const ZOOM_RANGE: (f64, f64) = (0.5, 3.0);

fn zoom(app: &AppHandle, change: impl FnOnce(f64) -> f64) {
    let (Some(window), Some(state)) = (window::main_window(app), app.try_state::<AppState>()) else {
        return;
    };
    let mut current = state.zoom.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let next = (change(*current).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1) * 100.0).round() / 100.0;
    if window.set_zoom(next).is_ok() {
        *current = next;
    }
}

/// The right-click menu on a file: the choice goes back to the command waiting on it, and the items that act on the file
/// act here.
fn file_choice(app: &AppHandle, id: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Some((answer, targets)) = state
        .file_menu
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    else {
        return;
    };
    let _ = answer.send(id.trim_start_matches("fm-").to_string());
    match id {
        "fm-reveal" => {
            let path = targets.full_path;
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(error) = desktop_core::files::reveal(&path) {
                    eprintln!("prime-desktop: could not show {}: {error:#}", path.display());
                }
            });
        }
        "fm-copy" => {
            let _ = app
                .clipboard()
                .write_text(targets.full_path.to_string_lossy().into_owned());
        }
        "fm-copy-rel" => {
            if let Some(relative) = targets.relative_path {
                let _ = app.clipboard().write_text(relative);
            }
        }
        _ => {}
    }
}

/// What a chosen menu item does. The edit items and Quit are the system's own and never reach here.
pub fn handle(app: &AppHandle, id: &str) {
    match id {
        "open-folder" => emit(app, "menu:open-folder", Value::Null),
        "check-updates" => {
            if let Some(state) = app.try_state::<AppState>() {
                state.updater.check();
            }
            emit(app, "menu:open-settings", Value::from("about"));
        }
        "view-reload" => {
            if let Some(window) = window::main_window(app) {
                let _ = window.eval("window.location.reload()");
            }
        }
        #[cfg(debug_assertions)]
        "view-devtools" => {
            if let Some(window) = window::main_window(app) {
                window.open_devtools();
            }
        }
        "view-fullscreen" => {
            if let Some(window) = window::main_window(app) {
                let _ = window.set_fullscreen(!window.is_fullscreen().unwrap_or(false));
            }
        }
        "zoom-in" => zoom(app, |current| current * ZOOM_STEP),
        "zoom-out" => zoom(app, |current| current / ZOOM_STEP),
        "zoom-reset" => zoom(app, |_| 1.0),
        other if other.starts_with("fm-") => file_choice(app, other),
        _ => {}
    }
}

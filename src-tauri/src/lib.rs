//! Prime Desktop's Tauri host: one window on the React renderer, and a thin command layer over `desktop-core`.
//! Every command a renderer calls is named after its IPC channel and lives in `commands/<group>.rs`.

mod commands;
mod menu;
mod runtime;
mod state;
mod window;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use desktop_core::agent::binary::BinaryManager;
use desktop_core::agent::manager::{Manager, ManagerConfig, Outbound, Settings};
use desktop_core::primefiles::PrimeFiles;
use desktop_core::store::{Store, electron_state_file};
use desktop_core::terminal::{Event as TerminalEvent, Terminals};
use desktop_core::updater::Updater;
use desktop_core::{platform, sandbox};
use serde_json::{Value, json};
use state::{AppState, emit};
use tauri::{AppHandle, Manager as _, RunEvent, WindowEvent};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::watch;

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(error) = app.notification().builder().title(title).body(body).show() {
        eprintln!("prime-desktop: could not show a notification: {error}");
    }
}

/// A chat finished a run. When the window is not in front, say so (a notification, and a count on the dock icon where the
/// system has one), as the user is somewhere else.
fn completed(app: &AppHandle, agent_id: &str, name: &str) {
    let focused = window::main_window(app).is_none_or(|window| window.is_focused().unwrap_or(true));
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if focused {
        return;
    }
    let settings = state.store.state().settings;
    let on = |key: &str| settings.get(key).and_then(Value::as_bool).unwrap_or(false);
    if on("notifications") {
        notify(app, name, "Agent finished working");
    }
    if on("dockBadge") {
        let count = {
            let mut unseen = lock(&state.unseen);
            unseen.insert(agent_id.to_string());
            unseen.len()
        };
        set_badge(app, Some(count as i64));
    }
}

fn set_badge(app: &AppHandle, count: Option<i64>) {
    #[cfg(target_os = "macos")]
    if let Some(window) = window::main_window(app) {
        let _ = window.set_badge_count(count);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, count);
}

fn build_state(app: &AppHandle) -> anyhow::Result<AppState> {
    let store = Arc::new(Store::open(platform::state_file()));
    // A moved data folder is a test or a trial, not the user's own, so it does not take the Electron build's projects.
    if std::env::var_os("PRIME_DESKTOP_DATA_DIR").is_none() {
        match store.import_electron_state(&electron_state_file()) {
            Ok(true) => eprintln!("prime-desktop: imported the Electron build's tabs and settings"),
            Ok(false) => {}
            Err(error) => eprintln!("prime-desktop: could not import the Electron build's state: {error:#}"),
        }
    }
    let mode = runtime::chosen_mode(&store.state().settings);
    let daemon_env = Arc::new(std::sync::Mutex::new(runtime::initial_env(mode)));
    let binary = Arc::new(BinaryManager::new({
        let app = app.clone();
        Arc::new(move |change| {
            emit(&app, "events", json!({"type": "binary_state", "payload": change}));
        })
    }));
    let sink: Arc<dyn Fn(Outbound) + Send + Sync> = {
        let app = app.clone();
        Arc::new(move |outbound| match outbound {
            Outbound::Event(event) => emit(&app, "events", event),
            Outbound::Toast(toast) => emit(&app, "toasts", toast),
            Outbound::Notify { title, body } => notify(&app, &title, &body),
            Outbound::Completion { agent_id, name } => completed(&app, &agent_id, &name),
        })
    };
    let manager = Manager::new(
        ManagerConfig {
            binary: {
                let binary = binary.clone();
                Arc::new(move || binary.binary())
            },
            socket: platform::daemon_socket(),
            daemon_env: {
                let env = daemon_env.clone();
                Arc::new(move || lock(&env).clone())
            },
            settings: {
                let store = store.clone();
                Arc::new(move || Settings::from_value(&Value::Object(store.state().settings)))
            },
        },
        sink,
    );
    let terminals = Arc::new(Terminals::new({
        let app = app.clone();
        Arc::new(move |event| match event {
            TerminalEvent::Data(data) => emit(&app, "terminal:data", json!(data)),
            TerminalEvent::Exit(exit) => emit(&app, "terminal:exit", json!(exit)),
        })
    }));
    Ok(AppState {
        store,
        prime: Arc::new(PrimeFiles::from_env()),
        manager,
        binary,
        terminals,
        updater: Arc::new(Updater::new(env!("CARGO_PKG_VERSION"))),
        resource_dir: app.path().resource_dir().unwrap_or_default(),
        file_menu: Default::default(),
        zoom: std::sync::Mutex::new(1.0),
        sandbox: std::sync::Mutex::new(sandbox::SandboxState::preparing(mode)),
        daemon_env,
        ready: watch::channel(false).0,
        unseen: Default::default(),
        picks: Default::default(),
        preparing: Default::default(),
    })
}

/// A project folder can be renamed or removed outside the app. A tab for a folder that is gone could never start its
/// agent, and every command on it would fail, so it is dropped before the window shows tabs.
fn drop_missing_tabs(state: &AppState) {
    let stored = state.store.state();
    let tabs: Vec<_> = stored
        .tabs
        .iter()
        .filter(|tab| std::path::Path::new(&tab.path).is_dir())
        .cloned()
        .collect();
    if tabs.len() == stored.tabs.len() {
        return;
    }
    let active = stored
        .active_tab_id
        .filter(|id| tabs.iter().any(|tab| &tab.id == id))
        .or_else(|| tabs.first().map(|tab| tab.id.clone()));
    if let Err(error) = state.store.set_tabs(tabs, active) {
        eprintln!("prime-desktop: could not drop the tabs of missing folders: {error:#}");
    }
}

/// Prepares the sandbox, then opens every saved project at once (a slow start for one must not hold up the others).
async fn start_agents(app: AppHandle) {
    let state = app.state::<AppState>();
    if state.binary.snapshot().status == "found" {
        runtime::prepare(&app).await;
    }
    // Without Prime Agent installed nothing can run; the install screen's button installs it and prepares the sandbox.
    state.until_ready().await;
    let mut opening = tokio::task::JoinSet::new();
    for tab in state.store.state().tabs {
        let app = app.clone();
        opening.spawn(async move {
            let state = app.state::<AppState>();
            if let Err(error) = state.manager.open_tab(&tab.id, PathBuf::from(&tab.path)).await {
                eprintln!("prime-desktop: could not restore {}: {error:#}", tab.path);
            }
        });
    }
    while opening.join_next().await.is_some() {}
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // The manager starts background tasks as it is built, which needs a runtime; this thread is not one of its own.
    let runtime = tauri::async_runtime::handle();
    let _in_runtime = runtime.inner().enter();
    app.manage(build_state(app)?);
    let state = app.state::<AppState>();
    tauri::async_runtime::block_on(state.binary.check());
    drop_missing_tabs(&state);
    app.set_menu(menu::bar(app)?)?;
    let window = window::create(app)?;
    // Windows draws the menus itself in the page's top bar. The native bar stays hidden but keeps the shortcuts.
    #[cfg(windows)]
    let _ = window.hide_menu();
    #[cfg(not(windows))]
    let _ = window;
    tauri::async_runtime::spawn(start_agents(app.clone()));
    Ok(())
}

static SHUT_DOWN: AtomicBool = AtomicBool::new(false);

/// Closes the chats and the terminals. Reached from more than one exit path, and only the first does the work.
fn shut_down(app: &AppHandle) {
    if SHUT_DOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(state) = app.try_state::<AppState>() {
        // A moment, no more: a daemon that does not answer must not hold the app open.
        let _ = tauri::async_runtime::block_on(tokio::time::timeout(
            std::time::Duration::from_secs(2),
            state.manager.release_idle_extras(),
        ));
        state.manager.shutdown_all();
        state.terminals.shutdown_all();
    }
}

pub fn run() {
    let mut builder = tauri::Builder::default();
    // A second copy would race the first for the agent's socket and leave every window with a dead agent, so a second
    // launch brings the first to the front instead. This plugin has to come first.
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::focus(app)
        }));
    }
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::store::state_initial,
            commands::store::tabs_list,
            commands::store::tab_add,
            commands::store::tab_remove,
            commands::store::tab_select,
            commands::store::session_pins_get,
            commands::store::session_pin_set,
            commands::store::settings_get,
            commands::store::settings_set,
            commands::shell::menu_popup,
            commands::shell::choose_folder,
            commands::shell::reveal,
            commands::shell::open_external,
            commands::shell::app_quit,
            commands::shell::choose_file,
            commands::shell::choose_save,
            commands::shell::app_changelog,
            commands::shell::app_logs,
            commands::shell::window_fullscreen,
            commands::agent::binary_get,
            commands::agent::binary_install,
            commands::agent::agent_create,
            commands::agent::agent_state,
            commands::agent::agent_command,
            commands::agent::agent_messages,
            commands::agent::agent_stats,
            commands::agent::agent_sessions,
            commands::agent::agent_resume,
            commands::agent::agent_session_delete,
            commands::agent::agent_commands,
            commands::agent::agent_harness,
            commands::agent::fleet_observe,
            commands::agent::external_list,
            commands::agent::external_message,
            commands::agent::models_custom_get,
            commands::agent::models_custom_set,
            commands::agent::models_custom_remove,
            commands::agent::models_openrouter_refresh,
            commands::agent::fleet_tree,
            commands::agent::fleet_messages,
            commands::agent::fleet_unobserve,
            commands::agent::fleet_send,
            commands::agent::fleet_schedules,
            commands::agent::fleet_schedule_add,
            commands::agent::fleet_schedule_cancel,
            commands::agent::fleet_heartbeat,
            commands::agent::fleet_heartbeat_action,
            commands::agent::dashboard_spend,
            commands::agent::dashboard_models,
            commands::agent::autonomy_get,
            commands::agent::autonomy_set,
            commands::agent::skills_list,
            commands::agent::skills_install,
            commands::agent::dialog_respond,
            commands::agent::auth_list,
            commands::agent::auth_set,
            commands::agent::auth_remove,
            commands::agent::auth_open_tui,
            commands::agent::rlm_get,
            commands::agent::rlm_set,
            commands::agent::session_export_html,
            commands::agent::session_share,
            commands::agent::sandbox_state,
            commands::agent::sandbox_set,
            commands::terminal::terminal_start,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_restart,
            commands::terminal::terminal_clear,
            commands::terminal::terminal_close,
            commands::git::git_list,
            commands::git::git_restore,
            commands::git::git_diff_files,
            commands::git::git_status,
            commands::git::git_file_diff,
            commands::git::git_stage,
            commands::git::git_unstage,
            commands::git::git_stage_all,
            commands::git::git_unstage_all,
            commands::git::git_commit,
            commands::files::artifact_choose,
            commands::files::artifact_read,
            commands::files::artifact_read_file,
            commands::files::artifact_reveal,
            commands::files::project_files,
            commands::files::file_menu,
            commands::updater::update_state,
            commands::updater::update_check,
            commands::updater::update_install,
        ])
        .on_menu_event(|app, event| menu::handle(app, event.id().as_ref()))
        .on_window_event(|window, event| {
            if let WindowEvent::Focused(true) = event {
                let app = window.app_handle();
                if let Some(state) = app.try_state::<AppState>()
                    && !lock(&state.unseen).is_empty()
                {
                    lock(&state.unseen).clear();
                    set_badge(app, None);
                }
            }
        })
        .setup(|app| setup(app.handle()))
        .build(tauri::generate_context!())
        .expect("error while building the Tauri host");
    app.run(|app, event| match event {
        // macOS keeps an app running with no window; the others quit with their last window.
        #[cfg(target_os = "macos")]
        RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => {
            if let Err(error) = window::create(app) {
                eprintln!("prime-desktop: could not reopen the window: {error}");
            }
        }
        RunEvent::Exit => shut_down(app),
        _ => {}
    });
}

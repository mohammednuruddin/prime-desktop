//! Readies the agent to run code: its Python environment and the sandbox around it. The work is `desktop_core::runtime`;
//! this reports it to the window and keeps the result where the daemon launcher reads it.

use std::sync::atomic::{AtomicU32, Ordering};

use desktop_core::runtime::{self, Paths};
use desktop_core::sandbox::{self, Mode, SandboxState};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager as _};

use crate::state::{AppState, emit};

/// The sandbox mode the user chose. Anything but an explicit `unprotected` is protected.
pub fn chosen_mode(settings: &serde_json::Map<String, Value>) -> Mode {
    Mode::from_setting(settings.get("sandbox").and_then(Value::as_str))
}

/// What the daemon starts with before the sandbox has been prepared: closed when the user wants protection, so a chat
/// opened too early cannot run code outside it.
pub fn initial_env(mode: Mode) -> Vec<(String, String)> {
    match mode {
        Mode::Protected => {
            sandbox::unavailable(
                mode,
                &desktop_core::platform::app_data_dir().join("sandbox"),
                String::new(),
                None,
            )
            .env
        }
        Mode::Unprotected => Vec::new(),
    }
}

fn publish(app: &AppHandle, next: SandboxState) {
    let state = app.state::<AppState>();
    *state.sandbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = next.clone();
    emit(
        app,
        "events",
        json!({"agentId": "", "type": "sandbox_state", "payload": next}),
    );
}

/// Prepares the environment and the sandbox for the chosen mode, shows each step on the install screen, and then lets the
/// daemon start with the result. Takes minutes the first time (Python and its packages are downloaded) and seconds after.
pub async fn prepare(app: &AppHandle) {
    let state = app.state::<AppState>();
    // One at a time: two would build the same folders and the later would overwrite the other's result. The mode is read
    // once the turn comes, so the last choice wins.
    let _one_at_a_time = state.preparing.lock().await;
    let mode = chosen_mode(&state.store.state().settings);
    if mode == Mode::Protected {
        // The old environment may be an unprotected one. Until this finishes, and if it fails, nothing runs outside the
        // sandbox.
        *state.daemon_env.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = initial_env(mode);
    }
    publish(app, SandboxState::preparing(mode));
    let paths = Paths::standard(&state.manager.binary());
    let binary = state.binary.clone();
    let steps = AtomicU32::new(0);
    let prepared = tokio::task::spawn_blocking(move || {
        let prepared = runtime::prepare(mode, &paths, &|step| {
            // The number of steps is not known up front, so the bar creeps towards the end instead of claiming a fraction.
            let done = steps.fetch_add(1, Ordering::SeqCst) + 1;
            binary.setup(Some(step), (0.9 + 0.02 * f64::from(done)).min(0.99));
        });
        binary.setup(None, 1.0);
        prepared
    })
    .await;
    let next = match prepared {
        Ok(prepared) => {
            *state.daemon_env.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = prepared.env;
            prepared.state
        }
        // The work panicked. The closed environment stays in place for a protected choice.
        Err(error) => SandboxState {
            mode: mode.name(),
            enforcement: "unavailable",
            backend: None,
            limitations: Vec::new(),
            reason: Some(format!("Preparing the sandbox failed: {error}")),
        },
    };
    publish(app, next);
    state.ready.send_replace(true);
}

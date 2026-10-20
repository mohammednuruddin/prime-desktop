//! What the commands share: the long-lived services, built once at start-up and managed by Tauri.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use desktop_core::agent::binary::BinaryManager;
use desktop_core::agent::manager::Manager;
use desktop_core::primefiles::PrimeFiles;
use desktop_core::sandbox::SandboxState;
use desktop_core::store::Store;
use desktop_core::terminal::Terminals;
use desktop_core::updater::Updater;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, watch};

/// The answer a context menu gives, once the menu event arrives. A popup returns before the chosen item's event does.
pub type MenuChoice = oneshot::Sender<String>;

pub struct AppState {
    pub store: Arc<Store>,
    pub prime: Arc<PrimeFiles>,
    pub manager: Manager,
    pub binary: Arc<BinaryManager>,
    pub terminals: Arc<Terminals>,
    pub updater: Arc<Updater>,
    /// Where the bundled resources are (the changelog, the MCP catalog).
    pub resource_dir: PathBuf,
    /// The context menu that is open, if any.
    pub file_menu: Mutex<Option<(MenuChoice, FileMenuTargets)>>,
    /// The page's zoom factor, which the View menu changes and menu positions need.
    pub zoom: Mutex<f64>,
    /// What the sandbox is doing, and what it can and cannot hold. See `runtime.rs`.
    pub sandbox: Mutex<SandboxState>,
    /// Added to the daemon's environment each time it starts. Until the sandbox is prepared this holds the closed
    /// environment, so a chat opened early cannot run code unprotected.
    pub daemon_env: Arc<Mutex<Vec<(String, String)>>>,
    /// Becomes true once the environment above is the prepared one. Chats are not opened before.
    pub ready: watch::Sender<bool>,
    /// Chats that finished while the window was out of focus, for the badge.
    pub unseen: Mutex<HashSet<String>>,
    /// Files the user chose in a system dialog.
    pub picks: Picks,
    /// One preparation of the sandbox at a time: they provision the same folders and set the same daemon environment.
    pub preparing: tokio::sync::Mutex<()>,
}

/// Files the user chose in a system dialog (a place to save a chat, a chat to import). The agent writes or reads them
/// even outside the open project, but only because the user named them there, not because the page asked.
#[derive(Default)]
pub struct Picks(Mutex<Vec<String>>);

impl Picks {
    const KEPT: usize = 16;

    fn key(path: &str) -> String {
        let simple = path.replace('/', "\\");
        if cfg!(windows) {
            simple.to_lowercase()
        } else {
            path.to_string()
        }
    }

    pub fn add(&self, path: &str) {
        let mut picks = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        picks.retain(|known| known != &Self::key(path));
        picks.push(Self::key(path));
        let extra = picks.len().saturating_sub(Self::KEPT);
        picks.drain(..extra);
    }

    pub fn has(&self, path: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&Self::key(path))
    }
}

impl AppState {
    /// Waits until the sandbox has been prepared (or has failed to be): the first run builds Python, which takes minutes.
    pub async fn until_ready(&self) {
        let mut ready = self.ready.subscribe();
        // The sender lives as long as the state, so this ends only when the value is true.
        let _ = ready.wait_for(|ready| *ready).await;
    }
}

/// What the file context menu's items act on.
#[derive(Clone)]
pub struct FileMenuTargets {
    pub full_path: PathBuf,
    pub relative_path: Option<String>,
}

pub fn emit(app: &AppHandle, channel: &str, payload: Value) {
    if let Err(error) = app.emit(channel, payload) {
        eprintln!("prime-desktop: could not send `{channel}` to the window: {error}");
    }
}

/// The project folders and the picked files the renderer may read, from the store.
pub fn readable_roots(store: &Store) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let state = store.state();
    (
        state.tabs.iter().map(|tab| PathBuf::from(&tab.path)).collect(),
        state.preview_files.iter().map(PathBuf::from).collect(),
    )
}

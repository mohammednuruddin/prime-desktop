//! Finding, checking and installing the Prime Agent executable the app runs.
//!
//! Only three places count: the copy shipped beside the app, the app-managed install of the pinned release, and a
//! `PRIME_AGENT_PATH` override for development. The shipped copy is the pinned release too, checked against its hashes when
//! the installer was built and signed with the app's own signature, which a machine that blocks unknown programs (Smart App
//! Control) accepts where it refuses the downloaded one. `PATH` is not searched: a `prime-agent` earlier on `PATH` is not the build the app was tested with, and
//! running whatever sits there is not something to do on the user's behalf.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use super::install::{Fetch, Https, install_pinned};
use super::release;
use crate::platform;

/// The renderer's `BinaryState`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BinaryState {
    /// `checking`, `found`, `installing`, `installed` or `error`.
    pub status: &'static str,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// What the app is doing while the status is `installing`, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

pub struct BinaryManager {
    state: Mutex<BinaryState>,
    on_change: Arc<dyn Fn(BinaryState) + Send + Sync>,
    fetch: Arc<dyn Fetch + Send + Sync>,
    install_dir: PathBuf,
    /// The folder of the agent shipped beside the app, when there is one.
    bundled_dir: Option<PathBuf>,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl BinaryManager {
    pub fn new(on_change: Arc<dyn Fn(BinaryState) + Send + Sync>) -> Self {
        let mut manager = Self::with(on_change, Arc::new(Https), release::install_dir());
        manager.bundled_dir = release::bundled_dir();
        manager
    }

    /// With the agent shipped beside the app in `dir` (tests bring their own).
    pub fn with_bundled(mut self, dir: PathBuf) -> Self {
        self.bundled_dir = Some(dir);
        self
    }

    /// With the download and the install folder chosen by the caller (tests bring their own).
    pub fn with(
        on_change: Arc<dyn Fn(BinaryState) + Send + Sync>,
        fetch: Arc<dyn Fetch + Send + Sync>,
        install_dir: PathBuf,
    ) -> Self {
        Self {
            state: Mutex::new(BinaryState {
                status: "checking",
                path: None,
                version: None,
                error: None,
                progress: None,
                stage: None,
            }),
            on_change,
            fetch,
            install_dir,
            bundled_dir: None,
        }
    }

    pub fn snapshot(&self) -> BinaryState {
        locked(&self.state).clone()
    }

    fn change(&self, change: impl FnOnce(&mut BinaryState)) {
        let snapshot = {
            let mut state = locked(&self.state);
            change(&mut state);
            state.clone()
        };
        (self.on_change)(snapshot);
    }

    /// Looks for a usable executable and records what it found.
    pub async fn check(&self) {
        self.change(|state| state.status = "checking");
        match self.find_local_install().await {
            Some((path, version)) => self.change(|state| {
                state.path = Some(path.to_string_lossy().into_owned());
                state.version = Some(version);
                state.status = "found";
                state.error = None;
            }),
            None => self.change(|state| {
                state.status = "error";
                state.error = Some("Prime Agent is not installed.".into());
            }),
        }
    }

    /// The executable and its reported version, if one is usable. An explicit override comes first, then the pinned
    /// install; each must exist and answer `--version`.
    async fn find_local_install(&self) -> Option<(PathBuf, String)> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(path) = locked(&self.state).path.clone() {
            candidates.push(PathBuf::from(path));
        }
        if let Some(path) = std::env::var_os("PRIME_AGENT_PATH").filter(|path| !path.is_empty()) {
            candidates.push(PathBuf::from(path));
        }
        if let Some(dir) = &self.bundled_dir {
            candidates.push(release::executable_in(dir));
        }
        candidates.push(release::executable_in(&self.install_dir));
        for candidate in candidates {
            if candidate.is_file()
                && let Some(version) = version_of(&candidate).await
            {
                return Some((candidate, version));
            }
        }
        None
    }

    /// Puts the pinned Prime Agent in place: a download checked against hashes committed in this app. It does not run
    /// Prime Agent's own installer, which also uninstalls other Prime Agent installs.
    pub async fn install(&self) {
        self.change(|state| {
            state.status = "installing";
            state.progress = Some(0.0);
        });
        let Some(asset) = release::current_asset() else {
            self.change(|state| {
                state.status = "error";
                state.progress = None;
                state.error = Some(format!(
                    "There is no Prime Agent build for {} on {}.",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ));
            });
            return;
        };
        let (fetch, dir, report) = (self.fetch.clone(), self.install_dir.clone(), self.on_change.clone());
        let progress_state = Arc::new(Mutex::new(locked(&self.state).clone()));
        let installed = tokio::task::spawn_blocking({
            let progress_state = progress_state.clone();
            move || {
                install_pinned(asset, &dir, fetch.as_ref(), &|fraction| {
                    let snapshot = {
                        let mut state = locked(&progress_state);
                        state.progress = Some(fraction * 0.9);
                        state.clone()
                    };
                    report(snapshot);
                })
            }
        })
        .await;
        let outcome = match installed {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        match outcome {
            Err(message) => self.change(|state| {
                state.status = "error";
                state.progress = None;
                state.error = Some(message);
            }),
            Ok(_) => match self.find_local_install().await {
                Some((path, version)) => self.change(|state| {
                    state.path = Some(path.to_string_lossy().into_owned());
                    state.version = Some(version);
                    state.status = "installed";
                    state.progress = Some(1.0);
                    state.error = None;
                }),
                None => self.change(|state| {
                    state.status = "error";
                    state.progress = None;
                    state.error = Some("Prime Agent was installed but could not be started.".into());
                }),
            },
        }
    }

    /// Shows the setup that follows an install (the Python environment, the sandbox) on the same screen: the status is
    /// `installing` with the stage in words, then it goes back to what it was.
    pub fn setup(&self, stage: Option<&str>, progress: f64) {
        self.change(|state| match stage {
            Some(stage) => {
                state.status = "installing";
                state.stage = Some(stage.to_string());
                state.progress = Some(progress);
            }
            None => {
                if state.status == "installing" {
                    state.status = if state.path.is_some() { "installed" } else { "error" };
                }
                state.stage = None;
                state.progress = None;
            }
        });
    }

    /// The executable to start. Falls back to the pinned install's path even when it is not there yet, so the error a
    /// failed start shows names a real location.
    pub fn binary(&self) -> PathBuf {
        match locked(&self.state).path.clone().map(PathBuf::from) {
            Some(path) if path.is_file() => path,
            _ => match self.bundled_dir.as_deref().map(release::executable_in) {
                Some(bundled) if bundled.is_file() => bundled,
                _ => release::executable_in(&self.install_dir),
            },
        }
    }
}

/// What `<binary> --version` prints, within ten seconds.
async fn version_of(binary: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    platform::quiet_async(&mut command);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn nothing_installed_is_an_error_not_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let manager = BinaryManager::with(
            Arc::new(move |state| sink.lock().unwrap().push(state.status)),
            Arc::new(Https),
            dir.path().join("0.10.0"),
        );
        // SAFETY: no other test in this binary reads or writes this variable.
        unsafe { std::env::remove_var("PRIME_AGENT_PATH") };
        manager.check().await;
        let state = manager.snapshot();
        assert_eq!(state.status, "error");
        assert_eq!(state.error.as_deref(), Some("Prime Agent is not installed."));
        assert_eq!(*seen.lock().unwrap(), ["checking", "error"]);
        assert_eq!(
            manager.binary(),
            release::executable_in(&dir.path().join("0.10.0")),
            "a real location for the error to name"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_executable_that_answers_version_is_found() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("0.10.0");
        std::fs::create_dir_all(&install).unwrap();
        let exe = release::executable_in(&install);
        std::fs::write(&exe, "#!/bin/sh\necho 0.10.0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let changes = Arc::new(AtomicUsize::new(0));
        let counter = changes.clone();
        let manager = BinaryManager::with(
            Arc::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(Https),
            install,
        );
        // SAFETY: as above.
        unsafe { std::env::remove_var("PRIME_AGENT_PATH") };
        manager.check().await;
        let state = manager.snapshot();
        assert_eq!(state.status, "found");
        assert_eq!(state.version.as_deref(), Some("0.10.0"));
        assert_eq!(manager.binary(), exe);
        assert_eq!(changes.load(Ordering::SeqCst), 2);

        // Something that does not answer is not usable.
        std::fs::write(&exe, "#!/bin/sh\nexit 3\n").unwrap();
        assert!(version_of(&exe).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_agent_shipped_with_the_app_is_used_before_a_downloaded_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = |folder: &Path, version: &str| {
            std::fs::create_dir_all(folder).unwrap();
            let exe = release::executable_in(folder);
            std::fs::write(&exe, format!("#!/bin/sh\necho {version}\n")).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            exe
        };
        let shipped = program(&dir.path().join("agent"), "shipped");
        program(&dir.path().join("0.10.0"), "downloaded");
        let manager = BinaryManager::with(Arc::new(|_| {}), Arc::new(Https), dir.path().join("0.10.0"))
            .with_bundled(dir.path().join("agent"));
        // SAFETY: as above.
        unsafe { std::env::remove_var("PRIME_AGENT_PATH") };
        assert_eq!(
            manager.binary(),
            shipped,
            "before any check, the shipped copy is the one to start"
        );
        manager.check().await;
        let state = manager.snapshot();
        assert_eq!(state.version.as_deref(), Some("shipped"));
        assert_eq!(state.path.as_deref(), Some(shipped.to_str().unwrap()));
    }

    #[tokio::test]
    async fn a_failed_install_says_why_and_clears_the_progress() {
        struct Refuse;
        impl Fetch for Refuse {
            fn download(&self, _: &str, _: &Path, _: u64, _: &dyn Fn(f64)) -> anyhow::Result<()> {
                anyhow::bail!("no network")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let manager = BinaryManager::with(Arc::new(|_| {}), Arc::new(Refuse), dir.path().join("0.10.0"));
        manager.install().await;
        let state = manager.snapshot();
        if release::current_asset().is_none() {
            assert_eq!(state.status, "error");
            return;
        }
        assert_eq!(state.status, "error");
        assert_eq!(state.error.as_deref(), Some("no network"));
        assert_eq!(state.progress, None);
    }
}

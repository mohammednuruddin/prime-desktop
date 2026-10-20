//! Getting the agent ready to run code: the Python environment its kernel uses, and the sandbox that kernel runs in.
//!
//! Both are the app's own and live in its data folder. This is the one place that decides what the daemon is started with,
//! so the host only has to call [`prepare`] and keep the result.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::kernel_env::{self, KernelEnv};
use crate::platform;
use crate::sandbox::{self, Mode, Prepared, SandboxState};

/// Where everything is.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The agent release's `prime-agent-runtime` folder, which the environment is built from.
    pub runtime_dir: PathBuf,
    pub venv: PathBuf,
    pub sandbox_dir: PathBuf,
    /// The sandbox helper, which ships beside the app.
    pub helper: PathBuf,
    pub uv: Option<PathBuf>,
}

impl Paths {
    /// The places for the agent executable at `binary`. `PRIME_DESKTOP_SANDBOX_HELPER` points at another helper (development).
    pub fn standard(binary: &Path) -> Self {
        let name = if cfg!(windows) {
            "desktop-sandbox.exe"
        } else {
            "desktop-sandbox"
        };
        let helper = std::env::var_os("PRIME_DESKTOP_SANDBOX_HELPER")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
            })
            .unwrap_or_else(|| PathBuf::from(name));
        Self {
            runtime_dir: binary
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("prime-agent-runtime"),
            venv: kernel_env::venv_dir(),
            sandbox_dir: platform::app_data_dir().join("sandbox"),
            helper,
            uv: kernel_env::find_uv(),
        }
    }
}

fn environment(paths: &Paths, report: &dyn Fn(&str)) -> Result<KernelEnv> {
    let identity = kernel_env::identity(&paths.runtime_dir, cfg!(windows))?;
    if kernel_env::is_ready(&paths.venv, &identity) {
        return KernelEnv::at(&paths.venv);
    }
    let uv = paths
        .uv
        .as_ref()
        .ok_or_else(|| anyhow!("uv, which builds it, was not found"))?;
    kernel_env::provision(uv, &paths.venv, &paths.runtime_dir, report)
}

/// The environment, then the sandbox, for the chosen mode. Blocking: a first run downloads Python and its packages and
/// tests the sandbox, which takes minutes. `report` hears each step before it starts.
///
/// Without a Python environment a protected kernel cannot start, and nothing falls back to running unprotected. A user who
/// chose unprotected is left with the agent's own way of building its environment, and is told why the app's did not work.
pub fn prepare(mode: Mode, paths: &Paths, report: &dyn Fn(&str)) -> Prepared {
    match environment(paths, report) {
        Ok(env) => sandbox::prepare(mode, &paths.helper, &paths.sandbox_dir, &env, report),
        Err(error) => {
            let reason = format!("The Python environment for running code could not be set up: {error:#}");
            match mode {
                Mode::Protected => sandbox::unavailable(mode, &paths.sandbox_dir, reason, None),
                Mode::Unprotected => Prepared {
                    env: Vec::new(),
                    state: SandboxState {
                        mode: mode.name(),
                        enforcement: "unenforced",
                        backend: None,
                        limitations: Vec::new(),
                        reason: Some(reason),
                    },
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths {
            runtime_dir: dir.join("prime-agent-runtime"),
            venv: dir.join("venv"),
            sandbox_dir: dir.join("sandbox"),
            helper: dir.join("no-such-helper"),
            uv: None,
        }
    }

    fn kernel_python(prepared: &Prepared) -> PathBuf {
        PathBuf::from(
            &prepared
                .env
                .iter()
                .find(|(key, _)| key == "PRIME_AGENT_KERNEL_PYTHON")
                .expect("the kernel is always pointed somewhere")
                .1,
        )
    }

    #[test]
    fn without_a_python_environment_a_protected_kernel_does_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let prepared = prepare(Mode::Protected, &paths(dir.path()), &|_| {});
        assert_eq!(prepared.state.enforcement, "unavailable");
        assert!(
            prepared
                .state
                .reason
                .as_deref()
                .unwrap()
                .contains("could not be set up")
        );
        assert!(
            !kernel_python(&prepared).exists(),
            "pointed at nothing, never at a plain Python"
        );
    }

    #[test]
    fn an_unprotected_choice_keeps_the_agents_own_environment_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let prepared = prepare(Mode::Unprotected, &paths(dir.path()), &|_| {});
        assert_eq!(prepared.state.enforcement, "unenforced");
        assert_eq!(prepared.state.mode, "unprotected");
        assert!(prepared.env.is_empty());
        assert!(
            prepared
                .state
                .reason
                .as_deref()
                .unwrap()
                .contains("could not be set up")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_ready_environment_is_reused_without_uv() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::create_dir_all(&paths.runtime_dir).unwrap();
        std::fs::write(
            paths.runtime_dir.join("pyproject.toml"),
            "[project]\nname='prime-agent-runtime'\n",
        )
        .unwrap();
        let identity = kernel_env::identity(&paths.runtime_dir, false).unwrap();
        std::fs::create_dir_all(paths.venv.join("bin")).unwrap();
        std::fs::write(paths.venv.join("bin").join("python"), "").unwrap();
        std::fs::write(paths.venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::fs::write(
            paths.venv.join(".desktop-kernel.json"),
            serde_json::json!({ "identity": identity }).to_string(),
        )
        .unwrap();
        let prepared = prepare(Mode::Protected, &paths, &|_| {});
        // It got as far as the sandbox, which then found no helper: the environment was not the problem.
        assert!(prepared.state.reason.as_deref().unwrap().contains("helper is missing"));
    }
}

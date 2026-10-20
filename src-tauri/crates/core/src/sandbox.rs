//! The sandbox helper wiring: provision the kernel, write the policy, attest, and report the enforcement state.
//!
//! The agent's model runs its code in one Python kernel. In protected mode that kernel is started through the sandbox
//! helper, which is copied to a file named `desktop-kernel` and set as the agent's kernel interpreter. The helper then runs
//! the real interpreter inside the operating system's sandbox, and refuses to start anything unless this computer has
//! passed the helper's own probe suite. There is no silent fallback: if protection cannot be had the kernel does not start,
//! and the state says why. Running without protection is a separate mode the user chooses, and it is always shown.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::kernel_env::KernelEnv;
use crate::platform;

/// What the user chose: the model's code runs in the sandbox, or it runs with the user's own access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Protected,
    Unprotected,
}

impl Mode {
    /// The `sandbox` setting. Anything but an explicit `unprotected` is protected: a typo must not lower the protection.
    pub fn from_setting(value: Option<&str>) -> Self {
        if value == Some("unprotected") {
            Mode::Unprotected
        } else {
            Mode::Protected
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Protected => "protected",
            Mode::Unprotected => "unprotected",
        }
    }
}

/// The renderer's view of the sandbox.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SandboxState {
    /// `protected` or `unprotected`: what the user chose.
    pub mode: &'static str,
    /// `preparing`, `enforced`, `unattested`, `unenforced` or `unavailable`: what is actually true.
    pub enforcement: &'static str,
    pub backend: Option<String>,
    /// What the sandbox cannot hold even when it enforces the rest. Shown to the user.
    pub limitations: Vec<String>,
    /// Why it is not enforced, in words.
    pub reason: Option<String>,
}

impl SandboxState {
    pub fn preparing(mode: Mode) -> Self {
        Self {
            mode: mode.name(),
            enforcement: "preparing",
            backend: None,
            limitations: Vec::new(),
            reason: None,
        }
    }
}

/// What the daemon needs to start with, and what to tell the user.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub env: Vec<(String, String)>,
    pub state: SandboxState,
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Every folder the interpreter is reached through: the environment, each hop of its symlink chain, and the install it
/// finally lands in. A link that points through a folder name the sandbox cannot see would dangle, so the policy names them
/// all, as the host would.
pub fn interpreter_roots(env: &KernelEnv) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![env.venv.clone()];
    let mut current = env.python.clone();
    for _ in 0..8 {
        let Ok(target) = std::fs::read_link(&current) else {
            break;
        };
        let real = if target.is_absolute() {
            target
        } else {
            current.parent().unwrap_or(Path::new("")).join(target)
        };
        if let Some(root) = real.parent().and_then(Path::parent) {
            roots.push(root.to_path_buf());
        }
        current = real;
    }
    let base = if cfg!(windows) { &env.base_python } else { &env.python };
    if let Ok(real) = std::fs::canonicalize(base) {
        if let Some(root) = real
            .parent()
            .and_then(Path::parent)
            .filter(|root| root.parent().is_some())
        {
            roots.push(root.to_path_buf());
        }
        // On Windows the interpreter sits directly in its install folder.
        if cfg!(windows)
            && let Some(dir) = real.parent()
        {
            roots.push(dir.to_path_buf());
        }
    }
    let mut seen = std::collections::HashSet::new();
    roots
        .into_iter()
        .map(crate::validate::simplify)
        .filter(|root| seen.insert(root.clone()))
        .collect()
}

/// The policy the kernel runs under. The working folder and the session folder are added by the helper per session; nothing
/// else on the computer is readable or writable, and there is no network.
pub fn kernel_policy(env: &KernelEnv, windows: bool) -> Value {
    let mut allowlist = vec![
        "PATH",
        "HOME",
        "LANG",
        "TERM",
        "PYTHONDONTWRITEBYTECODE",
        "PRIME_AGENT_SESSION_DIR",
    ];
    if windows {
        allowlist.extend(["SystemRoot", "PYTHONPATH"]);
    }
    let mut policy = json!({
        "schemaVersion": 1,
        "id": "kernel",
        "label": "Agent kernel",
        "readRoots": interpreter_roots(env),
        "network": "none",
        "envAllowlist": allowlist,
        "maxProcesses": 64,
    });
    if windows {
        // The agent's runtime and pandas import ctypes, whose module cannot start in a container without GUI access. The
        // helper lists that among its limitations.
        policy["allowGui"] = json!(true);
    }
    policy
}

/// The environment the daemon is started with so that its kernel goes through the helper.
pub fn protected_env(
    wrapper: &Path,
    policy: &Path,
    state_dir: &Path,
    env: &KernelEnv,
    windows: bool,
) -> Vec<(String, String)> {
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let mut vars = vec![
        ("PRIME_AGENT_KERNEL_PYTHON".to_string(), text(wrapper)),
        (
            "PRIME_DESKTOP_REAL_PYTHON".to_string(),
            text(if windows { &env.base_python } else { &env.python }),
        ),
        ("PRIME_DESKTOP_KERNEL_POLICY".to_string(), text(policy)),
        ("PRIME_DESKTOP_SANDBOX_STATE_DIR".to_string(), text(state_dir)),
    ];
    if windows {
        vars.push(("PRIME_DESKTOP_KERNEL_PYTHONPATH".to_string(), text(&env.site_packages)));
    }
    vars
}

/// What the helper says about a backend on this computer.
#[derive(Debug, Clone, PartialEq)]
pub struct Capability {
    pub available: bool,
    pub attested: bool,
}

fn helper_output(helper: &Path, state_dir: &Path, args: &[&str], timeout: Duration) -> Result<std::process::Output> {
    let mut command = std::process::Command::new(helper);
    command
        .args(args)
        .env("PRIME_DESKTOP_SANDBOX_STATE_DIR", state_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::quiet(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot start {}", helper.display()))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            bail!(
                "{} {} took longer than {} s",
                helper.display(),
                args.join(" "),
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `capabilities` for the `mxc` backend.
pub fn capability(helper: &Path, state_dir: &Path) -> Result<Capability> {
    let output = helper_output(helper, state_dir, &["capabilities"], Duration::from_secs(30))?;
    let list: Value =
        serde_json::from_slice(&output.stdout).context("the sandbox helper's capabilities are not JSON")?;
    let mxc = list
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["backend"] == "mxc"))
        .context("the sandbox helper reports no `mxc` backend")?;
    Ok(Capability {
        available: mxc["available"] == true,
        attested: mxc["attested"] == true,
    })
}

/// Runs the helper's probe suite on this computer and records that it passed. Takes seconds to a minute.
pub fn attest(helper: &Path, state_dir: &Path) -> Result<()> {
    let output = helper_output(
        helper,
        state_dir,
        &["attest", "--backend", "mxc"],
        Duration::from_secs(300),
    )?;
    if output.status.success() {
        return Ok(());
    }
    // The helper reports a failure as `{"error": …}` on its stdout, and a panic or a failing case on its stderr.
    let reported = serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|answer| answer["error"].as_str().map(str::to_string));
    let text = String::from_utf8_lossy(&output.stderr);
    bail!(
        "{}",
        reported.unwrap_or_else(|| text
            .trim()
            .lines()
            .last()
            .unwrap_or("the sandbox probe suite did not pass on this computer")
            .to_string())
    )
}

/// What the helper says a sandboxed command is limited by, from one trivial run under the kernel's own policy.
pub fn limitations(helper: &Path, state_dir: &Path, work: &Path, policy: &Value, env: &KernelEnv) -> Vec<String> {
    let run = || -> Result<Vec<String>> {
        std::fs::create_dir_all(work)?;
        let mut policy = policy.clone();
        policy["writeRoots"] = json!([work]);
        let files = work.join("probe");
        std::fs::create_dir_all(&files)?;
        let (policy_file, request_file) = (files.join("policy.json"), files.join("request.json"));
        std::fs::write(&policy_file, policy.to_string())?;
        let python = if cfg!(windows) { &env.base_python } else { &env.python };
        let environment = if cfg!(windows) {
            json!({"SystemRoot": std::env::var("SystemRoot").unwrap_or_default()})
        } else {
            json!({})
        };
        let request = json!({"argv": [python, "-c", "pass"], "cwd": work, "pty": false, "env": environment});
        std::fs::write(&request_file, request.to_string())?;
        let output = helper_output(
            helper,
            state_dir,
            &[
                "run",
                "--policy",
                &policy_file.to_string_lossy(),
                "--request",
                &request_file.to_string_lossy(),
            ],
            Duration::from_secs(60),
        )?;
        let result: Value = serde_json::from_slice(&output.stdout)?;
        let Some(list) = result["limitations"].as_array() else {
            bail!(
                "the helper answered {}",
                result.to_string().chars().take(300).collect::<String>()
            );
        };
        Ok(list.iter().filter_map(Value::as_str).map(str::to_string).collect())
    };
    // A failure to read them is itself worth showing: "no limitations" would be a claim nobody checked.
    run().unwrap_or_else(|error| vec![format!("What this sandbox cannot hold could not be read: {error:#}")])
}

/// Whether the kernel's interpreter starts and imports the agent's runtime through the wrapper, with everything in place.
/// This is the whole path the daemon will use, run once before the daemon is told to.
pub fn smoke_test(wrapper: &Path, vars: &[(String, String)], work: &Path) -> Result<()> {
    std::fs::create_dir_all(work)?;
    let mut command = std::process::Command::new(wrapper);
    command
        .args(["-c", "import rlm; print('kernel-ok')"])
        .current_dir(work)
        .envs(vars.iter().map(|(key, value)| (key, value)))
        .env("PRIME_AGENT_SESSION_DIR", work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::quiet(&mut command);
    let output = command
        .output()
        .with_context(|| format!("cannot start {}", wrapper.display()))?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("kernel-ok") {
        return Ok(());
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    bail!(
        "the kernel does not start inside the sandbox: {}",
        text.trim().lines().last().unwrap_or("no output")
    )
}

/// Copies the helper to the kernel wrapper's name when it is missing or different.
pub fn install_wrapper(helper: &Path, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let wrapper = dir.join(exe_name("desktop-kernel"));
    let same = crate::agent::release::sha256_file(&wrapper)
        .ok()
        .zip(crate::agent::release::sha256_file(helper).ok())
        .is_some_and(|(a, b)| a == b);
    if !same {
        // A running kernel keeps the old file open on Windows, so write beside it and move into place.
        let staged = dir.join(format!("{}.new", exe_name("desktop-kernel")));
        std::fs::copy(helper, &staged)?;
        if let Err(error) = std::fs::rename(&staged, &wrapper) {
            let _ = std::fs::remove_file(&staged);
            return Err(error).context(
                "cannot replace the kernel wrapper while a kernel is running; close the agents and try again",
            );
        }
    }
    Ok(wrapper)
}

/// A sandbox that cannot run: the kernel is pointed at a program that does not exist, so nothing runs unprotected.
pub fn unavailable(mode: Mode, dir: &Path, reason: String, backend: Option<String>) -> Prepared {
    Prepared {
        env: vec![(
            "PRIME_AGENT_KERNEL_PYTHON".to_string(),
            dir.join(exe_name("desktop-kernel-unavailable"))
                .to_string_lossy()
                .into_owned(),
        )],
        state: SandboxState {
            mode: mode.name(),
            enforcement: "unavailable",
            backend,
            limitations: Vec::new(),
            reason: Some(reason),
        },
    }
}

/// Gets everything in place for the chosen mode and reports what is true. Blocking: it may provision, attest and run the
/// kernel once, which takes from a second to a few minutes.
pub fn prepare(mode: Mode, helper: &Path, dir: &Path, env: &KernelEnv, report: &dyn Fn(&str)) -> Prepared {
    let windows = cfg!(windows);
    if mode == Mode::Unprotected {
        // The user's own access, by their choice. The environment is still the app's, so the kernel starts on every machine.
        let vars = vec![(
            "PRIME_AGENT_KERNEL_PYTHON".to_string(),
            env.python.to_string_lossy().into_owned(),
        )];
        return Prepared {
            env: vars,
            state: SandboxState {
                mode: mode.name(),
                enforcement: "unenforced",
                backend: None,
                limitations: Vec::new(),
                reason: Some("The model's code runs with your own access to this computer, by your choice.".into()),
            },
        };
    }
    let state_dir = dir.join("state");
    let unavailable = |reason: String, backend: Option<String>| unavailable(mode, dir, reason, backend);
    if !helper.is_file() {
        return unavailable(format!("The sandbox helper is missing: {}", helper.display()), None);
    }
    report("Checking what this computer can sandbox");
    let capability = match capability(helper, &state_dir) {
        Ok(capability) => capability,
        Err(error) => return unavailable(format!("{error:#}"), None),
    };
    if !capability.available {
        return unavailable("This computer cannot run the sandbox (Windows 11 build 26100.9278 or later, macOS 15 or later, or Linux with bubblewrap).".into(), Some("mxc".into()));
    }
    let wrapper = match install_wrapper(helper, dir) {
        Ok(wrapper) => wrapper,
        Err(error) => return unavailable(format!("{error:#}"), Some("mxc".into())),
    };
    let policy_file = dir.join("kernel-policy.json");
    let policy = kernel_policy(env, windows);
    if let Err(error) = std::fs::write(&policy_file, policy.to_string()) {
        return unavailable(format!("cannot write the sandbox policy: {error}"), Some("mxc".into()));
    }
    let vars = protected_env(&wrapper, &policy_file, &state_dir, env, windows);
    let mut attested = capability.attested;
    if !attested {
        report("Testing the sandbox on this computer (one time)");
        if let Err(error) = attest(helper, &state_dir) {
            return Prepared {
                env: vars,
                state: SandboxState {
                    mode: mode.name(),
                    enforcement: "unattested",
                    backend: Some("mxc".into()),
                    limitations: Vec::new(),
                    reason: Some(format!("{error:#}")),
                },
            };
        }
        attested = capability_after(helper, &state_dir);
    }
    if !attested {
        return Prepared {
            env: vars,
            state: SandboxState {
                mode: mode.name(),
                enforcement: "unattested",
                backend: Some("mxc".into()),
                limitations: Vec::new(),
                reason: Some("The sandbox test finished but was not recorded.".into()),
            },
        };
    }
    report("Starting the kernel inside the sandbox");
    let work = dir.join("work");
    if let Err(error) = smoke_test(&wrapper, &vars, &work) {
        return unavailable(format!("{error:#}"), Some("mxc".into()));
    }
    let limits = limitations(helper, &state_dir, &work, &policy, env);
    Prepared {
        env: vars,
        state: SandboxState {
            mode: mode.name(),
            enforcement: "enforced",
            backend: Some("mxc".into()),
            limitations: limits,
            reason: None,
        },
    }
}

fn capability_after(helper: &Path, state_dir: &Path) -> bool {
    capability(helper, state_dir).is_ok_and(|capability| capability.attested)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_at(venv: &Path) -> KernelEnv {
        KernelEnv {
            venv: venv.to_path_buf(),
            python: venv.join("bin").join("python"),
            base_python: venv.join("base").join("python.exe"),
            site_packages: venv.join("lib").join("site-packages"),
        }
    }

    #[test]
    fn an_unrecognised_setting_is_protected() {
        assert_eq!(Mode::from_setting(None), Mode::Protected);
        assert_eq!(Mode::from_setting(Some("protected")), Mode::Protected);
        assert_eq!(
            Mode::from_setting(Some("Unprotected ")),
            Mode::Protected,
            "only the exact word lowers protection"
        );
        assert_eq!(Mode::from_setting(Some("off")), Mode::Protected);
        assert_eq!(Mode::from_setting(Some("unprotected")), Mode::Unprotected);
    }

    #[test]
    fn the_kernel_policy_is_closed_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_at(dir.path());
        let unix = kernel_policy(&env, false);
        assert_eq!(unix["network"], "none");
        assert_eq!(unix["maxProcesses"], 64);
        assert!(
            unix.get("allowGui").is_none(),
            "GUI access is not granted where it is not needed"
        );
        assert!(
            unix["envAllowlist"]
                .as_array()
                .unwrap()
                .iter()
                .all(|name| name != "PRIME_AGENT_KERNEL_OWNER_PID" && name != "ANTHROPIC_API_KEY")
        );
        assert!(
            unix.get("writeRoots").is_none(),
            "the helper adds the working folder itself"
        );
        let windows = kernel_policy(&env, true);
        assert_eq!(windows["allowGui"], true);
        assert!(
            windows["envAllowlist"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "PYTHONPATH")
        );
    }

    #[test]
    fn the_roots_name_the_environment_and_never_the_home_folder() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_at(&dir.path().join("kernel").join("venv"));
        std::fs::create_dir_all(env.python.parent().unwrap()).unwrap();
        std::fs::write(&env.python, "").unwrap();
        let roots = interpreter_roots(&env);
        assert_eq!(roots[0], env.venv);
        let home = platform::home_dir();
        assert!(
            roots
                .iter()
                .all(|root| root != &home && !home.starts_with(root) || root == &env.venv),
            "{roots:?}"
        );
    }

    #[test]
    fn the_daemon_is_pointed_at_the_wrapper_and_told_the_real_interpreter() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_at(dir.path());
        let vars = protected_env(
            Path::new("/app/desktop-kernel"),
            Path::new("/app/policy.json"),
            Path::new("/app/state"),
            &env,
            false,
        );
        let get = |name: &str| {
            vars.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("PRIME_AGENT_KERNEL_PYTHON"), Some("/app/desktop-kernel"));
        assert_eq!(get("PRIME_DESKTOP_REAL_PYTHON"), Some(env.python.to_str().unwrap()));
        assert_eq!(get("PRIME_DESKTOP_KERNEL_POLICY"), Some("/app/policy.json"));
        assert!(get("PRIME_DESKTOP_KERNEL_PYTHONPATH").is_none());
        let windows = protected_env(
            Path::new("C:/app/desktop-kernel.exe"),
            Path::new("C:/p.json"),
            Path::new("C:/s"),
            &env,
            true,
        );
        assert_eq!(
            windows
                .iter()
                .find(|(key, _)| key == "PRIME_DESKTOP_REAL_PYTHON")
                .unwrap()
                .1,
            env.base_python.to_string_lossy()
        );
        assert!(windows.iter().any(|(key, _)| key == "PRIME_DESKTOP_KERNEL_PYTHONPATH"));
    }

    #[test]
    fn unprotected_runs_on_the_apps_own_environment_and_says_it_is_unenforced() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_at(dir.path());
        let prepared = prepare(Mode::Unprotected, Path::new("/nowhere"), dir.path(), &env, &|_| {});
        assert_eq!(prepared.state.enforcement, "unenforced");
        assert_eq!(prepared.state.mode, "unprotected");
        assert_eq!(
            prepared.env,
            vec![(
                "PRIME_AGENT_KERNEL_PYTHON".to_string(),
                env.python.to_string_lossy().into_owned()
            )]
        );
    }

    #[test]
    fn a_missing_helper_means_no_kernel_not_an_unprotected_one() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_at(dir.path());
        let prepared = prepare(
            Mode::Protected,
            &dir.path().join("no-such-helper"),
            dir.path(),
            &env,
            &|_| {},
        );
        assert_eq!(prepared.state.enforcement, "unavailable");
        assert!(prepared.state.reason.as_deref().unwrap().contains("missing"));
        let kernel = &prepared
            .env
            .iter()
            .find(|(key, _)| key == "PRIME_AGENT_KERNEL_PYTHON")
            .unwrap()
            .1;
        assert!(
            !Path::new(kernel).exists(),
            "the agent is pointed at a program that does not exist, so nothing runs"
        );
        assert!(
            prepared
                .env
                .iter()
                .all(|(_, value)| value != &env.python.to_string_lossy()),
            "never the plain interpreter"
        );
    }

    #[test]
    fn the_wrapper_is_copied_once_and_replaced_when_the_helper_changes() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("helper");
        std::fs::write(&helper, "one").unwrap();
        let wrapper = install_wrapper(&helper, &dir.path().join("sandbox")).unwrap();
        assert_eq!(std::fs::read_to_string(&wrapper).unwrap(), "one");
        std::fs::write(&helper, "two").unwrap();
        assert_eq!(
            std::fs::read_to_string(install_wrapper(&helper, &dir.path().join("sandbox")).unwrap()).unwrap(),
            "two"
        );
        assert!(
            !dir.path().join("sandbox").read_dir().unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".new"))
        );
    }
}

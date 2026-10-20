//! The Python environment the agent's kernel runs in.
//!
//! The agent builds this itself on first use, but its way fails on a Windows machine with Smart App Control: it builds the
//! `prime-agent-runtime` package in a throwaway environment whose `python.exe` is blocked, and it installs the newest
//! scientific packages, whose extension modules the machine has never seen and blocks. So the app builds the environment
//! itself, with the same packages the agent installs, in two ways that differ:
//!
//! - the build backend is installed into the environment and the runtime is built there (`UV_NO_BUILD_ISOLATION`), so no
//!   fresh `python.exe` has to start;
//! - on Windows the packages whose extension modules are large and numerous are pinned to older, widely used releases,
//!   which the machine already trusts (checked on Windows 11 with Smart App Control enforcing: numpy 1.26.4, pandas 2.2.3,
//!   scipy 1.13.1 and lxml 5.2.2 import; their newest releases did not).
//!
//! It is the app's own environment, in the app's data folder, so it never touches the one `prime-agent` keeps for itself.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::platform;

/// The Python the kernel runs on. The agent asks uv for the same.
pub const PYTHON_VERSION: &str = "3.11";

/// What the agent installs next to its runtime (`DEFAULT_RLM_EXTRA_PACKAGES` and the snapshot requirement).
const PACKAGES: &[&str] = &[
    "dill",
    "requests",
    "httpx",
    "pyyaml",
    "tomli",
    "python-dotenv",
    "pandas",
    "numpy",
    "scipy",
    "beautifulsoup4",
    "lxml",
    "pydantic",
    "tyro",
];

/// Older releases that a Smart App Control machine already trusts. Windows only: elsewhere the newest are fine.
const WINDOWS_PINS: &[(&str, &str)] = &[
    ("numpy", "1.26.4"),
    ("pandas", "2.2.3"),
    ("scipy", "1.13.1"),
    ("lxml", "5.2.2"),
];

/// What the agent checks before it accepts an interpreter as a kernel.
const IMPORTS: &str =
    "import rlm, dill, requests, httpx, yaml, tomli, dotenv, pandas, numpy, scipy, bs4, lxml, pydantic, tyro";

const MARKER: &str = ".desktop-kernel.json";

#[derive(Debug, Clone)]
pub struct KernelEnv {
    pub venv: PathBuf,
    /// The environment's own interpreter (a launcher on Windows).
    pub python: PathBuf,
    /// The interpreter the environment is made from: the one a sandbox on Windows has to run.
    pub base_python: PathBuf,
    pub site_packages: PathBuf,
}

pub fn venv_dir() -> PathBuf {
    platform::app_data_dir().join("kernel").join("venv")
}

fn python_in(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn site_packages_in(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Lib").join("site-packages")
    } else {
        venv.join("lib")
            .join(format!("python{PYTHON_VERSION}"))
            .join("site-packages")
    }
}

/// The interpreter an environment was made from, read from its `pyvenv.cfg`.
pub fn base_interpreter(venv: &Path) -> Result<PathBuf> {
    let text = std::fs::read_to_string(venv.join("pyvenv.cfg"))
        .with_context(|| format!("{} has no pyvenv.cfg", venv.display()))?;
    base_interpreter_from(&text)
}

fn base_interpreter_from(config: &str) -> Result<PathBuf> {
    let home = config
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == "home")
        .map(|(_, value)| value.trim())
        .filter(|home| !home.is_empty());
    let Some(home) = home else {
        bail!("pyvenv.cfg names no base interpreter")
    };
    Ok(Path::new(home).join(if cfg!(windows) { "python.exe" } else { "python3" }))
}

impl KernelEnv {
    pub fn at(venv: &Path) -> Result<Self> {
        let mut base_python = base_interpreter(venv)?;
        // Some bases ship only `python`, not `python3`.
        if !base_python.exists() {
            base_python.set_file_name(if cfg!(windows) { "python.exe" } else { "python" });
        }
        Ok(Self {
            venv: venv.to_path_buf(),
            python: python_in(venv),
            base_python,
            site_packages: site_packages_in(venv),
        })
    }
}

/// `uv`: an explicit override, the copy shipped beside the app, then the usual places. `PATH` comes after the shipped copy so
/// a stray `uv` earlier on it is not what the app depends on.
pub fn find_uv() -> Option<PathBuf> {
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PRIME_DESKTOP_UV").filter(|path| !path.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    if let Some(beside) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
    {
        candidates.push(beside);
    }
    candidates.push(platform::home_dir().join(".local").join("bin").join(name));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(name)));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// The packages to install next to the runtime, pinned where this system needs it.
pub fn requirements(windows: bool) -> Vec<String> {
    PACKAGES
        .iter()
        .map(|name| {
            match WINDOWS_PINS
                .iter()
                .find(|(pinned, _)| pinned == name)
                .filter(|_| windows)
            {
                Some((pinned, version)) => format!("{pinned}=={version}"),
                None => (*name).to_string(),
            }
        })
        .collect()
}

/// The Python skills the release ships, next to its runtime: `skills/<name>` folders that hold a `pyproject.toml`. The agent
/// imports them in the kernel (`edit`, `goal`, `compact` and the rest) and turns off any it cannot import, so they have to be
/// installed into the kernel's environment, as the agent's own bootstrap would.
pub fn skill_packages(runtime_dir: &Path) -> Vec<PathBuf> {
    let Some(skills) = runtime_dir.parent().map(|agent| agent.join("skills")) else {
        return Vec::new();
    };
    let mut packages: Vec<PathBuf> = std::fs::read_dir(skills)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("pyproject.toml").is_file())
        .collect();
    packages.sort();
    packages
}

/// What an environment must match to be reused: the agent release's runtime and skills, and the packages asked for.
pub fn identity(runtime_dir: &Path, windows: bool) -> Result<String> {
    let project = std::fs::read(runtime_dir.join("pyproject.toml"))
        .with_context(|| format!("{} is not a prime-agent-runtime", runtime_dir.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&project);
    for skill in skill_packages(runtime_dir) {
        hasher.update(skill.file_name().unwrap_or_default().to_string_lossy().as_bytes());
        hasher.update(std::fs::read(skill.join("pyproject.toml")).unwrap_or_default());
    }
    hasher.update(PYTHON_VERSION);
    for requirement in requirements(windows) {
        hasher.update(requirement);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Whether an environment was built for this identity and still has its interpreter.
pub fn is_ready(venv: &Path, identity: &str) -> bool {
    let recorded = std::fs::read_to_string(venv.join(MARKER))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    recorded.is_some_and(|marker| marker["identity"] == identity) && python_in(venv).is_file()
}

fn run_uv(uv: &Path, args: &[&str], extra: &[String], no_isolation: bool) -> Result<()> {
    let mut command = std::process::Command::new(uv);
    command
        .args(args)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if no_isolation {
        command.env("UV_NO_BUILD_ISOLATION", "1");
    }
    platform::quiet(&mut command);
    let output = command
        .output()
        .with_context(|| format!("cannot start {}", uv.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: String = stderr
        .trim()
        .lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    bail!("uv {} failed ({}):\n{tail}", args.join(" "), output.status)
}

/// Builds the environment at `venv` from the agent's runtime folder. `report` hears each step before it starts. Blocking: it
/// downloads and compiles, so callers run it off the async runtime.
pub fn provision(uv: &Path, venv: &Path, runtime_dir: &Path, report: &dyn Fn(&str)) -> Result<KernelEnv> {
    let windows = cfg!(windows);
    let identity = identity(runtime_dir, windows)?;
    if let Some(parent) = venv.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if venv.exists() {
        std::fs::remove_dir_all(venv)
            .with_context(|| format!("cannot remove the old environment {}", venv.display()))?;
    }
    let python = python_in(venv);
    let python_arg = python.to_string_lossy().into_owned();
    let runtime = runtime_dir.to_string_lossy().into_owned();

    report("Fetching Python");
    run_uv(uv, &["python", "install", PYTHON_VERSION], &[], false)?;
    report("Creating the Python environment");
    run_uv(
        uv,
        &["venv", &venv.to_string_lossy(), "--python", PYTHON_VERSION, "--seed"],
        &[],
        false,
    )?;
    report("Preparing the build tools");
    run_uv(
        uv,
        &["pip", "install", "--python", &python_arg, "hatchling"],
        &[],
        false,
    )?;
    report("Installing the agent's Python runtime, skills and packages");
    let mut install = vec![runtime];
    install.extend(
        skill_packages(runtime_dir)
            .into_iter()
            .map(|skill| skill.to_string_lossy().into_owned()),
    );
    install.extend(requirements(windows));
    run_uv(uv, &["pip", "install", "--python", &python_arg], &install, true)?;

    report("Checking the environment");
    let mut check = std::process::Command::new(&python);
    check.args(["-c", IMPORTS]).stdin(Stdio::null());
    platform::quiet(&mut check);
    let checked = check
        .output()
        .with_context(|| format!("cannot start {}", python.display()))?;
    if !checked.status.success() {
        bail!(
            "the new Python environment does not import its packages:\n{}",
            String::from_utf8_lossy(&checked.stderr).trim()
        );
    }
    std::fs::write(
        venv.join(MARKER),
        serde_json::json!({"identity": identity, "python": PYTHON_VERSION, "requirements": requirements(windows)})
            .to_string(),
    )?;
    KernelEnv::at(venv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_pins_old_scientific_packages_and_nothing_else() {
        let windows = requirements(true);
        assert!(windows.contains(&"numpy==1.26.4".to_string()));
        assert!(windows.contains(&"pandas==2.2.3".to_string()));
        assert!(windows.contains(&"scipy==1.13.1".to_string()));
        assert!(windows.contains(&"lxml==5.2.2".to_string()));
        assert!(
            windows.contains(&"pydantic".to_string()),
            "the runtime needs a recent pydantic, so it is not pinned"
        );
        let elsewhere = requirements(false);
        assert!(elsewhere.contains(&"numpy".to_string()));
        assert_eq!(windows.len(), elsewhere.len());
    }

    #[test]
    fn every_import_the_agent_checks_has_a_package() {
        let all = requirements(false).join(" ");
        for module in [
            "dill", "requests", "httpx", "yaml", "tomli", "dotenv", "pandas", "numpy", "scipy", "bs4", "lxml",
            "pydantic", "tyro",
        ] {
            assert!(IMPORTS.contains(module), "{module}");
        }
        for package in ["pyyaml", "python-dotenv", "beautifulsoup4", "tyro", "dill"] {
            assert!(all.contains(package), "{package}");
        }
    }

    #[test]
    fn the_base_interpreter_comes_from_pyvenv_cfg() {
        let cfg = "home = /opt/py/cpython-3.11\ninclude-system-site-packages = false\nversion = 3.11.15\n";
        let base = base_interpreter_from(cfg).unwrap();
        assert!(base.starts_with("/opt/py/cpython-3.11"));
        assert!(base_interpreter_from("version = 3.11").is_err());
        assert!(base_interpreter_from("home =  ").is_err());
    }

    #[test]
    fn the_skills_a_release_ships_are_found_and_part_of_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("prime-agent-runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("pyproject.toml"),
            "[project]\nname='prime-agent-runtime'\n",
        )
        .unwrap();
        let skills = dir.path().join("skills");
        for (name, has_package) in [("goal", true), ("edit", true), ("factory", false)] {
            std::fs::create_dir_all(skills.join(name)).unwrap();
            std::fs::write(skills.join(name).join("SKILL.md"), "prompt only").unwrap();
            if has_package {
                std::fs::write(
                    skills.join(name).join("pyproject.toml"),
                    format!("[project]\nname='{name}'\n"),
                )
                .unwrap();
            }
        }
        let names: Vec<_> = skill_packages(&runtime)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["edit", "goal"], "only folders with a package, in a fixed order");
        let before = identity(&runtime, false).unwrap();
        std::fs::write(
            skills.join("edit").join("pyproject.toml"),
            "[project]\nname='edit'\nversion='2'\n",
        )
        .unwrap();
        assert_ne!(
            before,
            identity(&runtime, false).unwrap(),
            "a changed skill rebuilds the environment"
        );
    }

    #[test]
    fn an_environment_is_reused_only_for_the_identity_it_was_built_for() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("pyproject.toml"),
            "[project]\nname='prime-agent-runtime'\nversion='0.1.0'\n",
        )
        .unwrap();
        let first = identity(&runtime, false).unwrap();
        assert_ne!(
            first,
            identity(&runtime, true).unwrap(),
            "the Windows pins change the identity"
        );
        std::fs::write(
            runtime.join("pyproject.toml"),
            "[project]\nname='prime-agent-runtime'\nversion='0.2.0'\n",
        )
        .unwrap();
        assert_ne!(
            first,
            identity(&runtime, false).unwrap(),
            "a new runtime needs a new environment"
        );
        assert!(identity(&dir.path().join("missing"), false).is_err());

        let venv = dir.path().join("venv");
        std::fs::create_dir_all(python_in(&venv).parent().unwrap()).unwrap();
        std::fs::write(python_in(&venv), "").unwrap();
        assert!(!is_ready(&venv, &first), "no marker, not ready");
        std::fs::write(venv.join(MARKER), serde_json::json!({"identity": first}).to_string()).unwrap();
        assert!(is_ready(&venv, &first));
        assert!(!is_ready(&venv, "something else"));
    }

    /// Builds a real environment from the real runtime: slow, needs uv and the network, and a prime-agent release extracted
    /// at `PRIME_DESKTOP_TEST_AGENT_DIR`. `cargo test --lib live_provision -- --ignored`.
    #[test]
    #[ignore]
    fn live_provision_builds_an_environment_the_agent_accepts() {
        let (Some(uv), Some(agent_dir)) = (find_uv(), std::env::var_os("PRIME_DESKTOP_TEST_AGENT_DIR")) else {
            eprintln!("SKIPPED: needs uv and PRIME_DESKTOP_TEST_AGENT_DIR");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path().join("venv");
        let env = provision(
            &uv,
            &venv,
            &Path::new(&agent_dir).join("prime-agent-runtime"),
            &|step| eprintln!("step: {step}"),
        )
        .unwrap();
        assert!(env.python.is_file());
        assert!(env.site_packages.is_dir());
        assert!(env.base_python.exists(), "{}", env.base_python.display());
        assert!(is_ready(
            &venv,
            &identity(&Path::new(&agent_dir).join("prime-agent-runtime"), cfg!(windows)).unwrap()
        ));
    }
}

//! `desktop-sandbox`: argv, cwd and a policy in, an enforced child out.
//!
//! ```text
//! desktop-sandbox run --policy <file> --request <file> [--backend auto|mxc|unsandboxed]
//! desktop-sandbox exec --policy <file> [--write-cwd] [--write-env NAME] [--require-attested] [--backend NAME] -- <program> [args]
//! desktop-sandbox capabilities
//! desktop-sandbox attest [--backend mxc]
//! ```
//! `run` prints one `ExecutionResult` as JSON on stdout. Exit 0: the helper ran the request (read the child's
//! outcome from the JSON). 2: the policy or request was invalid. 3: the backend cannot hold this policy here,
//! and nothing was run. The helper never falls back to running without a sandbox: that is the `unsandboxed`
//! backend, chosen by name.

mod attest;
mod backend;
mod backends;
mod cmdline;
mod exec;
mod probe;
mod shim;

use std::process::ExitCode;

use desktop_sandbox_policy::{Enforcement, PolicyInput, RequestInput};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn fail(code: u8, message: impl std::fmt::Display) -> ExitCode {
    println!("{}", serde_json::json!({ "error": message.to_string() }));
    ExitCode::from(code)
}

fn run(args: &[String]) -> ExitCode {
    let (Some(policy_path), Some(request_path)) = (flag(args, "--policy"), flag(args, "--request")) else {
        return fail(
            64,
            "usage: desktop-sandbox run --policy <file> --request <file> [--backend <name>]",
        );
    };
    let read = |path: &str| std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"));
    let (policy_text, request_text) = match (read(&policy_path), read(&request_path)) {
        (Ok(p), Ok(r)) => (p, r),
        (Err(e), _) | (_, Err(e)) => return fail(2, e),
    };
    let now = exec::now_ms();
    let policy = match PolicyInput::from_json(&policy_text).and_then(|p| p.resolve(now)) {
        Ok(policy) => policy,
        Err(e) => return fail(2, e),
    };
    let request = match RequestInput::from_json(&request_text).and_then(|r| r.resolve(&policy, now)) {
        Ok(request) => request,
        Err(e) => return fail(2, e),
    };
    let name = flag(args, "--backend").unwrap_or_else(|| "auto".into());
    let Some(backend) = backend::select(&name) else {
        return fail(64, format!("unknown backend `{name}`"));
    };
    match backend.check(&policy) {
        Err(reason) => {
            println!(
                "{}",
                serde_json::to_string(&backend::unavailable(backend.name(), reason)).expect("serialises")
            );
            ExitCode::from(3)
        }
        Ok(limitations) => {
            let mut result = backend.run(&policy, &request, limitations);
            // `enforced` is a claim about this host, so it is only made where the probe suite has passed.
            if result.enforcement == Enforcement::Enforced && !attest::is_valid(backend.name()) {
                result.enforcement = Enforcement::Unattested;
            }
            println!("{}", serde_json::to_string(&result).expect("serialises"));
            // A backend that could not start the process says so in the result, and the exit code agrees.
            if result.enforcement == Enforcement::Unavailable {
                ExitCode::from(3)
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

/// Whether making `dir` writable would hand over more than a project: the root of a drive, the user's home folder, or
/// any folder that contains the home folder. The daemon picks a session's working directory, and a session opened on
/// the home folder must not let the model's code write credentials, shell profiles or the agent's own state.
fn writable_root(dir: &std::path::Path, home: Option<&std::path::Path>) -> Result<std::path::PathBuf, String> {
    broad_root_check(dir, home, "writable")
}

fn too_broad_to_write(dir: &std::path::Path, home: &std::path::Path) -> bool {
    dir.parent().is_none() || home.starts_with(dir)
}

/// Resolves a folder a root flag names, or says why it cannot be allowed. Fails closed: a folder that cannot
/// be resolved, or a home folder that cannot be found to compare against, refuses rather than passes.
fn broad_root_check(
    dir: &std::path::Path,
    home: Option<&std::path::Path>,
    verb: &str,
) -> Result<std::path::PathBuf, String> {
    let resolved = std::fs::canonicalize(dir).map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;
    let home = home.ok_or("cannot find the home folder, so a writable folder cannot be checked against it")?;
    if too_broad_to_write(&resolved, home) {
        return Err(format!(
            "refusing to make {} {verb}: it is a drive root, the home folder or a parent of it",
            resolved.display()
        ));
    }
    Ok(resolved)
}

/// The user's home folder, from the environment or else the system's own record, so a launcher that clears the
/// environment does not hide it.
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::home_dir().and_then(|h| std::fs::canonicalize(h).ok())
}

/// Runs one program with its input and output bridged live, for programs that talk over pipes for as long as they
/// live (the agent's Python kernel). The policy file is the host's; the working directory and the directories named
/// by `--write-env` are added as write roots, because the daemon that starts the program chooses them per session.
/// The program's environment is this process's environment filtered by the policy's allowlist, so a secret in the
/// daemon's environment never reaches it. Exit 3: the backend cannot hold the policy and nothing ran. Exit 4:
/// `--require-attested` and this host has not passed the probe suite.
fn exec_cmd(args: &[String]) -> ExitCode {
    let Some(dash) = args.iter().position(|a| a == "--") else {
        return fail(
            64,
            "usage: desktop-sandbox exec --policy <file> [...] -- <program> [args]",
        );
    };
    let (flags, argv) = (&args[..dash], args[dash + 1..].to_vec());
    let stop = |code: u8, message: String| {
        eprintln!("desktop-sandbox: {message}");
        ExitCode::from(code)
    };
    if argv.is_empty() {
        return stop(64, "no program given after --".into());
    }
    let Some(policy_path) = flag(flags, "--policy") else {
        return stop(64, "--policy <file> is required".into());
    };
    let text = match std::fs::read_to_string(&policy_path) {
        Ok(text) => text,
        Err(e) => return stop(2, format!("cannot read {policy_path}: {e}")),
    };
    let mut input = match PolicyInput::from_json(&text) {
        Ok(input) => input,
        Err(e) => return stop(2, e.to_string()),
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => return stop(2, format!("cannot read the working directory: {e}")),
    };
    let home = home_dir();
    // The folders checked below are the ones granted, by their resolved path, and the resolved policy must hold exactly
    // those: a folder swapped for a link between the check and the grant would resolve somewhere else and be refused.
    let mut granted = Vec::new();
    if flags.iter().any(|a| a == "--write-cwd") {
        match writable_root(&cwd, home.as_deref()) {
            Ok(root) => granted.push(root),
            Err(message) => return stop(2, message),
        }
    }
    for (i, f) in flags.iter().enumerate() {
        if f == "--write-env"
            && let Some(dir) = flags.get(i + 1).and_then(std::env::var_os)
            && std::path::Path::new(&dir).is_dir()
        {
            match writable_root(std::path::Path::new(&dir), home.as_deref()) {
                Ok(root) => granted.push(root),
                Err(message) => return stop(2, message),
            }
        }
    }
    input.write_roots.extend(granted.iter().cloned());
    let mut read_granted = Vec::new();
    for (i, f) in flags.iter().enumerate() {
        if f == "--read-dir" {
            let Some(dir) = flags.get(i + 1) else {
                return stop(64, "--read-dir needs a folder".into());
            };
            match broad_root_check(std::path::Path::new(dir), home.as_deref(), "readable") {
                Ok(root) => read_granted.push(root),
                Err(message) => return stop(2, message),
            }
        }
    }
    input.read_roots.extend(read_granted.iter().cloned());
    let now = exec::now_ms();
    let policy = match input.resolve(now) {
        Ok(policy) => policy,
        Err(e) => return stop(2, e.to_string()),
    };
    if let Some(root) = read_granted.iter().find(|root| !policy.read_roots.contains(root)) {
        return stop(
            2,
            format!("{} changed while the sandbox was being set up", root.display()),
        );
    }
    if let Some(root) = granted.iter().find(|root| !policy.write_roots.contains(root)) {
        return stop(
            2,
            format!("{} changed while the sandbox was being set up", root.display()),
        );
    }
    let env = std::env::vars().filter(|(name, _)| policy.env_allowed(name)).collect();
    let request = match (RequestInput {
        argv,
        cwd,
        pty: false,
        env,
    })
    .resolve(&policy, now)
    {
        Ok(request) => request,
        Err(e) => return stop(2, e.to_string()),
    };
    let name = flag(flags, "--backend").unwrap_or_else(|| "auto".into());
    let Some(backend) = backend::select(&name) else {
        return stop(64, format!("unknown backend `{name}`"));
    };
    if let Err(reason) = backend.check(&policy) {
        return stop(3, reason);
    }
    if flags.iter().any(|a| a == "--require-attested")
        && backend.name() != "unsandboxed"
        && !attest::is_valid(backend.name())
    {
        return stop(
            4,
            format!(
                "this host has not passed the probe suite for {}; run `desktop-sandbox attest`",
                backend.name()
            ),
        );
    }
    match backend.exec(&policy, &request) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(reason) => stop(3, reason),
    }
}

/// Runs the probe suite through this helper and records the outcome. Exit 0 means attested; 1 means a probe failed
/// and any older record was removed.
fn attest_cmd(args: &[String]) -> ExitCode {
    let name = flag(args, "--backend").unwrap_or_else(|| "mxc".into());
    let Some(backend) = backend::select(&name) else {
        return fail(64, format!("unknown backend `{name}`"));
    };
    if backend.name() == "unsandboxed" {
        return fail(64, "the unsandboxed backend cannot be attested: it enforces nothing");
    }
    if let Err(reason) = backend.host() {
        attest::clear(backend.name());
        return fail(3, reason);
    }
    let runner = std::env::current_exe()
        .expect("a running exe has a path")
        .with_file_name(if cfg!(windows) {
            "desktop-sandbox-probes.exe"
        } else {
            "desktop-sandbox-probes"
        });
    if !runner.exists() {
        return fail(
            64,
            format!("the probe runner is missing next to the helper: {}", runner.display()),
        );
    }
    let me = std::env::current_exe().expect("a running exe has a path");
    let output = std::process::Command::new(&runner)
        .arg("--launcher")
        .arg(&me)
        .args(["--backend", backend.name(), "--json"])
        .output();
    let Ok(output) = output else {
        return fail(1, "could not run the probe runner");
    };
    let Ok(report) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return fail(
            1,
            format!(
                "the probe runner printed no report: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        );
    };
    let results = report["results"].as_array().cloned().unwrap_or_default();
    let ids = |status: &str| -> Vec<String> {
        results
            .iter()
            .filter(|r| r["status"] == status)
            .filter_map(|r| r["id"].as_str().map(String::from))
            .collect()
    };
    let failed = ids("fail");
    if !failed.is_empty() || results.is_empty() {
        attest::clear(backend.name());
        return fail(
            1,
            format!("not attested: {} probe(s) failed: {}", failed.len(), failed.join(", ")),
        );
    }
    let record = attest::new_record(
        backend.name(),
        report["catalogFingerprint"].as_str().unwrap_or("unknown").to_string(),
        ids("pass").len(),
        ids("known-gap"),
        ids("unavailable"),
    );
    if let Err(e) = attest::write(&record) {
        return fail(1, format!("could not store the attestation: {e}"));
    }
    println!("{}", serde_json::to_string_pretty(&record).expect("serialises"));
    ExitCode::SUCCESS
}

fn capabilities() -> ExitCode {
    let report: Vec<_> = ["mxc", "unsandboxed"]
        .iter()
        .filter_map(|name| backend::select(name))
        .map(|b| match b.host() {
            Ok(()) => serde_json::json!({
                "backend": b.name(),
                "available": true,
                "attested": b.name() != "unsandboxed" && attest::is_valid(b.name()),
                "host": attest::host_id(),
            }),
            Err(reason) => serde_json::json!({ "backend": b.name(), "available": false, "reason": reason }),
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&report).expect("serialises"));
    ExitCode::SUCCESS
}

/// The agent's kernel wrapper. The helper is copied to a file named `desktop-kernel` and set as the agent's kernel
/// interpreter (`PRIME_AGENT_KERNEL_PYTHON`). The agent runs it exactly as it would run Python, so the arguments are
/// Python's, and this runs the real Python inside the sandbox: `PRIME_DESKTOP_REAL_PYTHON` is the interpreter and
/// `PRIME_DESKTOP_KERNEL_POLICY` is the policy file. The agent's working directory and session directory become write roots.
/// It fails closed: with no sandbox available it starts nothing, so the kernel does not start.
/// The arguments `exec` receives for a kernel: the policy, the writable folders the daemon chooses per session, the shim
/// folder when there is one, then the real interpreter and whatever the daemon asked the interpreter to do.
fn kernel_exec_args(python: &str, policy: &str, rest: &[String], shim: Option<&std::path::Path>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--policy",
        policy,
        "--write-cwd",
        "--write-env",
        "PRIME_AGENT_SESSION_DIR",
        "--require-attested",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(shim) = shim {
        args.extend(["--read-dir".to_string(), shim.to_string_lossy().into_owned()]);
    }
    args.push("--".into());
    args.push(python.to_string());
    args.extend(rest.iter().cloned());
    args
}

fn kernel_main() -> ExitCode {
    let (Some(python), Some(policy)) = (
        std::env::var_os("PRIME_DESKTOP_REAL_PYTHON"),
        std::env::var_os("PRIME_DESKTOP_KERNEL_POLICY"),
    ) else {
        eprintln!("desktop-kernel: PRIME_DESKTOP_REAL_PYTHON and PRIME_DESKTOP_KERNEL_POLICY must be set by the host");
        return ExitCode::from(64);
    };
    // On Windows a virtual environment's `python.exe` is a launcher that needs GUI access, which the sandbox withholds, so
    // the host names the base interpreter and the environment's packages here. The kernel also needs the start-up shim
    // (see shim/sitecustomize.py). The policy must allow `PYTHONPATH`.
    let mut python_path: Vec<std::path::PathBuf> = std::env::var_os("PRIME_DESKTOP_KERNEL_PYTHONPATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    let shim = if cfg!(windows) {
        match shim::install(&attest::state_dir()) {
            Ok(dir) => Some(dir),
            Err(error) => {
                eprintln!("desktop-kernel: cannot set up the kernel shim: {error}");
                return ExitCode::from(3);
            }
        }
    } else {
        None
    };
    if let Some(shim) = &shim {
        python_path.insert(0, shim.clone());
    }
    if !python_path.is_empty()
        && let Ok(joined) = std::env::join_paths(&python_path)
    {
        // SAFETY: this runs first thing in a process that has started no threads.
        unsafe { std::env::set_var("PYTHONPATH", joined) };
    }
    let rest: Vec<String> = std::env::args().skip(1).collect();
    // An import check that already passed inside the sandbox, on this interpreter and these packages, is not run again.
    let state = attest::state_dir();
    let check = probe::cacheable(&rest).map(|code| {
        probe::key(
            code,
            std::path::Path::new(&python),
            &python_path_for_key(&std::env::var_os("PRIME_DESKTOP_KERNEL_PYTHONPATH")),
        )
    });
    if let Some(key) = &check
        && probe::passed_before(&state, key)
    {
        return ExitCode::SUCCESS;
    }
    let outcome = exec_cmd(&kernel_exec_args(
        &python.to_string_lossy(),
        &policy.to_string_lossy(),
        &rest,
        shim.as_deref(),
    ));
    if let Some(key) = &check
        && outcome == ExitCode::SUCCESS
    {
        probe::remember(&state, key);
    }
    outcome
}

/// The packages folders the host named for the interpreter (the shim folder is not one of them).
fn python_path_for_key(named: &Option<std::ffi::OsString>) -> Vec<std::path::PathBuf> {
    named
        .as_ref()
        .map(|paths| std::env::split_paths(paths).collect())
        .unwrap_or_default()
}

fn is_kernel_wrapper() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.file_stem()
                .map(|stem| stem.to_string_lossy().eq_ignore_ascii_case("desktop-kernel"))
        })
        .unwrap_or(false)
}

fn main() -> ExitCode {
    if is_kernel_wrapper() {
        return kernel_main();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args),
        Some("capabilities") => capabilities(),
        Some("attest") => attest_cmd(&args),
        Some("exec") => exec_cmd(&args),
        _ => fail(64, "usage: desktop-sandbox run|capabilities"),
    }
}

#[cfg(test)]
mod tests {
    use super::{kernel_exec_args, too_broad_to_write, writable_root};
    use std::path::Path;

    #[test]
    fn a_project_is_fine_but_the_root_the_home_and_its_parents_are_not() {
        let home = Path::new("/home/ada");
        assert!(!too_broad_to_write(Path::new("/home/ada/projects/app"), home));
        assert!(!too_broad_to_write(Path::new("/srv/work"), home));
        assert!(too_broad_to_write(Path::new("/"), home));
        assert!(too_broad_to_write(Path::new("/home"), home));
        assert!(too_broad_to_write(Path::new("/home/ada"), home));
    }

    #[test]
    fn a_kernel_names_the_shim_folder_only_when_there_is_one() {
        let rest = vec!["-m".to_string(), "rlm.repl".to_string()];
        let plain = kernel_exec_args("/py", "/pol.json", &rest, None);
        assert!(!plain.contains(&"--read-dir".to_string()));
        assert_eq!(&plain[plain.len() - 3..], ["/py", "-m", "rlm.repl"]);
        let shimmed = kernel_exec_args(
            "/py",
            "/pol.json",
            &rest,
            Some(std::path::Path::new("/state/kernel-shim")),
        );
        let at = shimmed.iter().position(|a| a == "--read-dir").unwrap();
        assert_eq!(shimmed[at + 1], "/state/kernel-shim");
        assert!(
            at < shimmed.iter().position(|a| a == "--").unwrap(),
            "flags come before the program"
        );
        assert_eq!(
            &shimmed[shimmed.len() - 3..],
            ["/py", "-m", "rlm.repl"],
            "the daemon's own arguments follow, untouched"
        );
        assert!(
            shimmed.contains(&"--require-attested".to_string()),
            "the attestation requirement is never dropped"
        );
    }

    #[test]
    fn an_unresolvable_folder_or_an_unknown_home_refuses() {
        let here = std::env::temp_dir();
        assert!(writable_root(Path::new("/no/such/folder/anywhere"), Some(Path::new("/home/ada"))).is_err());
        assert!(
            writable_root(&here, None).is_err(),
            "no home folder to compare against must refuse"
        );
    }
}

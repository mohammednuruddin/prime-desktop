//! `desktop-sandbox-probes`: run the allow/deny catalog against a sandbox helper.
//!
//! ```text
//! desktop-sandbox-probes --launcher <helper> [--backend <name>] [--only <text>] [--json] [--allow-unavailable]
//! desktop-sandbox-probes --self-check     # the suite against an unsandboxed control; proves the probes can fail
//! desktop-sandbox-probes --list
//! ```
//! Exit 0: every applicable case passed. 1: a case failed. 2: none failed but some were unavailable.

use std::path::PathBuf;
use std::process::ExitCode;

use desktop_sandbox_probes::catalog::{self, Os, cases};
use desktop_sandbox_probes::launcher::{ExternalLauncher, Launcher, UnsandboxedControl};
use desktop_sandbox_probes::runner::{Status, run_all, self_check, summarise};

fn agent_path() -> PathBuf {
    let exe = std::env::current_exe().expect("the runner knows its own path");
    exe.with_file_name(if cfg!(windows) {
        "probe-agent.exe"
    } else {
        "probe-agent"
    })
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut launcher, mut backend, mut only) = (None, None, None);
    let (mut json, mut allow_unavailable, mut check, mut list, mut control) = (false, false, false, false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--launcher" => launcher = args.next().map(PathBuf::from),
            "--backend" => backend = args.next(),
            "--only" => only = args.next(),
            "--json" => json = true,
            "--allow-unavailable" => allow_unavailable = true,
            "--self-check" => check = true,
            "--control" => control = true,
            "--list" => list = true,
            other => {
                eprintln!("unknown argument {other}");
                return ExitCode::from(64);
            }
        }
    }
    let agent = agent_path();
    if !agent.exists() {
        eprintln!("probe-agent was not found next to the runner at {}", agent.display());
        return ExitCode::from(64);
    }
    if list {
        for case in cases() {
            let here = case.only_on.is_empty() || case.only_on.contains(&Os::current());
            println!(
                "{:<48} {:?} {}",
                case.id,
                case.expect,
                if here { "" } else { "(other OS)" }
            );
        }
        return ExitCode::SUCCESS;
    }
    if check {
        let problems = self_check(&agent);
        if problems.is_empty() {
            println!("self-check: every probe that should fail with no sandbox does, and every control passes");
            return ExitCode::SUCCESS;
        }
        problems.iter().for_each(|p| eprintln!("self-check: {p}"));
        return ExitCode::from(1);
    }
    let launcher: Box<dyn Launcher> = match (launcher, control) {
        (Some(exe), _) => Box::new(ExternalLauncher {
            exe,
            extra_args: backend.map(|b| vec!["--backend".into(), b]).unwrap_or_default(),
        }),
        (None, true) => Box::new(UnsandboxedControl),
        (None, false) => {
            eprintln!("give --launcher <helper>, or --control, --self-check or --list");
            return ExitCode::from(64);
        }
    };
    let results = run_all(launcher.as_ref(), &agent, only.as_deref());
    if json {
        let report = serde_json::json!({ "catalogFingerprint": catalog::fingerprint(), "results": results });
        println!("{}", serde_json::to_string_pretty(&report).expect("results serialise"));
    } else {
        println!("launcher: {}", launcher.name());
        for r in &results {
            let mark = match r.status {
                Status::Pass => "pass       ",
                Status::Fail => "FAIL       ",
                Status::Unavailable => "unavailable",
                Status::KnownGap => "known gap  ",
                Status::NotApplicable => "n/a        ",
            };
            println!("{mark} {:<48} {}", r.id, r.detail);
        }
    }
    let (pass, fail, unavailable, gaps) = summarise(&results);
    eprintln!("pass={pass} fail={fail} known-gap={gaps} unavailable={unavailable}");
    if fail > 0 {
        ExitCode::from(1)
    } else if unavailable > 0 && !allow_unavailable {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

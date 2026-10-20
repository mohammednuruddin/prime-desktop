use std::path::Path;
use std::time::{Duration, Instant};

use desktop_sandbox_policy::{NetworkMode, PolicyInput, RequestInput, SCHEMA_VERSION};
use serde::Serialize;

use crate::catalog::{ALLOWED_VAR, Case, Check, EXTERNAL_PROBE_ADDR, Expect, Os, Plan, cases};
use crate::fixture::Fixture;
use crate::launcher::{Launch, Launcher, UnsandboxedControl};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Pass,
    Fail,
    /// The backend says it cannot hold this policy on this host. Never counted as a pass.
    Unavailable,
    /// The case failed, and the backend says in its limitations that it cannot hold it. Not a failure, never a pass.
    KnownGap,
    /// The case is for another operating system.
    NotApplicable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseResult {
    pub id: &'static str,
    pub category: &'static str,
    pub expect: Expect,
    pub status: Status,
    pub observed: Option<Expect>,
    pub detail: String,
    pub millis: u64,
}

fn result(
    case: &Case,
    status: Status,
    observed: Option<Expect>,
    detail: impl Into<String>,
    started: Instant,
) -> CaseResult {
    CaseResult {
        id: case.id,
        category: case.category,
        expect: case.expect,
        status,
        observed,
        detail: detail.into(),
        millis: started.elapsed().as_millis() as u64,
    }
}

fn policy_input(fx: &Fixture, plan: &Plan) -> PolicyInput {
    let mut allow = vec![ALLOWED_VAR.to_string(), "PATH".to_string()];
    if cfg!(windows) {
        allow.push("SystemRoot".into());
    }
    PolicyInput {
        schema_version: SCHEMA_VERSION,
        id: "probe".into(),
        label: "Probe".into(),
        read_roots: vec![fx.agent.parent().expect("the agent sits in a directory").to_path_buf()],
        write_roots: vec![fx.ws.clone()],
        denied_paths: vec![],
        protected_names: None,
        network: plan.network,
        env_allowlist: allow,
        max_processes: plan.max_processes,
        allow_pty: false,
        allow_gui: false,
        timeout_ms: plan.timeout_ms.or(Some(20_000)),
        max_output_bytes: plan.max_output_bytes,
        // Open network is only valid with an expiring grant.
        grant_expires_at: (plan.network == NetworkMode::Open).then(|| crate::launcher::now_ms() + 10 * 60_000),
    }
}

fn request_input(fx: &Fixture, plan: &Plan) -> RequestInput {
    let mut argv = vec![fx.agent.to_string_lossy().into_owned()];
    argv.extend(plan.args.iter().cloned());
    let mut env = plan.request_env.clone();
    if cfg!(windows) {
        // Windows programs fail to start sockets and many DLLs without it.
        if let Ok(root) = std::env::var("SystemRoot") {
            env.insert("SystemRoot".into(), root);
        }
    }
    RequestInput {
        argv,
        cwd: fx.ws.clone(),
        pty: false,
        env,
    }
}

fn internet_reachable() -> bool {
    use std::net::{SocketAddr, TcpStream};
    EXTERNAL_PROBE_ADDR
        .parse::<SocketAddr>()
        .is_ok_and(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok())
}

/// The probe program's report, from the last line of its output that parses as one.
fn probe_report(stdout: &str) -> Option<(bool, String)> {
    stdout.lines().rev().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        let ok = value.get("ok")?.as_bool()?;
        let note = value
            .get("error")
            .or_else(|| value.get("detail"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Some((ok, note))
    })
}

pub fn run_case(case: &Case, launcher: &dyn Launcher, agent: &Path) -> CaseResult {
    let started = Instant::now();
    if !case.only_on.is_empty() && !case.only_on.contains(&Os::current()) {
        return result(
            case,
            Status::NotApplicable,
            None,
            "for another operating system",
            started,
        );
    }
    let fx = match Fixture::create(agent) {
        Ok(fx) => fx,
        Err(e) => {
            return result(
                case,
                Status::Fail,
                None,
                format!("could not build the fixture: {e}"),
                started,
            );
        }
    };
    let plan = (case.build)(&fx);
    if plan.needs_links && !fx.links_ok {
        return result(
            case,
            Status::Unavailable,
            None,
            "this host cannot create directory links here",
            started,
        );
    }
    if plan.check == Check::External && !internet_reachable() {
        return result(
            case,
            Status::Unavailable,
            None,
            "this test host cannot reach the internet itself",
            started,
        );
    }
    let (policy, request) = (policy_input(&fx, &plan), request_input(&fx, &plan));
    let ran_at = Instant::now();
    let ran = match launcher.launch(&fx, &policy, &request, &plan.host_env) {
        Ok(Launch::Ran(ran)) => ran,
        Ok(Launch::Unavailable(reason)) => return result(case, Status::Unavailable, None, reason, started),
        Err(e) => return result(case, Status::Fail, None, e, started),
    };
    let elapsed = ran_at.elapsed();
    let report = probe_report(&ran.stdout);

    let observed = match plan.check {
        Check::Timeout => {
            let limit = Duration::from_millis(policy.timeout_ms.unwrap_or(0)) + Duration::from_millis(2_500);
            if ran.timed_out && elapsed <= limit {
                Expect::Deny
            } else {
                Expect::Allow
            }
        }
        Check::OutputCap => {
            let cap = policy.max_output_bytes.unwrap_or(u64::MAX);
            if ran.stdout_truncated && ran.stdout.len() as u64 <= cap {
                Expect::Deny
            } else {
                Expect::Allow
            }
        }
        Check::HostUnchanged => {
            let Some((_, note)) = report else {
                let why = format!(
                    "the probe program printed no report (exit {:?}, enforcement {:?}); stderr: {}",
                    ran.exit_code,
                    ran.enforcement,
                    ran.stderr.trim()
                );
                return result(case, Status::Fail, None, why, started);
            };
            // `finish` fails the case if the real file now exists or changed.
            return finish(
                case,
                &plan,
                Expect::Deny,
                format!("the program reported: {note}"),
                &ran.limitations,
                started,
            );
        }
        Check::Op | Check::Connection | Check::ArgvIntact | Check::External => {
            let Some((ok, note)) = report else {
                // No report is not a denial: the probe program never ran, so nothing was tested.
                let why = format!(
                    "the probe program printed no report (exit {:?}, enforcement {:?}); stderr: {}",
                    ran.exit_code,
                    ran.enforcement,
                    ran.stderr.trim()
                );
                return result(case, Status::Fail, None, why, started);
            };
            let reached = match plan.check {
                Check::Connection => fx.saw_connection(),
                // The program echoed what it received. Anything other than the original is damage.
                Check::ArgvIntact => note == plan.args[1],
                _ => true,
            };
            let outcome = if ok && reached { Expect::Allow } else { Expect::Deny };
            return finish(case, &plan, outcome, note, &ran.limitations, started);
        }
    };
    finish(case, &plan, observed, String::new(), &ran.limitations, started)
}

/// The single place a case is judged. It holds when the outcome is the expected one and no forbidden side effect
/// exists. When it does not hold, it is a known gap only if the backend itself lists the limitation.
fn finish(
    case: &Case,
    plan: &Plan,
    observed: Expect,
    note: String,
    limitations: &[String],
    started: Instant,
) -> CaseResult {
    let leaked = plan.must_not_exist.iter().find(|p| p.exists());
    if observed == case.expect && leaked.is_none() {
        return result(case, Status::Pass, Some(observed), note, started);
    }
    let why = match leaked {
        Some(path) => format!("a forbidden side effect exists: {}", path.display()),
        None => format!("expected {:?}, observed {observed:?}: {note}", case.expect),
    };
    if case
        .known_gap
        .is_some_and(|gap| limitations.iter().any(|l| l.contains(gap)))
    {
        return result(
            case,
            Status::KnownGap,
            Some(observed),
            format!("the backend lists this limitation: {why}"),
            started,
        );
    }
    result(case, Status::Fail, Some(observed), why, started)
}

pub fn run_all(launcher: &dyn Launcher, agent: &Path, only: Option<&str>) -> Vec<CaseResult> {
    cases()
        .iter()
        .filter(|c| only.is_none_or(|needle| c.id.contains(needle)))
        .map(|c| run_case(c, launcher, agent))
        .collect()
}

/// Runs the suite against the unsandboxed control and returns what is wrong with the probes themselves.
/// Allow cases and helper-held cases must pass. Every other deny case must fail, because a probe that
/// passes with no sandbox at all is vacuous.
pub fn self_check(agent: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for case in cases() {
        let got = run_case(&case, &UnsandboxedControl, agent);
        let want = match got.status {
            Status::NotApplicable | Status::Unavailable | Status::KnownGap => continue,
            _ if case.expect == Expect::Allow || case.control_holds => Status::Pass,
            _ => Status::Fail,
        };
        if got.status != want {
            problems.push(format!(
                "{}: with no sandbox it should be {want:?} but was {:?} ({})",
                case.id, got.status, got.detail
            ));
        }
    }
    problems
}

pub fn summarise(results: &[CaseResult]) -> (usize, usize, usize, usize) {
    let count = |s: Status| results.iter().filter(|r| r.status == s).count();
    (
        count(Status::Pass),
        count(Status::Fail),
        count(Status::Unavailable),
        count(Status::KnownGap),
    )
}

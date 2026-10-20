use desktop_sandbox_policy::{Denial, Enforcement, ExecutionResult, ResolvedPolicy, ResolvedRequest};

use crate::exec::now_ms;

/// One way of running a request. A backend either holds the policy as written, or says plainly that it cannot.
pub trait Backend {
    fn name(&self) -> &'static str;

    /// Whether this host can run the backend at all, independent of any policy.
    fn host(&self) -> Result<(), String>;

    /// `Ok` lists what this backend cannot hold about the policy even though it enforces the rest. `Err` is the
    /// reason the policy cannot run here at all; the helper then runs nothing.
    fn check(&self, policy: &ResolvedPolicy) -> Result<Vec<String>, String>;

    fn run(&self, policy: &ResolvedPolicy, request: &ResolvedRequest, limitations: Vec<String>) -> ExecutionResult;

    /// Runs the request with its standard input, output and error bridged live to this process, and returns the
    /// child's exit code. For long-lived programs that talk over pipes, such as the agent's Python kernel, where a
    /// result printed at the end is no use. The policy's timeout and output cap do not apply: the host owns the
    /// program's lifetime. `Err` means nothing was started, and the reason.
    fn exec(&self, policy: &ResolvedPolicy, request: &ResolvedRequest) -> Result<i32, String>;
}

/// What a caller gets when nothing was run, and why.
pub fn unavailable(backend: &str, reason: String) -> ExecutionResult {
    let now = now_ms();
    ExecutionResult {
        exit_code: None,
        timed_out: false,
        signal: None,
        backend: Some(backend.to_string()),
        enforcement: Enforcement::Unavailable,
        denials: vec![Denial {
            code: "backend-unavailable".into(),
            message: reason,
            retryable_in_boundary: false,
        }],
        limitations: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        started_at: now,
        finished_at: Some(now),
    }
}

/// The only backend that is chosen by name and never by default.
pub fn select(name: &str) -> Option<Box<dyn Backend>> {
    match name {
        "unsandboxed" => Some(Box::new(crate::backends::unsandboxed::Unsandboxed)),
        "mxc" | "auto" => Some(Box::new(crate::backends::mxc::Mxc)),
        _ => None,
    }
}

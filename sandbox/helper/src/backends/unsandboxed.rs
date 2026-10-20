use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use desktop_sandbox_policy::{Enforcement, ExecutionResult, ResolvedPolicy, ResolvedRequest};

use crate::backend::Backend;
use crate::exec::{now_ms, read_capped, text};

/// Runs the program with no OS sandbox. It is a named choice for a full-access session and never a fallback:
/// the result says `unenforced`, and the host must show that to the user. The helper's own limits still hold:
/// the environment is exactly the request's, and the timeout and output cap apply.
pub struct Unsandboxed;

impl Backend for Unsandboxed {
    fn name(&self) -> &'static str {
        "unsandboxed"
    }

    fn host(&self) -> Result<(), String> {
        Ok(())
    }

    fn check(&self, _policy: &ResolvedPolicy) -> Result<Vec<String>, String> {
        Ok(vec![
            "no OS sandbox: paths, network and child processes are not restricted".into(),
        ])
    }

    fn exec(&self, _policy: &ResolvedPolicy, request: &ResolvedRequest) -> Result<i32, String> {
        let status = Command::new(&request.argv[0])
            .args(&request.argv[1..])
            .current_dir(&request.cwd)
            .env_clear()
            .envs(&request.env)
            .status()
            .map_err(|e| format!("cannot start {}: {e}", request.argv[0]))?;
        Ok(status.code().unwrap_or(-1))
    }

    fn run(&self, policy: &ResolvedPolicy, request: &ResolvedRequest, limitations: Vec<String>) -> ExecutionResult {
        let started_at = now_ms();
        let spawned = Command::new(&request.argv[0])
            .args(&request.argv[1..])
            .current_dir(&request.cwd)
            .env_clear()
            .envs(&request.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                return crate::backend::unavailable("unsandboxed", format!("cannot start {}: {e}", request.argv[0]));
            }
        };
        let out = read_capped(
            Box::new(child.stdout.take().expect("piped")) as Box<dyn Read + Send>,
            policy.max_output_bytes,
        );
        let err = read_capped(
            Box::new(child.stderr.take().expect("piped")) as Box<dyn Read + Send>,
            policy.max_output_bytes,
        );

        let began = Instant::now();
        let (mut exit_code, mut timed_out) = (None, false);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_code = status.code();
                    break;
                }
                Ok(None) if began.elapsed() >= policy.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(15)),
                Err(_) => break,
            }
        }
        let ((stdout, stdout_truncated), (stderr, stderr_truncated)) =
            (out.join().unwrap_or_default(), err.join().unwrap_or_default());
        ExecutionResult {
            exit_code,
            timed_out,
            signal: None,
            backend: Some("unsandboxed".into()),
            enforcement: Enforcement::Unenforced,
            denials: Vec::new(),
            limitations,
            stdout: text(stdout),
            stderr: text(stderr),
            stdout_truncated,
            stderr_truncated,
            started_at,
            finished_at: Some(now_ms()),
        }
    }
}

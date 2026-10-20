use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use desktop_sandbox_policy::{Enforcement, ExecutionResult, PolicyInput, RequestInput};

use crate::fixture::Fixture;

pub enum Launch {
    Ran(ExecutionResult),
    /// The backend cannot hold this policy here, with its reason. A probe is then `unavailable`, never a pass.
    Unavailable(String),
}

/// Something that can run a request under a policy: the real helper, or the unsandboxed control.
pub trait Launcher {
    fn name(&self) -> String;
    fn launch(
        &self,
        fx: &Fixture,
        policy: &PolicyInput,
        request: &RequestInput,
        host_env: &[(String, String)],
    ) -> Result<Launch, String>;
}

pub struct Captured {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub killed_at_deadline: bool,
}

/// Waits for a child with a hard deadline, so a hung helper or an unenforced `spin` cannot hang the suite.
pub fn capture(mut child: Child, deadline: Duration) -> Captured {
    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let started = Instant::now();
    let (mut exit_code, mut killed) = (None, false);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                killed = true;
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    Captured {
        exit_code,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        killed_at_deadline: killed,
    }
}

fn write_json<T: serde::Serialize>(fx: &Fixture, name: &str, value: &T) -> Result<PathBuf, String> {
    let dir = fx.root.join("io");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Runs `<exe> run --policy <file> --request <file> [extra args]` and reads an `ExecutionResult` from its
/// standard output. This is the contract every backend helper implements.
pub struct ExternalLauncher {
    pub exe: PathBuf,
    pub extra_args: Vec<String>,
}

impl Launcher for ExternalLauncher {
    fn name(&self) -> String {
        format!("{} {}", self.exe.display(), self.extra_args.join(" "))
    }

    fn launch(
        &self,
        fx: &Fixture,
        policy: &PolicyInput,
        request: &RequestInput,
        host_env: &[(String, String)],
    ) -> Result<Launch, String> {
        let policy_file = write_json(fx, "policy.json", policy)?;
        let request_file = write_json(fx, "request.json", request)?;
        let child = Command::new(&self.exe)
            .arg("run")
            .arg("--policy")
            .arg(policy_file)
            .arg("--request")
            .arg(request_file)
            .args(&self.extra_args)
            .envs(host_env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.exe.display()))?;
        let deadline = Duration::from_millis(policy.timeout_ms.unwrap_or(120_000)) + Duration::from_secs(30);
        let got = capture(child, deadline);
        if got.killed_at_deadline {
            return Err("the helper did not return, even well after the policy timeout".into());
        }
        let text = String::from_utf8_lossy(&got.stdout);
        let result: ExecutionResult = serde_json::from_str(text.trim()).map_err(|e| {
            format!(
                "the helper did not print an ExecutionResult ({e}); exit {:?}; stderr: {}",
                got.exit_code,
                String::from_utf8_lossy(&got.stderr).trim()
            )
        })?;
        if result.enforcement == Enforcement::Unavailable {
            let reason = result
                .denials
                .first()
                .map(|d| d.message.clone())
                .unwrap_or_else(|| "no reason given".into());
            return Ok(Launch::Unavailable(reason));
        }
        Ok(Launch::Ran(result))
    }
}

/// The control: runs the program directly, with the environment built from the request alone and nothing
/// else. It enforces no path, network, process, timeout or output rule, so every OS-held deny case must
/// fail against it. If one passes, that probe cannot tell a closed door from an open one.
pub struct UnsandboxedControl;

impl Launcher for UnsandboxedControl {
    fn name(&self) -> String {
        "unsandboxed control".into()
    }

    fn launch(
        &self,
        _fx: &Fixture,
        policy: &PolicyInput,
        request: &RequestInput,
        host_env: &[(String, String)],
    ) -> Result<Launch, String> {
        let _ = host_env; // the control never sees the host's variables: it builds its environment from the request
        let started = now_ms();
        let child = Command::new(&request.argv[0])
            .args(&request.argv[1..])
            .current_dir(&request.cwd)
            .env_clear()
            .envs(&request.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", request.argv[0]))?;
        let deadline = Duration::from_millis(policy.timeout_ms.unwrap_or(120_000)) + Duration::from_secs(2);
        let got = capture(child, deadline);
        Ok(Launch::Ran(ExecutionResult {
            exit_code: got.exit_code,
            timed_out: false,
            signal: None,
            backend: Some("control".into()),
            enforcement: Enforcement::Unenforced,
            denials: Vec::new(),
            limitations: Vec::new(),
            stdout: String::from_utf8_lossy(&got.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&got.stderr).into_owned(),
            stdout_truncated: false,
            stderr_truncated: false,
            started_at: started,
            finished_at: Some(now_ms()),
        }))
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

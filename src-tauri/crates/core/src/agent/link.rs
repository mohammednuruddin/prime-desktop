//! The connection to the agent daemon, on Prime Agent's own client.
//!
//! One daemon serves every chat. The app finds it on its socket, checks it is the build it was tested with, starts it if
//! it is not running, and replaces it once if a different build holds the socket.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use pa_tui::daemon_client::{DaemonClient, DaemonClientEvent};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedReceiver;

use super::gate;
use super::session::Session;
use crate::platform;

/// A cold supervisor takes about 20 s to start listening (it loads its model catalog first), so waiting for it has to
/// last longer than that.
pub const START_TIMEOUT: Duration = Duration::from_secs(45);

const CREATE_TIMEOUT_MS: u64 = 120_000;

pub type Events = UnboundedReceiver<DaemonClientEvent>;
pub type Connected = (DaemonClient, Events);

pub struct DaemonLink {
    binary: PathBuf,
    socket: PathBuf,
    /// Added to the daemon's environment when the app starts it (the sandbox's kernel wrapper, for one).
    env: Vec<(String, String)>,
    /// When the daemon was last started. One started recently is still starting; another start in that window only logs
    /// "already owns the socket" and leaves one more process running.
    spawned_at: Mutex<Option<Instant>>,
    /// Where the environment of the daemon this app started is written down (see [`DaemonLink::environment_is_ours`]).
    record: PathBuf,
    /// One connection attempt at a time: chats open together, and each would otherwise try to replace the same daemon.
    connecting: tokio::sync::Mutex<()>,
}

/// What identifies the environment a daemon was started with: the variables the app adds, and the folder the agent keeps
/// its files in (which it inherits).
fn fingerprint(env: &[(String, String)]) -> String {
    let mut lines: Vec<String> = env.iter().map(|(key, value)| format!("{key}={value}")).collect();
    lines.sort();
    lines.push(format!("agent_dir={}", platform::prime_agent_dir().display()));
    hex::encode(Sha256::digest(lines.join("\n")))
}

impl DaemonLink {
    pub fn new(binary: PathBuf, socket: PathBuf, env: Vec<(String, String)>) -> Self {
        Self {
            binary,
            socket,
            env,
            spawned_at: Mutex::new(None),
            record: platform::app_data_dir().join("daemon.json"),
            connecting: tokio::sync::Mutex::new(()),
        }
    }

    #[cfg(test)]
    fn with_record(mut self, record: PathBuf) -> Self {
        self.record = record;
        self
    }

    /// Whether a running daemon was started with the environment this link would give it. A daemon outlives the window, so
    /// one left from an earlier run can hold a stale sandbox (or none): the app wrote down what it started and trusts only
    /// that. A daemon it did not start is trusted only when no environment is wanted, which is the unprotected choice.
    fn environment_is_ours(&self, hello: &Value) -> bool {
        let pid = hello.get("supervisorPid").and_then(Value::as_u64);
        let recorded = std::fs::read_to_string(&self.record)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        match recorded {
            Some(record) if pid.is_some() && record["pid"].as_u64() == pid => record["env"] == fingerprint(&self.env),
            _ => self.env.is_empty(),
        }
    }

    /// Whether the daemon answering is the one the record names, with an environment that is not the wanted one.
    fn is_recorded_stale(&self, hello: &Value) -> bool {
        let pid = hello.get("supervisorPid").and_then(Value::as_u64);
        let recorded = std::fs::read_to_string(&self.record)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        recorded.is_some_and(|record| {
            pid.is_some() && record["pid"].as_u64() == pid && record["env"] != fingerprint(&self.env)
        })
    }

    fn remember(&self, hello: &Value) {
        let Some(pid) = hello.get("supervisorPid").and_then(Value::as_u64) else {
            return;
        };
        if let Some(parent) = self.record.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &self.record,
            json!({"pid": pid, "env": fingerprint(&self.env)}).to_string(),
        );
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// A client for a daemon that passes the version gate, starting one if none answers.
    pub async fn connect(&self, skip_existing: bool) -> Result<Connected> {
        let _alone = self.connecting.lock().await;
        let mut replaced = false;
        if !skip_existing {
            match DaemonClient::connect(&self.socket).await {
                Ok((client, events)) => {
                    if gate::problem(client.hello()).is_none() && self.environment_is_ours(client.hello()) {
                        return Ok((client, events));
                    }
                    // Another build of Prime Agent holds the socket, such as a daemon the Prime CLI left running, or a
                    // daemon started with another sandbox than the one wanted now.
                    self.stop_stale(&client).await?;
                    replaced = true;
                }
                Err(error) => self.remove_dead_socket(&error),
            }
        }
        self.start_once(skip_existing || replaced);
        let deadline = Instant::now() + START_TIMEOUT;
        let mut last = None;
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            match DaemonClient::connect(&self.socket).await {
                Ok((client, events)) => {
                    // Just started from the pinned executable. If it still is not the build the app was tested with,
                    // starting again cannot change that.
                    if let Some(problem) = gate::problem(client.hello()) {
                        client.close();
                        bail!(problem);
                    }
                    // A daemon this app recorded with another environment that is still the one answering was not
                    // replaced (it was slow to go): it must not be written down as having the wanted one.
                    if self.is_recorded_stale(client.hello()) {
                        client.close();
                        bail!(
                            "Prime Agent's daemon from an earlier run could not be replaced. Close it and try again."
                        );
                    }
                    self.remember(client.hello());
                    return Ok((client, events));
                }
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("Prime Agent daemon did not start.")))
    }

    /// Ends the daemon on the socket, if one answers.
    pub async fn stop(&self) {
        if let Ok((client, _events)) = DaemonClient::connect(&self.socket).await {
            let _ = self.stop_stale(&client).await;
        }
    }

    /// A socket file with no listener means a daemon died without cleanup. The file has to go or a fresh supervisor
    /// cannot bind it. Only a refused connection counts: a daemon that is slow to answer must keep its socket.
    fn remove_dead_socket(&self, error: &anyhow::Error) {
        let refused = error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
        });
        if refused && !platform::is_named_pipe(&self.socket) {
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    fn start_once(&self, force: bool) {
        let Ok(mut spawned) = self.spawned_at.lock() else {
            return;
        };
        if !force && spawned.is_some_and(|at| at.elapsed() < START_TIMEOUT) {
            return;
        }
        *spawned = Some(Instant::now());
        // Prime Agent 0.10.0 on Windows loses a session's messages when its sessions folder is created while the
        // session starts (see docs/memory/rust-agent-windows-transcripts.md). Creating it first avoids that.
        let _ = std::fs::create_dir_all(platform::prime_agent_dir().join("sessions"));
        if let Some(parent) = self.socket.parent().filter(|_| !platform::is_named_pipe(&self.socket)) {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut command = Command::new(&self.binary);
        command
            .args(["--mode", "daemon", "--daemon-socket"])
            .arg(&self.socket)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        platform::detached(&mut command);
        // The handle is dropped on purpose: the daemon outlives the window.
        let _ = command.spawn();
    }

    async fn is_gone(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if DaemonClient::connect(&self.socket).await.is_err() {
                if !platform::is_named_pipe(&self.socket) {
                    let _ = std::fs::remove_file(&self.socket);
                }
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    async fn supervisor_pid(&self) -> Option<u32> {
        let (client, _events) = DaemonClient::connect(&self.socket).await.ok()?;
        let pid = client
            .hello()
            .get("supervisorPid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok());
        client.close();
        pid.filter(|pid| *pid > 0)
    }

    /// Asks the daemon on the socket to exit, and kills it only if it is still the same process afterwards.
    async fn stop_stale(&self, client: &DaemonClient) -> Result<()> {
        let stale = client
            .hello()
            .get("supervisorPid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok());
        if let Ok(command) = serde_json::from_value(json!({"type": "shutdown", "force": true})) {
            let _ = client.request_with_timeout(command, 5_000).await;
        }
        client.close();
        if self.is_gone(Duration::from_secs(3)).await {
            return Ok(());
        }
        let current = self.supervisor_pid().await;
        let Some(stale) = stale.filter(|pid| Some(*pid) == current) else {
            bail!(
                "Could not safely stop the stale Prime Agent daemon at {}",
                self.socket.display()
            );
        };
        kill(stale, false);
        if self.is_gone(Duration::from_secs(2)).await {
            return Ok(());
        }
        kill(stale, true);
        if self.is_gone(Duration::from_secs(2)).await {
            return Ok(());
        }
        bail!(
            "Timed out stopping the stale Prime Agent daemon at {}",
            self.socket.display()
        )
    }
}

fn kill(pid: u32, force: bool) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid.to_string(), "/T"]);
        if force {
            command.arg("/F");
        }
        command
    } else {
        let mut command = Command::new("kill");
        command.args([if force { "-KILL" } else { "-TERM" }, &pid.to_string()]);
        command
    };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    platform::quiet(&mut command);
    let _ = command.status();
}

/// What a new chat session starts with.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub cwd: PathBuf,
    pub thinking: String,
    /// `provider/model` or a bare model id.
    pub model: Option<String>,
    /// The autonomous-mode settings, already in the daemon's shape.
    pub autonomous: Value,
    pub rlm_max_depth: u64,
    /// `sse`, `websocket`, `websocket-cached` or `auto`.
    pub transport: String,
    /// More fields for the daemon's session config (a scripted model in tests).
    pub extra: Option<Value>,
}

pub struct Opened {
    pub session: Session,
    pub events: Events,
}

fn split_model(model: &str) -> (Option<&str>, &str) {
    match model.find('/') {
        Some(slash) if slash > 0 => (Some(&model[..slash]), &model[slash + 1..]),
        _ => (None, model),
    }
}

async fn create_session(client: &DaemonClient, create: &Value) -> Result<Value> {
    let command = serde_json::from_value(create.clone())?;
    let response = client.request_with_timeout(command, CREATE_TIMEOUT_MS).await?;
    crate::agent::session::data_of("create", response)
}

/// Starts receiving a session's events on this connection.
pub async fn attach(client: &DaemonClient, active_session_id: &str) -> Result<()> {
    let attach = json!({
        "type": "attach",
        "activeSessionId": active_session_id,
        "supportsExtensionUi": true,
        "clientId": format!("{}:{}", crate::product::product().slug(), uuid::Uuid::new_v4()),
        "capabilities": ["attach_snapshot", "event_sequence", "extension_ui"],
    });
    client.request_ok(serde_json::from_value(attach)?).await?;
    Ok(())
}

/// Creates a resident session and attaches to it.
pub async fn open_session(link: &DaemonLink, config: &SessionConfig) -> Result<Opened> {
    let (mut client, mut events) = link.connect(false).await?;
    let mut create = json!({
        "type": "create",
        "lifecycle": "resident",
        "config": {"cwd": config.cwd, "thinking": config.thinking, "autonomous": config.autonomous},
    });
    if let Some(Value::Object(extra)) = &config.extra {
        for (key, value) in extra {
            create["config"][key] = value.clone();
        }
    }
    if let Some(model) = config.model.as_deref().filter(|model| !model.is_empty()) {
        let (provider, id) = split_model(model);
        if let Some(provider) = provider {
            create["config"]["provider"] = provider.into();
        }
        create["config"]["model"] = id.into();
    }
    let created = match create_session(&client, &create).await {
        Ok(data) => data,
        // A daemon can outlive what started it, or lose ownership of its on-disk registry (an update, a crash of a
        // prior generation). Both leave this supervisor unrecoverable: ask it to exit, start a fresh one.
        Err(error)
            if ["worker pid", "ENOENT", "registry entry", "generation"]
                .iter()
                .any(|needle| error.to_string().contains(needle)) =>
        {
            link.stop_stale(&client).await?;
            (client, events) = link.connect(true).await?;
            create_session(&client, &create).await?
        }
        Err(error) => return Err(error),
    };
    let id = created["activeSessionId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("Prime Agent daemon did not return an active session."))?
        .to_string();
    let session = Session::new(client.clone(), id.clone());
    attach(&client, &id).await?;

    // A resident daemon can outlive the window and keep the model from the previous client. Apply the app's choice after
    // attach, before the first prompt can be sent.
    if let Some(model) = config.model.as_deref().filter(|model| !model.is_empty()) {
        let (provider, id) = split_model(model);
        let provider = match provider {
            Some(provider) => provider.to_string(),
            None => {
                let models = session
                    .call("get_available_models", json!({}))
                    .await
                    .unwrap_or(Value::Null);
                models["models"]
                    .as_array()
                    .and_then(|list| list.iter().find(|m| m["id"] == id))
                    .and_then(|m| m["provider"].as_str())
                    .unwrap_or("")
                    .to_string()
            }
        };
        if !provider.is_empty() {
            session
                .call("set_model", json!({"provider": provider, "modelId": id}))
                .await?;
        }
    }
    session
        .call(
            "set_rlm_max_depth",
            json!({"maxDepth": config.rlm_max_depth, "global": false}),
        )
        .await?;
    let _ = session
        .call("set_transport", json!({"transport": config.transport}))
        .await;
    Ok(Opened { session, events })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_in(dir: &Path, env: Vec<(String, String)>) -> DaemonLink {
        DaemonLink::new(PathBuf::from("prime-agent"), dir.join("sock"), env).with_record(dir.join("daemon.json"))
    }

    fn wrapper() -> Vec<(String, String)> {
        vec![("PRIME_AGENT_KERNEL_PYTHON".into(), "/app/desktop-kernel".into())]
    }

    #[test]
    fn a_daemon_is_reused_only_when_it_was_started_with_the_wanted_environment() {
        let dir = tempfile::tempdir().unwrap();
        let hello = json!({"supervisorPid": 4242});
        let protected = link_in(dir.path(), wrapper());
        assert!(
            !protected.environment_is_ours(&hello),
            "a daemon the app never wrote down may have no sandbox"
        );
        protected.remember(&hello);
        assert!(protected.environment_is_ours(&hello));
        assert!(
            !protected.environment_is_ours(&json!({"supervisorPid": 4243})),
            "a different process holds the socket now"
        );
        let other = link_in(
            dir.path(),
            vec![("PRIME_AGENT_KERNEL_PYTHON".into(), "/elsewhere/desktop-kernel".into())],
        );
        assert!(
            !other.environment_is_ours(&hello),
            "the wrapper moved, so the old daemon holds a stale one"
        );
    }

    #[test]
    fn without_a_wanted_environment_a_daemon_nobody_recorded_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let unprotected = link_in(dir.path(), Vec::new());
        assert!(unprotected.environment_is_ours(&json!({"supervisorPid": 7})));
        // But one that was started protected is not what the user chose now.
        link_in(dir.path(), wrapper()).remember(&json!({"supervisorPid": 7}));
        assert!(!unprotected.environment_is_ours(&json!({"supervisorPid": 7})));
    }

    #[test]
    fn the_daemon_a_record_calls_stale_is_never_stamped_as_wanted() {
        let dir = tempfile::tempdir().unwrap();
        let hello = json!({"supervisorPid": 9});
        link_in(dir.path(), Vec::new()).remember(&hello);
        let protected = link_in(dir.path(), wrapper());
        assert!(protected.is_recorded_stale(&hello));
        assert!(
            !protected.is_recorded_stale(&json!({"supervisorPid": 10})),
            "a new daemon is not the stale one"
        );
        assert!(!link_in(dir.path(), Vec::new()).is_recorded_stale(&hello));
    }

    #[test]
    fn the_order_of_variables_does_not_change_the_fingerprint() {
        let a = vec![("A".to_string(), "1".to_string()), ("B".to_string(), "2".to_string())];
        let b = vec![("B".to_string(), "2".to_string()), ("A".to_string(), "1".to_string())];
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_ne!(fingerprint(&a), fingerprint(&[]));
    }

    #[test]
    fn a_model_is_split_at_the_first_slash() {
        assert_eq!(split_model("openai/gpt-x"), (Some("openai"), "gpt-x"));
        assert_eq!(
            split_model("openrouter/anthropic/claude"),
            (Some("openrouter"), "anthropic/claude")
        );
        assert_eq!(split_model("gpt-x"), (None, "gpt-x"));
        assert_eq!(split_model("/odd"), (None, "/odd"));
    }
}

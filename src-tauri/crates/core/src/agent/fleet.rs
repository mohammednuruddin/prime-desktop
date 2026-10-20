//! The fleet panels: schedules and the heartbeat, messages between agents, the subagent tree, watching another session,
//! and the sessions other Prime Agent clients (the terminal) are running.
//!
//! Every function answers in the JSON the renderer reads. Questions the panels poll (the tree, the heartbeat, the
//! schedules) never start a session that is not running; actions the user takes may, through `Manager::session_of`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pa_tui::daemon_client::{DaemonClient, DaemonClientEvent};
use pa_types::daemon::DaemonCommand;
use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;

use super::manager::Manager;
use super::session::{Session, data_of};
use super::sessions::{now_ms, number_of, parse_iso_ms, scalar_text};
use crate::{platform, validate};

/// `String(...)` of the first of `keys` that is set, as JavaScript would print it.
fn first_text(record: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| record.get(*key))
        .find(|value| !value.is_null())
        .and_then(scalar_text)
}

/// [`first_text`], empty when none is set.
fn text_of(record: &Value, keys: &[&str]) -> String {
    first_text(record, keys).unwrap_or_default()
}

/// A whole number as JSON integer, so it reads `3` and not `3.0`.
pub(crate) fn num(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9e15 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

/// A required argument: at most `max` characters, not blank. Returned as given, because a message is sent as typed.
fn required<'a>(name: &str, value: &'a str, max: usize) -> Result<&'a str> {
    validate::text(name, value, max)?;
    if value.trim().is_empty() {
        bail!("{name} is required");
    }
    Ok(value)
}

fn command(value: Value) -> Result<DaemonCommand> {
    serde_json::from_value(value).context("not a daemon command")
}

fn list_command() -> Result<DaemonCommand> {
    command(json!({"type": "list", "includeClientOwned": true}))
}

// ---------- Schedules and the heartbeat ----------

/// A scheduled job in the renderer's `ScheduleJob` shape. Fields the daemon did not send are left out.
fn schedule_job(job: &Value) -> Value {
    let schedule = job.get("schedule").filter(|schedule| !schedule.is_null());
    let status = first_text(job, &["status"]).unwrap_or_else(|| "active".into());
    let cron = [
        schedule.and_then(|schedule| schedule.get("expression")),
        job.get("cron"),
    ]
    .into_iter()
    .flatten()
    .find(|value| !value.is_null())
    .and_then(scalar_text)
    .unwrap_or_default();
    let mut out = Map::new();
    out.insert("id".into(), text_of(job, &["id"]).into());
    out.insert("cron".into(), cron.into());
    out.insert("prompt".into(), text_of(job, &["prompt"]).into());
    out.insert("active".into(), (status == "active").into());
    out.insert("status".into(), status.into());
    for key in ["source", "runtimeKind", "deliveryMode", "schedule"] {
        if let Some(value) = job.get(key).filter(|value| !value.is_null()) {
            out.insert(key.into(), value.clone());
        }
    }
    for key in [
        "activeSessionId",
        "sessionId",
        "label",
        "createdAt",
        "updatedAt",
        "nextRunAt",
        "lastRunAt",
        "lastSkippedAt",
        "lastError",
    ] {
        if job[key].is_string() {
            out.insert(key.into(), job[key].clone());
        }
    }
    if job["runCount"].is_number() {
        out.insert("runCount".into(), job["runCount"].clone());
    }
    Value::Object(out)
}

/// The scheduled jobs of every connected chat, by agent id. A chat whose daemon does not answer lists none.
pub async fn list_schedules(manager: &Manager) -> Result<Value> {
    let mut out = Map::new();
    for chat in manager.chats() {
        let Some(session) = chat.session() else { continue };
        let jobs = match session.send(&json!({"type": "list_schedules"})).await {
            Ok(answer) => answer["jobs"]
                .as_array()
                .map(|jobs| jobs.iter().map(schedule_job).collect()),
            Err(_) => None,
        };
        out.insert(chat.id.clone(), Value::Array(jobs.unwrap_or_default()));
    }
    Ok(Value::Object(out))
}

pub async fn add_schedule(manager: &Manager, agent_id: &str, schedule: &str, prompt: &str) -> Result<Value> {
    let schedule = required("schedule", schedule, 500)?;
    let prompt = required("prompt", prompt, validate::DEFAULT_MAX_TEXT)?;
    let session = manager.session_of(agent_id).await?;
    session
        .send(&json!({"type": "add_schedule", "schedule": schedule, "prompt": prompt}))
        .await
}

pub async fn cancel_schedule(manager: &Manager, agent_id: &str, job_id: &str) -> Result<Value> {
    let job_id = required("job id", job_id, 256)?;
    let session = manager.session_of(agent_id).await?;
    session.send(&json!({"type": "cancel_schedule", "jobId": job_id})).await
}

/// The chat's heartbeat in the renderer's `Heartbeat` shape, or null when it has none or cannot be asked.
pub async fn get_heartbeat(manager: &Manager, agent_id: &str) -> Result<Value> {
    let Some(session) = manager.chat(agent_id).and_then(|chat| chat.session()) else {
        return Ok(Value::Null);
    };
    let Ok(answer) = session.send(&json!({"type": "get_heartbeat"})).await else {
        return Ok(Value::Null);
    };
    let beat = &answer["heartbeat"];
    if beat.is_null() {
        return Ok(Value::Null);
    }
    let schedule = [beat["schedule"].get("expression"), Some(&beat["schedule"])]
        .into_iter()
        .flatten()
        .find(|value| !value.is_null())
        .and_then(scalar_text)
        .unwrap_or_default();
    Ok(json!({
        "id": beat.get("id").cloned().unwrap_or(Value::Null),
        "schedule": schedule,
        "prompt": text_of(beat, &["prompt"]),
        "status": first_text(beat, &["status"]).unwrap_or_else(|| "active".into()),
    }))
}

pub async fn set_heartbeat(manager: &Manager, agent_id: &str, schedule: &str, prompt: &str) -> Result<Value> {
    let schedule = required("schedule", schedule, 500)?;
    let prompt = required("prompt", prompt, validate::DEFAULT_MAX_TEXT)?;
    let session = manager.session_of(agent_id).await?;
    session
        .send(&json!({"type": "set_heartbeat", "schedule": schedule, "prompt": prompt}))
        .await
}

pub async fn heartbeat_action(manager: &Manager, agent_id: &str, action: &str) -> Result<Value> {
    let action = required("action", action, 64)?;
    let session = manager.session_of(agent_id).await?;
    session
        .send(&json!({"type": "update_heartbeat", "action": action}))
        .await
}

// ---------- Messages between agents ----------

/// A message from one of the app's chats to another session. `mode` is `steer` or `follow_up`; the TypeScript accepted it
/// and dropped it on the way to the daemon, so the delivery selector did nothing. `auto` (or none) leaves the choice to
/// the daemon.
pub async fn send_message(
    manager: &Manager,
    agent_id: &str,
    target: &str,
    message: &str,
    mode: Option<&str>,
) -> Result<Value> {
    let target = required("target", target, 256)?;
    let message = required("message", message, validate::DEFAULT_MAX_TEXT)?;
    let session = manager.session_of(agent_id).await?;
    let mut command = json!({
        "type": "send_message",
        "targetActiveSessionId": target,
        "message": message,
        "fromActiveSessionId": session.id(),
    });
    if let Some(mode) = mode.filter(|mode| matches!(*mode, "steer" | "follow_up")) {
        command["deliveryMode"] = mode.into();
    }
    session.control(command).await
}

/// A message to a session of another Prime Agent client, on the daemon the terminal started. The app is no session, so
/// the message carries no sender.
pub async fn message_external(target: &str, message: &str) -> Result<Value> {
    if target.trim().is_empty() || message.trim().is_empty() {
        bail!("Target and message are required");
    }
    validate::text("target", target, 256)?;
    validate::text("message", message, validate::DEFAULT_MAX_TEXT)?;
    message_external_on(&cli_socket(), target.trim(), message).await
}

async fn message_external_on(socket: &Path, target: &str, message: &str) -> Result<Value> {
    let (client, _events) = DaemonClient::connect(socket).await?;
    let sent = match command(json!({"type": "send_message", "targetActiveSessionId": target, "message": message})) {
        Ok(command) => client
            .request_with_timeout(command, 30_000)
            .await
            .and_then(|response| data_of("send_message", response)),
        Err(error) => Err(error),
    };
    client.close();
    sent
}

// ---------- Other clients' sessions ----------

/// The daemon the Prime Agent terminal starts, which is not the app's own. The same location Prime Agent computes: a
/// socket under the temp folder named for the user on Unix, a fixed named pipe on Windows.
fn cli_socket() -> PathBuf {
    if cfg!(windows) {
        return PathBuf::from(r"\\.\pipe\prime-agent-daemon");
    }
    let tmp = std::env::var_os("TMPDIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    tmp.join(format!("prime-agent-{}", user_id())).join("daemon.sock")
}

fn user_id() -> String {
    static ID: LazyLock<String> = LazyLock::new(|| {
        let mut id = std::process::Command::new("id");
        id.arg("-ru");
        platform::quiet(&mut id);
        id.output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default()
    });
    ID.clone()
}

/// `external:list`: the terminal's sessions, newest first. Read-only; no daemon, or one that does not answer, is none.
pub async fn list_external() -> Result<Value> {
    Ok(Value::Array(list_external_on(&cli_socket()).await))
}

async fn list_external_on(socket: &Path) -> Vec<Value> {
    if !platform::is_named_pipe(socket) && !socket.exists() {
        return Vec::new();
    }
    let Ok(Ok((client, _events))) =
        tokio::time::timeout(Duration::from_millis(1_200), DaemonClient::connect(socket)).await
    else {
        return Vec::new();
    };
    let listed = match list_command() {
        Ok(list) => client.request_with_timeout(list, 8_000).await,
        Err(error) => Err(error),
    };
    client.close();
    let Ok(response) = listed else { return Vec::new() };
    if !response.success {
        return Vec::new();
    }
    let sessions = response.data.as_ref().and_then(|data| data["sessions"].as_array());
    let mut out: Vec<(f64, Value)> = Vec::new();
    for session in sessions.into_iter().flatten() {
        let runtime = session
            .get("runtimeKind")
            .filter(|kind| !kind.is_null())
            .and_then(scalar_text);
        if runtime.as_deref().unwrap_or("root") == "subagent" || text_of(session, &["lifecycle"]) == "archived" {
            continue;
        }
        let session_id = text_of(session, &["sessionId", "id"]);
        let active = Some(text_of(session, &["activeSessionId"]))
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| session_id.clone());
        if session_id.is_empty() || active.is_empty() {
            continue;
        }
        let status = status_of(session, false);
        let first = text_of(session, &["firstMessage"]);
        let first = if first == "(no messages)" { String::new() } else { first };
        let name = Some(text_of(session, &["sessionName"]))
            .filter(|name| !name.is_empty())
            .or_else(|| Some(first.chars().take(60).collect::<String>()).filter(|name| !name.is_empty()))
            .unwrap_or_else(|| session_id.chars().take(8).collect());
        let at = activity_time(session).unwrap_or(0.0);
        out.push((
            at,
            json!({
                "activeSessionId": active,
                "sessionId": session_id,
                "name": name,
                "task": first.chars().take(200).collect::<String>(),
                "cwd": text_of(session, &["cwd", "workingDirectory"]),
                "status": status,
                "isStreaming": session["isStreaming"] == true || status == "working",
                "lastActivityAt": num(at),
            }),
        ));
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().map(|(_, session)| session).collect()
}

/// What a daemon session is doing, in the renderer's words. A subagent can also be `archived`, or have a worker that
/// failed.
fn status_of(session: &Value, subagent: bool) -> &'static str {
    let raw = text_of(session, &["status", "activity"]).to_lowercase();
    if (subagent && text_of(session, &["workerState"]).eq_ignore_ascii_case("failed"))
        || matches!(raw.as_str(), "error" | "failed")
    {
        "error"
    } else if matches!(raw.as_str(), "working" | "queued" | "starting") || session["isStreaming"] == true {
        "working"
    } else if subagent && (text_of(session, &["lifecycle"]).eq_ignore_ascii_case("archived") || raw == "archived") {
        "archived"
    } else {
        "idle"
    }
}

fn activity_time(session: &Value) -> Option<f64> {
    parse_iso_ms(&text_of(session, &["lastActivityAt", "modified", "created"]))
}

// ---------- The subagent tree ----------

/// `fleet:tree`: the chat's subagents, nested under the sessions that started them. Empty when the chat is not connected.
pub async fn subagent_tree(manager: &Manager, agent_id: &str) -> Result<Value> {
    let Some(session) = manager.chat(agent_id).and_then(|chat| chat.session()) else {
        return Ok(json!([]));
    };
    let get_state = json!({"type": "get_state"});
    let (state, listed, children) = tokio::join!(
        session.send(&get_state),
        session.control(json!({"type": "list", "includeClientOwned": true})),
        child_details(&session),
    );
    let (state, listed) = (state?, listed?);
    let root = text_of(&state, &["sessionId"]);
    if root.is_empty() {
        return Ok(json!([]));
    }
    let mut details: HashMap<String, &Value> = HashMap::new();
    for child in &children {
        for key in [text_of(child, &["id"]), text_of(child, &["activeSessionId"])] {
            if !key.is_empty() {
                details.insert(key, child);
            }
        }
    }
    let mut by_parent: HashMap<String, Vec<&Value>> = HashMap::new();
    for session in listed["sessions"].as_array().into_iter().flatten() {
        let parent = text_of(session, &["parentSessionId"]);
        if text_of(session, &["runtimeKind"]) == "subagent" && !parent.is_empty() {
            by_parent.entry(parent).or_default().push(session);
        }
    }
    Ok(Value::Array(subagent_nodes(
        &root,
        &by_parent,
        &details,
        &mut vec![root.clone()],
    )))
}

/// The daemon's own account of each child (model, duration, preview). Without it the tree still lists the children.
async fn child_details(session: &Session) -> Vec<Value> {
    if !session.supports("authoritative_child_roster") {
        return Vec::new();
    }
    match session.call("get_rlm_children", json!({})).await {
        Ok(data) => data["children"].as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn subagent_nodes(
    parent: &str,
    by_parent: &HashMap<String, Vec<&Value>>,
    details: &HashMap<String, &Value>,
    ancestors: &mut Vec<String>,
) -> Vec<Value> {
    let mut nodes: Vec<(f64, Value)> = Vec::new();
    for session in by_parent.get(parent).into_iter().flatten() {
        let session_id = text_of(session, &["sessionId", "id"]);
        // A session that lists itself among its own ancestors would recurse forever.
        if session_id.is_empty() || ancestors.contains(&session_id) {
            continue;
        }
        let first = text_of(session, &["firstMessage"]);
        let at = activity_time(session).unwrap_or_else(now_ms);
        let active = text_of(session, &["activeSessionId"]);
        let detail = [text_of(session, &["rlmChildId"]), active.clone()]
            .iter()
            .find_map(|key| details.get(key.as_str()).copied());
        let mut node = Map::new();
        let id = first_text(session, &["rlmChildId", "activeSessionId", "id"]).unwrap_or_else(|| session_id.clone());
        node.insert("id".into(), id.into());
        node.insert("sessionId".into(), session_id.clone().into());
        node.insert(
            "activeSessionId".into(),
            if active.is_empty() { Value::Null } else { active.into() },
        );
        node.insert("parentSessionId".into(), parent.into());
        let name = first_text(session, &["sessionName", "rlmChildId"]).unwrap_or_else(|| "Subagent".into());
        node.insert("name".into(), name.into());
        node.insert(
            "depth".into(),
            num(session.get("rlmDepth").and_then(number_of).unwrap_or(1.0)),
        );
        node.insert("status".into(), status_of(session, true).into());
        node.insert(
            "task".into(),
            if first == "(no messages)" { String::new() } else { first }.into(),
        );
        node.insert("lastActivityAt".into(), num(at));
        if let Some(detail) = detail {
            for key in ["model", "answerPreview", "recap", "error"] {
                if detail[key].is_string() {
                    node.insert(key.into(), detail[key].clone());
                }
            }
            for key in ["durationMs", "toolUseCount", "tokenCount"] {
                if detail[key].is_number() {
                    node.insert(key.into(), detail[key].clone());
                }
            }
            if !detail["activity"].is_null() {
                node.insert("activity".into(), detail["activity"].clone());
            }
        }
        ancestors.push(session_id.clone());
        node.insert(
            "children".into(),
            Value::Array(subagent_nodes(&session_id, by_parent, details, ancestors)),
        );
        ancestors.pop();
        nodes.push((at, Value::Object(node)));
    }
    nodes.sort_by(|a, b| a.0.total_cmp(&b.0));
    nodes.into_iter().map(|(_, node)| node).collect()
}

/// `fleet:messages`: the transcript of one session the chat started (a subagent). Empty when the chat is not connected.
pub async fn subagent_messages(manager: &Manager, agent_id: &str, active_session_id: &str) -> Result<Value> {
    let Some(session) = manager.chat(agent_id).and_then(|chat| chat.session()) else {
        return Ok(json!([]));
    };
    let selector = active_session_id.trim();
    if selector.is_empty() {
        return Ok(json!([]));
    }
    validate::text("session id", selector, 256)?;
    let messages = session
        .client()
        .request_ok(command(json!({"type": "get_messages", "activeSessionId": selector}))?)
        .await?;
    Ok(messages
        .get("messages")
        .cloned()
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([])))
}

// ---------- Watching another session ----------

struct Observer {
    id: u64,
    /// The session id the daemon knows the target by, which the target the renderer named may not be.
    active_session_id: String,
    client: DaemonClient,
    task: JoinHandle<()>,
}

/// Sessions being watched, by the id the renderer named. A process-wide registry: the manager has no slot for them, so
/// `shutdown_observers` is what releases them on exit.
static OBSERVERS: LazyLock<Mutex<HashMap<String, Observer>>> = LazyLock::new(Mutex::default);
static NEXT_OBSERVER: AtomicU64 = AtomicU64::new(1);

fn observers() -> std::sync::MutexGuard<'static, HashMap<String, Observer>> {
    OBSERVERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The streamed text of a reply changes dozens of times a second. The panel keeps the last 200 entries, so these would
/// push everything useful out of it and cost a re-render each.
fn is_noise(event: &Value) -> bool {
    matches!(event["type"].as_str(), Some("message_update" | "tool_execution_update"))
}

/// The active session id a daemon knows `target` by, whether it was given as that id or as the saved session's id.
async fn active_id_on(client: &DaemonClient, target: &str) -> Result<Option<String>> {
    let listed = client.request_ok(list_command()?).await?;
    Ok(listed["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|session| {
            ["activeSessionId", "sessionId", "id"]
                .iter()
                .any(|key| session[*key] == target)
        })
        .map(|session| text_of(session, &["activeSessionId", "id"]))
        .filter(|id| !id.is_empty()))
}

/// Where `target` is running: the app's daemon, else the terminal's.
async fn locate(owner: &Session, target: &str) -> Result<(PathBuf, String)> {
    let app = owner.client().socket_path().to_path_buf();
    if let Some(active) = active_id_on(owner.client(), target).await? {
        return Ok((app, active));
    }
    let cli = cli_socket();
    if cli != app
        && (platform::is_named_pipe(&cli) || cli.exists())
        && let Ok(Ok((client, _events))) =
            tokio::time::timeout(Duration::from_millis(1_200), DaemonClient::connect(&cli)).await
    {
        let found = active_id_on(&client, target).await;
        client.close();
        if let Some(active) = found.ok().flatten() {
            return Ok((cli, active));
        }
    }
    bail!("No running session {target}")
}

/// `fleet:observe`: attaches a second connection to the session and forwards what it does to the renderer as
/// `fleet_event`s of `agent_id`. The TypeScript did this with a second `prime-agent --mode rpc` process, which the Rust
/// agent's rpc mode cannot do.
pub async fn observe(manager: &Manager, agent_id: &str, session_id: &str) -> Result<Value> {
    let target = required("session id", session_id, 256)?.trim().to_string();
    let owner = manager.session_of(agent_id).await?;
    let (socket, active) = locate(&owner, &target).await?;
    let (client, mut events) = DaemonClient::connect(&socket).await?;
    let attach = json!({
        "type": "attach",
        "activeSessionId": active,
        // Dialogs belong to the chat that owns the session, not to a watcher.
        "supportsExtensionUi": false,
        "clientId": format!("{}:observer:{}", crate::product::product().slug(), uuid::Uuid::new_v4()),
        "capabilities": ["event_sequence"],
    });
    if let Err(error) = client.request_ok(command(attach)?).await {
        client.close();
        return Err(error);
    }

    let id = NEXT_OBSERVER.fetch_add(1, Ordering::Relaxed);
    let (watcher, owner_id, watched, key) = (manager.clone(), agent_id.to_string(), active.clone(), target.clone());
    let task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                DaemonClientEvent::SessionEvent {
                    active_session_id,
                    event,
                    ..
                } if active_session_id == watched => {
                    // The chat that asked for this was closed.
                    if watcher.chat(&owner_id).is_none() {
                        break;
                    }
                    if !is_noise(&event) {
                        watcher.emit_event(&owner_id, "fleet_event", event);
                    }
                }
                DaemonClientEvent::SessionClosed { active_session_id, .. } if active_session_id == watched => break,
                DaemonClientEvent::DaemonClosing { .. } => break,
                _ => {}
            }
        }
        // Ended by itself: forget it, unless a newer watch has taken its place.
        let own = {
            let mut registry = observers();
            if registry.get(&key).is_some_and(|observer| observer.id == id) {
                registry.remove(&key)
            } else {
                None
            }
        };
        if let Some(own) = own {
            own.client.close();
        }
    });
    // The task's last step takes this lock too, so it cannot look for its entry before the entry exists.
    let previous = observers().insert(
        target,
        Observer {
            id,
            active_session_id: active,
            client,
            task,
        },
    );
    if let Some(previous) = previous {
        release(previous).await;
    }
    Ok(json!({
        "status": "observing",
        "observed": {
            "id": session_id, "name": session_id, "sessionId": session_id, "status": "observed",
            "projectPath": "", "observed": true, "lastEvent": "", "children": [],
        },
    }))
}

async fn release(observer: Observer) {
    observer.task.abort();
    if let Ok(detach) = command(json!({"type": "detach", "activeSessionId": observer.active_session_id})) {
        let _ = tokio::time::timeout(Duration::from_secs(2), observer.client.request(detach)).await;
    }
    observer.client.close();
}

/// `fleet:unobserve`: stops watching. The id may be the one given to `observe` or the active session id it resolved to.
pub async fn unobserve(session_id: &str) -> Result<Value> {
    let found: Vec<Observer> = {
        let mut registry = observers();
        let keys: Vec<String> = registry
            .iter()
            .filter(|(key, observer)| key.as_str() == session_id || observer.active_session_id == session_id)
            .map(|(key, _)| key.clone())
            .collect();
        keys.iter().filter_map(|key| registry.remove(key)).collect()
    };
    for observer in found {
        release(observer).await;
    }
    Ok(Value::Null)
}

/// Releases every watch. The host calls this when the app quits, next to `Manager::shutdown_all`.
pub fn shutdown_observers() {
    let all: Vec<Observer> = observers().drain().map(|(_, observer)| observer).collect();
    for observer in all {
        observer.task.abort();
        observer.client.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_keeps_what_the_daemon_sent_and_nothing_else() {
        let job = schedule_job(&json!({
            "id": "j1", "schedule": {"kind": "cron", "expression": "0 9 * * 1-5"}, "prompt": "check ci",
            "status": "paused", "source": "cron", "runtimeKind": "top-level", "sessionId": "s", "label": 5,
            "createdAt": "2026-10-16T09:00:00Z", "runCount": 3, "unrelated": true,
        }));
        assert_eq!(
            job,
            json!({
                "id": "j1", "cron": "0 9 * * 1-5", "prompt": "check ci", "active": false, "status": "paused",
                "source": "cron", "runtimeKind": "top-level", "schedule": {"kind": "cron", "expression": "0 9 * * 1-5"},
                "sessionId": "s", "createdAt": "2026-10-16T09:00:00Z", "runCount": 3,
            }),
            "a label that is not text and a field the renderer does not know are dropped"
        );
        let bare = schedule_job(&json!({"id": 4, "cron": "@daily"}));
        assert_eq!(
            bare,
            json!({"id": "4", "cron": "@daily", "prompt": "", "active": true, "status": "active"})
        );
        assert_eq!(schedule_job(&json!({}))["active"], true);
    }

    #[test]
    fn status_reads_the_daemons_words() {
        let status = |value: Value, sub| status_of(&value, sub);
        assert_eq!(status(json!({"activity": "idle"}), false), "idle");
        assert_eq!(status(json!({"status": "Working"}), false), "working");
        assert_eq!(status(json!({"activity": "queued"}), false), "working");
        assert_eq!(
            status(json!({"activity": "idle", "isStreaming": true}), false),
            "working"
        );
        assert_eq!(status(json!({"status": "failed", "isStreaming": true}), false), "error");
        assert_eq!(status(json!({"status": "error"}), true), "error");
        assert_eq!(
            status(json!({"workerState": "FAILED", "activity": "idle"}), true),
            "error"
        );
        assert_eq!(
            status(json!({"workerState": "failed"}), false),
            "idle",
            "only subagents are held to their worker"
        );
        assert_eq!(status(json!({"lifecycle": "archived"}), true), "archived");
        assert_eq!(status(json!({"activity": "archived"}), true), "archived");
        assert_eq!(status(json!({"lifecycle": "archived"}), false), "idle");
        assert_eq!(
            status(json!({"status": "idle", "activity": "working"}), false),
            "idle",
            "status wins over activity"
        );
    }

    #[test]
    fn the_tree_nests_by_parent_sorts_by_activity_and_survives_a_cycle() {
        let sessions = [
            json!({"sessionId": "late", "parentSessionId": "root", "rlmChildId": "c-late", "activeSessionId": "a-late",
                   "lastActivityAt": "2026-10-16T10:00:10Z", "rlmDepth": 1, "firstMessage": "(no messages)"}),
            json!({"sessionId": "early", "parentSessionId": "root", "activeSessionId": "a-early", "sessionName": "Early",
                   "lastActivityAt": "2026-10-16T10:00:00Z", "rlmDepth": "2", "firstMessage": "do the thing", "activity": "working"}),
            json!({"sessionId": "grand", "parentSessionId": "early", "lastActivityAt": "2026-10-16T10:00:05Z"}),
            // Names its own descendant as its parent.
            json!({"sessionId": "loop-a", "parentSessionId": "root"}),
            json!({"sessionId": "loop-b", "parentSessionId": "loop-a"}),
            json!({"sessionId": "root", "parentSessionId": "loop-b"}),
        ];
        let mut by_parent: HashMap<String, Vec<&Value>> = HashMap::new();
        for session in &sessions {
            by_parent
                .entry(text_of(session, &["parentSessionId"]))
                .or_default()
                .push(session);
        }
        let detail = json!({"id": "c-late", "model": "m", "durationMs": 1200, "toolUseCount": 3, "answerPreview": "done",
                            "activity": {"kind": "writing"}, "tokenCount": "many"});
        let details = HashMap::from([("c-late".to_string(), &detail)]);
        let tree = subagent_nodes("root", &by_parent, &details, &mut vec!["root".to_string()]);
        let names: Vec<_> = tree.iter().map(|n| n["sessionId"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["early", "late", "loop-a"],
            "oldest activity first; no activity time is newest"
        );
        let early = &tree[0];
        assert_eq!(early["id"], "a-early");
        assert_eq!(early["name"], "Early");
        assert_eq!(early["depth"], 2);
        assert_eq!(early["task"], "do the thing");
        assert_eq!(early["children"][0]["sessionId"], "grand");
        assert_eq!(early["children"][0]["name"], "Subagent");
        assert_eq!(early["children"][0]["activeSessionId"], Value::Null);
        assert_eq!(early["children"][0]["parentSessionId"], "early");
        let late = &tree[1];
        assert_eq!(late["id"], "c-late");
        assert_eq!(late["name"], "c-late");
        assert_eq!(late["task"], "", "no messages is no task");
        assert_eq!(
            (
                late["model"].clone(),
                late["durationMs"].clone(),
                late["activity"].clone()
            ),
            (json!("m"), json!(1200), json!({"kind": "writing"}))
        );
        assert!(
            late.get("tokenCount").is_none(),
            "a detail of the wrong type is left out"
        );
        // loop-a -> loop-b -> root, and root is the tree's own root: it is not listed under its descendants.
        assert_eq!(tree[2]["children"][0]["sessionId"], "loop-b");
        assert_eq!(tree[2]["children"][0]["children"], json!([]));
    }
}

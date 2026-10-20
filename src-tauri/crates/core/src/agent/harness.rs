//! Harness actions (the panels that read and steer the agent's internals), automatic chat titles, session export and
//! share.
//!
//! Ported from `agentManager.ts` (`harnessAction`, `autoNameSession`, `exportHtml`, `shareSession`) and
//! `@shared/sessionTitle`. Answers keep the shapes the renderer reads.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use pa_tui::daemon_client::{DaemonClient, DaemonClientEvent};
use regex::Regex;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::manager::{Chat, Manager, unix_ms};
use super::release::VERSION;
use super::session::Session;
use crate::{platform, validate};

// ---------- Small helpers ----------

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cached(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

/// `String(input[key] ?? '')`.
fn text_of(input: &Value, key: &str) -> String {
    match &input[key] {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => String::new(),
    }
}

/// `String(value)` for the items of a list the renderer sent.
fn js_string(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_string)
}

/// `{...base, ...extra}` for two JSON objects.
fn with_fields(base: Value, extra: Value) -> Value {
    let mut map = base.as_object().cloned().unwrap_or_default();
    map.extend(extra.as_object().cloned().unwrap_or_default());
    Value::Object(map)
}

// ---------- Session titles ----------

pub const TITLE_SYSTEM_PROMPT: &str = "You name chat sessions. Reply with only a title for the conversation: 2 to 6 words, sentence case, no quotes, no trailing punctuation, no emoji. Name the task or subject, not the user (e.g. \"EGFR binder design\", \"Fix CSV column parsing\", \"Single-cell QC for PBMC data\").";

fn clip(text: &str, max: usize) -> String {
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let flat = cached(&SPACES, r"\s+").replace_all(text, " ");
    let flat = flat.trim();
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat.to_string()
    }
}

/// What the model sees of the first exchange, clipped so a long one stays cheap.
pub fn title_prompt(user_text: &str, reply_text: &str) -> String {
    format!(
        "User:\n{}\n\nAssistant:\n{}\n\nTitle:",
        clip(user_text, 1200),
        clip(reply_text, 800)
    )
}

/// A usable title from a model's reply, or `None`.
pub fn clean_session_title(raw: &str) -> Option<String> {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    static MARKUP: OnceLock<Regex> = OnceLock::new();
    static QUOTES: OnceLock<Regex> = OnceLock::new();
    static TAIL: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    static PLACEHOLDER: OnceLock<Regex> = OnceLock::new();
    static LAST_WORD: OnceLock<Regex> = OnceLock::new();
    let line = raw.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    let title = cached(&LABEL, r"(?i)^(?:session\s+)?(?:title|name)\s*[:\-–]\s*").replace(line, "");
    let title = cached(&MARKUP, r"[*_`#]+").replace_all(&title, "");
    let title = cached(&QUOTES, r#"^["'“‘«]+|["'”’»]+$"#).replace_all(&title, "");
    let title = cached(&TAIL, r"[.!?;:,]+$").replace(&title, "");
    let title = cached(&SPACES, r"\s+").replace_all(&title, " ");
    let mut title = title.trim().to_string();
    if title.is_empty() || cached(&PLACEHOLDER, r"(?i)^(?:untitled|new chat|n/a|none)$").is_match(&title) {
        return None;
    }
    // A sentence rather than a title usually means the model ignored the brief.
    if title.split(' ').count() > 10 {
        return None;
    }
    if title.chars().count() > 60 {
        let head: String = title.chars().take(59).collect();
        title = format!("{}…", cached(&LAST_WORD, r"\s+\S*$").replace(&head, ""));
    }
    Some(title)
}

/// The text of a message's content: a string, or the text blocks of a list.
fn session_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.trim().to_string(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| block.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

/// Clears a chat's "titling" flag however `auto_name` ends, even when its task is dropped.
struct Titling(Arc<Chat>);

impl Drop for Titling {
    fn drop(&mut self) {
        lock(&self.0.state).titling = false;
    }
}

/// Gives a chat a title after its first exchange, when the setting is on and it has none. Called after every finished
/// run, so everything that makes it a no-op is checked first and cheaply.
pub async fn auto_name(manager: &Manager, chat: &Arc<Chat>) {
    let Some(session_id) = title_candidate(manager, chat) else {
        return;
    };
    let _titling = Titling(chat.clone());
    if let Err(error) = name_session(manager, chat, &session_id).await {
        let why: String = format!("{error:#}")
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        eprintln!("auto-naming skipped: {why}");
    }
}

/// The session to title, when there is one and this call has claimed the right to try (a few tries across turns, since
/// a provider can be rate-limited, then stop).
fn title_candidate(manager: &Manager, chat: &Chat) -> Option<String> {
    const MAX_ATTEMPTS: u32 = 3;
    let session_id = {
        let state = lock(&chat.state);
        let id = state.info.session_id.clone()?;
        let named = state.info.session_name.as_deref().is_some_and(|name| !name.is_empty());
        let attempts = state.title_attempts.get(&id).copied().unwrap_or(0);
        if named || attempts >= MAX_ATTEMPTS || state.titling || chat.session().is_none() {
            return None;
        }
        id
    };
    if !manager.settings().auto_name_sessions {
        return None;
    }
    let mut state = lock(&chat.state);
    let attempts = state.title_attempts.get(&session_id).copied().unwrap_or(0);
    if state.titling || attempts >= MAX_ATTEMPTS {
        return None;
    }
    state.title_attempts.insert(session_id.clone(), attempts + 1);
    state.titling = true;
    Some(session_id)
}

async fn name_session(manager: &Manager, chat: &Arc<Chat>, session_id: &str) -> Result<()> {
    let Some(session) = chat.session() else {
        return Ok(());
    };
    let answer = session.send(&json!({"type": "get_messages"})).await?;
    let messages = answer["messages"].as_array().map_or(&[][..], Vec::as_slice);
    let user_text = messages
        .iter()
        .find(|message| message["role"] == "user")
        .map(|message| session_text(&message["content"]))
        .unwrap_or_default();
    if user_text.trim().is_empty() {
        return Ok(());
    }
    let reply_text = messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .map(|message| session_text(&message["content"]))
        .find(|text| !text.trim().is_empty())
        .unwrap_or_default();
    let question = format!("{TITLE_SYSTEM_PROMPT}\n\n{}", title_prompt(&user_text, &reply_text));
    let reply = ask_side_question(
        &session,
        &format!("title-{}", unix_ms()),
        &question,
        Duration::from_secs(60),
    )
    .await?;
    let Some(title) = clean_session_title(&reply) else {
        return Ok(());
    };
    // The user may have named it, or switched chats, while this ran.
    let unchanged = {
        let state = lock(&chat.state);
        let named = state.info.session_name.as_deref().is_some_and(|name| !name.is_empty());
        state.info.session_id.as_deref() == Some(session_id) && !named
    };
    if !unchanged || chat.session().is_none() {
        return Ok(());
    }
    session
        .send(&json!({"type": "set_session_name", "name": title}))
        .await?;
    manager.update_info(chat, |info| info.session_name = Some(title.clone()));
    lock(&chat.state)
        .title_attempts
        .insert(session_id.to_string(), u32::MAX);
    manager.emit_event(
        &chat.id,
        "session_renamed",
        json!({"sessionId": session_id, "name": title}),
    );
    Ok(())
}

/// A question to the model that cannot use tools (the daemon blocks them in a side thread) and never enters the chat's
/// history. It runs on a connection of its own: the daemon sends a question's events to the clients attached to the
/// session, and the manager's connection does not pass them on.
async fn ask_side_question(session: &Session, id: &str, question: &str, wait: Duration) -> Result<String> {
    let (client, mut events) = DaemonClient::connect(session.client().socket_path()).await?;
    let own = Session::new(client.clone(), session.id().to_string());
    let outcome = async {
        let attach = json!({"clientId": format!("{}-side:{}", crate::product::product().slug(), uuid::Uuid::new_v4()), "capabilities": ["event_sequence", "slim_attach"]});
        own.call("attach", attach).await?;
        own.call("start_side_question", json!({"sideQuestionId": id, "question": question}))
            .await?;
        let finished = async {
            while let Some(event) = events.recv().await {
                if let DaemonClientEvent::SideQuestionEvent { event, .. } = event
                    && event["id"] == id
                    && event["status"] != "running"
                {
                    return Ok(event);
                }
            }
            Err(anyhow!("The connection closed before the answer."))
        };
        tokio::time::timeout(wait, finished)
            .await
            .map_err(|_| anyhow!("Side question timed out."))?
    }
    .await;
    // Closing also stops a question that is still running for this client.
    client.close();
    let event = outcome?;
    if event["status"] == "complete" {
        return Ok(event["answer"].as_str().unwrap_or("").to_string());
    }
    let status = event["status"].as_str().unwrap_or("failed");
    Err(anyhow!(
        "{}",
        event["errorMessage"]
            .as_str()
            .map_or_else(|| format!("Side question {status}"), str::to_string)
    ))
}

// ---------- Prime Agent's settings file ----------

/// Settings are read-modify-written from several actions; this keeps two of them from losing each other's change.
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

fn settings_path() -> PathBuf {
    platform::prime_agent_dir().join("settings.json")
}

/// For display: a missing or broken file reads as empty.
fn read_settings(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// For read-modify-write: a missing file is empty, but an unreadable or corrupt one must stop the write instead of being
/// replaced with `{}`.
fn read_settings_strict(path: &Path) -> Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => bail!("Could not read Prime Agent settings ({}): {error}", path.display()),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => bail!("Prime Agent settings ({}) must be a JSON object.", path.display()),
        Err(_) => bail!(
            "Prime Agent settings ({}) are not valid JSON. Fix or remove the file and try again.",
            path.display()
        ),
    }
}

/// Reads the settings strictly, lets `change` edit them, and writes them back whole (temp file, then rename).
fn update_settings<T>(path: &Path, change: impl FnOnce(&mut Map<String, Value>) -> Result<T>) -> Result<T> {
    let _serial = lock(&SETTINGS_LOCK);
    let mut settings = read_settings_strict(path)?;
    let result = change(&mut settings)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(name);
    let _ = std::fs::remove_file(&temp);
    let written = write_private(&temp, &serde_json::to_string_pretty(&Value::Object(settings))?)
        .and_then(|()| keep_permissions(path, &temp))
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.with_context(|| format!("Could not write {}", path.display()))?;
    Ok(result)
}

/// Writes a file only its owner can read: settings sit beside the agent's keys, and a rename over the original must
/// not widen who can read it.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents.as_bytes())
}

/// Gives the replacement the permissions of the file it replaces, when there is one.
fn keep_permissions(original: &Path, replacement: &Path) -> std::io::Result<()> {
    match std::fs::metadata(original) {
        Ok(meta) => std::fs::set_permissions(replacement, meta.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

const MODEL_ROLES: [&str; 3] = ["subagentDefaultModel", "auxiliaryModel", "providerBackupModel"];

fn model_roles_in(path: &Path) -> Value {
    let settings = read_settings(path);
    let roles = MODEL_ROLES.iter().map(|role| {
        let value = settings.get(*role).and_then(Value::as_str).unwrap_or("");
        ((*role).to_string(), Value::String(value.to_string()))
    });
    Value::Object(roles.collect())
}

fn set_model_role(path: &Path, key: &str, value: &str) -> Result<Value> {
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    if !MODEL_ROLES.contains(&key) {
        bail!("Unknown model role.");
    }
    if !value.is_empty() && (value.chars().count() > 512 || !cached(&SHAPE, r"^[a-zA-Z0-9._-]+/\S+$").is_match(value)) {
        bail!("Select a model with a provider and model ID.");
    }
    update_settings(path, |settings| {
        if value.is_empty() {
            settings.remove(key);
        } else {
            settings.insert(key.to_string(), Value::String(value.to_string()));
        }
        Ok(())
    })?;
    Ok(model_roles_in(path))
}

fn set_agent_traces(path: &Path, enabled: bool) -> Result<()> {
    update_settings(path, |settings| {
        let mut traces = settings
            .get("agentTraces")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        traces.insert("enabled".into(), Value::Bool(enabled));
        settings.insert("agentTraces".into(), Value::Object(traces));
        Ok(())
    })
}

fn agent_traces_enabled(path: &Path) -> bool {
    read_settings(path)
        .get("agentTraces")
        .is_some_and(|traces| traces["enabled"] == true)
}

/// A shell-like split of a command line into arguments, honouring single and double quotes. A backslash escapes the
/// next character; on Windows it only does so before a quote, since there it is a path separator
/// (`"C:\Program Files\node.exe"` must stay intact).
fn split_command_line(line: &str, windows: bool) -> Result<Vec<String>> {
    let (mut out, mut current, mut quote, mut pending) = (Vec::new(), String::new(), None::<char>, false);
    let mut chars = line.chars().peekable();
    let escapes = |next: char| !windows || next == '"' || next == '\'';
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            } else if c == '\\' && open == '"' && chars.peek().is_some_and(|next| escapes(*next)) {
                current.extend(chars.next());
            } else {
                current.push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            pending = true;
        } else if c == '\\' && chars.peek().is_some_and(|next| escapes(*next)) {
            current.extend(chars.next());
            pending = true;
        } else if c.is_whitespace() {
            if !current.is_empty() || pending {
                out.push(std::mem::take(&mut current));
            }
            pending = false;
        } else {
            current.push(c);
            pending = true;
        }
    }
    if quote.is_some() {
        bail!("MCP command has an unterminated quote.");
    }
    if !current.is_empty() || pending {
        out.push(current);
    }
    Ok(out)
}

/// What `mcp_set` asks for.
enum McpServer {
    Remove,
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        enabled: bool,
    },
    Http {
        url: String,
        oauth: bool,
        enabled: bool,
        bearer_token_env_var: Option<String>,
    },
}

fn mcp_server_from(input: &Value) -> Result<McpServer> {
    if input["remove"] == true {
        return Ok(McpServer::Remove);
    }
    let enabled = input["enabled"] != false;
    if input["type"] == "stdio" {
        // The UI sends one command line; the API also accepts command + args.
        let (command, args) = match input["commandLine"].as_str() {
            Some(line) => {
                let mut parts = split_command_line(line, cfg!(windows))?.into_iter();
                (parts.next().unwrap_or_default(), parts.collect())
            }
            None => (
                text_of(input, "command"),
                input["args"]
                    .as_array()
                    .map(|list| list.iter().map(js_string).collect())
                    .unwrap_or_default(),
            ),
        };
        let command = command.trim().to_string();
        if command.is_empty() {
            bail!("MCP stdio server needs a command.");
        }
        if std::iter::once(&command)
            .chain(&args)
            .any(|part: &String| part.contains('\0'))
        {
            bail!("MCP command arguments cannot contain NUL.");
        }
        let cwd = input["cwd"].as_str().map(str::trim).filter(|cwd| !cwd.is_empty());
        return Ok(McpServer::Stdio {
            command,
            args,
            cwd: cwd.map(str::to_string),
            enabled,
        });
    }
    let url = url::Url::parse(&text_of(input, "url")).map_err(|_| anyhow!("Invalid URL"))?;
    let local = url.scheme() == "http" && url.host_str() == Some("localhost");
    if url.scheme() != "https" && !local {
        bail!("MCP server URL must use HTTPS.");
    }
    Ok(McpServer::Http {
        url: url.to_string(),
        oauth: input["oauth"] != false,
        enabled,
        bearer_token_env_var: input["bearerTokenEnvVar"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_string),
    })
}

fn mcp_servers_of(settings: &Map<String, Value>) -> Map<String, Value> {
    settings
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// The servers as the panel shows them: only the fields it reads, only those present.
fn mcp_servers_view(settings: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (name, server) in mcp_servers_of(settings) {
        let mut view = Map::new();
        for key in [
            "type",
            "url",
            "command",
            "args",
            "cwd",
            "oauth",
            "enabled",
            "bearerTokenEnvVar",
        ] {
            match server.get(key) {
                Some(value) if key != "args" || value.is_array() => {
                    view.insert(key.to_string(), value.clone());
                }
                _ => {}
            }
        }
        out.insert(name, Value::Object(view));
    }
    Value::Object(out)
}

fn set_mcp_server(path: &Path, name: &str, config: McpServer) -> Result<Value> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    let key = name.trim();
    if !cached(&NAME, r"^[a-zA-Z0-9._-]+$").is_match(key) {
        bail!("MCP server name may use letters, numbers, dots, dashes, and underscores.");
    }
    update_settings(path, |settings| {
        let mut servers = mcp_servers_of(settings);
        let previous = servers.get(key).and_then(Value::as_object).cloned().unwrap_or_default();
        match config {
            McpServer::Remove => {
                servers.remove(key);
            }
            McpServer::Stdio {
                command,
                args,
                cwd,
                enabled,
            } => {
                let mut entry = Map::new();
                entry.insert("type".into(), json!("stdio"));
                entry.insert("command".into(), json!(command));
                if !args.is_empty() {
                    entry.insert("args".into(), json!(args));
                }
                if let Some(cwd) = cwd {
                    entry.insert("cwd".into(), json!(cwd));
                }
                entry.insert("enabled".into(), json!(enabled));
                for timeout in ["startupTimeoutMs", "callTimeoutMs"] {
                    if let Some(value) = previous.get(timeout).filter(|value| value.is_number()) {
                        entry.insert(timeout.into(), value.clone());
                    }
                }
                servers.insert(key.to_string(), Value::Object(entry));
            }
            McpServer::Http {
                url,
                oauth,
                enabled,
                bearer_token_env_var,
            } => {
                let mut entry = if previous.get("type") == Some(&json!("http")) {
                    previous
                } else {
                    Map::new()
                };
                entry.insert("type".into(), json!("http"));
                entry.insert("url".into(), json!(url));
                entry.insert("oauth".into(), json!(oauth));
                entry.insert("enabled".into(), json!(enabled));
                match bearer_token_env_var {
                    Some(var) => entry.insert("bearerTokenEnvVar".into(), json!(var)),
                    None => entry.remove("bearerTokenEnvVar"),
                };
                servers.insert(key.to_string(), Value::Object(entry));
            }
        }
        settings.insert("mcpServers".into(), Value::Object(servers));
        Ok(())
    })?;
    Ok(mcp_servers_view(&read_settings(path)))
}

// ---------- Running Prime Agent's command line ----------

#[derive(Debug)]
struct Captured {
    stdout: String,
    stderr: String,
    success: bool,
    /// Output past the cap was dropped.
    truncated: bool,
}

/// What was read of one stream.
#[derive(Default)]
struct Capped {
    bytes: Vec<u8>,
    /// More arrived than the cap allowed, and was dropped.
    over: bool,
}

/// Reads a stream to its end, keeping at most `max` bytes: the rest is read and dropped, so a chatty child never blocks
/// on a full pipe. What was read stays in `kept` even if this is cancelled.
async fn read_capped(mut reader: impl AsyncRead + Unpin, kept: &mut Capped, max: usize) {
    let mut chunk = [0u8; 8192];
    while let Ok(read @ 1..) = reader.read(&mut chunk).await {
        let room = max.saturating_sub(kept.bytes.len());
        kept.bytes.extend_from_slice(&chunk[..read.min(room)]);
        kept.over |= read > room;
    }
}

/// Runs a program to completion with a time limit and an output cap. No window on Windows, no stdin, and PATH carries
/// the usual install folders (a macOS app started from the Dock lacks them).
async fn run_capture(
    program: impl AsRef<OsStr>,
    args: &[String],
    cwd: &Path,
    limit: Duration,
    max_bytes: usize,
) -> Result<Captured> {
    let program = program.as_ref();
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("PATH", platform::with_extra_path(std::env::var("PATH").ok().as_deref()));
    platform::quiet_async(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("Could not start {}", program.to_string_lossy()))?;
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        bail!("Could not read the output of {}", program.to_string_lossy());
    };
    let (mut out, mut err) = (Capped::default(), Capped::default());
    let waited = tokio::time::timeout(limit, async {
        let readers = async {
            tokio::join!(
                read_capped(&mut stdout, &mut out, max_bytes),
                read_capped(&mut stderr, &mut err, max_bytes)
            );
        };
        tokio::pin!(readers);
        let mut drained = false;
        let status = tokio::select! {
            status = child.wait() => status,
            () = &mut readers => {
                drained = true;
                child.wait().await
            }
        };
        if !drained {
            // The child is gone. A background process it left may still hold the pipes, so they get a moment, not forever.
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut readers).await;
        }
        status
    })
    .await;
    let status = match waited {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            bail!(
                "{} did not finish within {} s",
                program.to_string_lossy(),
                limit.as_secs()
            );
        }
    };
    Ok(Captured {
        stdout: String::from_utf8_lossy(&out.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&err.bytes).into_owned(),
        success: status.success(),
        truncated: out.over || err.over,
    })
}

/// Prime Agent's command line, as `{ok, output, data?}`: the combined output, and its JSON when it is JSON.
pub(super) async fn run_prime(manager: &Manager, args: &[&str], cwd: Option<PathBuf>) -> Value {
    let binary = manager.binary();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let cwd = cwd.unwrap_or_else(platform::home_dir);
    let done = run_capture(&binary, &args, &cwd, Duration::from_secs(600), 10 * 1024 * 1024).await;
    match done {
        Ok(done) => {
            let output = format!("{}\n{}", done.stdout, done.stderr).trim().to_string();
            if done.success && !done.truncated {
                return match serde_json::from_str::<Value>(&output) {
                    Ok(data) => json!({"ok": true, "output": output, "data": data}),
                    Err(_) => json!({"ok": true, "output": output}),
                };
            }
            let why = if done.truncated {
                "The output was larger than the 10 MiB limit."
            } else {
                "The command did not succeed."
            };
            json!({"ok": false, "output": format!("{}\n{}\n{why}", done.stdout, done.stderr).trim()})
        }
        Err(error) => json!({"ok": false, "output": format!("{error:#}")}),
    }
}

// ---------- The session tree ----------

/// The daemon's flat tree nodes arranged as the nested tree Prime Agent's own client builds: parents hold their
/// children in time order. Everything here is iterative, since a long chat is a deep chain.
struct Tree {
    nodes: Vec<Value>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

impl Tree {
    fn from_flat(flat: &Value) -> Self {
        let nodes: Vec<Value> = flat.as_array().cloned().unwrap_or_default();
        let id_of = |node: &Value| node["entry"]["id"].as_str().map(str::to_string);
        let by_id: HashMap<String, usize> = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| id_of(node).map(|id| (id, index)))
            .collect();
        let (mut children, mut roots) = (vec![Vec::new(); nodes.len()], Vec::new());
        for (index, node) in nodes.iter().enumerate() {
            if id_of(node).is_none() {
                continue;
            }
            let parent = node["entry"]["parentId"]
                .as_str()
                .filter(|parent| Some(*parent) != node["entry"]["id"].as_str());
            match parent.and_then(|parent| by_id.get(parent)) {
                Some(parent) => children[*parent].push(index),
                None => roots.push(index),
            }
        }
        // ISO timestamps of one format compare in time order as text.
        let time = |index: &usize| nodes[*index]["entry"]["timestamp"].as_str().unwrap_or("").to_string();
        for siblings in &mut children {
            siblings.sort_by_cached_key(time);
        }
        Self { nodes, children, roots }
    }

    /// Every node's index and depth, parents before children.
    fn visit(&self, mut visit: impl FnMut(usize, usize)) {
        let mut stack: Vec<(usize, usize)> = self.roots.iter().rev().map(|root| (*root, 0)).collect();
        while let Some((index, depth)) = stack.pop() {
            visit(index, depth);
            stack.extend(self.children[index].iter().rev().map(|child| (*child, depth + 1)));
        }
    }

    /// Every node and its depth, parents before children.
    fn walk(&self, mut visit: impl FnMut(&Value, usize)) {
        self.visit(|index, depth| visit(&self.nodes[index], depth));
    }

    /// The nodes with a `children` list each.
    fn nested(&self) -> Vec<Value> {
        let mut order = Vec::with_capacity(self.nodes.len());
        self.visit(|index, _| order.push(index));
        let mut built: Vec<Option<Value>> = vec![None; self.nodes.len()];
        for index in order.into_iter().rev() {
            let kids: Vec<Value> = self.children[index]
                .iter()
                .filter_map(|kid| built[*kid].take())
                .collect();
            let mut node = self.nodes[index].clone();
            if let Value::Object(map) = &mut node {
                map.insert("children".into(), Value::Array(kids));
            }
            built[index] = Some(node);
        }
        self.roots.iter().filter_map(|root| built[*root].take()).collect()
    }
}

/// One row of the flat tree the `/tree` overlay lists.
fn tree_row(node: &Value, depth: usize) -> Value {
    let entry = &node["entry"];
    let message = &entry["message"];
    let role = message["role"]
        .as_str()
        .or_else(|| entry["type"].as_str())
        .unwrap_or("");
    json!({
        "entryId": entry["id"],
        "parentId": entry["parentId"],
        "depth": depth,
        "label": node["label"].as_str().unwrap_or(""),
        "text": session_text(&message["content"]),
        "role": role,
    })
}

async fn session_tree(session: &Session) -> Result<Value> {
    session.call("get_session_tree", json!({})).await
}

/// The refinements a chat recorded, oldest first along each branch.
fn refinement_history(tree: &Tree) -> Vec<Value> {
    let mut history = Vec::new();
    tree.walk(|node, _| {
        let entry = &node["entry"];
        let data = &entry["data"];
        let recorded = match data {
            Value::Null | Value::Bool(false) => false,
            Value::String(text) => !text.is_empty(),
            Value::Number(number) => number.as_f64() != Some(0.0),
            _ => true,
        };
        if entry["type"] == "custom" && entry["customType"] == "prime-agent.refinement" && recorded {
            history.push(data.clone());
        }
    });
    history
}

// ---------- The actions ----------

/// Questions that panels and overlays ask as they open. They must not start a worker that is not running; only actions
/// the user takes may.
pub fn is_read_only(action: &str) -> bool {
    matches!(
        action,
        "get_tree"
            | "get_tree_full"
            | "system_prompt"
            | "goal_state"
            | "heartbeats"
            | "trace_list"
            | "model_catalog"
            | "refinement_history"
    )
}

/// One harness action. Actions that touch Prime Agent's files or command line need no chat; the rest run on the
/// chat's session, which a user action reconnects if it dropped.
pub async fn action(manager: &Manager, agent_id: &str, action: &str, input: &Value) -> Result<Value> {
    match action {
        "mcp_catalog" => Ok(mcp_catalog_in(manager.binary().parent())),
        "model_roles" => Ok(model_roles_in(&settings_path())),
        "model_role_set" => {
            let roles = set_model_role(&settings_path(), &text_of(input, "key"), &text_of(input, "value"))?;
            reload_agents(manager).await;
            Ok(roles)
        }
        "traces" => {
            let mode = text_of(input, "mode");
            if mode == "on" || mode == "off" {
                set_agent_traces(&settings_path(), mode == "on")?;
            }
            Ok(json!({"enabled": agent_traces_enabled(&settings_path())}))
        }
        "update" => bail!(
            "Prime Agent {VERSION} is the version this app was tested with, and it is updated together with the app."
        ),
        "daemon_status" => Ok(run_prime(manager, &["status", "--json"], None).await),
        "daemon_shutdown" => Ok(run_prime(manager, &["shutdown", "--force", "--json"], None).await),
        "daemon_doctor" => Ok(run_prime(manager, &["doctor", "--json"], None).await),
        "daemon_recover" => {
            let result = run_prime(manager, &["doctor", "--fix", "--json"], None).await;
            reconnect_unhealthy(manager).await;
            Ok(result)
        }
        "package" => package(manager, agent_id, input).await,
        "mcp_get" => Ok(json!({"servers": mcp_servers_view(&read_settings(&settings_path()))})),
        "mcp_set" => {
            let config = mcp_server_from(input)?;
            let servers = set_mcp_server(&settings_path(), &text_of(input, "name"), config)?;
            // The daemon serves MCP config from the settings it loaded at session start, so a running agent keeps
            // launching the old command until it reloads.
            reload_agents(manager).await;
            Ok(json!({"servers": servers}))
        }
        "trace_list" => trace_list_in(&sessions_dir()).await,
        "trace_preview" => trace_preview_in(&sessions_dir(), &text_of(input, "path")).await,
        _ => session_action(manager, agent_id, action, input).await,
    }
}

/// Asks every running agent to reload; one that is mid-run reloads when the run ends.
async fn reload_agents(manager: &Manager) {
    for chat in manager.chats() {
        if lock(&chat.state).info.is_streaming {
            lock(&chat.state).pending_runtime_reload = true;
        } else if let Some(session) = chat.session() {
            let _ = session.send(&json!({"type": "reload"})).await;
        }
    }
}

/// Reconnects the chats that are not connected, after a repair.
async fn reconnect_unhealthy(manager: &Manager) {
    for chat in manager.chats() {
        let broken = {
            let status = lock(&chat.state).info.status.clone();
            chat.session().is_none() || status == "error" || status == "stopped"
        };
        if broken {
            // A failed start has already told the user why.
            let _ = manager.ensure_connected(&chat).await;
        }
    }
}

/// The services Prime Agent offers to connect, from the catalog that ships beside its executable.
fn mcp_catalog_in(dir: Option<&Path>) -> Value {
    let catalog = dir
        .and_then(|dir| std::fs::read_to_string(dir.join("mcp-services.bundled.json")).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let services = catalog.as_ref().and_then(|catalog| catalog["entries"].as_array());
    Value::Array(
        services
            .into_iter()
            .flatten()
            .map(|entry| {
                json!({
                    "id": entry["server"],
                    "name": entry["label"],
                    "description": entry["description"].as_str().unwrap_or(""),
                    "auth": entry["auth"]["strategy"],
                })
            })
            .collect(),
    )
}

async fn package(manager: &Manager, agent_id: &str, input: &Value) -> Result<Value> {
    let command = match text_of(input, "command").as_str() {
        "" => "list".to_string(),
        other => other.to_string(),
    };
    if !["list", "install", "remove", "update"].contains(&command.as_str()) {
        bail!("Unsupported package action");
    }
    let source = text_of(input, "source").trim().to_string();
    if (command == "install" || command == "remove") && source.is_empty() {
        bail!("Package source is required");
    }
    // A source is an operand. One that looks like an option would be read as one.
    if source.starts_with('-') || source.contains('\0') {
        bail!("Package source is invalid");
    }
    let mut args = vec!["package", command.as_str()];
    if command == "update" {
        // `package update` alone also updates Prime Agent itself, and this app pins the build it was tested with.
        if ["self", "pi", "prime-agent"].contains(&source.as_str()) {
            bail!("Prime Agent is updated together with the app.");
        }
        if source.is_empty() {
            args.push("--extensions");
        }
    }
    if !source.is_empty() {
        args.push(&source);
    }
    let result = run_prime(manager, &args, manager.project_path(agent_id)).await;
    if result["ok"] == true {
        for chat in manager.chats() {
            lock(&chat.state).skills.clear();
            if let Some(session) = chat.session() {
                tokio::spawn(async move {
                    let _ = session.send(&json!({"type": "reload"})).await;
                });
            }
        }
    }
    Ok(result)
}

fn sessions_dir() -> PathBuf {
    platform::prime_agent_dir().join("sessions")
}

/// The newest saved session files, for the diagnostics panel.
async fn trace_list_in(dir: &Path) -> Result<Value> {
    let dir = dir.to_path_buf();
    let mut files = tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // A file can go between listing and stat; it is simply not listed.
            let Ok(meta) = entry.metadata() else { continue };
            if !name.ends_with(".jsonl") || !meta.is_file() {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0.0, |age| age.as_secs_f64() * 1000.0);
            let path = entry.path().to_string_lossy().into_owned();
            files.push(json!({"path": path, "name": name, "size": meta.len(), "modifiedAt": modified}));
        }
        files
    })
    .await?;
    files.sort_by(|a, b| {
        b["modifiedAt"]
            .as_f64()
            .partial_cmp(&a["modifiedAt"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    files.truncate(200);
    Ok(json!({"files": files}))
}

/// The start of one saved session file. Only files inside the sessions folder may be read.
async fn trace_preview_in(dir: &Path, path: &str) -> Result<Value> {
    const PREVIEW_CHARS: usize = 200_000;
    let outside = || anyhow!("Trace path is outside the session directory");
    let path = validate::existing_file("trace path", path).map_err(|_| outside())?;
    if !validate::is_within(dir, &path) || path.extension().is_none_or(|ext| ext != "jsonl") {
        return Err(outside());
    }
    let file = tokio::fs::File::open(&path).await?;
    let size = file.metadata().await?.len();
    // Four bytes cover the longest character, so this reads enough to know whether more than PREVIEW_CHARS follow.
    let mut bytes = Vec::new();
    file.take(4 * PREVIEW_CHARS as u64 + 4).read_to_end(&mut bytes).await?;
    let text = String::from_utf8_lossy(&bytes);
    let truncated = text.chars().count() > PREVIEW_CHARS;
    let preview: String = text.chars().take(PREVIEW_CHARS).collect();
    Ok(json!({"path": path.to_string_lossy(), "size": size, "preview": preview, "truncated": truncated}))
}

/// The queue as the tray shows it.
async fn queue_view(session: &Session) -> Result<Value> {
    let queue = session.call("get_queue", json!({})).await?;
    Ok(with_fields(
        queue,
        json!({
            "mutationSupported": session.supports("queue_message_mutation"),
            "sendQueuedSupported": session.supports("abort_and_send_queued"),
        }),
    ))
}

fn queue_mutation(raw: &Value) -> Result<Value> {
    let lane = |value: &Value| if value == "followUp" { "followUp" } else { "steering" };
    match raw["type"].as_str() {
        Some("move") => Ok(json!({"type": "move", "direction": if raw["direction"] == -1 { -1 } else { 1 }})),
        Some("replace") => Ok(json!({"type": "replace", "text": text_of(raw, "text"), "lane": lane(&raw["lane"])})),
        // A request that names no known change must not fall through to deleting a message.
        Some("delete") => Ok(json!({"type": "delete"})),
        _ => bail!("Unknown queue change"),
    }
}

/// A non-negative whole number from what the renderer sent (`Math.max(0, Math.floor(Number(x)))`).
fn index_of(value: &Value) -> u64 {
    let number = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    number
        .filter(|number| number.is_finite() && *number >= 0.0)
        .map_or(0, |number| number as u64)
}

/// A path the renderer sent, made absolute against the project when it is relative (the daemon's own working folder).
fn path_in_project(project: &Path, raw: &str) -> Result<PathBuf> {
    let raw = validate::non_empty("path", raw, 4096)?;
    if raw.contains('\0') {
        bail!("path must not contain NUL");
    }
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() { path } else { project.join(path) })
}

async fn session_action(manager: &Manager, agent_id: &str, action: &str, input: &Value) -> Result<Value> {
    let Some(chat) = manager.chat(agent_id) else {
        bail!("Unknown or disconnected agent");
    };
    if chat.session().is_none() && !is_read_only(action) {
        manager.reconnect(&chat).await;
    }
    let Some(session) = chat.session() else {
        bail!("Unknown or disconnected agent");
    };
    match action {
        "capabilities" => Ok(json!({
            "queueMutation": session.supports("queue_message_mutation"),
            "sessionTree": true,
            "sideQuestions": true,
            "childCancellation": true,
            "resources": true,
            "modelCatalog": session.supports("model_catalog"),
            "jsonlExport": true,
        })),
        "queue" => queue_view(&session).await,
        "queue_send_now" => {
            if !session.supports("abort_and_send_queued") {
                bail!("Restart with the updated Prime Agent to send queued messages now.");
            }
            session.call("abort_and_send_queued", json!({})).await?;
            queue_view(&session).await
        }
        "queue_mutate" => {
            if !session.supports("queue_message_mutation") {
                let queue = session.call("get_queue", json!({})).await?;
                return Ok(with_fields(json!({"status": "unsupported"}), queue));
            }
            let mutation = queue_mutation(&input["mutation"])?;
            let change = json!({
                "lane": if input["lane"] == "followUp" { "followUp" } else { "steering" },
                "index": index_of(&input["index"]),
                "expectedText": text_of(input, "expectedText"),
                "mutation": mutation,
            });
            let status = session.call("mutate_queued_message", change).await?["status"].clone();
            let queue = session.call("get_queue", json!({})).await?;
            Ok(with_fields(
                with_fields(json!({"status": status}), queue),
                json!({"mutationSupported": true}),
            ))
        }
        "queue_clear" => session.call("clear_queue", json!({})).await,
        "queue_abort_clear" => session.call("abort_and_clear_queue", json!({})).await,
        "get_tree" => {
            let data = session_tree(&session).await?;
            let mut nodes = Vec::new();
            Tree::from_flat(&data["flatNodes"]).walk(|node, depth| nodes.push(tree_row(node, depth)));
            Ok(json!({"nodes": nodes, "leafId": data["leafId"]}))
        }
        "get_tree_full" => {
            let data = session_tree(&session).await?;
            Ok(json!({"tree": Tree::from_flat(&data["flatNodes"]).nested(), "leafId": data["leafId"]}))
        }
        "navigate_tree" => {
            let fields = json!({
                "targetId": text_of(input, "targetId"),
                "summarize": input["summarize"] != false,
                "customInstructions": input["instructions"].as_str(),
                "label": input["label"].as_str(),
            });
            session.call("navigate_tree", fields).await
        }
        "label_tree" => {
            let fields = json!({"entryId": text_of(input, "entryId"), "label": input["label"].as_str()});
            session.call("set_session_entry_label", fields).await?;
            let data = session_tree(&session).await?;
            Ok(json!({"tree": Tree::from_flat(&data["flatNodes"]).nested(), "leafId": data["leafId"]}))
        }
        "family" => {
            // The roster is authoritative where the daemon offers it; without it there is nothing to list.
            if !session.supports("authoritative_child_roster") {
                return Ok(json!({"children": []}));
            }
            let roster = session.call("get_rlm_children", json!({})).await?;
            Ok(json!({"children": roster["children"].as_array().cloned().unwrap_or_default()}))
        }
        "family_cancel" => {
            let cancelled = session
                .call("cancel_rlm_child", json!({"childId": text_of(input, "childId")}))
                .await?;
            Ok(json!({"cancelled": cancelled["cancelled"]}))
        }
        "system_prompt" => {
            let prompt = session.call("get_system_prompt", json!({})).await?;
            Ok(json!({"text": prompt["systemPrompt"]}))
        }
        "side_question_start" => {
            let id = match text_of(input, "id") {
                id if id.is_empty() => format!("desktop-side-{}", unix_ms()),
                id => id,
            };
            let turns = input["previousTurns"].as_array().map(|turns| {
                turns
                    .iter()
                    .map(|turn| json!({"question": text_of(turn, "question"), "answer": text_of(turn, "answer")}))
                    .collect::<Vec<_>>()
            });
            if turns.as_ref().is_some_and(|turns| !turns.is_empty()) && !session.supports("side_question_transcript") {
                bail!(
                    "the daemon is running an older build without side-conversation follow-ups; restart the daemon and try again"
                );
            }
            let fields = json!({"sideQuestionId": id, "question": text_of(input, "question"), "previousTurns": turns});
            session.call("start_side_question", fields).await?;
            Ok(json!({"id": id, "started": true}))
        }
        "side_question_abort" => {
            let aborted = session
                .call("abort_side_question", json!({"sideQuestionId": text_of(input, "id")}))
                .await?;
            Ok(json!({"aborted": aborted["aborted"]}))
        }
        "side_question" => {
            let answer = ask_side_question(
                &session,
                &format!("desktop-side-{}", unix_ms()),
                &text_of(input, "question"),
                Duration::from_secs(120),
            )
            .await?;
            Ok(json!({"answer": answer}))
        }
        "fast" => {
            let context = session.call("get_session_context", json!({})).await?;
            let enabled = context["context"]["serviceTier"] == "priority";
            let tier = if enabled { "default" } else { "priority" };
            session.call("set_service_tier", json!({"serviceTier": tier})).await?;
            Ok(json!({"enabled": !enabled}))
        }
        "scoped_models" => {
            let models: Vec<Value> = input["models"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|model| json!({"provider": text_of(model, "provider"), "modelId": text_of(model, "modelId")}))
                        .collect()
                })
                .unwrap_or_default();
            session
                .call("set_scoped_models", json!({"scopedModels": models}))
                .await?;
            Ok(json!({"saved": true}))
        }
        "resources" => session.call("get_resource_snapshot", json!({})).await,
        "model_catalog" => {
            if session.supports("model_catalog") {
                return session.call("get_model_catalog", json!({})).await;
            }
            let models = session.call("get_available_models", json!({})).await?["models"].clone();
            let mut providers: Vec<Value> = Vec::new();
            for model in models.as_array().into_iter().flatten() {
                if !providers.contains(&model["provider"]) {
                    providers.push(model["provider"].clone());
                }
            }
            Ok(json!({"models": models, "configuredProviders": providers}))
        }
        "set_transport" => {
            let transport = text_of(input, "transport");
            let known = ["sse", "websocket", "websocket-cached"].contains(&transport.as_str());
            let sent = if known { transport.as_str() } else { "auto" };
            session.call("set_transport", json!({"transport": sent})).await?;
            Ok(json!({"transport": if transport.is_empty() { "auto" } else { transport.as_str() }}))
        }
        "goal_state" => {
            let state = session.call("get_connection_state", json!({})).await?;
            Ok(json!({"goal": state["goal"]}))
        }
        "goal_command" => {
            let command = match text_of(input, "command") {
                command if command.is_empty() => "/goal status".to_string(),
                command => command,
            };
            session.send(&json!({"type": "prompt", "message": command})).await?;
            Ok(json!({"accepted": true}))
        }
        "autonomous_command" => {
            let mode = match text_of(input, "mode") {
                mode if mode.is_empty() => "status".to_string(),
                mode => mode,
            };
            session
                .send(&json!({"type": "prompt", "message": format!("/autonomous {mode}")}))
                .await?;
            Ok(json!({"accepted": true}))
        }
        "heartbeats" => {
            // The catalog is the daemon's, across sessions, so this asks without naming one.
            if !session.supports("heartbeat_catalog") {
                return Ok(json!({"heartbeats": []}));
            }
            let catalog = session.control(json!({"type": "heartbeats_list"})).await?;
            Ok(json!({"heartbeats": catalog["heartbeats"]}))
        }
        "heartbeat_manage" => {
            if !session.supports("heartbeat_management") {
                bail!("Heartbeat management requires a newer Prime Agent daemon.");
            }
            let verb = text_of(input, "action");
            let command = json!({
                "type": "heartbeat_manage",
                "activeSessionId": text_of(input, "activeSessionId"),
                "jobId": text_of(input, "jobId"),
                "action": if verb == "pause" || verb == "resume" { verb.as_str() } else { "stop" },
            });
            Ok(session.control(command).await?["heartbeat"].clone())
        }
        "heartbeat_set" => {
            let fields = json!({
                "schedule": text_of(input, "schedule"),
                "prompt": text_of(input, "prompt"),
                "deliveryMode": if input["deliveryMode"] == "follow_up" { "follow_up" } else { "steer" },
                "promoteOwnedSession": false,
            });
            Ok(session.call("heartbeat_set", fields).await?["heartbeat"].clone())
        }
        "refine" => {
            // A refinement runs the model over the whole chat, which takes far longer than a daemon request normally may.
            let mut command =
                json!({"type": "refine", "activeSessionId": session.id(), "global": input["global"] == true});
            for key in ["instructions", "rollbackId"] {
                if let Some(text) = input[key].as_str() {
                    command[key] = text.into();
                }
            }
            let command = serde_json::from_value(command).context("not a daemon command")?;
            let response = session.client().request_with_timeout(command, 10 * 60 * 1000).await?;
            super::session::data_of("refine", response)
        }
        "refinement_history" => {
            let data = session_tree(&session).await?;
            Ok(json!({"history": refinement_history(&Tree::from_flat(&data["flatNodes"]))}))
        }
        "export_jsonl" => {
            let output = match input["outputPath"].as_str() {
                Some(raw) => Some(path_in_project(&chat.path(), raw)?.to_string_lossy().into_owned()),
                None => None,
            };
            let saved = session.call("export_jsonl", json!({"outputPath": output})).await?;
            Ok(json!({"path": saved["path"]}))
        }
        "reload" => {
            session.call("reload", json!({})).await?;
            Ok(json!({"reloaded": true}))
        }
        "import" => {
            let project = chat.path();
            let from = path_in_project(&project, &text_of(input, "path"))?;
            let from = validate::existing_file("import path", &from.to_string_lossy())?;
            let fields = json!({"inputPath": from.to_string_lossy(), "cwdOverride": project.to_string_lossy()});
            session.call("import_jsonl", fields).await
        }
        other => bail!("Unknown harness action: {other}"),
    }
}

// ---------- Export and share ----------

/// Exports the chat as an HTML page and returns where it went. `output_path` is the file to write; without one the
/// daemon picks.
pub async fn export_html(manager: &Manager, agent_id: &str, output_path: Option<&str>) -> Result<String> {
    let chat = manager.chat(agent_id).ok_or_else(|| anyhow!("Agent not connected"))?;
    let session = chat.session().ok_or_else(|| anyhow!("Agent not connected"))?;
    let output = match output_path {
        Some(raw) => Some(path_in_project(&chat.path(), raw)?.to_string_lossy().into_owned()),
        None => None,
    };
    let saved = session
        .send(&json!({"type": "export_html", "outputPath": output}))
        .await?;
    Ok(saved["path"]
        .as_str()
        .map(str::to_string)
        .or(output)
        .unwrap_or_default())
}

/// A folder only this user can enter, removed when dropped: a shared export holds the whole conversation.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("desktop-share-{}", uuid::Uuid::new_v4().simple()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Uploads the chat as a secret GitHub gist (through the user's `gh`) and returns its address and the viewer's.
pub async fn share_session(manager: &Manager, agent_id: &str) -> Result<Value> {
    share_session_via(manager, agent_id, Path::new("gh")).await
}

/// [`share_session`] with a particular GitHub CLI: a full path where `gh` is not on PATH, or a stand-in under test.
pub async fn share_session_via(manager: &Manager, agent_id: &str, gh: &Path) -> Result<Value> {
    let folder = PrivateDir::create()?;
    let file = folder.0.join(format!("prime-session-{}.html", unix_ms()));
    export_html(manager, agent_id, Some(&file.to_string_lossy())).await?;
    let args = |args: &[&str]| args.iter().map(|arg| (*arg).to_string()).collect::<Vec<String>>();
    let logged_in = run_capture(
        gh,
        &args(&["auth", "status"]),
        &folder.0,
        Duration::from_secs(30),
        64 * 1024,
    )
    .await;
    if !logged_in.is_ok_and(|done| done.success) {
        bail!("GitHub CLI is not logged in. Run gh auth login first.");
    }
    let file_arg = file.to_string_lossy().into_owned();
    let created = run_capture(
        gh,
        &args(&["gist", "create", "--public=false", &file_arg]),
        &folder.0,
        Duration::from_secs(120),
        64 * 1024,
    )
    .await
    .map_err(|error| anyhow!("Failed to create gist: {error:#}"))?;
    if !created.success {
        let why = created.stderr.trim();
        bail!("{}", if why.is_empty() { "Failed to create gist" } else { why });
    }
    let printed = if created.stdout.trim().is_empty() {
        &created.stderr
    } else {
        &created.stdout
    };
    let gist_url = printed.trim().lines().last().unwrap_or("").trim().to_string();
    let gist_id = gist_url.rsplit('/').next().unwrap_or("");
    if gist_id.is_empty() {
        bail!("Failed to parse gist ID from gh output");
    }
    Ok(json!({"gistUrl": gist_url, "previewUrl": format!("https://pi.dev/session/{gist_id}")}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(raw: &str) -> Option<String> {
        clean_session_title(raw)
    }

    #[test]
    fn model_output_is_cleaned_into_a_short_title() {
        assert_eq!(
            title("Fix CSV column alignment\n").as_deref(),
            Some("Fix CSV column alignment")
        );
        assert_eq!(
            title("Title: \"EGFR binder design.\"").as_deref(),
            Some("EGFR binder design")
        );
        assert_eq!(title("**Single-cell QC**").as_deref(), Some("Single-cell QC"));
        assert_eq!(
            title("\n\n  Protein folding run  \nextra").as_deref(),
            Some("Protein folding run")
        );
        assert_eq!(
            title("Session name - Plot the results").as_deref(),
            Some("Plot the results")
        );
        assert_eq!(title("# `fix` bug!").as_deref(), Some("fix bug"));
        assert_eq!(title("“Quoted title”").as_deref(), Some("Quoted title"));
        assert_eq!(title("Windows line\r\nsecond").as_deref(), Some("Windows line"));
    }

    #[test]
    fn empty_placeholder_and_sentence_length_replies_are_rejected() {
        assert_eq!(title(""), None);
        assert_eq!(title("   \n  "), None);
        assert_eq!(title("Untitled"), None);
        assert_eq!(title("New chat."), None);
        assert_eq!(title("N/A"), None);
        assert_eq!(title("None"), None);
        assert_eq!(
            title("Sure! Here is a title that I think captures the conversation you had with the assistant"),
            None
        );
        assert_eq!(title("Title:"), None, "a label with nothing after it is not a title");
    }

    #[test]
    fn a_long_title_is_cut_at_a_word_and_marked() {
        let long = "Investigating the relationship between genome assembly quality and annotation";
        let cut = title(long).unwrap();
        assert!(cut.ends_with('…'));
        assert!(cut.chars().count() <= 60);
        assert_eq!(cut, "Investigating the relationship between genome assembly…");
        let one_word = "x".repeat(80);
        assert_eq!(title(&one_word).unwrap(), format!("{}…", "x".repeat(59)));
        let ten = "one two three four five six seven eight nine ten";
        assert_eq!(
            title(ten).as_deref(),
            Some(ten),
            "ten words is the most a title may have"
        );
        assert_eq!(title(&format!("{ten} eleven")), None);
    }

    #[test]
    fn the_title_prompt_clips_long_exchanges() {
        let prompt = title_prompt(&"a".repeat(5000), &"b".repeat(5000));
        assert!(prompt.chars().count() < 2200);
        assert!(prompt.ends_with("Title:"));
        assert!(prompt.starts_with("User:\n"));
        assert!(prompt.contains(&format!("{}…", "a".repeat(1200))));
        assert!(prompt.contains(&format!("{}…", "b".repeat(800))));
        assert_eq!(
            title_prompt("  fix\n\n  this  ", "ok"),
            "User:\nfix this\n\nAssistant:\nok\n\nTitle:"
        );
        assert_eq!(title_prompt("é".repeat(1300).as_str(), "").matches('é').count(), 1200);
    }

    #[test]
    fn message_text_comes_from_strings_and_text_blocks() {
        assert_eq!(session_text(&json!("  hi ")), "hi");
        assert_eq!(
            session_text(&json!([{"type": "text", "text": "a"}, {"type": "image"}, "x", {"text": "b"}])),
            "a   b"
        );
        assert_eq!(session_text(&json!(null)), "");
        assert_eq!(session_text(&json!({"text": "no"})), "");
    }

    #[test]
    fn only_the_panel_questions_are_read_only() {
        for action in [
            "get_tree",
            "get_tree_full",
            "system_prompt",
            "goal_state",
            "heartbeats",
            "trace_list",
            "model_catalog",
            "refinement_history",
        ] {
            assert!(is_read_only(action), "{action}");
        }
        for action in [
            "queue",
            "reload",
            "refine",
            "navigate_tree",
            "fast",
            "import",
            "family",
            "resources",
            "",
        ] {
            assert!(!is_read_only(action), "{action}");
        }
    }

    #[test]
    fn command_lines_split_like_a_shell() {
        let split = |line: &str| split_command_line(line, false).unwrap();
        assert_eq!(
            split("uvx git+https://github.com/googlecolab/colab-mcp"),
            ["uvx", "git+https://github.com/googlecolab/colab-mcp"]
        );
        assert_eq!(
            split(r#"npx -y "@scope/pkg" --name 'two words'"#),
            ["npx", "-y", "@scope/pkg", "--name", "two words"]
        );
        assert_eq!(split(r"a\ b  c"), ["a b", "c"]);
        assert_eq!(split(r#"x """#), ["x", ""]);
        assert_eq!(split(r#"say "a \"quoted\" word""#), ["say", r#"a "quoted" word"#]);
        assert!(split("   ").is_empty());
        let error = split_command_line(r#"oops "unterminated"#, false).unwrap_err();
        assert!(error.to_string().contains("unterminated quote"));
    }

    #[test]
    fn windows_paths_keep_their_backslashes() {
        let split = |line: &str| split_command_line(line, true).unwrap();
        assert_eq!(
            split(r#""C:\Program Files\nodejs\node.exe" server.js"#),
            [r"C:\Program Files\nodejs\node.exe", "server.js"]
        );
        assert_eq!(split(r"C:\tools\x.exe --flag"), [r"C:\tools\x.exe", "--flag"]);
        assert_eq!(split(r#"run "say \"hi\"""#), ["run", r#"say "hi""#]);
    }

    fn flat(entries: &[(&str, Option<&str>, &str)]) -> Value {
        Value::Array(
            entries
                .iter()
                .map(|(id, parent, time)| {
                    json!({"entry": {"id": id, "parentId": parent, "timestamp": time, "type": "message",
                        "message": {"role": "user", "content": format!("text {id}")}}, "label": if *id == "b" { json!("keep") } else { json!(null) }})
                })
                .collect(),
        )
    }

    #[test]
    fn flat_nodes_become_a_tree_with_children_in_time_order() {
        let tree = Tree::from_flat(&flat(&[
            ("a", None, "2025-01-01T00:00:00.000Z"),
            ("c", Some("a"), "2025-01-01T00:00:03.000Z"),
            ("b", Some("a"), "2025-01-01T00:00:02.000Z"),
            ("d", Some("b"), "2025-01-01T00:00:04.000Z"),
            ("self", Some("self"), "2025-01-01T00:00:05.000Z"),
            ("orphan", Some("gone"), "2025-01-01T00:00:06.000Z"),
        ]));
        let mut rows = Vec::new();
        tree.walk(|node, depth| rows.push(tree_row(node, depth)));
        let seen: Vec<(String, u64)> = rows
            .iter()
            .map(|row| {
                (
                    row["entryId"].as_str().unwrap().to_string(),
                    row["depth"].as_u64().unwrap(),
                )
            })
            .collect();
        let expect = [("a", 0), ("b", 1), ("d", 2), ("c", 1), ("self", 0), ("orphan", 0)];
        assert_eq!(seen, expect.map(|(id, depth)| (id.to_string(), depth)));
        assert_eq!(rows[1]["label"], "keep");
        assert_eq!(rows[0]["label"], "");
        assert_eq!(rows[0]["text"], "text a");
        assert_eq!(rows[0]["role"], "user");
        assert_eq!(rows[0]["parentId"], Value::Null);

        let nested = tree.nested();
        assert_eq!(nested.len(), 3);
        assert_eq!(nested[0]["entry"]["id"], "a");
        assert_eq!(nested[0]["children"][0]["entry"]["id"], "b");
        assert_eq!(nested[0]["children"][0]["children"][0]["entry"]["id"], "d");
        assert_eq!(nested[0]["children"][1]["entry"]["id"], "c");
        assert_eq!(nested[0]["children"][1]["children"], json!([]));
    }

    #[test]
    fn a_nodeless_or_garbled_tree_is_empty_not_an_error() {
        for raw in [
            json!(null),
            json!([]),
            json!({"x": 1}),
            json!([{"no": "entry"}, 4, "x"]),
        ] {
            let tree = Tree::from_flat(&raw);
            assert!(tree.nested().is_empty());
        }
    }

    #[test]
    fn a_long_chain_is_walked_without_recursion() {
        let entries: Vec<Value> = (0..5000)
            .map(|index| {
                let parent = (index > 0).then(|| format!("n{}", index - 1));
                json!({"entry": {"id": format!("n{index}"), "parentId": parent, "timestamp": "2025-01-01T00:00:00.000Z"}})
            })
            .collect();
        let tree = Tree::from_flat(&Value::Array(entries));
        let mut deepest = 0;
        tree.walk(|_, depth| deepest = deepest.max(depth));
        assert_eq!(deepest, 4999);
        // Building the nested form is iterative too; dropping it is not, so it is flattened by hand here.
        let mut nested = tree.nested();
        let mut walked = 0;
        while let Some(mut node) = nested.pop() {
            walked += 1;
            if let Some(kids) = node["children"].as_array_mut() {
                nested.append(kids);
            }
        }
        assert_eq!(walked, 5000);
    }

    #[test]
    fn refinements_are_found_on_every_branch() {
        let node = |id: &str, parent: Option<&str>, custom: &str, data: Value| json!({"entry": {"id": id, "parentId": parent, "timestamp": id, "type": "custom", "customType": custom, "data": data}});
        let tree = Tree::from_flat(&Value::Array(vec![
            node("1", None, "prime-agent.refinement", json!({"id": "first"})),
            node("2", Some("1"), "other", json!({"id": "no"})),
            node("3", Some("1"), "prime-agent.refinement", json!(null)),
            node("4", Some("3"), "prime-agent.refinement", json!({"id": "second"})),
            node("5", Some("1"), "prime-agent.refinement", json!("text")),
        ]));
        assert_eq!(
            refinement_history(&tree),
            [json!({"id": "first"}), json!({"id": "second"}), json!("text")]
        );
    }

    #[test]
    fn queue_changes_are_checked_before_they_are_sent() {
        assert_eq!(
            queue_mutation(&json!({"type": "move", "direction": -1})).unwrap(),
            json!({"type": "move", "direction": -1})
        );
        assert_eq!(
            queue_mutation(&json!({"type": "move", "direction": "up"})).unwrap(),
            json!({"type": "move", "direction": 1})
        );
        assert_eq!(
            queue_mutation(&json!({"type": "replace", "text": "new", "lane": "followUp"})).unwrap(),
            json!({"type": "replace", "text": "new", "lane": "followUp"})
        );
        assert_eq!(
            queue_mutation(&json!({"type": "replace"})).unwrap(),
            json!({"type": "replace", "text": "", "lane": "steering"})
        );
        assert_eq!(
            queue_mutation(&json!({"type": "delete"})).unwrap(),
            json!({"type": "delete"})
        );
        for odd in [
            json!({}),
            json!(null),
            json!({"type": "drop-everything"}),
            json!({"type": 3}),
        ] {
            assert!(
                queue_mutation(&odd).is_err(),
                "{odd}: an unknown change must not delete anything"
            );
        }
    }

    #[test]
    fn indexes_are_whole_non_negative_numbers() {
        assert_eq!(index_of(&json!(2)), 2);
        assert_eq!(index_of(&json!(2.9)), 2);
        assert_eq!(index_of(&json!("3")), 3);
        for odd in [json!(-4), json!(null), json!("x"), json!(true), json!({})] {
            assert_eq!(index_of(&odd), 0, "{odd}");
        }
    }

    #[test]
    fn paths_from_the_renderer_are_made_absolute_against_the_project() {
        let project = Path::new("/work/proj");
        assert_eq!(
            path_in_project(project, "out/chat.html").unwrap(),
            Path::new("/work/proj/out/chat.html")
        );
        assert_eq!(
            path_in_project(project, " /tmp/x.html ").unwrap(),
            Path::new("/tmp/x.html")
        );
        assert!(path_in_project(project, "  ").is_err());
        assert!(path_in_project(project, "a\0b").is_err());
        assert!(path_in_project(project, &"a".repeat(5000)).is_err());
    }

    fn settings_file(contents: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent").join("settings.json");
        if let Some(contents) = contents {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }
        (dir, path)
    }

    fn written(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_settings_file_that_cannot_be_read_back_is_never_overwritten() {
        let corrupt = "{ \"defaultModel\": \"x\", ";
        let (_dir, path) = settings_file(Some(corrupt));
        let mcp = McpServer::Http {
            url: "https://example.com/mcp".into(),
            oauth: false,
            enabled: true,
            bearer_token_env_var: None,
        };
        assert!(
            set_mcp_server(&path, "docs", mcp)
                .unwrap_err()
                .to_string()
                .contains("not valid JSON")
        );
        assert!(
            set_model_role(&path, "auxiliaryModel", "openai/gpt-5")
                .unwrap_err()
                .to_string()
                .contains("not valid JSON")
        );
        assert!(set_agent_traces(&path, true).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
        std::fs::write(&path, "[1, 2]").unwrap();
        assert!(
            set_agent_traces(&path, true)
                .unwrap_err()
                .to_string()
                .contains("JSON object")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1, 2]");
        // Display paths stay lenient.
        assert_eq!(model_roles_in(&path)["auxiliaryModel"], "");
        assert!(!agent_traces_enabled(&path));
    }

    #[test]
    fn traces_are_switched_in_place_and_keep_the_other_settings() {
        let (_dir, path) = settings_file(None);
        set_agent_traces(&path, true).unwrap();
        assert_eq!(written(&path), json!({"agentTraces": {"enabled": true}}));
        std::fs::write(&path, r#"{"defaultModel": "m", "agentTraces": {"dir": "x"}}"#).unwrap();
        set_agent_traces(&path, true).unwrap();
        assert_eq!(
            written(&path),
            json!({"defaultModel": "m", "agentTraces": {"dir": "x", "enabled": true}})
        );
        assert!(agent_traces_enabled(&path));
        set_agent_traces(&path, false).unwrap();
        assert!(!agent_traces_enabled(&path));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no temp file is left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_rewritten_settings_file_is_never_more_readable_than_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let (_dir, path) = settings_file(Some(r#"{"defaultModel": "m"}"#));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        set_agent_traces(&path, true).unwrap();
        assert_eq!(mode(&path), 0o600, "an owner-only file stays owner-only");
        let fresh = path.with_file_name("new-settings.json");
        set_agent_traces(&fresh, true).unwrap();
        assert_eq!(mode(&fresh), 0o600, "a new file is created owner-only");
    }

    #[test]
    fn model_roles_are_validated_set_and_cleared() {
        let (_dir, path) = settings_file(Some(r#"{"defaultModel": "m"}"#));
        assert_eq!(
            set_model_role(&path, "auxiliaryModel", "openai/gpt-5").unwrap(),
            json!({"subagentDefaultModel": "", "auxiliaryModel": "openai/gpt-5", "providerBackupModel": ""})
        );
        assert_eq!(written(&path)["defaultModel"], "m");
        assert_eq!(
            set_model_role(&path, "auxiliaryModel", "openrouter/a/b:free").unwrap()["auxiliaryModel"],
            "openrouter/a/b:free"
        );
        assert_eq!(
            set_model_role(&path, "auxiliaryModel", "").unwrap()["auxiliaryModel"],
            ""
        );
        assert!(written(&path).get("auxiliaryModel").is_none());
        for (key, value, why) in [
            ("defaultModel", "a/b", "Unknown model role."),
            ("auxiliaryModel", "no-provider", "provider and model ID"),
            ("auxiliaryModel", "a/b c", "provider and model ID"),
            (
                "auxiliaryModel",
                &format!("a/{}", "x".repeat(600)),
                "provider and model ID",
            ),
        ] {
            let error = set_model_role(&path, key, value).unwrap_err().to_string();
            assert!(error.contains(why), "{key} {value}: {error}");
        }
    }

    #[test]
    fn mcp_servers_are_stored_validated_and_removed() {
        let (_dir, path) = settings_file(Some(
            r#"{"defaultModel": "x", "mcpServers": {"docs": {"type": "http", "url": "https://example.com/mcp", "startupTimeoutMs": 5}}}"#,
        ));
        let stdio = mcp_server_from(&json!({"type": "stdio", "commandLine": "uvx git+https://x/y --flag 'a b'", "cwd": " /w ", "name": "colab"})).unwrap();
        let servers = set_mcp_server(&path, "colab-mcp", stdio).unwrap();
        assert_eq!(
            servers["colab-mcp"],
            json!({"type": "stdio", "command": "uvx", "args": ["git+https://x/y", "--flag", "a b"], "cwd": "/w", "enabled": true})
        );
        assert_eq!(servers["docs"]["url"], "https://example.com/mcp");
        let saved = written(&path);
        assert_eq!(saved["defaultModel"], "x");
        assert!(saved["mcpServers"]["colab-mcp"].get("url").is_none());
        assert_eq!(
            saved["mcpServers"]["docs"]["startupTimeoutMs"], 5,
            "fields the panel does not edit survive"
        );

        // An http server keeps what the panel does not edit and drops a bearer variable that was cleared.
        let http = |bearer: Value| {
            mcp_server_from(&json!({"url": "https://example.com/mcp", "oauth": false, "bearerTokenEnvVar": bearer}))
                .unwrap()
        };
        set_mcp_server(&path, "docs", http(json!("DOCS_TOKEN"))).unwrap();
        assert_eq!(written(&path)["mcpServers"]["docs"]["bearerTokenEnvVar"], "DOCS_TOKEN");
        assert_eq!(written(&path)["mcpServers"]["docs"]["startupTimeoutMs"], 5);
        set_mcp_server(&path, "docs", http(json!(""))).unwrap();
        assert!(written(&path)["mcpServers"]["docs"].get("bearerTokenEnvVar").is_none());

        let servers = set_mcp_server(&path, "colab-mcp", mcp_server_from(&json!({"remove": true})).unwrap()).unwrap();
        assert!(servers.get("colab-mcp").is_none());
        assert!(
            set_mcp_server(&path, "bad name", McpServer::Remove)
                .unwrap_err()
                .to_string()
                .contains("letters, numbers")
        );
        assert!(set_mcp_server(&path, "  ", McpServer::Remove).is_err());
    }

    #[test]
    fn mcp_input_is_refused_when_it_is_unsafe_or_incomplete() {
        let error = |input: Value| mcp_server_from(&input).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(error(json!({"type": "stdio", "command": "  "})).contains("needs a command"));
        assert!(error(json!({"type": "stdio", "commandLine": ""})).contains("needs a command"));
        assert!(error(json!({"type": "stdio", "command": "x", "args": ["a\0b"]})).contains("NUL"));
        assert!(error(json!({"type": "stdio", "commandLine": "x \"open"})).contains("unterminated"));
        assert!(error(json!({"url": "http://example.com/mcp"})).contains("HTTPS"));
        assert!(error(json!({"url": "ftp://localhost/x"})).contains("HTTPS"));
        assert!(error(json!({"url": "not a url"})).contains("Invalid URL"));
        assert!(error(json!({})).contains("Invalid URL"));
        // Plain http is for a server on this machine.
        assert!(matches!(
            mcp_server_from(&json!({"url": "http://localhost:8080/mcp"})),
            Ok(McpServer::Http { .. })
        ));
        let Ok(McpServer::Stdio { command, args, .. }) =
            mcp_server_from(&json!({"type": "stdio", "command": " node ", "args": ["a", 3]}))
        else {
            panic!("a stdio server")
        };
        assert_eq!(
            (command.as_str(), args),
            ("node", vec!["a".to_string(), "3".to_string()])
        );
    }

    #[test]
    fn the_catalog_is_read_from_beside_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(mcp_catalog_in(Some(dir.path())), json!([]));
        assert_eq!(mcp_catalog_in(None), json!([]));
        std::fs::write(
            dir.path().join("mcp-services.bundled.json"),
            r#"{"version": 2, "entries": [{"server": "svc", "label": "Service", "auth": {"strategy": "oauth"}}]}"#,
        )
        .unwrap();
        assert_eq!(
            mcp_catalog_in(Some(dir.path())),
            json!([{"id": "svc", "name": "Service", "description": "", "auth": "oauth"}])
        );
        std::fs::write(dir.path().join("mcp-services.bundled.json"), "not json").unwrap();
        assert_eq!(mcp_catalog_in(Some(dir.path())), json!([]));
    }

    #[tokio::test]
    async fn saved_sessions_are_listed_newest_first_and_previewed_only_from_their_folder() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            trace_list_in(&root.path().join("missing")).await.unwrap(),
            json!({"files": []})
        );
        for (name, body) in [("old.jsonl", "{}\n"), ("new.jsonl", "{\"a\":1}\n"), ("notes.txt", "x")] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        let old = std::fs::File::options()
            .write(true)
            .open(dir.join("old.jsonl"))
            .unwrap();
        old.set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        let listed = trace_list_in(&dir).await.unwrap();
        let names: Vec<&str> = listed["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["new.jsonl", "old.jsonl"]);
        assert_eq!(listed["files"][0]["size"], 8);

        let new = dir.join("new.jsonl");
        let preview = trace_preview_in(&dir, &new.to_string_lossy()).await.unwrap();
        assert_eq!(
            (
                preview["preview"].as_str(),
                preview["truncated"].as_bool(),
                preview["size"].as_u64()
            ),
            (Some("{\"a\":1}\n"), Some(false), Some(8))
        );

        std::fs::write(root.path().join("secret.jsonl"), "no").unwrap();
        std::fs::write(dir.join("notes.txt"), "no").unwrap();
        for outside in [
            root.path().join("secret.jsonl"),
            dir.join("notes.txt"),
            dir.join("..").join("secret.jsonl"),
            dir.join("missing.jsonl"),
        ] {
            let error = trace_preview_in(&dir, &outside.to_string_lossy())
                .await
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                "Trace path is outside the session directory",
                "{}",
                outside.display()
            );
        }
        assert!(trace_preview_in(&dir, "relative.jsonl").await.is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("secret.jsonl"), dir.join("link.jsonl")).unwrap();
            assert!(
                trace_preview_in(&dir, &dir.join("link.jsonl").to_string_lossy())
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn a_big_session_is_previewed_by_its_start_and_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.jsonl");
        // 'é' is two bytes, so the file is larger than the preview in bytes and in characters.
        std::fs::write(&big, "é".repeat(300_000)).unwrap();
        let preview = trace_preview_in(dir.path(), &big.to_string_lossy()).await.unwrap();
        assert_eq!(preview["preview"].as_str().unwrap().chars().count(), 200_000);
        assert_eq!(preview["truncated"], true);
        assert_eq!(preview["size"], 600_000);
        let exact = dir.path().join("exact.jsonl");
        std::fs::write(&exact, "é".repeat(200_000)).unwrap();
        let preview = trace_preview_in(dir.path(), &exact.to_string_lossy()).await.unwrap();
        assert_eq!(
            preview["truncated"], false,
            "exactly the preview length is not truncated"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_is_limited_in_time_and_output_and_does_not_hang_on_inherited_pipes() {
        let here = std::env::temp_dir();
        let sh = |script: &str| vec!["-c".to_string(), script.to_string()];
        let done = run_capture(
            "sh",
            &sh("echo out; echo err >&2"),
            &here,
            Duration::from_secs(10),
            1024,
        )
        .await
        .unwrap();
        assert_eq!(
            (done.stdout.as_str(), done.stderr.as_str(), done.success, done.truncated),
            ("out\n", "err\n", true, false)
        );
        let done = run_capture("sh", &sh("exit 3"), &here, Duration::from_secs(10), 1024)
            .await
            .unwrap();
        assert!(!done.success);
        let done = run_capture(
            "sh",
            &sh("head -c 100000 /dev/zero | tr '\\0' x"),
            &here,
            Duration::from_secs(10),
            1024,
        )
        .await
        .unwrap();
        assert_eq!((done.stdout.len(), done.truncated), (1024, true));
        let slow = run_capture("sh", &sh("sleep 5"), &here, Duration::from_millis(200), 1024).await;
        assert!(slow.unwrap_err().to_string().contains("did not finish"));
        let started = std::time::Instant::now();
        let done = run_capture(
            "sh",
            &sh("echo hi; (sleep 30 &) ; true"),
            &here,
            Duration::from_secs(10),
            1024,
        )
        .await
        .unwrap();
        assert_eq!(done.stdout, "hi\n");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "a background process holding the pipes must not hold this up"
        );
        let missing = run_capture("definitely-not-a-program", &[], &here, Duration::from_secs(1), 1024).await;
        assert!(missing.unwrap_err().to_string().contains("Could not start"));
    }

    #[test]
    fn the_private_folder_is_closed_to_others_and_removed() {
        let dir = PrivateDir::create().unwrap();
        let path = dir.0.clone();
        assert!(path.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }
        std::fs::write(path.join("x.html"), "secret").unwrap();
        drop(dir);
        assert!(!path.exists());
    }
}

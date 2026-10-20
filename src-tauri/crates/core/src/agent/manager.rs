//! One agent per project tab: sessions, queues, events for the renderer.
//!
//! A project has a primary chat and up to five more that run at the same time. Each chat is its own daemon session on its
//! own connection. The manager turns the daemon's event stream into the `AgentInfo` and message list the renderer shows,
//! and sends the renderer's commands to the right session. The daemon and its worker are Prime Agent's.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use pa_tui::daemon_client::DaemonClientEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use super::link::{DaemonLink, Events, SessionConfig, attach, open_session};
use super::session::Session;
use super::shared::{base_name, is_internal_state_restore_message, model_key_from_state, parse_model_list};
use super::{harness, sessions};

pub const MAX_CHATS_PER_PROJECT: usize = 6;

/// What the manager sends toward the renderer and the desktop.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// `{agentId, type, payload}` on the `events` channel.
    Event(Value),
    /// `{id, kind, text}` on the `toasts` channel.
    Toast(Value),
    /// A native notification.
    Notify { title: String, body: String },
    /// A chat finished a run.
    Completion { agent_id: String, name: String },
}

pub type Sink = Arc<dyn Fn(Outbound) + Send + Sync>;

/// The app settings the manager reads, from the settings JSON the store holds.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub thinking_level: String,
    pub model: Option<String>,
    pub rlm_max_depth: u64,
    pub transport: String,
    pub auto_compaction: bool,
    pub auto_retry: bool,
    pub checkpoints: bool,
    pub notifications: bool,
    pub auto_name_sessions: bool,
    pub autonomous: Value,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            thinking_level: "medium".into(),
            model: None,
            rlm_max_depth: 1,
            transport: "auto".into(),
            auto_compaction: true,
            auto_retry: true,
            checkpoints: true,
            notifications: true,
            auto_name_sessions: true,
            autonomous: json!({"enabled": false, "gates": [], "gateRetries": 2, "maxContinuations": 8, "maxTurns": 30, "maxTokens": 100000, "maxSeconds": 3600}),
        }
    }
}

impl Settings {
    pub fn from_value(value: &Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    /// The autonomous block in the shape the daemon takes on `create`.
    fn daemon_autonomous(&self) -> Value {
        let a = &self.autonomous;
        let number = |key: &str, default: u64| a.get(key).and_then(Value::as_u64).unwrap_or(default);
        json!({
            "enabled": a.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            "maxContinuations": number("maxContinuations", 8),
            "maxTurns": number("maxTurns", 30),
            "maxTokens": number("maxTokens", 100_000),
            "timeoutMs": number("maxSeconds", 3600) * 1000,
            "gates": {"commands": a.get("gates").cloned().unwrap_or_else(|| json!([])), "maxRetries": number("gateRetries", 2)},
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    pub attempt: u64,
    pub max_attempts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionUi {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub statuses: BTreeMap<String, String>,
    pub widgets: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_text: Option<String>,
}

/// The renderer's `AgentInfo` (`src/shared/types.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub id: String,
    pub tab_id: String,
    pub name: String,
    pub path: String,
    pub status: String,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub message_count: u64,
    pub cost: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub context_percent: Option<f64>,
    pub context_tokens: Option<f64>,
    pub context_window: Option<f64>,
    pub is_streaming: bool,
    pub session_name: Option<String>,
    pub session_id: Option<String>,
    pub retry: Option<Retry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_ui: Option<ExtensionUi>,
}

#[derive(Debug, Default)]
pub struct Queued {
    pub steer: Vec<String>,
    pub follow_ups: Vec<String>,
}

pub struct ChatState {
    pub info: AgentInfo,
    pub messages: Vec<Value>,
    /// Open extension dialogs by id.
    pub dialogs: HashMap<String, Value>,
    pub queued: Queued,
    pub commands: Option<Vec<Value>>,
    pub available_models: Option<Vec<Value>>,
    pub stats: Option<Value>,
    pub skills: Vec<Value>,
    pub last_event_at: Instant,
    pub pending_runtime_reload: bool,
    pub pending_model_switch: Option<(Option<String>, String)>,
    /// Counts connection attempts and releases, so a late event or reply from an older connection is recognised.
    pub starts: u64,
    pub titling: bool,
    /// Auto-title attempts per session id.
    pub title_attempts: HashMap<String, u32>,
    /// The project's checkpoints, newest last. Kept on the primary chat: every parallel chat shares the one list.
    pub checkpoints: Vec<crate::git::Checkpoint>,
}

/// One chat: a daemon session and what the renderer shows of it.
pub struct Chat {
    pub id: String,
    pub tab_id: String,
    pub path: Mutex<PathBuf>,
    pub state: Mutex<ChatState>,
    session: Mutex<Option<Session>>,
    connecting: AsyncMutex<()>,
    /// Serializes replacing the session with prompts, so a new-chat click and a fast prompt cannot cross.
    session_change: AsyncMutex<()>,
    /// One checkpoint operation at a time per project, so two cannot overwrite each other's list.
    checkpoint_lock: AsyncMutex<()>,
}

impl Chat {
    pub fn session(&self) -> Option<Session> {
        self.session.lock().ok().and_then(|session| session.clone())
    }

    pub fn path(&self) -> PathBuf {
        self.path.lock().map(|path| path.clone()).unwrap_or_default()
    }

    fn is_idle(&self) -> bool {
        let state = self.state.lock().unwrap();
        self.session().is_some()
            && state.info.status == "idle"
            && !state.info.is_streaming
            && state.dialogs.is_empty()
            && !self.connecting_now()
    }

    fn connecting_now(&self) -> bool {
        self.connecting.try_lock().is_err()
    }
}

/// How the manager reaches the world outside it.
pub struct ManagerConfig {
    /// The Prime Agent executable to start. Looked up on use, since the install can finish while the app runs.
    pub binary: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    pub socket: PathBuf,
    /// Added to the daemon's environment when the app starts it (the sandbox's kernel wrapper, for one). Read each time the
    /// daemon is started, since it changes with the sandbox mode.
    pub daemon_env: Arc<dyn Fn() -> Vec<(String, String)> + Send + Sync>,
    pub settings: Arc<dyn Fn() -> Settings + Send + Sync>,
}

/// The daemon launcher, and what it was made for: a different binary or environment needs a new one.
type LinkKey = (PathBuf, Vec<(String, String)>, Arc<DaemonLink>);

struct Inner {
    config: ManagerConfig,
    sink: Sink,
    chats: Mutex<HashMap<String, Arc<Chat>>>,
    link: Mutex<Option<LinkKey>>,
    published: Mutex<HashMap<String, String>>,
    /// Failed starts per project: its chats share one daemon, so they back off together.
    start_backoff: Mutex<HashMap<String, (u32, Instant)>>,
    reconnect_attempts: Mutex<HashMap<String, Instant>>,
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

/// Delay before the next attempt to start a chat after failed starts: 15 s, 30 s, 60 s, then 2 min. A failed start
/// launches a worker that can take half a minute to fail, so repeated clicks and polls must not stack those up.
pub fn start_retry_delay(failures: u32) -> Duration {
    Duration::from_millis((15_000u64 << failures.saturating_sub(1).min(10)).min(120_000))
}

pub fn primary_agent_id(tab_id: &str) -> String {
    format!("agent-{tab_id}")
}

fn toast_id() -> String {
    format!("t{}-{}", unix_ms(), &uuid::Uuid::new_v4().simple().to_string()[..4])
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn message_key(message: &Value) -> String {
    match message.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
        Some(id) => format!("id:{id}"),
        None => {
            let show = |key: &str| match message.get(key) {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            format!("ts:{}:{}", show("timestamp"), show("role"))
        }
    }
}

fn user_count(messages: &[Value]) -> u64 {
    messages.iter().filter(|message| message["role"] == "user").count() as u64
}

fn visible(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter(|message| !is_internal_state_restore_message(message))
        .cloned()
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Manager {
    pub fn new(config: ManagerConfig, sink: Sink) -> Self {
        let inner = Arc::new(Inner {
            config,
            sink,
            chats: Mutex::default(),
            link: Mutex::default(),
            published: Mutex::default(),
            start_backoff: Mutex::default(),
            reconnect_attempts: Mutex::default(),
        });
        let manager = Self { inner };
        manager.spawn_run_watchdog();
        manager
    }

    pub fn settings(&self) -> Settings {
        (self.inner.config.settings)()
    }

    /// The Prime Agent executable, for the few things that run its command line.
    pub fn binary(&self) -> PathBuf {
        (self.inner.config.binary)()
    }

    /// Every chat of every project.
    pub fn chats(&self) -> Vec<Arc<Chat>> {
        let mut chats: Vec<_> = lock(&self.inner.chats).values().cloned().collect();
        chats.sort_by(|a, b| a.id.cmp(&b.id));
        chats
    }

    fn emit(&self, outbound: Outbound) {
        (self.inner.sink)(outbound);
    }

    pub fn emit_event(&self, agent_id: &str, kind: &str, payload: Value) {
        self.emit(Outbound::Event(
            json!({"agentId": agent_id, "type": kind, "payload": payload}),
        ));
    }

    pub fn toast(&self, kind: &str, text: &str) {
        self.emit(Outbound::Toast(json!({"id": toast_id(), "kind": kind, "text": text})));
    }

    pub fn chat(&self, agent_id: &str) -> Option<Arc<Chat>> {
        lock(&self.inner.chats).get(agent_id).cloned()
    }

    fn require(&self, agent_id: &str) -> Result<Arc<Chat>> {
        self.chat(agent_id).ok_or_else(|| anyhow!("Agent not connected"))
    }

    fn tab_chats(&self, tab_id: &str) -> Vec<Arc<Chat>> {
        let mut chats: Vec<_> = lock(&self.inner.chats)
            .values()
            .filter(|chat| chat.tab_id == tab_id)
            .cloned()
            .collect();
        chats.sort_by(|a, b| a.id.cmp(&b.id));
        chats
    }

    pub fn project_agent_id(&self, agent_id: &str) -> String {
        self.chat(agent_id)
            .map_or_else(|| agent_id.to_string(), |chat| primary_agent_id(&chat.tab_id))
    }

    pub fn project_path(&self, agent_id: &str) -> Option<PathBuf> {
        self.chat(agent_id).map(|chat| chat.path())
    }

    pub fn all_infos(&self) -> Vec<Value> {
        let mut chats: Vec<_> = lock(&self.inner.chats).values().cloned().collect();
        chats.sort_by(|a, b| a.id.cmp(&b.id));
        chats
            .iter()
            .filter_map(|chat| serde_json::to_value(&lock(&chat.state).info).ok())
            .collect()
    }

    /// Changes a chat's info and publishes it if anything changed.
    pub fn update_info(&self, chat: &Arc<Chat>, change: impl FnOnce(&mut AgentInfo)) {
        change(&mut lock(&chat.state).info);
        self.publish(chat);
    }

    // ---------- Lifecycle ----------

    pub async fn open_tab(&self, tab_id: &str, path: PathBuf) -> Result<Value> {
        if !path.is_dir() {
            bail!("Project folder does not exist: {}", path.display());
        }
        let id = primary_agent_id(tab_id);
        let chat = match self.chat(&id) {
            Some(existing) => {
                *lock(&existing.path) = path.clone();
                lock(&existing.state).info.path = path.to_string_lossy().into_owned();
                existing
            }
            None => {
                let chat = self.create_chat(tab_id, path, id);
                lock(&self.inner.chats).insert(chat.id.clone(), chat.clone());
                self.publish(&chat);
                chat
            }
        };
        self.ensure_connected(&chat).await?;
        let info = serde_json::to_value(&lock(&chat.state).info)?;
        Ok(info)
    }

    fn create_chat(&self, tab_id: &str, path: PathBuf, id: String) -> Arc<Chat> {
        Arc::new(Chat {
            state: Mutex::new(blank_state(&id, tab_id, &path)),
            id,
            tab_id: tab_id.to_string(),
            path: Mutex::new(path),
            session: Mutex::new(None),
            connecting: AsyncMutex::new(()),
            session_change: AsyncMutex::new(()),
            checkpoint_lock: AsyncMutex::new(()),
        })
    }

    async fn open_slot(&self, tab_id: &str) -> Result<Arc<Chat>> {
        let primary = self
            .chat(&primary_agent_id(tab_id))
            .ok_or_else(|| anyhow!("Project is not open"))?;
        if self.tab_chats(tab_id).len() >= MAX_CHATS_PER_PROJECT {
            bail!("Up to {MAX_CHATS_PER_PROJECT} chats can run at once in a project. Stop one first.");
        }
        let id = format!(
            "{}--{}",
            primary_agent_id(tab_id),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let chat = self.create_chat(tab_id, primary.path(), id);
        lock(&self.inner.chats).insert(chat.id.clone(), chat.clone());
        self.publish(&chat);
        if let Err(error) = self.ensure_connected(&chat).await {
            self.close_chat(&chat.id).await;
            return Err(error);
        }
        Ok(chat)
    }

    async fn release_idle_slots(&self, tab_id: &str, keep: &str) {
        let primary = primary_agent_id(tab_id);
        let idle: Vec<_> = self
            .tab_chats(tab_id)
            .into_iter()
            .filter(|chat| chat.id != primary && chat.id != keep && chat.is_idle())
            .collect();
        for chat in idle {
            self.close_chat(&chat.id).await;
        }
    }

    fn focus_chat(&self, chat: &Chat) {
        self.emit_event(&chat.id, "chat_focus", json!({"tabId": chat.tab_id}));
    }

    /// Starts a fresh chat. A busy chat keeps running; the new one gets its own slot.
    pub async fn new_chat(&self, agent_id: &str) -> Result<String> {
        let chat = self.require(agent_id)?;
        let target = if chat.is_idle() {
            let _change = chat.session_change.lock().await;
            self.new_session(&chat.id).await?;
            chat.clone()
        } else {
            // A slot whose chat has no messages yet is already a blank new chat.
            let blank = self
                .tab_chats(&chat.tab_id)
                .into_iter()
                .find(|slot| slot.id != chat.id && slot.is_idle() && lock(&slot.state).info.message_count == 0);
            match blank {
                Some(slot) => slot,
                None => self.open_slot(&chat.tab_id).await?,
            }
        };
        self.release_idle_slots(&chat.tab_id, &target.id).await;
        self.focus_chat(&target);
        Ok(target.id.clone())
    }

    /// Opens a saved chat without interrupting any chat that is still running.
    pub async fn open_session(&self, agent_id: &str, session_path: &str) -> Result<String> {
        let chat = self.require(agent_id)?;
        let session_id = sessions::read_session_id(std::path::Path::new(session_path));
        let live = session_id.as_ref().and_then(|wanted| {
            self.tab_chats(&chat.tab_id)
                .into_iter()
                .find(|slot| lock(&slot.state).info.session_id.as_ref() == Some(wanted))
        });
        let target = match live {
            Some(slot) => slot,
            None => {
                let target = if chat.is_idle() {
                    chat.clone()
                } else {
                    match self.tab_chats(&chat.tab_id).into_iter().find(|slot| slot.is_idle()) {
                        Some(slot) => slot,
                        None => self.open_slot(&chat.tab_id).await?,
                    }
                };
                self.resume_session(&target.id, session_path).await?;
                target
            }
        };
        self.release_idle_slots(&chat.tab_id, &target.id).await;
        self.focus_chat(&target);
        Ok(target.id.clone())
    }

    /// Stops a chat slot's connection and forgets it. The project's primary chat is never closed here.
    pub async fn close_chat(&self, agent_id: &str) {
        let Some(chat) = self.chat(agent_id) else { return };
        if chat.id == primary_agent_id(&chat.tab_id) {
            return;
        }
        let idle = chat.is_idle();
        lock(&self.inner.chats).remove(agent_id);
        lock(&self.inner.published).remove(agent_id);
        lock(&chat.state).starts += 1;
        if let Some(session) = chat.session.lock().ok().and_then(|mut session| session.take()) {
            // A chat slot nobody needs any more is stopped, not just disconnected: resident sessions outlive their
            // connection, and idle workers would pile up in the daemon.
            if idle {
                let _ = tokio::time::timeout(Duration::from_secs(3), session.call("kill", json!({}))).await;
            }
            session.close();
        }
        self.emit_event(agent_id, "agent_closed", json!({"tabId": chat.tab_id}));
    }

    /// Closes a project: its extra chats, and its first chat. A chat that is working keeps going in the daemon.
    pub async fn close_tab(&self, tab_id: &str) {
        for chat in self.tab_chats(tab_id) {
            if chat.id == primary_agent_id(tab_id) {
                if chat.is_idle()
                    && let Some(session) = chat.session()
                {
                    let _ = tokio::time::timeout(Duration::from_secs(3), session.call("kill", json!({}))).await;
                }
                self.close_agent(&chat.id);
            } else {
                self.close_chat(&chat.id).await;
            }
        }
    }

    pub fn close_agent(&self, agent_id: &str) {
        if let Some(chat) = lock(&self.inner.chats).remove(agent_id) {
            lock(&self.inner.published).remove(agent_id);
            lock(&chat.state).starts += 1;
            if let Some(session) = lock(&chat.session).take() {
                session.close();
            }
        }
    }

    /// Stops the extra chats of every project that are not working, before the app exits, so idle workers do not pile up in
    /// the daemon. Each project's first chat, and any chat that is working, keeps going as a resident session.
    pub async fn release_idle_extras(&self) {
        let idle: Vec<_> = self
            .chats()
            .into_iter()
            .filter(|chat| chat.id != primary_agent_id(&chat.tab_id) && chat.is_idle())
            .collect();
        for chat in idle {
            if let Some(session) = chat.session() {
                let _ = session.call("kill", json!({})).await;
            }
        }
    }

    /// Chats that are still running keep going as resident daemon sessions; the connections just close.
    pub fn shutdown_all(&self) {
        let chats: Vec<_> = lock(&self.inner.chats).drain().map(|(_, chat)| chat).collect();
        for chat in chats {
            if let Some(session) = lock(&chat.session).take() {
                session.close();
            }
        }
    }

    // ---------- Connecting ----------

    fn link(&self) -> Arc<DaemonLink> {
        let binary = (self.inner.config.binary)();
        let env = (self.inner.config.daemon_env)();
        let mut slot = lock(&self.inner.link);
        match slot.as_ref() {
            Some((known_binary, known_env, link)) if *known_binary == binary && *known_env == env => link.clone(),
            _ => {
                let link = Arc::new(DaemonLink::new(
                    binary.clone(),
                    self.inner.config.socket.clone(),
                    env.clone(),
                ));
                *slot = Some((binary, env, link.clone()));
                link
            }
        }
    }

    /// Ends the daemon the chats talk to and forgets their connections, so the next command starts a fresh one with the
    /// environment as it is now. For when the sandbox mode changes: a daemon keeps the environment it was started with.
    pub async fn restart_daemon(&self) {
        let chats = self.chats();
        for chat in &chats {
            lock(&chat.state).starts += 1;
            if let Some(session) = lock(&chat.session).take() {
                session.close();
            }
        }
        self.link().stop().await;
        for chat in chats {
            self.update_info(&chat, |info| info.status = "stopped".into());
        }
    }

    /// Polls (queue, commands, harness views) arrive constantly. When the connection has dropped, reconnect instead of
    /// failing every request until the user happens to send a prompt. A failing attempt is retried at most every 10 s.
    pub async fn reconnect(&self, chat: &Arc<Chat>) {
        if chat.connecting_now() {
            let _wait = chat.connecting.lock().await;
            return;
        }
        {
            let mut attempts = lock(&self.inner.reconnect_attempts);
            if attempts
                .get(&chat.id)
                .is_some_and(|last| last.elapsed() < Duration::from_secs(10))
            {
                return;
            }
            attempts.insert(chat.id.clone(), Instant::now());
        }
        let _ = self.ensure_connected(chat).await;
    }

    pub async fn ensure_connected(&self, chat: &Arc<Chat>) -> Result<()> {
        if chat.session().is_some() {
            return Ok(());
        }
        let _connecting = chat.connecting.lock().await;
        if chat.session().is_some() {
            return Ok(());
        }
        // Chats of one project share its daemon, so a failed start backs off for all of them.
        let wait = lock(&self.inner.start_backoff)
            .get(&chat.tab_id)
            .and_then(|(_, retry_after)| retry_after.checked_duration_since(Instant::now()));
        if let Some(wait) = wait {
            bail!(
                "Prime Agent did not start this chat. Trying again in {} s.",
                wait.as_secs_f64().ceil() as u64
            );
        }
        self.update_info(chat, |info| info.status = "starting".into());
        match self.connect(chat).await {
            Ok(()) => {
                lock(&self.inner.start_backoff).remove(&chat.tab_id);
                Ok(())
            }
            Err(error) => {
                let mut backoff = lock(&self.inner.start_backoff);
                let failures = backoff.get(&chat.tab_id).map_or(0, |(failures, _)| *failures) + 1;
                backoff.insert(
                    chat.tab_id.clone(),
                    (failures, Instant::now() + start_retry_delay(failures)),
                );
                drop(backoff);
                self.update_info(chat, |info| info.status = "error".into());
                self.toast("error", &format!("prime-agent failed: {error}"));
                Err(error)
            }
        }
    }

    async fn connect(&self, chat: &Arc<Chat>) -> Result<()> {
        let settings = self.settings();
        let config = SessionConfig {
            cwd: chat.path(),
            thinking: settings.thinking_level.clone(),
            model: settings.model.clone(),
            autonomous: settings.daemon_autonomous(),
            rlm_max_depth: settings.rlm_max_depth,
            transport: settings.transport.clone(),
            extra: None,
        };
        let opened = open_session(&self.link(), &config).await?;
        let session = opened.session.clone();
        let start_no = {
            let mut state = lock(&chat.state);
            state.starts += 1;
            state.starts
        };
        *lock(&chat.session) = Some(session.clone());
        self.spawn_event_pump(chat.clone(), start_no, opened.events);

        self.update_info(chat, |info| {
            info.status = "idle".into();
            info.thinking_level = Some(settings.thinking_level.clone());
        });
        if let Ok(state) = session.send(&json!({"type": "get_state"})).await {
            if lock(&chat.state).starts != start_no {
                return Ok(());
            }
            self.update_info(chat, |info| {
                info.model = model_key_from_state(&state["model"]);
                info.session_name = state["sessionName"].as_str().map(str::to_string);
                info.session_id = state["sessionId"].as_str().map(str::to_string);
                info.message_count = state["messageCount"].as_u64().unwrap_or(0);
                info.is_streaming = state["isStreaming"].as_bool().unwrap_or(false);
            });
        }
        // Listing models can take a long time, so it loads in the background instead of holding up the tab.
        let (manager, loading, load_session) = (self.clone(), chat.clone(), session.clone());
        tokio::spawn(async move { manager.load_available_models(&loading, &load_session, start_no).await });

        if settings.auto_compaction {
            let _ = session
                .send(&json!({"type": "set_auto_compaction", "enabled": true}))
                .await;
        }
        if settings.auto_retry {
            let _ = session.send(&json!({"type": "set_auto_retry", "enabled": true})).await;
        }
        let _ = session
            .send(&json!({"type": "set_thinking_level", "level": settings.thinking_level}))
            .await;
        if let Ok(commands) = session.send(&json!({"type": "get_commands"})).await
            && lock(&chat.state).starts == start_no
        {
            lock(&chat.state).commands = commands["commands"].as_array().cloned();
            self.publish(chat);
        }
        if let Ok(messages) = session.send(&json!({"type": "get_messages"})).await
            && lock(&chat.state).starts == start_no
        {
            let list = visible(messages["messages"].as_array().map_or(&[][..], Vec::as_slice));
            let mut state = lock(&chat.state);
            state.info.message_count = user_count(&list);
            state.messages = list;
            drop(state);
            self.publish(chat);
        }
        Ok(())
    }

    async fn load_available_models(&self, chat: &Arc<Chat>, session: &Session, start_no: u64) {
        let models = session
            .send(&json!({"type": "get_available_models"}))
            .await
            .unwrap_or(Value::Null);
        if lock(&chat.state).starts != start_no {
            return;
        }
        let parsed = parse_model_list(&models);
        lock(&chat.state).available_models = Some(if parsed.is_empty() {
            local_model_catalog()
        } else {
            parsed
        });
        self.publish(chat);
    }

    fn spawn_event_pump(&self, chat: Arc<Chat>, start_no: u64, mut events: Events) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut last_sequence = 0u64;
            while let Some(event) = events.recv().await {
                if lock(&chat.state).starts != start_no {
                    return;
                }
                match event {
                    DaemonClientEvent::SessionEvent {
                        active_session_id,
                        event,
                        meta_sequence,
                    } => {
                        let ours = chat.session().is_some_and(|session| session.id() == active_session_id);
                        if !ours {
                            continue;
                        }
                        let gap = last_sequence != 0 && meta_sequence > last_sequence + 1;
                        if meta_sequence != 0 {
                            last_sequence = meta_sequence;
                        }
                        manager.handle_event(&chat, &event);
                        if gap {
                            manager.resync(&chat, start_no).await;
                        }
                    }
                    DaemonClientEvent::SessionClosed { active_session_id, .. }
                        if chat.session().is_some_and(|session| session.id() == active_session_id) =>
                    {
                        break;
                    }
                    DaemonClientEvent::DaemonClosing { .. } => break,
                    DaemonClientEvent::SideQuestionEvent {
                        active_session_id,
                        event,
                    } if chat.session().is_some_and(|session| session.id() == active_session_id) => {
                        manager.emit_event(&chat.id, "side_question_event", json!({ "event": event }));
                    }
                    _ => {}
                }
            }
            // The connection ended: the daemon went away or closed the session. The next command reconnects.
            if lock(&chat.state).starts == start_no {
                if let Some(session) = lock(&chat.session).take() {
                    session.close();
                }
                manager.update_info(&chat, |info| info.status = "stopped".into());
            }
        });
    }

    /// Events were missed (the sequence jumped): read the truth from the daemon.
    async fn resync(&self, chat: &Arc<Chat>, start_no: u64) {
        let Some(session) = chat.session() else { return };
        let (get_messages, get_state) = (json!({"type": "get_messages"}), json!({"type": "get_state"}));
        let (messages, state) = tokio::join!(session.send(&get_messages), session.send(&get_state));
        if lock(&chat.state).starts != start_no {
            return;
        }
        if let Ok(messages) = messages {
            let list = visible(messages["messages"].as_array().map_or(&[][..], Vec::as_slice));
            let mut guard = lock(&chat.state);
            guard.info.message_count = user_count(&list);
            guard.messages = list;
        }
        if let Ok(state) = state {
            lock(&chat.state).info.is_streaming = state["isStreaming"].as_bool().unwrap_or(false);
        }
        self.publish(chat);
    }

    /// A run's `agent_end` can be lost (dropped connection, worker restart), leaving a chat stuck on "working" with a Stop
    /// button that has nothing to stop. Chats that claim to be working but have been silent are checked against the daemon.
    fn spawn_run_watchdog(&self) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let Some(inner) = weak.upgrade() else { return };
                let manager = Manager { inner };
                let chats: Vec<_> = lock(&manager.inner.chats).values().cloned().collect();
                for chat in chats {
                    let stale = {
                        let state = lock(&chat.state);
                        (state.info.is_streaming || state.info.status == "working")
                            && state.last_event_at.elapsed() > Duration::from_secs(20)
                    };
                    if stale {
                        manager.sync_run_state(&chat).await;
                    }
                }
            }
        });
    }

    /// Corrects a chat's working/idle state from the daemon when events were missed.
    pub async fn sync_run_state(&self, chat: &Arc<Chat>) {
        let Some(session) = chat.session() else { return };
        let Ok(state) = session.send(&json!({"type": "get_state"})).await else {
            return;
        };
        let working = {
            let state = lock(&chat.state);
            state.info.is_streaming || state.info.status == "working"
        };
        if state["isStreaming"] == true || !working || self.chat(&chat.id).is_none() {
            return;
        }
        {
            let mut guard = lock(&chat.state);
            guard.info.is_streaming = false;
            guard.info.status = "idle".into();
            guard.info.retry = None;
            guard.last_event_at = Instant::now();
        }
        self.publish(chat);
        self.refresh_stats(chat);
    }

    // ---------- Events ----------

    fn handle_event(&self, chat: &Arc<Chat>, event: &Value) {
        let Some(kind) = event["type"].as_str().filter(|kind| !kind.is_empty()) else {
            return;
        };
        lock(&chat.state).last_event_at = Instant::now();
        let mut follow: Vec<Follow> = Vec::new();
        {
            let mut guard = lock(&chat.state);
            let state = &mut *guard;
            match kind {
                "agent_start" => {
                    state.info.status = "working".into();
                    state.info.is_streaming = true;
                }
                "agent_end" => {
                    state.info.status = "idle".into();
                    state.info.is_streaming = false;
                    state.info.retry = None;
                    // Prime Agent owns and drains its steering/follow-up queue. `session_action_update` is a read-only
                    // snapshot for display; replaying an item here would queue it again after every turn.
                    follow.push(Follow::RefreshStats);
                    follow.push(Follow::AutoName);
                    if let Some((provider, model_id)) = state.pending_model_switch.take() {
                        follow.push(Follow::SetModel(provider, model_id));
                    }
                    if std::mem::take(&mut state.pending_runtime_reload) {
                        follow.push(Follow::Reload);
                    }
                    follow.push(Follow::Completion);
                }
                "message_update" | "message_start" | "message_end" => {
                    if let Some(message) = event.get("message").filter(|m| m.is_object()) {
                        upsert_message(state, message);
                    }
                }
                "custom_message" => custom_message(state, event),
                "session_replaced" => {
                    state.info.extension_ui = None;
                    let list = visible(event["messages"].as_array().map_or(&[][..], Vec::as_slice));
                    apply_state(state, &event["state"], list);
                }
                "session_resynced" => {
                    state.info.extension_ui = None;
                    let list = visible(event["snapshot"]["messages"].as_array().map_or(&[][..], Vec::as_slice));
                    apply_state(state, &event["snapshot"]["state"], list);
                }
                "session_started" => {
                    state.info.extension_ui = None;
                    // A fresh session means the previous run is gone. Clear eagerly; agent_start/turn_start set these
                    // again if work actually resumes.
                    state.info.is_streaming = false;
                    state.info.status = "idle".into();
                    state.info.retry = None;
                }
                "turn_start" => state.info.status = "working".into(),
                "turn_end" => {
                    if let Some(message) = event.get("message").filter(|m| m.is_object()) {
                        upsert_message(state, message);
                    }
                    for result in event["toolResults"].as_array().into_iter().flatten() {
                        upsert_tool_result(state, result);
                    }
                    follow.push(Follow::RefreshStats);
                }
                "tool_execution_end" => {
                    // Only a finished call can carry this error, and serializing every streamed update of a large output
                    // is expensive.
                    if event["toolName"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case("ipython"))
                        && event
                            .get("result")
                            .unwrap_or(event)
                            .to_string()
                            .contains(r#"host request type \"agent_message.send\" is not available"#)
                    {
                        state.pending_runtime_reload = true;
                    }
                }
                "session_action_update" => {
                    let strings = |value: &Value| {
                        value
                            .as_array()
                            .map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect())
                            .unwrap_or_default()
                    };
                    state.queued.steer = strings(&event["actions"]["steering"]);
                    state.queued.follow_ups = strings(&event["actions"]["followUps"]);
                }
                "auto_retry_start" => {
                    state.info.retry = Some(Retry {
                        attempt: event["attempt"].as_u64().unwrap_or(0),
                        max_attempts: event["maxAttempts"].as_u64().unwrap_or(0),
                        error_message: event["errorMessage"].as_str().map(str::to_string),
                    });
                    follow.push(Follow::Toast(
                        "warning".into(),
                        format!("Retrying (attempt {})", event["attempt"].as_u64().unwrap_or(0)),
                    ));
                }
                "auto_retry_end" => {
                    state.info.retry = None;
                    // Exhausted retries must not die silently: surface why the run ended.
                    if event["success"] == false
                        && let Some(error) = event["finalError"].as_str().filter(|e| !e.trim().is_empty())
                    {
                        let short: String = error.chars().take(180).collect();
                        follow.push(Follow::Toast("error".into(), format!("Retries failed — {short}")));
                    }
                }
                "extension_ui_request" => extension_ui_request(state, &chat.id, event, &mut follow),
                "parse_error" => follow.push(Follow::Toast("error".into(), "Protocol parse error from agent".into())),
                _ => {}
            }
        }
        self.publish(chat);
        self.emit_event(&chat.id, kind, event.clone());
        for step in follow {
            self.follow(chat, step);
        }
    }

    fn follow(&self, chat: &Arc<Chat>, step: Follow) {
        match step {
            Follow::RefreshStats => self.refresh_stats(chat),
            Follow::AutoName => {
                let (manager, chat) = (self.clone(), chat.clone());
                tokio::spawn(async move { harness::auto_name(&manager, &chat).await });
            }
            Follow::SetModel(provider, model_id) => {
                let (manager, chat) = (self.clone(), chat.clone());
                tokio::spawn(async move {
                    let Some(session) = chat.session() else { return };
                    let command = json!({"type": "set_model", "provider": provider, "modelId": model_id});
                    match session.send(&command).await {
                        Ok(_) => {
                            let key = provider
                                .as_ref()
                                .map_or_else(|| model_id.clone(), |p| format!("{p}/{model_id}"));
                            manager.update_info(&chat, |info| info.model = Some(key));
                            manager.toast("success", &format!("Model switched to {model_id}"));
                        }
                        Err(_) => manager.toast("error", "Could not switch model after the task finished."),
                    }
                });
            }
            Follow::Reload => {
                let (manager, chat) = (self.clone(), chat.clone());
                tokio::spawn(async move {
                    let Some(session) = chat.session() else { return };
                    if session.send(&json!({"type": "reload"})).await.is_ok() {
                        manager.toast("info", "Agent messaging runtime refreshed. Retry the follow-up.");
                    }
                });
            }
            Follow::Completion => {
                let name = lock(&chat.state).info.name.clone();
                self.emit(Outbound::Completion {
                    agent_id: chat.id.clone(),
                    name,
                });
            }
            Follow::Toast(kind, text) => self.toast(&kind, &text),
            Follow::Notify(title, body) => {
                if self.settings().notifications {
                    self.toast("info", &format!("{title}: {body}"));
                    self.emit(Outbound::Notify { title, body });
                }
            }
        }
    }

    pub fn refresh_stats(&self, chat: &Arc<Chat>) {
        let (manager, chat) = (self.clone(), chat.clone());
        tokio::spawn(async move {
            let Some(session) = chat.session() else { return };
            if let Ok(stats) = session.send(&json!({"type": "get_session_stats"})).await {
                manager.apply_stats(&chat, stats);
            }
        });
    }

    fn apply_stats(&self, chat: &Arc<Chat>, stats: Value) {
        {
            let mut guard = lock(&chat.state);
            guard.info.tokens_in = stats["tokens"]["input"].as_u64().unwrap_or(0);
            guard.info.tokens_out = stats["tokens"]["output"].as_u64().unwrap_or(0);
            guard.info.cost = stats["cost"].as_f64().unwrap_or(0.0);
            guard.info.context_percent = stats["contextUsage"]["percent"].as_f64();
            guard.info.context_tokens = stats["contextUsage"]["tokens"].as_f64();
            guard.info.context_window = stats["contextUsage"]["contextWindow"].as_f64();
            guard.stats = Some(stats);
        }
        self.publish(chat);
    }

    /// Sends the chat's info to the renderer, but only when it changed: every daemon event ends here, including each
    /// streamed token, and an unchanged info would re-render the whole window.
    pub fn publish(&self, chat: &Arc<Chat>) {
        // A released slot can still deliver a late event; do not resurrect it.
        if !self.chat(&chat.id).is_some_and(|known| Arc::ptr_eq(&known, chat)) {
            return;
        }
        let info = lock(&chat.state).info.clone();
        let Ok(snapshot) = serde_json::to_string(&info) else {
            return;
        };
        {
            let mut published = lock(&self.inner.published);
            if published.get(&chat.id) == Some(&snapshot) {
                return;
            }
            published.insert(chat.id.clone(), snapshot);
        }
        self.emit_event(
            &chat.id,
            "agent_info",
            serde_json::to_value(&info).unwrap_or(Value::Null),
        );
    }

    // ---------- Commands ----------

    /// The session behind a chat, reconnecting if it dropped.
    pub async fn session_of(&self, agent_id: &str) -> Result<Session> {
        let chat = self.require(agent_id)?;
        self.ensure_connected(&chat).await?;
        chat.session().ok_or_else(|| anyhow!("Agent not connected"))
    }

    pub async fn run_command(&self, agent_id: &str, cmd: Value) -> Result<Value> {
        let kind = cmd["type"].as_str().unwrap_or("").to_string();
        let chat = self.chat(agent_id);
        if kind == "get_available_models" {
            if let Some(session) = chat.as_ref().and_then(|chat| chat.session())
                && let Ok(answer) = session.send(&cmd).await
            {
                let models = parse_model_list(&answer);
                if !models.is_empty() {
                    if let Some(chat) = &chat {
                        lock(&chat.state).available_models = Some(models.clone());
                    }
                    return Ok(json!({"models": models}));
                }
            }
            let cached = chat
                .as_ref()
                .and_then(|chat| lock(&chat.state).available_models.clone())
                .filter(|models| !models.is_empty());
            return Ok(json!({"models": cached.unwrap_or_else(local_model_catalog)}));
        }
        let chat = chat.ok_or_else(|| anyhow!("Agent not connected"))?;
        self.ensure_connected(&chat).await?;
        let session = chat.session().ok_or_else(|| anyhow!("Agent not connected"))?;
        if kind == "new_session" {
            // Replacing a busy chat's session would stop its run, so new_chat gives the new chat its own slot then.
            return Ok(json!({"agentId": self.new_chat(agent_id).await?}));
        }
        drop(chat.session_change.lock().await);
        let working = {
            let state = lock(&chat.state);
            state.info.is_streaming || state.info.status == "working"
        };
        // A steer or queued follow-up lands mid-run; a snapshot then would capture half-finished work as a "before" state.
        let mid_run = working || cmd["streamingBehavior"] == "steer";
        if kind == "prompt" && self.settings().checkpoints && !mid_run {
            self.create_checkpoint(&chat, "before-prompt", None).await;
        }
        match kind.as_str() {
            "set_model" => {
                let provider = cmd["provider"].as_str().map(str::to_string);
                let model_id = cmd["modelId"].as_str().map(str::to_string);
                match session.send(&cmd).await {
                    Ok(answer) => {
                        // The daemon echoes the model that is actually active now; trust it over what was requested.
                        let data = answer.get("data").unwrap_or(&answer);
                        let model = data.get("model").unwrap_or(data);
                        let actual_provider = model["provider"].as_str().map(str::to_string).or(provider);
                        let actual_id = model["id"].as_str().map(str::to_string).or(model_id);
                        if let Some(id) = actual_id {
                            let key = actual_provider.map_or_else(|| id.clone(), |p| format!("{p}/{id}"));
                            lock(&chat.state).info.model = Some(key);
                        }
                        lock(&chat.state).pending_model_switch = None;
                        self.publish(&chat);
                        Ok(answer)
                    }
                    Err(error) => {
                        // A run in flight may hold its model until it finishes. Keep the request and apply it on
                        // agent_end instead of dropping it silently.
                        if working {
                            if let Some(id) = model_id {
                                lock(&chat.state).pending_model_switch = Some((provider, id));
                            }
                            self.toast("info", "Model will switch when the current task finishes.");
                            return Ok(json!({"queued": true}));
                        }
                        Err(error)
                    }
                }
            }
            "abort" => {
                // Aborting a run that already ended produces no agent_end, so confirm the outcome with the daemon.
                let result = session.send(&cmd).await;
                let (manager, chat) = (self.clone(), chat.clone());
                tokio::spawn(async move {
                    manager.sync_run_state(&chat).await;
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    manager.sync_run_state(&chat).await;
                });
                result
            }
            _ => session.send(&cmd).await,
        }
    }

    /// The whole history. `chat.messages` mirrors the daemon's context, which drops everything before a compaction, so
    /// the full history comes from the session file, plus whatever is live but not written yet.
    pub async fn get_messages(&self, agent_id: &str) -> Vec<Value> {
        let Some(chat) = self.chat(agent_id) else {
            return Vec::new();
        };
        let session_id = lock(&chat.state).info.session_id.clone();
        let live = lock(&chat.state).messages.clone();
        let Some(file) = session_id.and_then(|id| sessions::session_file_for(&id)) else {
            return live;
        };
        let history = tokio::task::spawn_blocking(move || super::transcript::read_session_transcript(&file))
            .await
            .ok()
            .flatten();
        let Some(history) = history.filter(|history| !history.is_empty()) else {
            return live;
        };
        let saved: std::collections::HashSet<String> = history.iter().map(message_key).collect();
        let extra = live
            .into_iter()
            .filter(|message| message["role"] != "compactionSummary" && !saved.contains(&message_key(message)));
        visible(&history.iter().cloned().chain(extra).collect::<Vec<_>>())
    }

    pub async fn get_stats(&self, agent_id: &str) -> Value {
        let Some(chat) = self.chat(agent_id) else {
            return Value::Null;
        };
        if let Some(session) = chat.session()
            && let Ok(stats) = session.send(&json!({"type": "get_session_stats"})).await
        {
            self.apply_stats(&chat, stats);
        }
        lock(&chat.state).stats.clone().unwrap_or(Value::Null)
    }

    pub async fn get_commands(&self, agent_id: &str) -> Vec<Value> {
        let Some(chat) = self.chat(agent_id) else {
            return Vec::new();
        };
        if let Some(commands) = lock(&chat.state).commands.clone() {
            return commands;
        }
        let Some(session) = chat.session() else {
            return Vec::new();
        };
        let commands = session
            .send(&json!({"type": "get_commands"}))
            .await
            .ok()
            .and_then(|answer| answer["commands"].as_array().cloned())
            .unwrap_or_default();
        lock(&chat.state).commands = Some(commands.clone());
        self.publish(&chat);
        commands
    }

    pub async fn new_session(&self, agent_id: &str) -> Result<Value> {
        let chat = self.require(agent_id)?;
        let session = chat.session().ok_or_else(|| anyhow!("Agent not connected"))?;
        let answer = session.send(&json!({"type": "new_session"})).await?;
        lock(&chat.state).messages.clear();
        // Replacing the session orphans the previous run: its agent_end never arrives, so streaming state is re-synced
        // from the daemon here or the composer stays stuck on "working" in the fresh chat.
        let state = session.send(&json!({"type": "get_state"})).await.unwrap_or(Value::Null);
        let session_id = state["sessionId"].as_str().map(str::to_string);
        self.update_info(&chat, |info| {
            info.session_id = session_id.clone();
            info.session_name = state["sessionName"].as_str().map(str::to_string);
            info.message_count = 0;
            info.is_streaming = state["isStreaming"].as_bool().unwrap_or(false);
            info.status = if info.is_streaming { "working" } else { "idle" }.into();
            info.retry = None;
        });
        self.emit_event(agent_id, "session_started", json!({"sessionId": session_id}));
        Ok(answer)
    }

    pub async fn resume_session(&self, agent_id: &str, session_path: &str) -> Result<Value> {
        let chat = self.require(agent_id)?;
        let session = chat.session().ok_or_else(|| anyhow!("Agent not connected"))?;
        let cwd = chat.path();
        let switched = session
            .call(
                "switch_session",
                json!({"sessionPath": session_path, "cwdOverride": cwd}),
            )
            .await;
        let answer = match switched {
            Ok(answer) => answer,
            // The chat is still running in the daemon from before the app was closed. Take it over instead of refusing.
            Err(error) => match active_elsewhere(&error.to_string()) {
                Some(running) => self.take_over(&chat, &session, &running).await?,
                None => return Err(error),
            },
        };
        let session = chat.session().ok_or_else(|| anyhow!("Agent not connected"))?;
        // Independent reads; one round trip instead of two.
        let (get_messages, get_state) = (json!({"type": "get_messages"}), json!({"type": "get_state"}));
        let (messages, state) = tokio::join!(session.send(&get_messages), session.send(&get_state));
        if let Ok(messages) = messages {
            lock(&chat.state).messages = visible(messages["messages"].as_array().map_or(&[][..], Vec::as_slice));
        }
        if let Ok(state) = state {
            let count = lock(&chat.state).messages.len() as u64;
            self.update_info(&chat, |info| {
                info.session_id = state["sessionId"]
                    .as_str()
                    .map(str::to_string)
                    .or(info.session_id.take());
                info.session_name = state["sessionName"].as_str().map(str::to_string);
                info.message_count = state["messageCount"].as_u64().unwrap_or(count);
                info.is_streaming = state["isStreaming"].as_bool().unwrap_or(false);
                info.status = if info.is_streaming { "working" } else { "idle" }.into();
                info.retry = None;
            });
        } else {
            self.publish(&chat);
        }
        self.emit_event(agent_id, "session_resumed", json!({"sessionPath": session_path}));
        self.refresh_stats(&chat);
        Ok(answer)
    }

    /// Makes the chat show the session that is already running in the daemon under `running`: attaches to it and lets go
    /// of the one the chat had, which is stopped, as it is the empty one the app started or an idle chat. The chats are
    /// files, so nothing is lost.
    async fn take_over(&self, chat: &Arc<Chat>, current: &Session, running: &str) -> Result<Value> {
        if running == current.id() {
            // The chat already shows that session.
            return Ok(json!({"takenOver": running}));
        }
        if self
            .chats()
            .iter()
            .any(|other| other.id != chat.id && other.session().is_some_and(|session| session.id() == running))
        {
            bail!("That chat is already open in another chat window of this project.");
        }
        attach(current.client(), running).await?;
        let taken = Session::new(current.client().clone(), running.to_string());
        *lock(&chat.session) = Some(taken);
        let released = current.clone();
        tokio::spawn(async move {
            let _ = released.call("kill", json!({})).await;
        });
        Ok(json!({"takenOver": running}))
    }

    pub async fn delete_session(&self, agent_id: &str, session_path: &str) -> Result<Vec<sessions::SessionSummary>> {
        let chat = self.require(agent_id)?;
        let session = chat.session().ok_or_else(|| anyhow!("Agent not connected"))?;
        let project = chat.path();
        let list = |project: PathBuf| async move {
            tokio::task::spawn_blocking(move || sessions::list_sessions(Some(&project)))
                .await
                .unwrap_or_default()
        };
        let Some(target) = list(project.clone())
            .await
            .into_iter()
            .find(|s| s.session_file == session_path)
        else {
            return Ok(list(project).await);
        };
        // The chat may be live in another parallel slot of this project.
        let slot = self.tab_chats(&chat.tab_id).into_iter().find(|other| {
            other.id != chat.id && lock(&other.state).info.session_id.as_deref() == Some(target.session_id.as_str())
        });
        if let Some(slot) = slot {
            if !slot.is_idle() {
                bail!("This chat is still running. Stop it before deleting.");
            }
            if slot.id == primary_agent_id(&slot.tab_id) {
                self.new_session(&slot.id).await?;
            } else {
                self.close_chat(&slot.id).await;
            }
        }
        let was_active = lock(&chat.state).info.session_id.as_deref() == Some(target.session_id.as_str());
        if was_active {
            self.new_session(agent_id).await?;
        }
        let deleted = session
            .call("delete_saved_session", json!({"sessionPath": session_path}))
            .await;
        let failed = match &deleted {
            Ok(result) if result["ok"] == false => Some(
                result["error"]
                    .as_str()
                    .unwrap_or("Could not delete session")
                    .to_string(),
            ),
            Err(error) => Some(error.to_string()),
            Ok(_) => None,
        };
        if let Some(message) = failed {
            if was_active {
                let _ = self.resume_session(agent_id, session_path).await;
            }
            bail!(message);
        }
        Ok(list(project).await)
    }

    // ---------- Checkpoints (approval and rollback) ----------

    fn primary_of(&self, chat: &Arc<Chat>) -> Arc<Chat> {
        self.chat(&primary_agent_id(&chat.tab_id))
            .unwrap_or_else(|| chat.clone())
    }

    /// Snapshots the project so a turn can be undone. Checkpoints belong to the project, so every parallel chat shares one
    /// list. `None` when the folder is not a git repository or the snapshot failed.
    pub async fn create_checkpoint(
        &self,
        chat: &Arc<Chat>,
        label: &str,
        keep: Option<&str>,
    ) -> Option<crate::git::Checkpoint> {
        let primary = self.primary_of(chat);
        let _one_at_a_time = primary.checkpoint_lock.lock().await;
        let mut history = std::mem::take(&mut lock(&primary.state).checkpoints);
        let made = crate::git::create_checkpoint(
            &primary.path(),
            &primary.id,
            label,
            keep,
            &mut history,
            &crate::git::checkpoint_dir(),
        )
        .await;
        lock(&primary.state).checkpoints = history;
        made
    }

    /// The project's checkpoints. With no chat open they come from what was stored on disk.
    pub async fn list_checkpoints(&self, agent_id: &str) -> Vec<crate::git::Checkpoint> {
        match self.chat(agent_id) {
            Some(chat) => lock(&self.primary_of(&chat).state).checkpoints.clone(),
            None => crate::git::list_stored_checkpoints(&crate::git::checkpoint_dir(), agent_id).await,
        }
    }

    /// Puts the project back to a checkpoint, after saving the present state as a checkpoint of its own.
    pub async fn restore_checkpoint(&self, agent_id: &str, sha: &str) -> Result<()> {
        let chat = self.require(agent_id)?;
        let primary = self.primary_of(&chat);
        let _one_at_a_time = primary.checkpoint_lock.lock().await;
        let mut history = std::mem::take(&mut lock(&primary.state).checkpoints);
        let restored = crate::git::restore_checkpoint(
            &primary.path(),
            &primary.id,
            sha,
            &mut history,
            &crate::git::checkpoint_dir(),
        )
        .await;
        lock(&primary.state).checkpoints = history;
        restored
    }

    /// Records the model choice on every connected chat.
    pub async fn set_model_pref(&self, model: Option<String>) {
        let Some(model) = model else { return };
        let (provider, id) = match model.split_once('/') {
            Some((provider, id)) => (Some(provider.to_string()), id.to_string()),
            None => (None, model),
        };
        let chats: Vec<_> = lock(&self.inner.chats).values().cloned().collect();
        for chat in chats {
            if let Some(session) = chat.session() {
                let _ = session
                    .send(&json!({"type": "set_model", "provider": provider, "modelId": id}))
                    .await;
            }
        }
    }

    /// Drops cached catalogs and asks every resident chat to reload, so changes like a newly added custom model become
    /// selectable without a restart.
    pub async fn reload_agents(&self) {
        let chats: Vec<_> = lock(&self.inner.chats).values().cloned().collect();
        for chat in chats {
            {
                let mut state = lock(&chat.state);
                state.skills.clear();
                state.available_models = None;
            }
            if let Some(session) = chat.session() {
                let _ = session.send(&json!({"type": "reload"})).await;
            }
        }
        // Model pickers refetch once every runtime has re-read models.json.
        self.emit_event("", "models_changed", json!({}));
    }

    /// Answers an open extension dialog.
    pub async fn respond_dialog(&self, dialog_id: &str, value: Value, cancelled: bool) -> Result<()> {
        let owner = lock(&self.inner.chats)
            .values()
            .find(|chat| lock(&chat.state).dialogs.contains_key(dialog_id))
            .cloned();
        let Some(chat) = owner else { return Ok(()) };
        lock(&chat.state).dialogs.remove(dialog_id);
        self.publish(&chat);
        let Some(session) = chat.session() else { return Ok(()) };
        let mut response = json!({"type": "extension_ui_response", "id": dialog_id});
        if cancelled {
            response["cancelled"] = json!(true);
        } else {
            response["value"] = value;
        }
        session.send(&response).await.map(|_| ())
    }

    pub fn dialogs(&self, agent_id: &str) -> Vec<Value> {
        self.chat(agent_id)
            .map(|chat| lock(&chat.state).dialogs.values().cloned().collect())
            .unwrap_or_default()
    }
}

fn blank_state(id: &str, tab_id: &str, path: &std::path::Path) -> ChatState {
    ChatState {
        info: AgentInfo {
            id: id.to_string(),
            tab_id: tab_id.to_string(),
            name: base_name(&path.to_string_lossy()),
            path: path.to_string_lossy().into_owned(),
            status: "starting".into(),
            model: None,
            thinking_level: None,
            message_count: 0,
            cost: 0.0,
            tokens_in: 0,
            tokens_out: 0,
            context_percent: None,
            context_tokens: None,
            context_window: None,
            is_streaming: false,
            session_name: None,
            session_id: None,
            retry: None,
            extension_ui: None,
        },
        messages: Vec::new(),
        dialogs: HashMap::new(),
        queued: Queued::default(),
        commands: None,
        available_models: None,
        stats: None,
        skills: Vec::new(),
        last_event_at: Instant::now(),
        pending_runtime_reload: false,
        pending_model_switch: None,
        starts: 0,
        titling: false,
        title_attempts: HashMap::new(),
        checkpoints: Vec::new(),
    }
}

/// Work to do after the state lock is released.
enum Follow {
    RefreshStats,
    AutoName,
    SetModel(Option<String>, String),
    Reload,
    Completion,
    Toast(String, String),
    Notify(String, String),
}

fn upsert_message(state: &mut ChatState, message: &Value) {
    if is_internal_state_restore_message(message) {
        let key = message_key(message);
        state.messages.retain(|existing| message_key(existing) != key);
        return;
    }
    if let Some(entries) = message["entries"].as_array() {
        for entry in entries.iter().filter(|entry| !is_internal_state_restore_message(entry)) {
            let key = message_key(entry);
            match state.messages.iter().position(|existing| message_key(existing) == key) {
                Some(index) => state.messages[index] = entry.clone(),
                None => state.messages.push(entry.clone()),
            }
        }
        state.info.message_count = user_count(&state.messages);
        return;
    }
    // Streaming updates almost always target the newest message, so search from the end and only recount when a
    // message is actually added.
    let key = message_key(message);
    match state.messages.iter().rposition(|existing| message_key(existing) == key) {
        Some(index) => state.messages[index] = message.clone(),
        None => {
            state.messages.push(message.clone());
            state.info.message_count = user_count(&state.messages);
        }
    }
}

fn upsert_tool_result(state: &mut ChatState, result: &Value) {
    let id = &result["toolCallId"];
    match state.messages.iter().position(|existing| &existing["toolCallId"] == id) {
        Some(index) => state.messages[index] = result.clone(),
        None => state.messages.push(result.clone()),
    }
}

fn apply_state(state: &mut ChatState, daemon_state: &Value, messages: Vec<Value>) {
    state.messages = messages;
    state.info.session_id = daemon_state["sessionId"]
        .as_str()
        .map(str::to_string)
        .or(state.info.session_id.take());
    state.info.session_name = daemon_state["sessionName"].as_str().map(str::to_string);
    state.info.is_streaming = daemon_state["isStreaming"].as_bool().unwrap_or(false);
    state.info.message_count = user_count(&state.messages);
}

fn custom_message(state: &mut ChatState, event: &Value) {
    if event["display"] == false || event["customType"] == "ipython_state_restored" {
        return;
    }
    let custom_type = event["customType"].as_str().unwrap_or("");
    match custom_type {
        "agent_message" | "refinement_outcome" => {
            let mut message = event.clone();
            message["role"] = json!("assistant");
            upsert_message(state, &message);
        }
        "rlm_child_failure"
        | "rlm_child_terminal_notice"
        | "mcp_connection_outcome"
        | "session_slash_command"
        | "session_slash_command_result"
        | "compaction_outcome" => {
            let stamp = match &event["timestamp"] {
                Value::Null => unix_ms().to_string(),
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let mut message = event.clone();
            message["role"] = json!("system");
            message["id"] = json!(format!("sys-{custom_type}-{stamp}"));
            message["content"] = json!(event["content"].as_str().unwrap_or(""));
            upsert_message(state, &message);
        }
        _ => {}
    }
}

fn extension_ui_request(state: &mut ChatState, agent_id: &str, event: &Value, follow: &mut Vec<Follow>) {
    let method = event["method"].as_str().unwrap_or("");
    let text = |key: &str| event[key].as_str().map(str::to_string);
    if ["confirm", "select", "input", "editor"].contains(&method) {
        let id = text("id").unwrap_or_default();
        let title = text("title").unwrap_or_else(|| method.to_string());
        let mut dialog = json!({"id": id, "agentId": agent_id, "method": method, "title": title});
        for (key, field) in [("message", "message"), ("prefill", "prefill")] {
            if let Some(value) = text(field) {
                dialog[key] = json!(value);
            }
        }
        if let Some(options) = event["options"].as_array() {
            dialog["options"] = json!(options.iter().filter_map(Value::as_str).collect::<Vec<_>>());
        }
        state.dialogs.insert(id, dialog);
        let name = state.info.name.clone();
        follow.push(Follow::Notify(name, format!("Agent needs input: {title}")));
        return;
    }
    if method == "notify" {
        let kind = if event["notifyType"] == "error" {
            "error"
        } else {
            "info"
        };
        follow.push(Follow::Toast(kind.into(), text("message").unwrap_or_default()));
        return;
    }
    let mut ui = state.info.extension_ui.take().unwrap_or_default();
    match method {
        "setStatus" => {
            let key = text("statusKey").unwrap_or_else(|| "extension".into());
            match text("statusText") {
                Some(value) => ui.statuses.insert(key, value),
                None => ui.statuses.remove(&key),
            };
        }
        "setWidget" => {
            let key = text("widgetKey").unwrap_or_else(|| "extension".into());
            match event["widgetLines"].as_array() {
                Some(lines) => {
                    let lines: Vec<String> = lines
                        .iter()
                        .map(|line| line.as_str().map_or_else(|| line.to_string(), str::to_string))
                        .collect();
                    let placement = if event["widgetPlacement"] == "belowEditor" {
                        "belowEditor"
                    } else {
                        "aboveEditor"
                    };
                    ui.widgets.insert(key, json!({"lines": lines, "placement": placement}));
                }
                None => {
                    ui.widgets.remove(&key);
                }
            }
        }
        "setTitle" => ui.title = text("title"),
        "setWorkingMessage" => ui.working_message = text("message"),
        "setEditorText" | "set_editor_text" => ui.editor_text = Some(text("text").unwrap_or_default()),
        _ => {}
    }
    state.info.extension_ui = Some(ui);
}

/// The models `models.json` offers, for when the daemon lists none.
/// The running session a `switch_session` was refused for ("Session is already active in ac4d…: <path>").
fn active_elsewhere(message: &str) -> Option<String> {
    static SHAPE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"Session is already active in ([A-Za-z0-9_-]+)").unwrap());
    SHAPE.captures(message).map(|found| found[1].to_string())
}

pub fn local_model_catalog() -> Vec<Value> {
    let file = crate::platform::prime_agent_dir().join("models.json");
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|raw| parse_model_list(&raw))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_switch_names_the_session_that_is_running() {
        let refusal = "the daemon rejected the switch_session request: Session is already active in ac4d46b03d54: \\\\?\\C:\\x\\s.jsonl";
        assert_eq!(active_elsewhere(refusal).as_deref(), Some("ac4d46b03d54"));
        assert_eq!(active_elsewhere("Session not found"), None);
    }

    fn state() -> ChatState {
        blank_state("agent-t", "t", std::path::Path::new("/work/proj"))
    }

    #[test]
    fn retry_delays_double_to_two_minutes() {
        let seconds: Vec<u64> = (1..=6).map(|n| start_retry_delay(n).as_secs()).collect();
        assert_eq!(seconds, [15, 30, 60, 120, 120, 120]);
        assert_eq!(start_retry_delay(0).as_secs(), 15);
        assert_eq!(start_retry_delay(1000).as_secs(), 120);
    }

    #[test]
    fn messages_are_keyed_by_id_or_by_time_and_role() {
        assert_eq!(message_key(&json!({"id": "a", "role": "user"})), "id:a");
        assert_eq!(message_key(&json!({"timestamp": 5, "role": "user"})), "ts:5:user");
        assert_eq!(message_key(&json!({"role": "assistant"})), "ts::assistant");
    }

    #[test]
    fn streaming_updates_replace_the_newest_message_and_new_ones_are_counted() {
        let mut s = state();
        upsert_message(&mut s, &json!({"id": "u1", "role": "user", "content": "hi"}));
        upsert_message(&mut s, &json!({"id": "a1", "role": "assistant", "content": "he"}));
        upsert_message(&mut s, &json!({"id": "a1", "role": "assistant", "content": "hello"}));
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[1]["content"], "hello");
        assert_eq!(s.info.message_count, 1);
        upsert_message(&mut s, &json!({"id": "u2", "role": "user", "content": "again"}));
        assert_eq!(s.info.message_count, 2);
        upsert_tool_result(
            &mut s,
            &json!({"toolCallId": "c1", "role": "toolResult", "content": "one"}),
        );
        upsert_tool_result(
            &mut s,
            &json!({"toolCallId": "c1", "role": "toolResult", "content": "two"}),
        );
        assert_eq!(s.messages.iter().filter(|m| m["toolCallId"] == "c1").count(), 1);
    }

    #[test]
    fn internal_messages_are_dropped_not_shown() {
        let mut s = state();
        upsert_message(
            &mut s,
            &json!({"id": "x", "role": "assistant", "content": "<ipython_state_restored>"}),
        );
        assert!(s.messages.is_empty());
        upsert_message(&mut s, &json!({"id": "a", "role": "assistant", "content": "ok"}));
        upsert_message(
            &mut s,
            &json!({"id": "a", "role": "assistant", "display": false, "customType": "ipython_state"}),
        );
        assert!(
            s.messages.is_empty(),
            "an update that turns a message internal removes it"
        );
    }

    #[test]
    fn extension_ui_requests_open_dialogs_or_update_the_ui() {
        let mut s = state();
        let mut follow = Vec::new();
        extension_ui_request(
            &mut s,
            "agent-t",
            &json!({"id": "d1", "method": "confirm", "title": "Sure?", "options": ["yes", "no"]}),
            &mut follow,
        );
        assert_eq!(s.dialogs["d1"]["title"], "Sure?");
        assert_eq!(s.dialogs["d1"]["options"], json!(["yes", "no"]));
        assert!(matches!(follow.last(), Some(Follow::Notify(_, text)) if text.contains("Sure?")));
        extension_ui_request(
            &mut s,
            "agent-t",
            &json!({"method": "setStatus", "statusKey": "k", "statusText": "busy"}),
            &mut follow,
        );
        extension_ui_request(
            &mut s,
            "agent-t",
            &json!({"method": "setWidget", "widgetKey": "w", "widgetLines": ["a", "b"], "widgetPlacement": "belowEditor"}),
            &mut follow,
        );
        let ui = s.info.extension_ui.clone().unwrap();
        assert_eq!(ui.statuses["k"], "busy");
        assert_eq!(ui.widgets["w"]["placement"], "belowEditor");
        extension_ui_request(
            &mut s,
            "agent-t",
            &json!({"method": "setStatus", "statusKey": "k"}),
            &mut follow,
        );
        assert!(
            s.info.extension_ui.clone().unwrap().statuses.is_empty(),
            "a status with no text is removed"
        );
    }

    #[test]
    fn custom_messages_become_assistant_or_system_rows() {
        let mut s = state();
        custom_message(
            &mut s,
            &json!({"customType": "agent_message", "content": "hello", "timestamp": 1}),
        );
        assert_eq!(s.messages[0]["role"], "assistant");
        custom_message(
            &mut s,
            &json!({"customType": "compaction_outcome", "content": "done", "timestamp": 2}),
        );
        assert_eq!(s.messages[1]["role"], "system");
        assert_eq!(s.messages[1]["id"], "sys-compaction_outcome-2");
        custom_message(&mut s, &json!({"customType": "agent_message", "display": false}));
        custom_message(&mut s, &json!({"customType": "something_else", "content": "x"}));
        assert_eq!(s.messages.len(), 2);
    }

    #[test]
    fn the_daemons_autonomous_block_is_built_from_the_settings() {
        let settings = Settings::from_value(
            &json!({"autonomous": {"enabled": true, "gates": ["make test"], "gateRetries": 3, "maxSeconds": 60}}),
        );
        let daemon = settings.daemon_autonomous();
        assert_eq!(daemon["enabled"], true);
        assert_eq!(daemon["timeoutMs"], 60_000);
        assert_eq!(daemon["gates"]["commands"], json!(["make test"]));
        assert_eq!(daemon["gates"]["maxRetries"], 3);
        assert_eq!(daemon["maxTurns"], 30, "missing fields take the defaults");
    }
}

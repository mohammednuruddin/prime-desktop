//! One daemon session: the app's chat, as Prime Agent's daemon sees it.
//!
//! Commands are built as JSON and parsed into Prime Agent's own `DaemonCommand` type before they are sent, so a field
//! the daemon would not accept fails here with a message instead of on the wire.

use anyhow::{Context, Result, anyhow, bail};
use pa_tui::daemon_client::DaemonClient;
use pa_types::daemon::DaemonCommand;
use serde_json::{Map, Value, json};

/// The `data` of a response that must have succeeded.
pub fn data_of(command: &str, response: pa_types::daemon::DaemonResponse) -> Result<Value> {
    if !response.success {
        bail!("{}", response.error.unwrap_or_else(|| format!("{command} failed")));
    }
    Ok(response.data.unwrap_or(Value::Null))
}

#[derive(Clone)]
pub struct Session {
    client: DaemonClient,
    id: String,
}

fn text<'a>(command: &'a Value, key: &str) -> &'a str {
    command.get(key).and_then(Value::as_str).unwrap_or("")
}

impl Session {
    pub fn new(client: DaemonClient, id: String) -> Self {
        Self { client, id }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn client(&self) -> &DaemonClient {
        &self.client
    }

    pub fn supports(&self, capability: &str) -> bool {
        self.client.supports_server_capability(capability)
    }

    pub fn close(&self) {
        self.client.close();
    }

    /// One command to this session. Null fields are dropped, as an undefined field is in the TypeScript client.
    pub async fn call(&self, kind: &str, fields: Value) -> Result<Value> {
        let mut command = Map::new();
        command.insert("type".into(), kind.into());
        command.insert("activeSessionId".into(), self.id.clone().into());
        if let Value::Object(extra) = fields {
            command.extend(extra.into_iter().filter(|(_, value)| !value.is_null()));
        }
        let command: DaemonCommand = serde_json::from_value(Value::Object(command))
            .with_context(|| format!("`{kind}` is not a command this Prime Agent build accepts"))?;
        self.client.request_ok(command).await
    }

    /// A command outside any one session (listing, shutdown): the supervisor's.
    pub async fn control(&self, command: Value) -> Result<Value> {
        let command: DaemonCommand = serde_json::from_value(command).context("not a daemon command")?;
        self.client.request_ok(command).await
    }

    async fn provider_of(&self, model_id: &str) -> Result<String> {
        let models = self.call("get_available_models", json!({})).await?;
        let found = models["models"]
            .as_array()
            .and_then(|list| list.iter().find(|model| model["id"] == model_id))
            .and_then(|model| model["provider"].as_str());
        found
            .map(str::to_string)
            .ok_or_else(|| anyhow!("A provider and model are required."))
    }

    /// The renderer's agent commands, translated to daemon commands. Answers keep the shapes the TypeScript transport
    /// returned, because the renderer reads them.
    pub async fn send(&self, cmd: &Value) -> Result<Value> {
        let kind = text(cmd, "type");
        let message = text(cmd, "message");
        let images = cmd.get("images").cloned().unwrap_or(Value::Null);
        Ok(match kind {
            "get_state" => self.call("get_connection_state", json!({})).await?,
            "get_messages" => json!({ "messages": self.call("get_messages", json!({})).await?["messages"] }),
            "get_commands" => json!({ "commands": self.call("get_commands", json!({})).await?["commands"] }),
            "get_available_models" => {
                json!({ "models": self.call("get_available_models", json!({})).await?["models"] })
            }
            "get_session_stats" => self.call("get_session_stats", json!({})).await?,
            "prompt" => {
                let behavior = cmd.get("streamingBehavior").cloned().unwrap_or_else(|| json!("followUp"));
                self.call("prompt", json!({"message": message, "images": images, "streamingBehavior": behavior})).await?
            }
            "steer" => self.call("steer", json!({"message": message, "images": images})).await?,
            "follow_up" => self.call("follow_up", json!({"message": message, "images": images})).await?,
            "abort" => self.call("abort", json!({})).await?,
            "abort_retry" => self.call("abort_retry", json!({})).await?,
            "compact" => self.call("compact", json!({"customInstructions": cmd.get("customInstructions")})).await?,
            "refine" => {
                let fields = json!({"instructions": cmd.get("instructions"), "rollbackId": cmd.get("rollbackId"), "global": cmd.get("global")});
                self.call("refine", fields).await?
            }
            "new_session" => self.call("new_session", json!({})).await?,
            "switch_session" => self.call("switch_session", json!({"sessionPath": text(cmd, "sessionPath")})).await?,
            "fork" | "clone" => {
                let tree = self.call("get_session_tree", json!({})).await?;
                let leaf = tree["leafId"].as_str().unwrap_or("");
                let entry = if kind == "fork" { cmd.get("entryId").and_then(Value::as_str).filter(|id| !id.is_empty()).unwrap_or(leaf) } else { leaf };
                if entry.is_empty() {
                    bail!("No message is available to {kind}.");
                }
                self.call("fork", json!({"entryId": entry})).await?
            }
            "set_model" => {
                let model_id = text(cmd, "modelId");
                let provider = match text(cmd, "provider") {
                    "" if !model_id.is_empty() => self.provider_of(model_id).await.unwrap_or_default(),
                    given => given.to_string(),
                };
                if provider.is_empty() || model_id.is_empty() {
                    bail!("A provider and model are required.");
                }
                self.call("set_model", json!({"provider": provider, "modelId": model_id})).await?
            }
            "set_thinking_level" => {
                let level = Some(text(cmd, "level")).filter(|level| !level.is_empty()).unwrap_or("medium");
                self.call("set_thinking_level", json!({"level": level})).await?
            }
            "set_auto_compaction" => self.call("set_auto_compaction", json!({"enabled": cmd["enabled"].as_bool().unwrap_or(false)})).await?,
            "set_auto_retry" => self.call("set_auto_retry", json!({"enabled": cmd["enabled"].as_bool().unwrap_or(false)})).await?,
            "set_session_name" => self.call("set_session_name", json!({"name": text(cmd, "name")})).await?,
            "bash" => self.call("execute_bash", json!({"command": text(cmd, "command")})).await?,
            "reload" => self.call("reload", json!({})).await?,
            "export_html" => json!({ "path": self.call("export_html", json!({"outputPath": cmd.get("outputPath")})).await?["path"] }),
            "get_fork_messages" => {
                json!({ "messages": self.call("get_user_messages_for_forking", json!({})).await?["messages"] })
            }
            "get_last_assistant_text" => json!({ "text": self.call("get_last_assistant_text", json!({})).await?["text"] }),
            "set_rlm_max_depth" => {
                let depth = cmd["maxDepth"].as_u64().ok_or_else(|| anyhow!("maxDepth must be a whole number"))?;
                self.call("set_rlm_max_depth", json!({"maxDepth": depth, "global": cmd["global"].as_bool().unwrap_or(false)})).await?
            }
            "get_rlm_max_depth_status" => self.call("get_rlm_max_depth_status", json!({})).await?,
            "list_schedules" => json!({ "jobs": self.call("cron_list", json!({"includeInactive": true})).await?["jobs"] }),
            "add_schedule" => {
                self.call("cron_add", json!({"schedule": text(cmd, "schedule"), "prompt": text(cmd, "prompt"), "promoteOwnedSession": false}))
                    .await?["job"]
                    .clone()
            }
            "cancel_schedule" => self.call("cron_cancel", json!({"jobId": text(cmd, "jobId")})).await?["job"].clone(),
            "get_heartbeat" => json!({ "heartbeat": self.call("heartbeat_get", json!({})).await?["heartbeat"] }),
            "set_heartbeat" => {
                let fields = json!({"schedule": text(cmd, "schedule"), "prompt": text(cmd, "prompt"), "deliveryMode": cmd.get("deliveryMode"), "promoteOwnedSession": false});
                self.call("heartbeat_set", fields).await?["heartbeat"].clone()
            }
            "update_heartbeat" => self.call("heartbeat_update", json!({"action": text(cmd, "action")})).await?["heartbeat"].clone(),
            "send_message" => {
                let command = json!({
                    "type": "send_message",
                    "targetActiveSessionId": text(cmd, "targetActiveSessionId"),
                    "message": message,
                    "fromActiveSessionId": self.id,
                });
                self.control(command).await?
            }
            "extension_ui_response" => {
                let mut response = cmd.as_object().cloned().unwrap_or_default();
                response.remove("type");
                response.remove("id");
                self.call("extension_ui_response", json!({"requestId": text(cmd, "id"), "response": response})).await?
            }
            other => bail!("Unsupported daemon transport command: {other}"),
        })
    }
}

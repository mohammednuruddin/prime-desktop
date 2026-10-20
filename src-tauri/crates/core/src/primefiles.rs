//! Prime Agent files the app reads and writes: auth.json, models.json, settings.json, the MCP catalog, the changelog.
//!
//! These live in Prime Agent's own folder and are shared with the Prime CLI, which can change them while the app runs.
//! So every read is fresh, every write is a read-modify-write of the whole file and goes through a temp file and a
//! rename (never a half-file), and a file that cannot be understood is never replaced by an empty one.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::store::{quarantine, write_atomic};

const PROVIDERS: [(&str, &str); 15] = [
    ("openai", "OpenAI"),
    ("openai-codex", "OpenAI Codex"),
    ("anthropic", "Anthropic"),
    ("google", "Google Gemini"),
    ("openrouter", "OpenRouter"),
    ("prime-inference", "Prime Inference"),
    ("xai", "xAI"),
    ("groq", "Groq"),
    ("mistral", "Mistral"),
    ("deepseek", "DeepSeek"),
    ("fireworks", "Fireworks"),
    ("cerebras", "Cerebras"),
    ("moonshotai", "Moonshot AI"),
    ("minimax", "MiniMax"),
    ("zai", "ZAI"),
];

const MODEL_ROLES: [&str; 3] = ["subagentDefaultModel", "auxiliaryModel", "providerBackupModel"];
const CUSTOM_MODEL_APIS: [&str; 4] = [
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
    "google-generative-ai",
];

/// Prime Agent only lists models whose provider has a credential, so a server that needs no key still gets a
/// placeholder; OpenAI-compatible servers ignore the resulting `Bearer none` header.
const KEYLESS_API_KEY: &str = "none";

const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const OPENROUTER_TIMEOUT: Duration = Duration::from_secs(30);
/// The catalog is a megabyte or two; this stops a wrong or hostile answer from filling memory.
const OPENROUTER_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The Prime Agent release ships its merged MCP service catalog beside the executable under this name.
pub const MCP_CATALOG_FILE: &str = "mcp-services.bundled.json";
const CHANGELOG_LIMIT: usize = 12_000;
const CHANGELOG_MISSING: &str = "Changelog not found. Installed Prime Agent package did not include CHANGELOG.md.";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuthProvider {
    pub id: String,
    pub name: String,
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomModelSummary {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomProviderSummary {
    pub provider: String,
    pub base_url: String,
    pub api: String,
    pub has_key: bool,
    pub models: Vec<CustomModelSummary>,
}

/// What the "add a model provider" form sends.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomProviderInput {
    pub provider: String,
    pub base_url: String,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    pub models: Vec<CustomModelInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomModelInput {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// One entry of the MCP service picker.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpService {
    pub id: String,
    pub name: String,
    pub description: String,
    pub auth: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpServerInput {
    Http {
        url: String,
        oauth: bool,
        enabled: bool,
        bearer_token_env_var: Option<String>,
    },
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        enabled: bool,
    },
}

/// The Prime Agent folder (`~/.prime/agent`, or wherever `PRIME_AGENT_CODING_AGENT_DIR` points). Every method is
/// blocking file work; async callers run it on a blocking thread.
pub struct PrimeFiles {
    dir: PathBuf,
    /// Serializes this app's own read-modify-writes, so two calls cannot overwrite each other's change. The Prime CLI
    /// is another process and is not covered; the window is the few milliseconds between a read and its rename.
    write_lock: Mutex<()>,
}

impl PrimeFiles {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    /// The folder Prime Agent itself uses on this machine.
    pub fn from_env() -> Self {
        Self::new(crate::platform::prime_agent_dir())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Prime Agent's log folder (`agent.jsonl` is in it).
    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_json(&self, name: &str, value: &Map<String, Value>, private: bool) -> Result<()> {
        let path = self.path(name);
        let text = serde_json::to_string_pretty(value)?;
        write_atomic(&path, text.as_bytes(), private).with_context(|| format!("Could not write {}", path.display()))
    }

    // ---------- auth.json ----------

    /// Every known provider and every other one in the file, with whether a credential is stored. Never the key.
    pub fn list_auth_providers(&self) -> Vec<AuthProvider> {
        let data = read_object(&self.path("auth.json"));
        let known = PROVIDERS.iter().map(|(id, name)| (id.to_string(), name.to_string()));
        let extra = data
            .keys()
            .filter(|id| !PROVIDERS.iter().any(|(known, _)| known == id))
            .map(|id| (id.clone(), id.clone()));
        known
            .chain(extra)
            .map(|(id, name)| AuthProvider {
                configured: data.get(&id).is_some_and(truthy),
                id,
                name,
            })
            .collect()
    }

    pub fn set_auth_key(&self, provider: &str, key: &str) -> Result<()> {
        let _guard = self.lock();
        let mut data = read_for_update(&self.path("auth.json"))?;
        data.insert(provider.into(), json!({ "type": "api_key", "key": key.trim() }));
        self.write_json("auth.json", &data, true)
    }

    pub fn remove_auth(&self, provider: &str) -> Result<()> {
        let _guard = self.lock();
        let mut data = read_for_update(&self.path("auth.json"))?;
        data.shift_remove(provider);
        self.write_json("auth.json", &data, true)
    }

    // ---------- settings.json ----------

    /// For display: a missing, unreadable or corrupt file is empty.
    pub fn read_prime_settings(&self) -> Map<String, Value> {
        read_object(&self.path("settings.json"))
    }

    /// For read-modify-write: a missing file is empty, but an unreadable or corrupt one stops the write instead of
    /// being replaced with `{}`.
    fn read_settings_strict(&self) -> Result<Map<String, Value>> {
        let path = self.path("settings.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => bail!("Could not read Prime Agent settings ({}): {error}", path.display()),
        };
        match serde_json::from_str::<Value>(strip_bom(&text)) {
            Ok(Value::Object(settings)) => Ok(settings),
            Ok(_) => bail!("Prime Agent settings ({}) must be a JSON object.", path.display()),
            Err(_) => bail!(
                "Prime Agent settings ({}) are not valid JSON. Fix or remove the file and try again.",
                path.display()
            ),
        }
    }

    /// Reads settings.json strictly, lets `change` edit it, writes it back.
    fn edit_settings<R>(&self, change: impl FnOnce(&mut Map<String, Value>) -> Result<R>) -> Result<R> {
        let _guard = self.lock();
        let mut settings = self.read_settings_strict()?;
        let result = change(&mut settings)?;
        // 0600 like Prime Agent's own writes: an MCP server's arguments can carry a token.
        self.write_json("settings.json", &settings, true)?;
        Ok(result)
    }

    pub fn model_roles(&self) -> Map<String, Value> {
        let settings = self.read_prime_settings();
        let text = |role: &str| Value::from(settings.get(role).and_then(Value::as_str).unwrap_or_default());
        MODEL_ROLES.iter().map(|role| (role.to_string(), text(role))).collect()
    }

    /// Sets (or, with an empty `value`, clears) one of the model roles, and returns all of them.
    pub fn set_model_role(&self, key: &str, value: &str) -> Result<Map<String, Value>> {
        if !MODEL_ROLES.contains(&key) {
            bail!("Unknown model role.");
        }
        if !value.is_empty() && (!is_model_selector(value) || value.chars().count() > 512) {
            bail!("Select a model with a provider and model ID.");
        }
        self.edit_settings(|settings| {
            if value.is_empty() {
                settings.shift_remove(key);
            } else {
                settings.insert(key.into(), value.into());
            }
            Ok(())
        })?;
        Ok(self.model_roles())
    }

    pub fn write_rlm_max_depth(&self, max_depth: i64) -> Result<()> {
        self.edit_settings(|settings| {
            settings.insert("rlmMaxDepth".into(), max_depth.into());
            Ok(())
        })
    }

    pub fn read_rlm_max_depth(&self, fallback: i64) -> i64 {
        let value = self.read_prime_settings().get("rlmMaxDepth").and_then(whole_number);
        value.filter(|depth| *depth >= 0).unwrap_or(fallback)
    }

    pub fn set_agent_traces_enabled(&self, enabled: bool) -> Result<()> {
        self.edit_settings(|settings| {
            let mut traces = settings
                .get("agentTraces")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            traces.insert("enabled".into(), enabled.into());
            settings.insert("agentTraces".into(), Value::Object(traces));
            Ok(())
        })
    }

    pub fn agent_traces_enabled(&self) -> bool {
        let settings = self.read_prime_settings();
        settings.get("agentTraces").and_then(|traces| traces.get("enabled")) == Some(&Value::Bool(true))
    }

    // ---------- MCP servers (settings.json) ----------

    /// The configured MCP servers, with only the fields the settings screen shows.
    pub fn mcp_servers(&self) -> Map<String, Value> {
        const SHOWN: [&str; 8] = [
            "type",
            "url",
            "command",
            "args",
            "cwd",
            "oauth",
            "enabled",
            "bearerTokenEnvVar",
        ];
        let settings = self.read_prime_settings();
        let shown = |server: &Value| {
            let fields = SHOWN.iter().filter_map(|field| {
                let value = server
                    .get(*field)
                    .filter(|value| *field != "args" || value.is_array())?;
                Some((field.to_string(), value.clone()))
            });
            Value::Object(fields.collect())
        };
        mcp_servers_of(&settings)
            .iter()
            .map(|(name, server)| (name.clone(), shown(server)))
            .collect()
    }

    /// Adds, replaces or (with `None`) removes one MCP server, and returns all of them.
    pub fn set_mcp_server(&self, name: &str, config: Option<McpServerInput>) -> Result<Map<String, Value>> {
        let key = name.trim();
        if !is_plain_name(key) {
            bail!("MCP server name may use letters, numbers, dots, dashes, and underscores.");
        }
        self.edit_settings(|settings| {
            let config = config.map(checked_mcp_config).transpose()?;
            let mut servers = mcp_servers_of(settings);
            match config {
                Some(server) => {
                    let server = server.merged_over(servers.get(key));
                    servers.insert(key.into(), server);
                }
                None => {
                    servers.shift_remove(key);
                }
            }
            settings.insert("mcpServers".into(), Value::Object(servers));
            Ok(())
        })?;
        Ok(self.mcp_servers())
    }

    // ---------- models.json: custom providers ----------

    pub fn custom_providers(&self) -> Vec<CustomProviderSummary> {
        let file = read_object(&self.path("models.json"));
        providers_of(&file)
            .iter()
            .map(|(id, config)| summarize_provider(id, config))
            .collect()
    }

    /// Fetches OpenRouter's model list with the stored key and merges it into the `openrouter` provider, keeping
    /// whatever was added to the models by hand.
    pub fn refresh_openrouter_models(&self) -> Result<()> {
        self.refresh_models_from(OPENROUTER_MODELS_URL)
    }

    fn refresh_models_from(&self, url: &str) -> Result<()> {
        let key = read_object(&self.path("auth.json"))
            .get("openrouter")
            .and_then(|entry| entry.get("key"))
            .and_then(Value::as_str)
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .context("Add an OpenRouter API key before refreshing models.")?;
        // No redirects: the key must go to this host and nowhere else.
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(OPENROUTER_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .build();
        let mut response = ureq::Agent::new_with_config(config)
            .get(url)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {key}"))
            .call()
            .map_err(|error| anyhow!("OpenRouter model refresh failed ({error})."))?;
        if !response.status().is_success() {
            bail!("OpenRouter model refresh failed ({}).", response.status().as_u16());
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(OPENROUTER_MAX_BYTES)
            .read_to_string()
            .map_err(|error| anyhow!("OpenRouter model refresh failed ({error})."))?;
        let payload = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
        let Some(listed) = payload.get("data").and_then(Value::as_array) else {
            bail!("OpenRouter returned an invalid model catalog.");
        };

        let _guard = self.lock();
        let mut file = read_for_update(&self.path("models.json"))?;
        let mut providers = providers_of(&file);
        let existing = providers
            .get("openrouter")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let prior: Vec<&Map<String, Value>> = (existing.get("models").and_then(Value::as_array).into_iter().flatten())
            .filter_map(Value::as_object)
            .collect();
        let models: Vec<Value> = listed
            .iter()
            .filter(|model| model.is_object())
            .filter_map(|model| {
                let id = scalar_text(model.get("id")).unwrap_or_default().trim().to_string();
                let name = scalar_text(model.get("name"))
                    .unwrap_or_else(|| id.clone())
                    .trim()
                    .to_string();
                if id.is_empty() {
                    return None;
                }
                // The last entry with an id wins, as when the TypeScript built a map of them.
                let mut merged = (prior.iter().rev())
                    .find(|model| scalar_text(model.get("id")).unwrap_or_default() == id)
                    .map(|model| (*model).clone())
                    .unwrap_or_default();
                merged.insert("id".into(), id.into());
                merged.insert("name".into(), name.into());
                Some(Value::Object(merged))
            })
            .collect();
        if models.is_empty() {
            bail!("OpenRouter returned no models.");
        }
        let mut openrouter = existing;
        openrouter.insert("models".into(), Value::Array(models));
        providers.insert("openrouter".into(), Value::Object(openrouter));
        file.insert("providers".into(), Value::Object(providers));
        self.write_json("models.json", &file, true)
    }

    /// Adds or updates one custom model provider, and returns all of them. `None` is what the renderer sends when it
    /// has no config.
    pub fn set_custom_provider(&self, config: Option<CustomProviderInput>) -> Result<Vec<CustomProviderSummary>> {
        let Some(config) = config else {
            bail!("Provider config is required");
        };
        let guard = self.lock();
        let mut file = read_for_update(&self.path("models.json"))?;
        let mut providers = providers_of(&file);
        let key = config.provider.trim();
        if !is_plain_name(key) {
            bail!("Provider name may use letters, numbers, dots, dashes, and underscores.");
        }
        let known = providers.get(key).is_some_and(truthy) || PROVIDERS.iter().any(|(id, _)| *id == key);
        let existing = providers
            .get(key)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        // A provider merged over a built-in (e.g. openrouter model additions) or an already-configured one may omit
        // baseUrl and keep riding on that endpoint; a brand-new custom provider has nowhere to send requests without
        // one.
        let base_url = config.base_url.trim();
        let base_url = if !base_url.is_empty() {
            let url = url::Url::parse(base_url).map_err(|_| anyhow!("Base URL must be a valid URL."))?;
            // Plain HTTP is allowed for self-hosted and relayed endpoints; the settings form warns that keys travel
            // unencrypted over it.
            if !["http", "https"].contains(&url.scheme()) {
                bail!("Base URL must start with http:// or https://.");
            }
            Some(url.to_string())
        } else if known {
            None
        } else {
            bail!("Base URL is required.");
        };
        // A saved key must not follow the provider to an address the user did not enter it for: a changed address needs
        // the key typed again.
        if let Some(new) = &base_url {
            let stored =
                |value: Option<&Value>| value.is_some_and(|key| truthy(key) && key.as_str() != Some(KEYLESS_API_KEY));
            let has_credential =
                stored(existing.get("apiKey")) || stored(read_object(&self.path("auth.json")).get(key));
            let current = scalar_text(existing.get("baseUrl"));
            let changed = current.as_deref().map(|url| url.trim_end_matches('/')) != Some(new.trim_end_matches('/'));
            if has_credential && changed && non_blank(config.api_key.as_deref()).is_none() {
                bail!(
                    "Enter the API key again when you change this provider's address, so a saved key is never sent somewhere new."
                );
            }
        }
        let api = non_blank(config.api.as_deref())
            .or_else(|| scalar_text(existing.get("api")).filter(|api| !api.is_empty()))
            .unwrap_or_else(|| "openai-completions".into());
        if !CUSTOM_MODEL_APIS.contains(&api.as_str()) {
            bail!("Unsupported API type: {api}");
        }
        let wanted: Vec<(&str, Option<String>)> = (config.models.iter())
            .map(|model| (model.id.trim(), non_blank(model.name.as_deref())))
            .filter(|(id, _)| !id.is_empty())
            .collect();
        if wanted.is_empty() {
            bail!("At least one model ID is required.");
        }

        // Merge instead of replace so hand-edited fields (compat, cost, ...) survive.
        let mut merged = existing.clone();
        if let Some(base_url) = base_url {
            merged.insert("baseUrl".into(), base_url.into());
        }
        merged.insert("api".into(), api.into());
        if let Some(api_key) = non_blank(config.api_key.as_deref()) {
            merged.insert("apiKey".into(), api_key.into());
        } else if !existing.get("apiKey").is_some_and(truthy)
            && !read_object(&self.path("auth.json")).get(key).is_some_and(truthy)
        {
            merged.insert("apiKey".into(), KEYLESS_API_KEY.into());
        }
        let prior = existing.get("models").and_then(Value::as_array);
        let models = wanted.into_iter().map(|(id, name)| {
            let same = |model: &&Value| scalar_text(model.get("id")).unwrap_or_default() == id;
            let mut model = (prior.into_iter().flatten().find(same))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            model.insert("id".into(), id.into());
            if let Some(name) = name {
                model.insert("name".into(), name.into());
            }
            Value::Object(model)
        });
        merged.insert("models".into(), models.collect());
        providers.insert(key.into(), Value::Object(merged));
        file.insert("providers".into(), Value::Object(providers));
        self.write_json("models.json", &file, true)?;
        drop(guard);
        Ok(self.custom_providers())
    }

    pub fn remove_custom_provider(&self, provider_id: &str) -> Result<Vec<CustomProviderSummary>> {
        {
            let _guard = self.lock();
            let mut file = read_for_update(&self.path("models.json"))?;
            let mut providers = providers_of(&file);
            providers.shift_remove(provider_id.trim());
            file.insert("providers".into(), Value::Object(providers));
            self.write_json("models.json", &file, true)?;
        }
        Ok(self.custom_providers())
    }
}

impl McpServerInput {
    /// The settings.json entry for this server. What the file already holds for the name carries over only where
    /// it is not part of what the form sets: an http server keeps its other fields, a stdio one keeps its timeouts.
    fn merged_over(self, previous: Option<&Value>) -> Value {
        match self {
            McpServerInput::Stdio {
                command,
                args,
                cwd,
                enabled,
            } => {
                let mut server = Map::new();
                server.insert("type".into(), "stdio".into());
                server.insert("command".into(), command.into());
                if !args.is_empty() {
                    server.insert("args".into(), args.into());
                }
                if let Some(cwd) = cwd {
                    server.insert("cwd".into(), cwd.into());
                }
                server.insert("enabled".into(), enabled.into());
                for timeout in ["startupTimeoutMs", "callTimeoutMs"] {
                    if let Some(value) = previous
                        .and_then(|server| server.get(timeout))
                        .filter(|value| value.is_number())
                    {
                        server.insert(timeout.into(), value.clone());
                    }
                }
                Value::Object(server)
            }
            McpServerInput::Http {
                url,
                oauth,
                enabled,
                bearer_token_env_var,
            } => {
                let is_http = previous.and_then(|server| server.get("type")).and_then(Value::as_str) == Some("http");
                let mut server = previous
                    .and_then(Value::as_object)
                    .filter(|_| is_http)
                    .cloned()
                    .unwrap_or_default();
                server.insert("type".into(), "http".into());
                server.insert("url".into(), url.into());
                server.insert("oauth".into(), oauth.into());
                server.insert("enabled".into(), enabled.into());
                match bearer_token_env_var.filter(|name| !name.is_empty()) {
                    Some(name) => server.insert("bearerTokenEnvVar".into(), name.into()),
                    None => server.shift_remove("bearerTokenEnvVar"),
                };
                Value::Object(server)
            }
        }
    }
}

/// Validates and normalizes an MCP server request: a trimmed command, a URL in its normal form.
fn checked_mcp_config(config: McpServerInput) -> Result<McpServerInput> {
    Ok(match config {
        McpServerInput::Stdio {
            command,
            args,
            cwd,
            enabled,
        } => {
            let command = command.trim().to_string();
            if command.is_empty() {
                bail!("MCP stdio server needs a command.");
            }
            if std::iter::once(&command).chain(&args).any(|part| part.contains('\0')) {
                bail!("MCP command arguments cannot contain NUL.");
            }
            McpServerInput::Stdio {
                command,
                args,
                cwd: non_blank(cwd.as_deref()),
                enabled,
            }
        }
        McpServerInput::Http {
            url,
            oauth,
            enabled,
            bearer_token_env_var,
        } => {
            let url = url::Url::parse(&url).map_err(|_| anyhow!("Invalid URL"))?;
            // Plain http is for a server on this machine only; anything else crosses the network and needs TLS.
            let local = url.scheme() == "http" && url.host_str() == Some("localhost");
            if url.scheme() != "https" && !local {
                bail!("MCP server URL must use HTTPS.");
            }
            McpServerInput::Http {
                url: url.to_string(),
                oauth,
                enabled,
                bearer_token_env_var,
            }
        }
    })
}

/// A shell-like command line split into arguments, honouring single and double quotes and backslash escapes.
pub fn split_command_line(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    // An empty quoted argument (`x ""`) is still an argument.
    let mut pending = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            } else if c == '\\'
                && open == '"'
                && let Some(escaped) = chars.next()
            {
                current.push(escaped);
            } else {
                current.push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            pending = true;
        } else if c == '\\'
            && let Some(escaped) = chars.next()
        {
            current.push(escaped);
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

/// The services the MCP picker offers, from the catalog Prime Agent ships (`resource_dir` is the folder holding
/// [`MCP_CATALOG_FILE`]). Only that bundled catalog is read; user-declared catalog sources are not merged in.
pub fn mcp_catalog(resource_dir: &Path) -> Result<Vec<McpService>> {
    let path = resource_dir.join(MCP_CATALOG_FILE);
    let text =
        fs::read_to_string(&path).with_context(|| format!("MCP service catalog not found ({})", path.display()))?;
    let catalog: Value = serde_json::from_str(strip_bom(&text)).context("MCP service catalog is not valid JSON")?;
    let entries = catalog
        .get("entries")
        .and_then(Value::as_array)
        .context("MCP service catalog has no entries")?;
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
            let id = text(entry.get("server"))?;
            Some(McpService {
                name: text(entry.get("label")).unwrap_or_else(|| id.clone()),
                description: text(entry.get("description")).unwrap_or_default(),
                auth: text(entry.get("auth").and_then(|auth| auth.get("strategy"))).unwrap_or_else(|| "unknown".into()),
                id,
            })
        })
        .collect())
}

/// The "what's new" text: the first `CHANGELOG.md` found in `dirs`, cut to a size the pane can show.
pub fn changelog_text(dirs: &[&Path]) -> String {
    for dir in dirs {
        if let Ok(bytes) = fs::read(dir.join("CHANGELOG.md")) {
            return String::from_utf8_lossy(&bytes).chars().take(CHANGELOG_LIMIT).collect();
        }
    }
    CHANGELOG_MISSING.into()
}

// ---------- helpers ----------

fn strip_bom(text: &str) -> &str {
    text.trim_start_matches('\u{feff}')
}

/// A JSON object from a file; a missing, unreadable or corrupt file is empty.
fn read_object(path: &Path) -> Map<String, Value> {
    let parsed = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(strip_bom(&text)).ok());
    match parsed {
        Some(Value::Object(object)) => object,
        _ => Map::new(),
    }
}

/// A JSON object from a file about to be rewritten. A missing file is empty. A file that is not an object is moved
/// aside to `<name>.corrupt` rather than overwritten (it may hold keys the user cannot get back), and one that cannot
/// be read at all stops the write.
fn read_for_update(path: &Path) -> Result<Map<String, Value>> {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(strip_bom(&text)) {
            Ok(Value::Object(object)) => Ok(object),
            _ => {
                quarantine(path);
                Ok(Map::new())
            }
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
    }
}

fn mcp_servers_of(settings: &Map<String, Value>) -> Map<String, Value> {
    settings
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn providers_of(file: &Map<String, Value>) -> Map<String, Value> {
    file.get("providers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn summarize_provider(provider_id: &str, config: &Value) -> CustomProviderSummary {
    let api_key = config.get("apiKey");
    let models = config
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = scalar_text(model.get("id")).filter(|id| !id.is_empty())?;
            let name = scalar_text(model.get("name")).unwrap_or_else(|| id.clone());
            Some(CustomModelSummary { id, name })
        });
    CustomProviderSummary {
        provider: provider_id.into(),
        base_url: scalar_text(config.get("baseUrl")).unwrap_or_default(),
        api: scalar_text(config.get("api")).unwrap_or_else(|| "openai-completions".into()),
        has_key: api_key.is_some_and(truthy) && api_key != Some(&Value::from(KEYLESS_API_KEY)),
        models: models.collect(),
    }
}

/// JavaScript truthiness: what `Boolean(value)` said about a value read from a file.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// A string, number or boolean as text; anything else (including null) is `None`.
fn scalar_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// An integer, including a whole number written as `2.0`.
fn whole_number(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.fract() == 0.0 && number.abs() < 1e15)
            .map(|number| number as i64)
    })
}

fn non_blank(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// Letters, digits, dots, dashes and underscores, at least one.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// `provider/model-id`: a plain provider name, a slash, then an id with no whitespace.
fn is_model_selector(value: &str) -> bool {
    value.split_once('/').is_some_and(|(provider, model)| {
        is_plain_name(provider) && !model.is_empty() && !model.contains(char::is_whitespace)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    fn prime() -> (tempfile::TempDir, PrimeFiles) {
        let dir = tempfile::tempdir().unwrap();
        let files = PrimeFiles::new(dir.path());
        (dir, files)
    }

    fn read_json(dir: &tempfile::TempDir, name: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(dir.path().join(name)).unwrap()).unwrap()
    }

    fn write(dir: &tempfile::TempDir, name: &str, text: &str) {
        fs::write(dir.path().join(name), text).unwrap();
    }

    fn err(result: Result<impl std::fmt::Debug>) -> String {
        format!("{:#}", result.unwrap_err())
    }

    fn leftovers(dir: &tempfile::TempDir) -> Vec<String> {
        let names = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned());
        names.filter(|name| name.ends_with(".tmp")).collect()
    }

    fn http(url: &str) -> McpServerInput {
        McpServerInput::Http {
            url: url.into(),
            oauth: false,
            enabled: true,
            bearer_token_env_var: None,
        }
    }

    fn stdio(command: &str, args: &[&str]) -> McpServerInput {
        McpServerInput::Stdio {
            command: command.into(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: None,
            enabled: true,
        }
    }

    // ----- Prime Agent settings writers (tests/prime-files.test.ts) -----

    #[test]
    fn refuses_to_overwrite_an_unparseable_settings_json() {
        let (dir, files) = prime();
        let corrupt = "{ \"defaultModel\": \"x\", ";
        write(&dir, "settings.json", corrupt);
        assert!(err(files.write_rlm_max_depth(2)).contains("not valid JSON"));
        assert!(err(files.set_agent_traces_enabled(true)).contains("not valid JSON"));
        assert!(err(files.set_mcp_server("docs", Some(http("https://example.com/mcp")))).contains("not valid JSON"));
        assert!(err(files.set_model_role("auxiliaryModel", "openai/gpt-5")).contains("not valid JSON"));
        assert_eq!(fs::read_to_string(dir.path().join("settings.json")).unwrap(), corrupt);
        assert!(leftovers(&dir).is_empty());
    }

    #[test]
    fn stores_stdio_mcp_servers_with_command_and_args() {
        let (dir, files) = prime();
        write(
            &dir,
            "settings.json",
            &json!({ "defaultModel": "x", "mcpServers": { "docs": { "type": "http", "url": "https://example.com/mcp" } } }).to_string(),
        );
        let uvx = "/Users/me/.local/bin/uvx";
        let args = ["git+https://github.com/googlecolab/colab-mcp"];
        let servers = files.set_mcp_server("colab-mcp", Some(stdio(uvx, &args))).unwrap();
        assert_eq!(
            servers["colab-mcp"],
            json!({ "type": "stdio", "command": uvx, "args": args, "enabled": true })
        );
        assert_eq!(servers["docs"]["type"], "http");
        assert_eq!(servers["docs"]["url"], "https://example.com/mcp");
        let written = read_json(&dir, "settings.json");
        assert_eq!(written["defaultModel"], "x");
        assert_eq!(
            written["mcpServers"]["colab-mcp"],
            json!({ "type": "stdio", "command": uvx, "args": args, "enabled": true })
        );
        assert!(written["mcpServers"]["colab-mcp"].get("url").is_none());
        assert!(err(files.set_mcp_server("bad", Some(stdio("  ", &[])))).contains("needs a command"));
    }

    #[test]
    fn splits_mcp_command_lines_like_a_shell() {
        let split = |line: &str| split_command_line(line).unwrap();
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
        assert!(err(split_command_line(r#"oops "unterminated"#)).contains("unterminated quote"));
    }

    #[test]
    fn command_line_splitting_edge_cases() {
        let split = |line: &str| split_command_line(line).unwrap();
        assert!(split("").is_empty());
        assert!(split("   \t ").is_empty());
        assert_eq!(split(r#""a\"b" 'c\d'"#), ["a\"b", r"c\d"]);
        assert_eq!(split(r"trailing\"), ["trailing\\"]);
        assert_eq!(split(r#"a"b c"d"#), ["ab cd"]);
        assert_eq!(
            split("C:\\\\Program\\ Files\\\\x.exe --flag"),
            ["C:\\Program Files\\x.exe", "--flag"]
        );
        assert_eq!(split(r#""" """#), ["", ""]);
        assert!(split_command_line("'open").is_err());
    }

    #[test]
    fn refuses_to_overwrite_a_settings_json_that_is_not_an_object() {
        let (dir, files) = prime();
        write(&dir, "settings.json", "[1, 2]");
        assert!(err(files.write_rlm_max_depth(2)).contains("JSON object"));
        assert_eq!(fs::read_to_string(dir.path().join("settings.json")).unwrap(), "[1, 2]");
    }

    #[test]
    fn creates_settings_json_when_missing_and_preserves_other_keys() {
        let (dir, files) = prime();
        files.write_rlm_max_depth(3).unwrap();
        assert_eq!(read_json(&dir, "settings.json"), json!({ "rlmMaxDepth": 3 }));
        write(
            &dir,
            "settings.json",
            &json!({ "defaultModel": "m", "agentTraces": { "dir": "x" } }).to_string(),
        );
        files.set_agent_traces_enabled(true).unwrap();
        assert_eq!(
            read_json(&dir, "settings.json"),
            json!({ "defaultModel": "m", "agentTraces": { "dir": "x", "enabled": true } })
        );
        assert!(files.agent_traces_enabled());
        files.set_agent_traces_enabled(false).unwrap();
        assert!(!files.agent_traces_enabled());
    }

    #[test]
    fn keeps_lenient_reads_for_display_paths() {
        let (dir, files) = prime();
        write(&dir, "settings.json", "nope");
        assert!(files.read_prime_settings().is_empty());
        assert_eq!(files.read_rlm_max_depth(4), 4);
        assert!(!files.agent_traces_enabled());
        assert!(files.mcp_servers().is_empty());
        write(&dir, "settings.json", "[1]");
        assert!(files.read_prime_settings().is_empty());
    }

    #[test]
    fn the_rlm_depth_must_be_a_whole_number_of_zero_or_more() {
        let (dir, files) = prime();
        for (text, expected) in [
            ("3", 3),
            ("0", 0),
            ("2.0", 2),
            ("-1", 9),
            ("1.5", 9),
            ("\"2\"", 9),
            ("null", 9),
            ("true", 9),
        ] {
            write(&dir, "settings.json", &format!("{{\"rlmMaxDepth\":{text}}}"));
            assert_eq!(files.read_rlm_max_depth(9), expected, "{text}");
        }
        write(&dir, "settings.json", "{}");
        assert_eq!(files.read_rlm_max_depth(7), 7);
    }

    #[test]
    fn settings_keep_their_key_order_when_rewritten() {
        let (dir, files) = prime();
        write(
            &dir,
            "settings.json",
            r#"{ "zebra": 1, "apple": 2, "mcpServers": {}, "middle": 3 }"#,
        );
        files.write_rlm_max_depth(2).unwrap();
        let text = fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let positions: Vec<usize> = ["zebra", "apple", "mcpServers", "middle", "rlmMaxDepth"]
            .iter()
            .map(|key| text.find(key).unwrap())
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn settings_json_is_private_like_prime_agents_own_writes() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, files) = prime();
        files.write_rlm_max_depth(2).unwrap();
        assert_eq!(
            fs::metadata(dir.path().join("settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        // A file made world-readable by an older build is tightened on the next write.
        fs::set_permissions(dir.path().join("settings.json"), fs::Permissions::from_mode(0o644)).unwrap();
        files.write_rlm_max_depth(3).unwrap();
        assert_eq!(
            fs::metadata(dir.path().join("settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn a_byte_order_mark_in_settings_is_tolerated() {
        let (dir, files) = prime();
        write(&dir, "settings.json", "\u{feff}{\"defaultModel\":\"x\"}");
        files.write_rlm_max_depth(2).unwrap();
        assert_eq!(
            read_json(&dir, "settings.json"),
            json!({ "defaultModel": "x", "rlmMaxDepth": 2 })
        );
    }

    // ----- model roles -----

    #[test]
    fn model_roles_are_read_set_and_cleared() {
        let (dir, files) = prime();
        let empty = files.model_roles();
        assert_eq!(
            Value::Object(empty),
            json!({ "subagentDefaultModel": "", "auxiliaryModel": "", "providerBackupModel": "" })
        );
        let roles = files.set_model_role("auxiliaryModel", "openai/gpt-5.1:high").unwrap();
        assert_eq!(roles["auxiliaryModel"], "openai/gpt-5.1:high");
        assert_eq!(
            read_json(&dir, "settings.json")["auxiliaryModel"],
            "openai/gpt-5.1:high"
        );
        let roles = files.set_model_role("auxiliaryModel", "").unwrap();
        assert_eq!(roles["auxiliaryModel"], "");
        assert!(read_json(&dir, "settings.json").get("auxiliaryModel").is_none());
        write(
            &dir,
            "settings.json",
            r#"{ "subagentDefaultModel": 5, "providerBackupModel": "a/b" }"#,
        );
        assert_eq!(files.model_roles()["subagentDefaultModel"], "");
        assert_eq!(files.model_roles()["providerBackupModel"], "a/b");
    }

    #[test]
    fn model_roles_reject_unknown_keys_and_malformed_selectors() {
        let (dir, files) = prime();
        assert_eq!(err(files.set_model_role("defaultModel", "a/b")), "Unknown model role.");
        let message = "Select a model with a provider and model ID.";
        for bad in [
            "gpt-5",
            "/gpt",
            "openai/",
            "open ai/gpt",
            "openai/gp t",
            "openai/gpt\n",
            "a/b\t",
            &format!("a/{}", "x".repeat(512)),
        ] {
            assert_eq!(err(files.set_model_role("auxiliaryModel", bad)), message, "{bad:?}");
        }
        assert!(
            files
                .set_model_role("auxiliaryModel", &format!("a/{}", "x".repeat(510)))
                .is_ok()
        );
        assert!(
            files
                .set_model_role("auxiliaryModel", "a.b-c_d/with/slashes:tag")
                .is_ok()
        );
        assert!(
            !dir.path().join("settings.json").exists()
                || read_json(&dir, "settings.json").get("auxiliaryModel").is_some()
        );
    }

    // ----- MCP servers -----

    #[test]
    fn http_mcp_servers_need_https_unless_they_are_on_this_machine() {
        let (_dir, files) = prime();
        let message = "MCP server URL must use HTTPS.";
        assert_eq!(
            err(files.set_mcp_server("a", Some(http("http://example.com/mcp")))),
            message
        );
        assert_eq!(
            err(files.set_mcp_server("a", Some(http("ftp://localhost/mcp")))),
            message
        );
        assert_eq!(
            err(files.set_mcp_server("a", Some(http("file://localhost/etc/passwd")))),
            message
        );
        assert_eq!(err(files.set_mcp_server("a", Some(http("not a url")))), "Invalid URL");
        assert_eq!(err(files.set_mcp_server("a", Some(http("")))), "Invalid URL");
        let servers = files
            .set_mcp_server("local", Some(http("http://localhost:8080/mcp")))
            .unwrap();
        assert_eq!(servers["local"]["url"], "http://localhost:8080/mcp");
        let servers = files.set_mcp_server("tls", Some(http("https://Example.com"))).unwrap();
        assert_eq!(servers["tls"]["url"], "https://example.com/");
    }

    #[test]
    fn mcp_server_names_are_checked_and_trimmed() {
        let (_dir, files) = prime();
        let message = "MCP server name may use letters, numbers, dots, dashes, and underscores.";
        for bad in ["", "  ", "has space", "a/b", "ünï", "a:b"] {
            assert_eq!(
                err(files.set_mcp_server(bad, Some(http("https://x.example/mcp")))),
                message,
                "{bad:?}"
            );
            assert_eq!(err(files.set_mcp_server(bad, None)), message, "{bad:?}");
        }
        let servers = files
            .set_mcp_server("  ok.name_1-x  ", Some(http("https://x.example/mcp")))
            .unwrap();
        assert!(servers.contains_key("ok.name_1-x"));
    }

    #[test]
    fn rewriting_an_http_server_keeps_unrelated_fields_and_drops_a_cleared_token_variable() {
        let (dir, files) = prime();
        write(
            &dir,
            "settings.json",
            &json!({ "mcpServers": { "docs": {
                "type": "http", "url": "https://old.example/mcp", "oauth": true, "enabled": false,
                "bearerTokenEnvVar": "OLD", "startupTimeoutMs": 5000, "headers": { "x": "y" }
            } } })
            .to_string(),
        );
        let with_token = McpServerInput::Http {
            url: "https://new.example/mcp".into(),
            oauth: false,
            enabled: true,
            bearer_token_env_var: Some("DOCS_TOKEN".into()),
        };
        files.set_mcp_server("docs", Some(with_token)).unwrap();
        let docs = read_json(&dir, "settings.json")["mcpServers"]["docs"].clone();
        assert_eq!(docs["bearerTokenEnvVar"], "DOCS_TOKEN");
        assert_eq!(docs["startupTimeoutMs"], 5000);
        assert_eq!(docs["headers"], json!({ "x": "y" }));
        assert_eq!(docs["url"], "https://new.example/mcp");
        assert_eq!(docs["oauth"], false);
        files
            .set_mcp_server("docs", Some(http("https://new.example/mcp")))
            .unwrap();
        let docs = read_json(&dir, "settings.json")["mcpServers"]["docs"].clone();
        assert!(docs.get("bearerTokenEnvVar").is_none());
        let empty_token = McpServerInput::Http {
            url: "https://new.example/mcp".into(),
            oauth: false,
            enabled: true,
            bearer_token_env_var: Some(String::new()),
        };
        files.set_mcp_server("docs", Some(empty_token)).unwrap();
        assert!(
            read_json(&dir, "settings.json")["mcpServers"]["docs"]
                .get("bearerTokenEnvVar")
                .is_none()
        );
    }

    #[test]
    fn rewriting_a_stdio_server_keeps_its_timeouts_and_drops_other_old_fields() {
        let (dir, files) = prime();
        write(
            &dir,
            "settings.json",
            &json!({ "mcpServers": { "py": {
                "type": "stdio", "command": "old", "args": ["a"], "env": { "K": "V" },
                "startupTimeoutMs": 9000, "callTimeoutMs": "slow"
            } } })
            .to_string(),
        );
        let new = McpServerInput::Stdio {
            command: " python ".into(),
            args: vec![],
            cwd: Some("  /work ".into()),
            enabled: false,
        };
        files.set_mcp_server("py", Some(new)).unwrap();
        assert_eq!(
            read_json(&dir, "settings.json")["mcpServers"]["py"],
            json!({ "type": "stdio", "command": "python", "cwd": "/work", "enabled": false, "startupTimeoutMs": 9000 })
        );
        let blank_cwd = McpServerInput::Stdio {
            command: "p".into(),
            args: vec![],
            cwd: Some("   ".into()),
            enabled: true,
        };
        files.set_mcp_server("py", Some(blank_cwd)).unwrap();
        assert!(
            read_json(&dir, "settings.json")["mcpServers"]["py"]
                .get("cwd")
                .is_none()
        );
    }

    #[test]
    fn stdio_arguments_cannot_hide_a_nul_byte() {
        let (dir, files) = prime();
        assert_eq!(
            err(files.set_mcp_server("a", Some(stdio("run", &["ok", "bad\0arg"])))),
            "MCP command arguments cannot contain NUL."
        );
        assert_eq!(
            err(files.set_mcp_server("a", Some(stdio("ru\0n", &[])))),
            "MCP command arguments cannot contain NUL."
        );
        assert!(!dir.path().join("settings.json").exists());
    }

    #[test]
    fn removing_an_mcp_server_keeps_the_others_in_order() {
        let (dir, files) = prime();
        write(
            &dir,
            "settings.json",
            &json!({ "mcpServers": { "a": {}, "b": {}, "c": {} } }).to_string(),
        );
        let servers = files.set_mcp_server("b", None).unwrap();
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["a", "c"]);
        let servers = files.set_mcp_server("missing", None).unwrap();
        assert_eq!(servers.len(), 2);
    }

    #[test]
    fn the_server_list_shows_only_the_fields_the_screen_uses() {
        let (dir, files) = prime();
        write(
            &dir,
            "settings.json",
            &json!({ "mcpServers": {
                "h": { "type": "http", "url": "https://h.example", "oauth": true, "enabled": true, "secretHeader": "s" },
                "s": { "type": "stdio", "command": "c", "args": "not-an-array", "cwd": "/w", "env": { "K": "V" } },
                "odd": null
            } })
            .to_string(),
        );
        let servers = files.mcp_servers();
        assert_eq!(
            servers["h"],
            json!({ "type": "http", "url": "https://h.example", "oauth": true, "enabled": true })
        );
        assert_eq!(servers["s"], json!({ "type": "stdio", "command": "c", "cwd": "/w" }));
        assert_eq!(servers["odd"], json!({}));
    }

    // ----- custom providers -----

    fn model(id: &str) -> CustomModelInput {
        CustomModelInput {
            id: id.into(),
            name: None,
        }
    }

    fn provider(name: &str, base_url: &str, models: Vec<CustomModelInput>) -> CustomProviderInput {
        CustomProviderInput {
            provider: name.into(),
            base_url: base_url.into(),
            api: None,
            api_key: None,
            models,
        }
    }

    fn find<'a>(providers: &'a [CustomProviderSummary], name: &str) -> &'a CustomProviderSummary {
        providers.iter().find(|summary| summary.provider == name).unwrap()
    }

    #[test]
    fn a_brand_new_provider_needs_a_base_url() {
        let (dir, files) = prime();
        let result = files.set_custom_provider(Some(provider("my-llm", "", vec![model("model-a")])));
        assert_eq!(err(result), "Base URL is required.");
        assert!(!dir.path().join("models.json").exists());
    }

    #[test]
    fn keyless_providers_get_a_placeholder_key_so_prime_agent_lists_their_models() {
        let (dir, files) = prime();
        let saved = files
            .set_custom_provider(Some(provider(
                "keyless",
                "http://box.local:5001/v1",
                vec![model("model-a")],
            )))
            .unwrap();
        assert!(!find(&saved, "keyless").has_key);
        assert_eq!(read_json(&dir, "models.json")["providers"]["keyless"]["apiKey"], "none");
        let mut with_key = provider("keyless", "", vec![model("model-a")]);
        with_key.api_key = Some("real-key".into());
        let saved = files.set_custom_provider(Some(with_key)).unwrap();
        assert!(find(&saved, "keyless").has_key);
        assert_eq!(
            read_json(&dir, "models.json")["providers"]["keyless"]["apiKey"],
            "real-key"
        );
        // Saving again without a key keeps the stored one.
        files
            .set_custom_provider(Some(provider("keyless", "", vec![model("model-a")])))
            .unwrap();
        assert_eq!(
            read_json(&dir, "models.json")["providers"]["keyless"]["apiKey"],
            "real-key"
        );
    }

    #[test]
    fn a_saved_key_does_not_follow_a_provider_to_a_new_address() {
        let (dir, files) = prime();
        let mut first = provider("relay", "https://relay.example/v1", vec![model("m")]);
        first.api_key = Some("secret".into());
        files.set_custom_provider(Some(first)).unwrap();
        // Moving the address without typing the key again is refused, and nothing changes.
        let moved = files.set_custom_provider(Some(provider(
            "relay",
            "https://elsewhere.example/v1",
            vec![model("m")],
        )));
        assert!(err(moved).contains("API key again"));
        assert_eq!(
            read_json(&dir, "models.json")["providers"]["relay"]["baseUrl"],
            "https://relay.example/v1"
        );
        // The same address, or a new one with the key, is fine.
        files
            .set_custom_provider(Some(provider("relay", "https://relay.example/v1/", vec![model("m2")])))
            .unwrap();
        let mut moved = provider("relay", "https://elsewhere.example/v1", vec![model("m")]);
        moved.api_key = Some("new-secret".into());
        files.set_custom_provider(Some(moved)).unwrap();
        // A login kept in auth.json is guarded the same way.
        files.set_auth_key("groq", "gsk_1").unwrap();
        let stolen = files.set_custom_provider(Some(provider("groq", "https://evil.example/v1", vec![model("m")])));
        assert!(err(stolen).contains("API key again"));
    }

    #[test]
    fn a_provider_with_a_login_in_auth_json_gets_no_placeholder() {
        let (dir, files) = prime();
        files.set_auth_key("groq", "gsk_1").unwrap();
        files
            .set_custom_provider(Some(provider("groq", "", vec![model("m")])))
            .unwrap();
        assert!(
            read_json(&dir, "models.json")["providers"]["groq"]
                .get("apiKey")
                .is_none()
        );
    }

    #[test]
    fn accepts_http_and_https_base_urls_and_rejects_other_schemes() {
        let (_dir, files) = prime();
        let saved = files
            .set_custom_provider(Some(provider(
                "relay",
                "http://10.0.0.5:8000/v1",
                vec![model("model-a")],
            )))
            .unwrap();
        assert_eq!(find(&saved, "relay").base_url, "http://10.0.0.5:8000/v1");
        let bad = files.set_custom_provider(Some(provider("bad", "file:///etc/passwd", vec![model("model-a")])));
        assert_eq!(err(bad), "Base URL must start with http:// or https://.");
        for nonsense in ["localhost:5001", "not a url", "http://"] {
            let bad = files.set_custom_provider(Some(provider("bad", nonsense, vec![model("m")])));
            let message = err(bad);
            assert!(message.starts_with("Base URL must"), "{nonsense}: {message}");
        }
        let saved = files
            .set_custom_provider(Some(provider("bare", "  https://llm.example.com  ", vec![model("m")])))
            .unwrap();
        assert_eq!(find(&saved, "bare").base_url, "https://llm.example.com/");
    }

    #[test]
    fn the_base_url_may_be_omitted_for_built_in_or_already_configured_providers() {
        let (_dir, files) = prime();
        let built_in = files
            .set_custom_provider(Some(provider("openrouter", "", vec![model("model-a")])))
            .unwrap();
        assert_eq!(find(&built_in, "openrouter").base_url, "");

        files
            .set_custom_provider(Some(provider(
                "my-llm",
                "https://llm.example.com/v1",
                vec![model("model-a")],
            )))
            .unwrap();
        let updated = files
            .set_custom_provider(Some(provider("my-llm", "", vec![model("model-b")])))
            .unwrap();
        let mine = find(&updated, "my-llm");
        assert_eq!(mine.base_url, "https://llm.example.com/v1");
        assert_eq!(
            mine.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["model-b"]
        );
    }

    #[test]
    fn provider_input_is_validated() {
        let (dir, files) = prime();
        let url = "https://x.example/v1";
        assert_eq!(err(files.set_custom_provider(None)), "Provider config is required");
        let name = "Provider name may use letters, numbers, dots, dashes, and underscores.";
        for bad in ["", " ", "a b", "a/b", "ünï"] {
            assert_eq!(
                err(files.set_custom_provider(Some(provider(bad, url, vec![model("m")])))),
                name,
                "{bad:?}"
            );
        }
        assert_eq!(
            err(files.set_custom_provider(Some(provider("p", url, vec![])))),
            "At least one model ID is required."
        );
        assert_eq!(
            err(files.set_custom_provider(Some(provider("p", url, vec![model("  ")])))),
            "At least one model ID is required."
        );
        let mut odd_api = provider("p", url, vec![model("m")]);
        odd_api.api = Some("soap".into());
        assert_eq!(
            err(files.set_custom_provider(Some(odd_api))),
            "Unsupported API type: soap"
        );
        assert!(!dir.path().join("models.json").exists());
    }

    #[test]
    fn the_api_type_defaults_and_sticks() {
        let (_dir, files) = prime();
        let url = "https://x.example/v1";
        let saved = files
            .set_custom_provider(Some(provider("p", url, vec![model("m")])))
            .unwrap();
        assert_eq!(find(&saved, "p").api, "openai-completions");
        let mut anthropic = provider("p", "", vec![model("m")]);
        anthropic.api = Some(" anthropic-messages ".into());
        let saved = files.set_custom_provider(Some(anthropic)).unwrap();
        assert_eq!(find(&saved, "p").api, "anthropic-messages");
        let saved = files
            .set_custom_provider(Some(provider("p", "", vec![model("m")])))
            .unwrap();
        assert_eq!(find(&saved, "p").api, "anthropic-messages");
    }

    #[test]
    fn saving_a_provider_keeps_hand_edited_fields_and_other_top_level_keys() {
        let (dir, files) = prime();
        write(
            &dir,
            "models.json",
            &json!({
                "$schema": "x",
                "providers": {
                    "mine": {
                        "baseUrl": "https://old.example/v1", "api": "openai-responses", "apiKey": "k", "compat": { "a": 1 },
                        "models": [{ "id": "keep", "cost": { "input": 1 }, "name": "Old" }, { "id": "gone" }]
                    },
                    "other": { "baseUrl": "https://other.example" }
                }
            })
            .to_string(),
        );
        let mut update = provider(
            "mine",
            "https://new.example/v1",
            vec![
                model("keep"),
                CustomModelInput {
                    id: " fresh ".into(),
                    name: Some(" Fresh ".into()),
                },
            ],
        );
        update.api = None;
        // The address moves, so the key is typed again (see a_saved_key_does_not_follow_a_provider_to_a_new_address).
        update.api_key = Some("k2".into());
        files.set_custom_provider(Some(update)).unwrap();
        let file = read_json(&dir, "models.json");
        assert_eq!(file["$schema"], "x");
        assert_eq!(
            file["providers"]["other"],
            json!({ "baseUrl": "https://other.example" })
        );
        let mine = &file["providers"]["mine"];
        assert_eq!(mine["baseUrl"], "https://new.example/v1");
        assert_eq!(mine["api"], "openai-responses");
        assert_eq!(mine["apiKey"], "k2");
        assert_eq!(mine["compat"], json!({ "a": 1 }));
        assert_eq!(
            mine["models"],
            json!([{ "id": "keep", "cost": { "input": 1 }, "name": "Old" }, { "id": "fresh", "name": "Fresh" }])
        );
        let keys: Vec<&String> = file["providers"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["mine", "other"]);
    }

    #[test]
    fn provider_summaries_cope_with_odd_files() {
        let (dir, files) = prime();
        write(
            &dir,
            "models.json",
            &json!({ "providers": {
                "a": { "baseUrl": 5, "api": "openai-responses", "apiKey": "none", "models": [
                    { "id": "x", "name": "X" }, { "id": "y" }, { "name": "no id" }, "str", null, { "id": 7, "name": null }, { "id": "" }
                ] },
                "b": "not an object",
                "c": null,
                "d": { "apiKey": "real", "models": "nope" }
            } })
            .to_string(),
        );
        let summaries = files.custom_providers();
        assert_eq!(summaries.len(), 4);
        let a = find(&summaries, "a");
        assert_eq!(
            (a.base_url.as_str(), a.api.as_str(), a.has_key),
            ("5", "openai-responses", false)
        );
        let models: Vec<(&str, &str)> = a.models.iter().map(|m| (m.id.as_str(), m.name.as_str())).collect();
        assert_eq!(models, [("x", "X"), ("y", "y"), ("7", "7")]);
        let b = find(&summaries, "b");
        assert_eq!(
            (b.base_url.as_str(), b.api.as_str(), b.models.len()),
            ("", "openai-completions", 0)
        );
        assert!(find(&summaries, "d").has_key);
        assert!(
            files
                .set_custom_provider(Some(provider("new", "https://n.example", vec![model("m")])))
                .is_ok()
        );
    }

    #[test]
    fn the_summary_serializes_with_the_renderer_field_names() {
        let summary = CustomProviderSummary {
            provider: "p".into(),
            base_url: "u".into(),
            api: "a".into(),
            has_key: true,
            models: vec![CustomModelSummary {
                id: "i".into(),
                name: "n".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(summary).unwrap(),
            json!({ "provider": "p", "baseUrl": "u", "api": "a", "hasKey": true, "models": [{ "id": "i", "name": "n" }] })
        );
        let input: CustomProviderInput =
            serde_json::from_value(json!({ "provider": "p", "baseUrl": "u", "models": [{ "id": "m" }] })).unwrap();
        assert!(input.api.is_none() && input.api_key.is_none() && input.models[0].name.is_none());
        let null_config: Option<CustomProviderInput> = serde_json::from_value(Value::Null).unwrap();
        assert!(null_config.is_none());
    }

    #[test]
    fn removing_a_provider_leaves_the_others_and_unknown_ids_are_harmless() {
        let (dir, files) = prime();
        let url = "https://x.example";
        files
            .set_custom_provider(Some(provider("a", url, vec![model("m")])))
            .unwrap();
        files
            .set_custom_provider(Some(provider("b", url, vec![model("m")])))
            .unwrap();
        let left = files.remove_custom_provider(" a ").unwrap();
        assert_eq!(left.iter().map(|p| p.provider.as_str()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(files.remove_custom_provider("nope").unwrap().len(), 1);
        assert!(read_json(&dir, "models.json")["providers"].get("a").is_none());
    }

    // ----- auth.json -----

    #[test]
    fn auth_providers_list_known_ones_first_then_any_extra_without_exposing_keys() {
        let (dir, files) = prime();
        let none = files.list_auth_providers();
        assert_eq!(none.len(), 15);
        assert!(none.iter().all(|p| !p.configured));
        assert_eq!(
            none[0],
            AuthProvider {
                id: "openai".into(),
                name: "OpenAI".into(),
                configured: false
            }
        );
        write(
            &dir,
            "auth.json",
            &json!({ "zeta": { "type": "oauth" }, "anthropic": { "type": "api_key", "key": "sk-secret" }, "empty": null, "off": false })
                .to_string(),
        );
        let listed = files.list_auth_providers();
        let ids: Vec<&str> = listed.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(&ids[15..], ["zeta", "empty", "off"]);
        let configured: Vec<&str> = listed.iter().filter(|p| p.configured).map(|p| p.id.as_str()).collect();
        assert_eq!(configured, ["anthropic", "zeta"]);
        assert_eq!(listed[15].name, "zeta");
        assert!(!serde_json::to_string(&listed).unwrap().contains("sk-secret"));
    }

    #[test]
    fn setting_a_key_stores_it_trimmed_and_private() {
        let (dir, files) = prime();
        files.set_auth_key("openai", "  sk-123\n").unwrap();
        files.set_auth_key("groq", "gsk").unwrap();
        let auth = read_json(&dir, "auth.json");
        assert_eq!(auth["openai"], json!({ "type": "api_key", "key": "sk-123" }));
        assert_eq!(auth["groq"]["key"], "gsk");
        assert!(
            files
                .list_auth_providers()
                .iter()
                .find(|p| p.id == "openai")
                .unwrap()
                .configured
        );
        assert!(leftovers(&dir).is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("auth.json")).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn removing_a_key_leaves_the_rest_of_the_file() {
        let (dir, files) = prime();
        write(
            &dir,
            "auth.json",
            &json!({ "openai": { "key": "a" }, "weird": { "type": "oauth", "refresh": "r" } }).to_string(),
        );
        files.remove_auth("openai").unwrap();
        assert_eq!(
            read_json(&dir, "auth.json"),
            json!({ "weird": { "type": "oauth", "refresh": "r" } })
        );
        files.remove_auth("never-there").unwrap();
        assert_eq!(read_json(&dir, "auth.json").as_object().unwrap().len(), 1);
    }

    #[test]
    fn a_new_prime_agent_folder_is_created_for_the_first_key() {
        let dir = tempfile::tempdir().unwrap();
        let files = PrimeFiles::new(dir.path().join("not").join("yet"));
        files.set_auth_key("openai", "k").unwrap();
        assert!(dir.path().join("not").join("yet").join("auth.json").exists());
    }

    #[test]
    fn a_corrupt_auth_file_is_kept_aside_not_overwritten() {
        let (dir, files) = prime();
        write(&dir, "auth.json", "{ \"openai\": { \"key\": \"precious\"");
        assert!(files.list_auth_providers().iter().all(|p| !p.configured));
        files.set_auth_key("groq", "new").unwrap();
        assert_eq!(
            read_json(&dir, "auth.json"),
            json!({ "groq": { "type": "api_key", "key": "new" } })
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("auth.json.corrupt")).unwrap(),
            "{ \"openai\": { \"key\": \"precious\""
        );
    }

    #[test]
    fn an_unreadable_auth_file_stops_the_write() {
        let (dir, files) = prime();
        // A directory where the file should be: reading it fails in a way that is not "missing".
        fs::create_dir(dir.path().join("auth.json")).unwrap();
        assert!(files.set_auth_key("openai", "k").is_err());
        assert!(dir.path().join("auth.json").is_dir());
    }

    #[test]
    fn concurrent_updates_from_the_app_never_lose_each_other() {
        let (dir, files) = prime();
        let files = Arc::new(files);
        let workers: Vec<_> = (0..12)
            .map(|n| {
                let files = files.clone();
                std::thread::spawn(move || {
                    files.set_auth_key(&format!("p{n}"), &format!("key{n}")).unwrap();
                    files.set_mcp_server(&format!("s{n}"), Some(stdio("run", &[]))).unwrap();
                    files
                        .set_custom_provider(Some(provider(&format!("c{n}"), "https://x.example", vec![model("m")])))
                        .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(read_json(&dir, "auth.json").as_object().unwrap().len(), 12);
        assert_eq!(
            read_json(&dir, "settings.json")["mcpServers"]
                .as_object()
                .unwrap()
                .len(),
            12
        );
        assert_eq!(
            read_json(&dir, "models.json")["providers"].as_object().unwrap().len(),
            12
        );
        assert!(leftovers(&dir).is_empty());
    }

    // ----- OpenRouter refresh -----

    /// Serves one canned HTTP response on a local port and returns the URL and the request it received.
    fn serve_once(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/v1/models", listener.local_addr().unwrap());
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn refreshing_needs_an_openrouter_key() {
        let (dir, files) = prime();
        let message = "Add an OpenRouter API key before refreshing models.";
        assert_eq!(err(files.refresh_openrouter_models()), message);
        write(
            &dir,
            "auth.json",
            &json!({ "openrouter": { "type": "api_key", "key": "   " } }).to_string(),
        );
        assert_eq!(err(files.refresh_openrouter_models()), message);
        write(
            &dir,
            "auth.json",
            &json!({ "openrouter": { "type": "oauth" } }).to_string(),
        );
        assert_eq!(err(files.refresh_openrouter_models()), message);
        assert!(!dir.path().join("models.json").exists());
    }

    #[test]
    fn refreshing_merges_the_catalog_into_the_openrouter_provider() {
        let (dir, files) = prime();
        files.set_auth_key("openrouter", " or-key ").unwrap();
        write(
            &dir,
            "models.json",
            &json!({ "other": 1, "providers": { "openrouter": {
                "compat": { "x": true }, "apiKey": "stays",
                "models": [{ "id": "a/one", "cost": { "input": 2 }, "name": "Old name" }, { "id": "a/dropped" }]
            }, "mine": { "baseUrl": "https://m.example" } } })
            .to_string(),
        );
        let body = json!({ "data": [
            { "id": "a/one", "name": " New name " }, { "id": " b/two " }, { "name": "no id" }, "junk", null, { "id": "", "name": "empty" }
        ] })
        .to_string();
        let (url, request) = serve_once("200 OK", &body);
        files.refresh_models_from(&url).unwrap();
        let request = request.join().unwrap();
        assert!(request.starts_with("GET /api/v1/models "), "{request}");
        assert!(
            request.to_lowercase().contains("authorization: bearer or-key\r\n"),
            "{request}"
        );
        assert!(request.to_lowercase().contains("accept: application/json"), "{request}");

        let file = read_json(&dir, "models.json");
        assert_eq!(file["other"], 1);
        assert_eq!(file["providers"]["mine"], json!({ "baseUrl": "https://m.example" }));
        let openrouter = &file["providers"]["openrouter"];
        assert_eq!(openrouter["compat"], json!({ "x": true }));
        assert_eq!(openrouter["apiKey"], "stays");
        assert_eq!(
            openrouter["models"],
            json!([{ "id": "a/one", "cost": { "input": 2 }, "name": "New name" }, { "id": "b/two", "name": "b/two" }])
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("models.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(leftovers(&dir).is_empty());
    }

    #[test]
    fn refreshing_reports_http_errors_and_bad_catalogs_without_touching_models_json() {
        let (dir, files) = prime();
        files.set_auth_key("openrouter", "or-key").unwrap();
        write(&dir, "models.json", r#"{"providers":{}}"#);
        for (status, body, expected) in [
            (
                "401 Unauthorized",
                r#"{"error":"no"}"#,
                "OpenRouter model refresh failed (401).",
            ),
            (
                "500 Internal Server Error",
                "",
                "OpenRouter model refresh failed (500).",
            ),
            (
                "200 OK",
                "<html>not json</html>",
                "OpenRouter returned an invalid model catalog.",
            ),
            (
                "200 OK",
                r#"{"data":"nope"}"#,
                "OpenRouter returned an invalid model catalog.",
            ),
            (
                "200 OK",
                r#"{"models":[]}"#,
                "OpenRouter returned an invalid model catalog.",
            ),
            ("200 OK", r#"{"data":[]}"#, "OpenRouter returned no models."),
            (
                "200 OK",
                r#"{"data":[{"name":"x"},3]}"#,
                "OpenRouter returned no models.",
            ),
        ] {
            let (url, server) = serve_once(status, body);
            assert_eq!(err(files.refresh_models_from(&url)), expected, "{status} {body}");
            server.join().unwrap();
        }
        assert_eq!(
            fs::read_to_string(dir.path().join("models.json")).unwrap(),
            r#"{"providers":{}}"#
        );
    }

    #[test]
    fn a_redirect_is_not_followed_so_the_key_stays_with_the_first_host() {
        let (_dir, files) = prime();
        files.set_auth_key("openrouter", "or-key").unwrap();
        let (target, never) = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/stolen", listener.local_addr().unwrap());
            let watcher = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(600));
                listener.accept().is_ok()
            });
            (url, watcher)
        };
        let (url, server) = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/api/v1/models", listener.local_addr().unwrap());
            let reply =
                format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let handle = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                stream.write_all(reply.as_bytes()).unwrap();
            });
            (url, handle)
        };
        assert_eq!(
            err(files.refresh_models_from(&url)),
            "OpenRouter model refresh failed (302)."
        );
        server.join().unwrap();
        assert!(!never.join().unwrap(), "the redirect target was contacted");
    }

    #[test]
    fn an_unreachable_server_is_an_error_that_does_not_contain_the_key() {
        let (_dir, files) = prime();
        files.set_auth_key("openrouter", "or-secret-key").unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let message = err(files.refresh_models_from(&format!("http://127.0.0.1:{port}/")));
        assert!(message.starts_with("OpenRouter model refresh failed ("), "{message}");
        assert!(!message.contains("or-secret-key"));
    }

    // ----- catalog and changelog -----

    #[test]
    fn the_mcp_catalog_comes_from_the_bundled_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(MCP_CATALOG_FILE),
            json!({ "version": 2, "counts": {}, "entries": [
                { "server": "linear", "service": "linear", "label": "Linear", "description": "Issues", "auth": { "strategy": "oauth" } },
                { "server": "bare", "label": "Bare", "auth": { "strategy": "api_key" } },
                { "server": "nolabel" },
                { "label": "no server id" },
                "junk"
            ] })
            .to_string(),
        )
        .unwrap();
        let catalog = mcp_catalog(dir.path()).unwrap();
        let service = |id: &str, name: &str, description: &str, auth: &str| McpService {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            auth: auth.into(),
        };
        assert_eq!(
            catalog,
            [
                service("linear", "Linear", "Issues", "oauth"),
                service("bare", "Bare", "", "api_key"),
                service("nolabel", "nolabel", "", "unknown")
            ]
        );
        assert_eq!(
            serde_json::to_value(&catalog[0]).unwrap(),
            json!({ "id": "linear", "name": "Linear", "description": "Issues", "auth": "oauth" })
        );
    }

    #[test]
    fn a_missing_or_broken_catalog_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(err(mcp_catalog(dir.path())).contains("not found"));
        fs::write(dir.path().join(MCP_CATALOG_FILE), "{ nope").unwrap();
        assert!(err(mcp_catalog(dir.path())).contains("not valid JSON"));
        fs::write(dir.path().join(MCP_CATALOG_FILE), r#"{"entries":"x"}"#).unwrap();
        assert!(err(mcp_catalog(dir.path())).contains("no entries"));
    }

    #[test]
    fn the_changelog_is_the_first_one_found_and_is_cut_to_size() {
        let (first, second, empty) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        assert_eq!(
            changelog_text(&[empty.path()]),
            "Changelog not found. Installed Prime Agent package did not include CHANGELOG.md."
        );
        assert_eq!(changelog_text(&[]), changelog_text(&[empty.path()]));
        fs::write(second.path().join("CHANGELOG.md"), "second").unwrap();
        assert_eq!(changelog_text(&[empty.path(), second.path()]), "second");
        fs::write(first.path().join("CHANGELOG.md"), "é".repeat(13_000)).unwrap();
        let text = changelog_text(&[first.path(), second.path()]);
        assert_eq!(text.chars().count(), 12_000);
        assert!(text.chars().all(|c| c == 'é'));
        // A folder named CHANGELOG.md cannot be read, so the next candidate is tried.
        let tricky = tempfile::tempdir().unwrap();
        fs::create_dir(tricky.path().join("CHANGELOG.md")).unwrap();
        assert_eq!(changelog_text(&[tricky.path(), second.path()]), "second");
        let invalid = tempfile::tempdir().unwrap();
        fs::write(invalid.path().join("CHANGELOG.md"), b"ok \xff end").unwrap();
        assert_eq!(changelog_text(&[invalid.path()]), "ok \u{fffd} end");
    }

    #[test]
    fn the_folder_follows_the_environment_default_and_the_logs_dir_is_inside_it() {
        let files = PrimeFiles::new("/some/agent");
        assert_eq!(files.dir(), Path::new("/some/agent"));
        assert_eq!(files.logs_dir(), Path::new("/some/agent").join("logs"));
    }
}

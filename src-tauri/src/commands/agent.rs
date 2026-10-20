//! The agent: its program, its chats, their messages and the panels around them. Most of it is one call into the manager or
//! one of the agent modules; the work is in `desktop-core`.

use desktop_core::agent::{fleet, harness, insights, sessions};
use desktop_core::validate;
use serde_json::{Value, json};
use tauri::{AppHandle, State};

use super::{CmdResult, blocking, err, ok};
use crate::state::AppState;

fn require_text(name: &str, value: &str, max: usize) -> Result<String, String> {
    validate::non_empty(name, value, max).map_err(err)
}

// ----- the agent program -----

/// `window.prime.binaryGet`
#[tauri::command]
pub async fn binary_get(state: State<'_, AppState>) -> CmdResult {
    ok(state.binary.snapshot())
}

/// `window.prime.binaryInstall`: the pinned Prime Agent, then everything it needs to run code (the Python environment and the
/// sandbox), reported through the same state the install screen already shows.
#[tauri::command]
pub async fn binary_install(app: AppHandle, state: State<'_, AppState>) -> CmdResult {
    state.binary.install().await;
    if state.binary.snapshot().status == "installed" {
        crate::runtime::prepare(&app).await;
    }
    ok(state.binary.snapshot())
}

// ----- chats -----

/// `window.prime.agentCreate`
#[tauri::command]
pub async fn agent_create(state: State<'_, AppState>, tab_id: String) -> CmdResult {
    let tab = state
        .store
        .state()
        .tabs
        .into_iter()
        .find(|tab| tab.id == tab_id)
        .ok_or("Unknown tab")?;
    let path = validate::existing_dir("project path", &tab.path).map_err(err)?;
    state.until_ready().await;
    state.manager.open_tab(&tab.id, path).await.map_err(err)
}

/// `window.prime.agentState`
#[tauri::command]
pub async fn agent_state(state: State<'_, AppState>) -> CmdResult {
    ok(state.manager.all_infos())
}

/// `window.prime.agentCommand`: one of the allow-listed commands, sent to the chat.
#[tauri::command]
pub async fn agent_command(state: State<'_, AppState>, agent_id: String, cmd: Value) -> CmdResult {
    let cmd = validate::agent_command(&cmd).map_err(err)?.clone();
    if cmd["type"] == "set_model"
        && let Some(id) = cmd["modelId"].as_str().filter(|id| !id.is_empty())
    {
        let model = cmd["provider"]
            .as_str()
            .filter(|provider| !provider.is_empty())
            .map_or_else(|| id.to_string(), |provider| format!("{provider}/{id}"));
        let store = state.store.clone();
        tokio::task::spawn_blocking(move || store.set_settings(&json!({"model": model})));
    }
    state.manager.run_command(&agent_id, cmd).await.map_err(err)
}

/// `window.prime.agentMessages`
#[tauri::command]
pub async fn agent_messages(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    ok(state.manager.get_messages(&agent_id).await)
}

/// `window.prime.agentStats`
#[tauri::command]
pub async fn agent_stats(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    Ok(state.manager.get_stats(&agent_id).await)
}

/// `window.prime.agentSessions`
#[tauri::command]
pub async fn agent_sessions(state: State<'_, AppState>, agent_id: Option<String>) -> CmdResult {
    sessions::agent_sessions(&state.manager, agent_id.as_deref())
        .await
        .map_err(err)
}

/// `window.prime.agentResume`: opening a chat never interrupts a running one.
#[tauri::command]
pub async fn agent_resume(state: State<'_, AppState>, agent_id: String, path: String) -> CmdResult {
    let target = sessions::checked_session(&state.manager, &agent_id, &path)
        .await
        .map_err(err)?;
    ok(state.manager.open_session(&agent_id, &target).await.map_err(err)?)
}

/// `window.prime.agentSessionDelete`
#[tauri::command]
pub async fn agent_session_delete(state: State<'_, AppState>, agent_id: String, path: String) -> CmdResult {
    let target = sessions::checked_session(&state.manager, &agent_id, &path)
        .await
        .map_err(err)?;
    let remaining = state.manager.delete_session(&agent_id, &target).await.map_err(err)?;
    let store = state.store.clone();
    blocking(move || store.set_session_pinned(&target, false)).await?;
    ok(remaining)
}

/// `window.prime.agentCommands`
#[tauri::command]
pub async fn agent_commands(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    ok(state.manager.get_commands(&agent_id).await)
}

/// A file the page names for the agent to write or read: inside the chat's project, or one the user just chose in a
/// system dialog. Anything else is refused, since the agent writes it as the user.
fn user_file(state: &AppState, agent_id: &str, raw: &str) -> Result<(), String> {
    let project = state.manager.project_path(agent_id).ok_or("Agent not connected")?;
    let named = std::path::PathBuf::from(raw.trim());
    let named = if named.is_absolute() {
        named
    } else {
        project.join(named)
    };
    if validate::is_within(&project, &named) || state.picks.has(&named.to_string_lossy()) {
        return Ok(());
    }
    Err("That file is outside the project. Choose it in the file dialog.".into())
}

/// `window.prime.agentHarness`
#[tauri::command]
pub async fn agent_harness(
    state: State<'_, AppState>,
    agent_id: String,
    action: String,
    input: Option<Value>,
) -> CmdResult {
    let input = input.unwrap_or_else(|| json!({}));
    let named = match action.as_str() {
        "export_jsonl" => input["outputPath"].as_str(),
        "import" => input["path"].as_str(),
        _ => None,
    };
    if let Some(raw) = named {
        user_file(&state, &agent_id, raw)?;
    }
    harness::action(&state.manager, &agent_id, &action, &input)
        .await
        .map_err(err)
}

// ----- fleet -----

/// `window.prime.fleetObserve`
#[tauri::command]
pub async fn fleet_observe(state: State<'_, AppState>, agent_id: String, session_id: String) -> CmdResult {
    fleet::observe(&state.manager, &agent_id, &session_id)
        .await
        .map_err(err)
}

/// `window.prime.fleetUnobserve`
#[tauri::command]
pub async fn fleet_unobserve(session_id: String) -> CmdResult {
    fleet::unobserve(&session_id).await.map_err(err)
}

/// `window.prime.fleetTree`
#[tauri::command]
pub async fn fleet_tree(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    fleet::subagent_tree(&state.manager, &agent_id).await.map_err(err)
}

/// `window.prime.fleetMessages`
#[tauri::command]
pub async fn fleet_messages(state: State<'_, AppState>, agent_id: String, active_session_id: String) -> CmdResult {
    fleet::subagent_messages(&state.manager, &agent_id, &active_session_id)
        .await
        .map_err(err)
}

/// `window.prime.fleetSend`
#[tauri::command]
pub async fn fleet_send(
    state: State<'_, AppState>,
    agent_id: String,
    target: String,
    message: String,
    mode: Option<String>,
) -> CmdResult {
    fleet::send_message(&state.manager, &agent_id, &target, &message, mode.as_deref())
        .await
        .map_err(err)
}

/// `window.prime.fleetSchedules`
#[tauri::command]
pub async fn fleet_schedules(state: State<'_, AppState>) -> CmdResult {
    fleet::list_schedules(&state.manager).await.map_err(err)
}

/// `window.prime.fleetScheduleAdd`
#[tauri::command]
pub async fn fleet_schedule_add(
    state: State<'_, AppState>,
    agent_id: String,
    schedule: String,
    prompt: String,
) -> CmdResult {
    fleet::add_schedule(&state.manager, &agent_id, &schedule, &prompt)
        .await
        .map_err(err)
}

/// `window.prime.fleetScheduleCancel`
#[tauri::command]
pub async fn fleet_schedule_cancel(state: State<'_, AppState>, agent_id: String, job_id: String) -> CmdResult {
    fleet::cancel_schedule(&state.manager, &agent_id, &job_id)
        .await
        .map_err(err)
}

/// `window.prime.fleetHeartbeat`
#[tauri::command]
pub async fn fleet_heartbeat(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    fleet::get_heartbeat(&state.manager, &agent_id).await.map_err(err)
}

/// `window.prime.fleetHeartbeatAction`
#[tauri::command]
pub async fn fleet_heartbeat_action(state: State<'_, AppState>, agent_id: String, action: String) -> CmdResult {
    fleet::heartbeat_action(&state.manager, &agent_id, &action)
        .await
        .map_err(err)
}

/// `window.prime.externalList`: agents running in the Prime CLI, read-only.
#[tauri::command]
pub async fn external_list() -> CmdResult {
    fleet::list_external().await.map_err(err)
}

/// `window.prime.externalMessage`
#[tauri::command]
pub async fn external_message(target: String, message: String) -> CmdResult {
    fleet::message_external(&target, &message).await.map_err(err)
}

// ----- models and keys -----

/// `window.prime.modelsCustomGet`
#[tauri::command]
pub async fn models_custom_get(state: State<'_, AppState>) -> CmdResult {
    let prime = state.prime.clone();
    ok(blocking(move || Ok(prime.custom_providers())).await?)
}

/// `window.prime.modelsCustomSet`: saves a provider, then asks every chat to reload so its models are selectable now.
#[tauri::command]
pub async fn models_custom_set(state: State<'_, AppState>, config: Value) -> CmdResult {
    let text = |key: &str| config.get(key).and_then(Value::as_str).map(str::to_string);
    let models = config["models"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|model| desktop_core::primefiles::CustomModelInput {
                    id: model["id"].as_str().unwrap_or("").to_string(),
                    name: model["name"].as_str().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    let input = desktop_core::primefiles::CustomProviderInput {
        provider: text("provider").unwrap_or_default(),
        base_url: text("baseUrl").unwrap_or_default(),
        api: text("api"),
        api_key: text("apiKey"),
        models,
    };
    let prime = state.prime.clone();
    let providers = blocking(move || prime.set_custom_provider(Some(input))).await?;
    state.manager.reload_agents().await;
    ok(providers)
}

/// `window.prime.modelsCustomRemove`
#[tauri::command]
pub async fn models_custom_remove(state: State<'_, AppState>, provider_id: String) -> CmdResult {
    let prime = state.prime.clone();
    let providers = blocking(move || prime.remove_custom_provider(&provider_id)).await?;
    state.manager.reload_agents().await;
    ok(providers)
}

/// `window.prime.modelsOpenRouterRefresh`
#[tauri::command]
pub async fn models_openrouter_refresh(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    let prime = state.prime.clone();
    blocking(move || prime.refresh_openrouter_models()).await?;
    state
        .manager
        .run_command(&agent_id, json!({"type": "get_available_models"}))
        .await
        .map_err(err)
}

/// `window.prime.authList`
#[tauri::command]
pub async fn auth_list(state: State<'_, AppState>) -> CmdResult {
    let prime = state.prime.clone();
    ok(blocking(move || Ok(prime.list_auth_providers())).await?)
}

/// `window.prime.authSet`
#[tauri::command]
pub async fn auth_set(state: State<'_, AppState>, provider: String, key: String) -> CmdResult {
    let (provider, key) = (
        require_text("provider", &provider, 128)?,
        require_text("API key", &key, 4_096)?,
    );
    let prime = state.prime.clone();
    blocking(move || prime.set_auth_key(&provider, &key)).await?;
    ok(true)
}

/// `window.prime.authRemove`
#[tauri::command]
pub async fn auth_remove(state: State<'_, AppState>, provider: String) -> CmdResult {
    let provider = require_text("provider", &provider, 128)?;
    let prime = state.prime.clone();
    blocking(move || prime.remove_auth(&provider)).await?;
    ok(true)
}

/// `window.prime.authOpenTui`: opens Prime Agent's own screen, where sign-in lives, in a terminal window.
#[tauri::command]
pub async fn auth_open_tui(state: State<'_, AppState>) -> CmdResult {
    let binary = state.manager.binary();
    blocking(move || desktop_core::platform::open_in_terminal(&binary)).await?;
    Ok(Value::Null)
}

// ----- panels -----

/// `window.prime.dashboardSpend`
#[tauri::command]
pub async fn dashboard_spend(state: State<'_, AppState>) -> CmdResult {
    insights::spend(&state.manager).await.map_err(err)
}

/// `window.prime.dashboardModels`: the models the open chats use.
#[tauri::command]
pub async fn dashboard_models(state: State<'_, AppState>) -> CmdResult {
    ok(state
        .manager
        .all_infos()
        .iter()
        .filter_map(|info| info["model"].as_str().map(str::to_string))
        .collect::<Vec<_>>())
}

fn active_agent(state: &AppState) -> Option<String> {
    state
        .store
        .state()
        .active_tab_id
        .map(|tab| desktop_core::agent::manager::primary_agent_id(&tab))
}

/// `window.prime.autonomyGet`
#[tauri::command]
pub async fn autonomy_get(state: State<'_, AppState>) -> CmdResult {
    insights::get_autonomy(&state.manager, active_agent(&state).as_deref())
        .await
        .map_err(err)
}

/// `window.prime.autonomySet`: saves the change, then tells the running agent when autonomous mode was switched.
#[tauri::command]
pub async fn autonomy_set(state: State<'_, AppState>, patch: Value) -> CmdResult {
    let current = state.manager.settings().autonomous;
    let merged = insights::merge_autonomy(&current, &patch).map_err(err)?;
    let store = state.store.clone();
    blocking(move || store.set_settings(&json!({"autonomous": merged}))).await?;
    let agent = active_agent(&state);
    insights::apply_autonomy(
        &state.manager,
        agent.as_deref(),
        patch.get("enabled").and_then(Value::as_bool),
    )
    .await;
    insights::get_autonomy(&state.manager, agent.as_deref())
        .await
        .map_err(err)
}

/// `window.prime.skillsList`
#[tauri::command]
pub async fn skills_list(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    insights::list_skills(&state.manager, &agent_id).await.map_err(err)
}

/// `window.prime.skillsInstall`
#[tauri::command]
pub async fn skills_install(state: State<'_, AppState>, source: String) -> CmdResult {
    insights::install_skill_package(&state.manager, &source)
        .await
        .map_err(err)
}

/// `window.prime.dialogRespond`
#[tauri::command]
pub async fn dialog_respond(
    state: State<'_, AppState>,
    dialog_id: String,
    value: Value,
    cancelled: Option<bool>,
) -> CmdResult {
    state
        .manager
        .respond_dialog(&dialog_id, value, cancelled.unwrap_or(false))
        .await
        .map_err(err)?;
    Ok(Value::Null)
}

/// `window.prime.rlmGet`
#[tauri::command]
pub async fn rlm_get(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    let current = state.store.state().settings.get("rlmMaxDepth").and_then(Value::as_i64);
    let prime = state.prime.clone();
    let fallback = match current {
        Some(depth) => depth,
        None => blocking(move || Ok(prime.read_rlm_max_depth(1))).await?,
    };
    insights::get_rlm_max_depth(&state.manager, &agent_id, fallback)
        .await
        .map_err(err)
}

/// `window.prime.rlmSet`
#[tauri::command]
pub async fn rlm_set(state: State<'_, AppState>, agent_id: String, max_depth: u32, global: Option<bool>) -> CmdResult {
    let result = insights::set_rlm_max_depth(&state.manager, &agent_id, i64::from(max_depth), global != Some(false))
        .await
        .map_err(err)?;
    let depth = result["maxDepth"].as_i64().unwrap_or(i64::from(max_depth));
    let (store, prime) = (state.store.clone(), state.prime.clone());
    blocking(move || {
        store.set_settings(&json!({"rlmMaxDepth": depth}))?;
        prime.write_rlm_max_depth(depth)
    })
    .await?;
    Ok(result)
}

/// `window.prime.exportHtml`
#[tauri::command]
pub async fn session_export_html(
    state: State<'_, AppState>,
    agent_id: String,
    output_path: Option<String>,
) -> CmdResult {
    if let Some(raw) = output_path.as_deref() {
        user_file(&state, &agent_id, raw)?;
    }
    ok(harness::export_html(&state.manager, &agent_id, output_path.as_deref())
        .await
        .map_err(err)?)
}

/// `window.prime.shareSession`
#[tauri::command]
pub async fn session_share(state: State<'_, AppState>, agent_id: String) -> CmdResult {
    harness::share_session(&state.manager, &agent_id).await.map_err(err)
}

// ----- the sandbox (new in this host; the Electron build has no equivalent) -----

/// What the sandbox is doing right now and what it cannot hold.
#[tauri::command]
pub async fn sandbox_state(state: State<'_, AppState>) -> CmdResult {
    ok(state.sandbox.lock().map_err(err)?.clone())
}

/// Chooses protected or unprotected, prepares it, and starts the agent again with the result. The chats' work in progress
/// is not lost: they are saved sessions the daemon opens again.
#[tauri::command]
pub async fn sandbox_set(app: AppHandle, state: State<'_, AppState>, mode: String) -> CmdResult {
    let mode = if mode == "unprotected" {
        "unprotected"
    } else {
        "protected"
    };
    let store = state.store.clone();
    blocking(move || store.set_settings(&json!({"sandbox": mode}))).await?;
    crate::runtime::prepare(&app).await;
    state.manager.restart_daemon().await;
    ok(state.sandbox.lock().map_err(err)?.clone())
}

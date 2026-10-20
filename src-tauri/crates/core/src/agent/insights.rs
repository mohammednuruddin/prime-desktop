//! What the dashboard and the settings panels show of the agent: spend, autonomy, skills and the recursion depth.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::harness::run_prime;
use super::manager::Manager;
use super::sessions::{now_ms, parse_iso_ms, read_session_id, sessions_dir, utc_day};
use super::shared::base_name;
use crate::{platform, validate};

/// How long one question to the daemon may take before the panel shows what it has.
const ASK: Duration = Duration::from_secs(5);

/// The last chunk of a session file that is read for its token use: files run to many megabytes.
const TAIL_BYTES: u64 = 512 * 1024;
const TAIL_LINES: usize = 200;
const DAY: Duration = Duration::from_secs(24 * 3600);

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------- spend ----------

fn count(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|number| number.max(0.0) as u64))
        .unwrap_or(0)
}

fn tail_lines(path: &Path) -> Option<Vec<String>> {
    let mut file = File::open(path).ok()?;
    let start = file.metadata().ok()?.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if start > 0 {
        // The chunk starts mid-line.
        lines.next();
    }
    let lines: Vec<&str> = lines.collect();
    Some(
        lines[lines.len().saturating_sub(TAIL_LINES)..]
            .iter()
            .map(|line| (*line).to_string())
            .collect(),
    )
}

/// The newest token counts in a session file's last records, and the day of its newest timestamp.
fn latest_usage(lines: &[String], fallback_day: &str) -> Option<(String, u64, u64)> {
    let mut usage = None;
    let mut day = fallback_day.to_string();
    for line in lines {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let found = Some(&record["usage"])
            .filter(|usage| !usage.is_null())
            .or_else(|| Some(&record["message"]["usage"]).filter(|usage| !usage.is_null()));
        if let Some(found) = found {
            let (input, output) = (count(&found["input"]), count(&found["output"]));
            if input > 0 || output > 0 {
                usage = Some((input, output));
            }
        }
        let stamp = match &record["timestamp"] {
            Value::String(text) => parse_iso_ms(text),
            Value::Number(number) => number.as_f64(),
            _ => None,
        };
        if let Some(stamped) = stamp.and_then(utc_day) {
            day = stamped;
        }
    }
    usage
        .filter(|(input, _)| *input > 0)
        .map(|(input, output)| (day, input, output))
}

/// Token use per day from the chats saved in the last 24 hours, other than those listed as open (their live totals are
/// counted separately, and counting the file as well would count them twice).
fn recent_usage(dir: &Path, now: f64, open: &HashSet<String>) -> Vec<(String, u64, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let today = utc_day(now).unwrap_or_default();
    let mut found = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let recent = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            // A modification time in the future (clock skew) counts as new.
            .is_some_and(|modified| SystemTime::now().duration_since(modified).unwrap_or_default() <= DAY);
        let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("");
        if !recent || open.contains(stem) || read_session_id(&path).is_some_and(|id| open.contains(&id)) {
            continue;
        }
        if let Some(usage) = tail_lines(&path).and_then(|lines| latest_usage(&lines, &today)) {
            found.push(usage);
        }
    }
    found
}

/// `dashboard:spend`: tokens and cost of the open chats, with the chats saved in the last day on top.
pub async fn spend(manager: &Manager) -> Result<Value> {
    let today = utc_day(now_ms()).unwrap_or_default();
    let mut days: BTreeMap<String, (f64, u64, u64)> = BTreeMap::new();
    let mut open = HashSet::new();
    let (mut total_cost, mut total_in, mut total_out) = (0.0_f64, 0_u64, 0_u64);
    for chat in manager.chats() {
        if let Some(id) = lock(&chat.state).info.session_id.clone() {
            open.insert(id);
        }
        let stats = tokio::time::timeout(ASK, manager.get_stats(&chat.id))
            .await
            .unwrap_or(Value::Null);
        if stats.is_null() {
            continue;
        }
        let (input, output) = (count(&stats["tokens"]["input"]), count(&stats["tokens"]["output"]));
        let cost = stats["cost"].as_f64().unwrap_or(0.0);
        total_cost += cost;
        total_in += input;
        total_out += output;
        let day = days.entry(today.clone()).or_default();
        day.0 += cost;
        day.1 += input;
        day.2 += output;
    }
    let saved = tokio::task::spawn_blocking(move || recent_usage(&sessions_dir(), now_ms(), &open)).await?;
    for (day, input, output) in saved {
        let entry = days.entry(day).or_default();
        entry.1 += input;
        entry.2 += output;
    }
    let points: Vec<Value> = days
        .into_iter()
        .map(
            |(date, (cost, input, output))| json!({"date": date, "cost": cost, "tokensIn": input, "tokensOut": output}),
        )
        .collect();
    Ok(json!({
        "points": points,
        "totals": {"cost": total_cost, "tokensIn": total_in, "tokensOut": total_out},
    }))
}

// ---------- autonomy ----------

const AUTONOMY_NUMBERS: [&str; 5] = ["gateRetries", "maxContinuations", "maxTurns", "maxTokens", "maxSeconds"];

/// The autonomous settings with a renderer's change applied. Only the known settings are accepted, each of the kind it
/// must be: the gate commands run in the user's project, so a malformed list must not get as far as the agent.
pub fn merge_autonomy(current: &Value, patch: &Value) -> Result<Value> {
    let Some(patch) = patch.as_object() else {
        bail!("Autonomous settings must be an object");
    };
    let mut merged = current.as_object().cloned().unwrap_or_default();
    for (key, value) in patch {
        match key.as_str() {
            "enabled" if value.is_boolean() => {}
            "enabled" => bail!("enabled must be true or false"),
            "gates" => {
                let Some(gates) = value.as_array().filter(|gates| gates.len() <= 20) else {
                    bail!("gates must be a list of at most 20 commands");
                };
                for gate in gates {
                    validate::non_empty("gate command", gate.as_str().unwrap_or(""), 2000)?;
                }
            }
            name if AUTONOMY_NUMBERS.contains(&name) => {
                let number = value.as_f64().unwrap_or(f64::NAN);
                validate::number(name, number, Some(0.0), Some(100_000_000.0))?;
                if number.fract() != 0.0 {
                    bail!("{name} must be a whole number");
                }
            }
            other => bail!("Unknown autonomous setting: {other}"),
        }
        merged.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(merged))
}

fn progress_of(config: &Value, goal: &Value) -> Value {
    let used = |key: &str| goal.get(key).map_or(0, count);
    let gates: Vec<Value> = config["gates"]
        .as_array()
        .map(|gates| {
            gates
                .iter()
                .map(|command| json!({"command": command, "lastResult": null, "attempts": 0}))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "turns": 0,
        "maxTurns": config["maxTurns"],
        "tokens": used("tokensUsed"),
        "maxTokens": config["maxTokens"],
        "seconds": used("timeUsedSeconds"),
        "maxSeconds": config["maxSeconds"],
        "continuations": used("continuationsUsed"),
        "maxContinuations": config["maxContinuations"],
        "active": goal.get("active").and_then(Value::as_bool).unwrap_or(false),
        "gates": gates,
    })
}

/// `autonomy:get`: the saved settings, and the running goal of the agent when it has one.
pub async fn get_autonomy(manager: &Manager, agent_id: Option<&str>) -> Result<Value> {
    let config = manager.settings().autonomous;
    let session = agent_id.and_then(|id| manager.chat(id)).and_then(|chat| chat.session());
    let goal = match session {
        Some(session) => match tokio::time::timeout(ASK, session.send(&json!({"type": "get_state"}))).await {
            Ok(Ok(state)) => state["goal"].clone(),
            _ => Value::Null,
        },
        None => Value::Null,
    };
    Ok(json!({"progress": progress_of(&config, &goal), "config": config, "goal": goal}))
}

/// Tells a running agent that autonomous mode was switched, after the setting is saved. A stopped agent reads the saved
/// setting when it starts.
pub async fn apply_autonomy(manager: &Manager, agent_id: Option<&str>, enabled: Option<bool>) {
    let (Some(enabled), Some(chat)) = (enabled, agent_id.and_then(|id| manager.chat(id))) else {
        return;
    };
    if let Some(session) = chat.session() {
        let message = format!("/autonomous {}", if enabled { "on" } else { "off" });
        let _ = tokio::time::timeout(ASK, session.send(&json!({"type": "prompt", "message": message}))).await;
    }
}

// ---------- skills ----------

/// The skills, prompt templates and extensions of a resource snapshot, in the renderer's `SkillInfo` shape.
fn skills_from_snapshot(snapshot: &Value) -> Vec<Value> {
    let text = |value: &Value| value.as_str().unwrap_or("").to_string();
    let scope = |item: &Value| text(&item["sourceInfo"]["scope"]);
    let items = |key: &str| snapshot[key].as_array().cloned().unwrap_or_default();
    let mut found = Vec::new();
    for (key, source) in [("skills", "skill"), ("prompts", "prompt")] {
        for item in items(key) {
            found.push(json!({
                "name": text(&item["name"]),
                "description": text(&item["description"]),
                "source": source,
                "location": scope(&item),
                "path": text(&item["filePath"]),
            }));
        }
    }
    for item in items("extensions") {
        let path = text(&item["path"]);
        let name = Some(base_name(&path))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "extension".into());
        found.push(json!({
            "name": name,
            "description": "Prime Agent extension",
            "source": "extension",
            "location": scope(&item),
            "path": path,
        }));
    }
    found
}

/// The skills and prompt templates among a chat's slash commands.
fn skills_from_commands(commands: &[Value]) -> Vec<Value> {
    commands
        .iter()
        .filter(|command| matches!(command["source"].as_str(), Some("skill" | "prompt")))
        .map(|command| {
            json!({
                "name": command["name"],
                "description": command["description"].as_str().unwrap_or(""),
                "source": command["source"],
                "location": command["location"],
                "path": command["path"],
            })
        })
        .collect()
}

/// `skills:list`: remembered per chat after the first answer.
pub async fn list_skills(manager: &Manager, agent_id: &str) -> Result<Value> {
    let Some(chat) = manager.chat(agent_id) else {
        return Ok(json!([]));
    };
    let known = lock(&chat.state).skills.clone();
    if !known.is_empty() {
        return Ok(Value::Array(known));
    }
    let mut found = Vec::new();
    if let Some(session) = chat.session()
        && let Ok(Ok(snapshot)) = tokio::time::timeout(ASK, session.call("get_resource_snapshot", json!({}))).await
    {
        found = skills_from_snapshot(&snapshot);
    }
    if found.is_empty() {
        found = skills_from_commands(&manager.get_commands(agent_id).await);
        lock(&chat.state).skills = found.clone();
        manager.publish(&chat);
    } else {
        lock(&chat.state).skills = found.clone();
    }
    Ok(Value::Array(found))
}

/// `skills:install`: `prime-agent package install <source>`, run from a project folder (or home).
pub async fn install_skill_package(manager: &Manager, source: &str) -> Result<Value> {
    let source = validate::non_empty("package source", source, 2000)?;
    if source.starts_with('-') || source.contains('\0') {
        bail!("package source is not a package");
    }
    let cwd: PathBuf = manager
        .chats()
        .first()
        .map(|chat| chat.path())
        .filter(|path| path.is_dir())
        .unwrap_or_else(platform::home_dir);
    let done = run_prime(manager, &["package", "install", &source], Some(cwd)).await;
    if done["ok"].as_bool().unwrap_or(false) {
        Ok(json!({"ok": true}))
    } else {
        let why = done["output"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or("The install failed.");
        Ok(json!({"ok": false, "error": why}))
    }
}

// ---------- recursion depth ----------

/// `rlm:get`: what the running agent reports, or the saved value when it is not running.
pub async fn get_rlm_max_depth(manager: &Manager, agent_id: &str, fallback: i64) -> Result<Value> {
    if let Some(session) = manager.chat(agent_id).and_then(|chat| chat.session()) {
        return tokio::time::timeout(ASK, session.send(&json!({"type": "get_rlm_max_depth_status"})))
            .await
            .map_err(|_| anyhow::anyhow!("Prime Agent did not answer"))?;
    }
    Ok(json!({"maxDepth": fallback, "source": "global"}))
}

/// `rlm:set`: applies the depth (0 to 10) to the running agent; the caller saves it.
pub async fn set_rlm_max_depth(manager: &Manager, agent_id: &str, max_depth: i64, global: bool) -> Result<Value> {
    let clamped = max_depth.clamp(0, 10);
    if let Some(session) = manager.chat(agent_id).and_then(|chat| chat.session()) {
        let answer = tokio::time::timeout(
            ASK,
            session.send(&json!({"type": "set_rlm_max_depth", "maxDepth": clamped, "global": global})),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Prime Agent did not answer"))??;
        return Ok(json!({"maxDepth": answer["maxDepth"], "source": answer["source"]}));
    }
    Ok(json!({"maxDepth": clamped, "source": "global"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::UNIX_EPOCH;

    fn session_file(dir: &Path, name: &str, records: &[Value]) {
        let mut file = File::create(dir.join(name)).unwrap();
        for record in records {
            writeln!(file, "{record}").unwrap();
        }
    }

    #[test]
    fn the_newest_usage_and_day_in_a_session_file_are_read() {
        let lines: Vec<String> = [
            r#"{"timestamp":"2026-10-14T08:00:00.000Z","message":{"usage":{"input":10,"output":2}}}"#,
            "not json",
            r#"{"timestamp":"2026-10-15T09:30:00.000Z","usage":{"input":120,"output":45}}"#,
            r#"{"timestamp":"2026-10-15T09:31:00.000Z","message":{"role":"user"}}"#,
        ]
        .iter()
        .map(|line| (*line).to_string())
        .collect();
        assert_eq!(latest_usage(&lines, "2026-10-16"), Some(("2026-10-15".into(), 120, 45)));
        assert_eq!(latest_usage(&lines[1..2], "2026-10-16"), None, "no counts is no usage");
    }

    #[test]
    fn recent_chats_count_once_and_open_ones_not_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let usage = |input: u64| json!({"timestamp": "2026-10-16T01:00:00Z", "usage": {"input": input, "output": 1}});
        session_file(dir.path(), "closed.jsonl", &[json!({"id": "closed"}), usage(5)]);
        session_file(dir.path(), "open.jsonl", &[json!({"id": "open"}), usage(7)]);
        session_file(dir.path(), "renamed.jsonl", &[json!({"id": "live-id"}), usage(9)]);
        std::fs::write(dir.path().join("notes.txt"), "not a session").unwrap();
        let open: HashSet<String> = ["open".to_string(), "live-id".to_string()].into();
        let found = recent_usage(dir.path(), parse_iso_ms("2026-10-16T12:00:00Z").unwrap(), &open);
        assert_eq!(found, vec![("2026-10-16".to_string(), 5, 1)]);
    }

    #[test]
    fn a_large_file_is_read_from_its_tail_without_its_cut_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jsonl");
        let mut file = File::create(&path).unwrap();
        let filler = json!({"text": "x".repeat(1000)}).to_string();
        for _ in 0..1500 {
            writeln!(file, "{filler}").unwrap();
        }
        writeln!(file, r#"{{"usage":{{"input":3,"output":4}}}}"#).unwrap();
        let lines = tail_lines(&path).unwrap();
        assert!(lines.len() <= TAIL_LINES);
        assert!(
            lines.iter().all(|line| serde_json::from_str::<Value>(line).is_ok()),
            "no cut line"
        );
        assert_eq!(
            latest_usage(&lines, "d").map(|(_, input, output)| (input, output)),
            Some((3, 4))
        );
    }

    #[test]
    fn autonomy_changes_are_checked_and_merged() {
        let current = json!({"enabled": false, "gates": [], "maxTurns": 30});
        let merged = merge_autonomy(
            &current,
            &json!({"enabled": true, "gates": ["cargo test"], "maxTurns": 5}),
        )
        .unwrap();
        assert_eq!(merged, json!({"enabled": true, "gates": ["cargo test"], "maxTurns": 5}));
        for bad in [
            json!({"enabled": "yes"}),
            json!({"maxTurns": -1}),
            json!({"maxTurns": 1.5}),
            json!({"maxTurns": "9"}),
            json!({"gates": "make"}),
            json!({"gates": [""]}),
            json!({"gates": [1]}),
            json!({"surprise": 1}),
        ] {
            assert!(merge_autonomy(&current, &bad).is_err(), "{bad} was accepted");
        }
        assert!(merge_autonomy(&current, &json!([1])).is_err());
    }

    #[test]
    fn progress_reads_the_goal_and_the_limits() {
        let config =
            json!({"gates": ["make"], "maxTurns": 30, "maxTokens": 1000, "maxSeconds": 60, "maxContinuations": 8});
        let goal = json!({"active": true, "tokensUsed": 400, "timeUsedSeconds": 12, "continuationsUsed": 2});
        let progress = progress_of(&config, &goal);
        assert_eq!(progress["tokens"], 400);
        assert_eq!(progress["maxTokens"], 1000);
        assert_eq!(progress["seconds"], 12);
        assert_eq!(progress["continuations"], 2);
        assert_eq!(progress["active"], true);
        assert_eq!(
            progress["gates"],
            json!([{"command": "make", "lastResult": null, "attempts": 0}])
        );
        assert_eq!(progress_of(&config, &Value::Null)["active"], false);
    }

    #[test]
    fn skills_come_from_the_snapshot_or_the_commands() {
        let snapshot = json!({
            "skills": [{"name": "review", "description": "Reviews", "filePath": "/s/review.md", "sourceInfo": {"scope": "user"}}],
            "prompts": [{"name": "fix", "filePath": "/p/fix.md"}],
            "extensions": [{"path": "C:\\ext\\notes.js"}, {"path": ""}],
        });
        let found = skills_from_snapshot(&snapshot);
        let names: Vec<_> = found.iter().map(|skill| skill["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["review", "fix", "notes.js", "extension"]);
        assert_eq!(found[0]["location"], "user");
        assert_eq!(found[1]["location"], "");
        assert_eq!(found[2]["source"], "extension");
        let commands = [
            json!({"name": "a", "source": "skill", "description": "A"}),
            json!({"name": "b", "source": "extension"}),
            json!({"name": "c", "source": "prompt"}),
        ];
        let from_commands = skills_from_commands(&commands);
        assert_eq!(from_commands.len(), 2);
        assert_eq!(from_commands[1]["description"], "");
    }

    #[test]
    fn a_file_dated_in_the_future_counts_as_recent() {
        let dir = tempfile::tempdir().unwrap();
        session_file(dir.path(), "now.jsonl", &[json!({"usage": {"input": 1, "output": 1}})]);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as f64;
        assert_eq!(recent_usage(dir.path(), now, &HashSet::new()).len(), 1);
    }
}

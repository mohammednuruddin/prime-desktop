//! Saved chats: listing the session files, reading their ids, finding a file by session id.
//!
//! Prime Agent writes one `<session id>.jsonl` per chat under its sessions folder, one JSON record per line. The app
//! lists them for the sidebar, so listing has to stay cheap: files reach megabytes, and the sidebar asks every 15 s per
//! open project. A summary is cached per file and reused while the file's size and modification time are unchanged.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;

use super::manager::Manager;
use super::shared::base_name;
use crate::{platform, validate};

/// The renderer's `SessionSummary`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_file: String,
    pub session_id: String,
    pub message_count: u64,
    pub working_directory: Option<String>,
    pub mtime: f64,
    pub name: Option<String>,
}

struct Cached {
    modified: SystemTime,
    size: u64,
    /// `None` for a file that holds no chat yet (the daemon creates a session at start-up, before any prompt).
    summary: Option<SessionSummary>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(Mutex::default);

fn cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Cached>> {
    CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn sessions_dir() -> PathBuf {
    platform::prime_agent_dir().join("sessions")
}

/// Chats saved under Prime Agent's sessions folder, newest first; only those of `project` when given.
pub fn list_sessions(project: Option<&Path>) -> Vec<SessionSummary> {
    list_in(&sessions_dir(), project)
}

fn list_in(dir: &Path, project: Option<&Path>) -> Vec<SessionSummary> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let files: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
        .map(|entry| entry.path())
        .collect();
    let present: HashSet<&PathBuf> = files.iter().collect();
    cache().retain(|path, _| path.parent() != Some(dir) || present.contains(path));

    let mut out = Vec::new();
    for path in &files {
        // A session can be deleted between the directory read and the stat; skip it rather than fail the whole list.
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        let cached = cache()
            .get(path)
            .filter(|cached| cached.modified == modified && cached.size == meta.len())
            .map(|cached| cached.summary.clone());
        let summary = match cached {
            Some(summary) => summary,
            None => {
                let Ok(summary) = summarize(path, epoch_ms(modified)) else {
                    continue;
                };
                cache().insert(
                    path.clone(),
                    Cached {
                        modified,
                        size: meta.len(),
                        summary: summary.clone(),
                    },
                );
                summary
            }
        };
        out.extend(summary.filter(|summary| in_project(summary.working_directory.as_deref(), project)));
    }
    out.sort_by(|a, b| b.mtime.total_cmp(&a.mtime));
    out
}

fn epoch_ms(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// Reads one session file in a single pass without holding its records: only the header, the counts and the first
/// prompt are kept. A line that does not parse (the daemon may be appending to it right now) is skipped, so a chat
/// being written does not vanish from the list.
fn summarize(path: &Path, modified_ms: f64) -> std::io::Result<Option<SessionSummary>> {
    let mut reader = BufReader::new(File::open(path)?);
    let (mut first, mut header) = (None::<Value>, None::<Value>);
    let (mut messages, mut first_prompt, mut name, mut prompt_at) = (0u64, None::<String>, None::<String>, None);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let Ok(record) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if first.is_none() {
            first = Some(record.clone());
        }
        let kind = record["type"].as_str().unwrap_or("");
        if kind == "session" && header.is_none() {
            header = Some(record.clone());
        }
        // The latest rename wins. `sessionName` counts on any record; `name` only on the rename records.
        let named = match record.get("sessionName").filter(|value| !value.is_null()) {
            Some(value) => Some(value),
            None if kind == "session_name_change" || kind == "session_info" => record.get("name"),
            None => None,
        };
        if let Some(text) = named.and_then(Value::as_str).filter(|text| !text.trim().is_empty()) {
            name = Some(text.to_string());
        }
        if kind == "message" {
            messages += 1;
            let message = &record["message"];
            if message["role"] == "user" {
                if first_prompt.is_none() {
                    first_prompt = Some(extract_session_text(&message["content"]));
                }
                prompt_at = prompt_time(&record).or(prompt_at);
            }
        }
    }
    // Starting the app creates a resident session before the user sends anything. That is not a chat and must not
    // take a blank row in the sidebar.
    if first_prompt.is_none() && name.is_none() {
        return Ok(None);
    }
    let header = header.or(first).unwrap_or(Value::Null);
    let working_directory = [
        header.get("cwd"),
        header.get("workingDirectory"),
        header["entries"].get(0).and_then(|entry| entry.get("workingDirectory")),
    ]
    .into_iter()
    .flatten()
    .find(|value| !value.is_null())
    .and_then(Value::as_str)
    .filter(|dir| !dir.is_empty())
    .map(str::to_string);
    let stem = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let stem = stem.strip_suffix(".jsonl").unwrap_or(&stem);
    Ok(Some(SessionSummary {
        session_file: path.to_string_lossy().into_owned(),
        session_id: ["id", "sessionId"]
            .iter()
            .filter_map(|key| header.get(*key))
            .find(|value| !value.is_null())
            .and_then(scalar_text)
            .unwrap_or_else(|| stem.to_string()),
        message_count: if messages > 0 {
            messages
        } else {
            header.get("messageCount").and_then(number_of).map_or(0, |n| n as u64)
        },
        working_directory,
        mtime: prompt_at.unwrap_or(modified_ms),
        name: name.or_else(|| session_title(&first_prompt.unwrap_or_default())),
    }))
}

/// JavaScript's `String(value)` for the scalar types a session header holds.
pub(crate) fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(_) | Value::Bool(_) => Some(match number_of(value) {
            Some(n) if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 => (n as i64).to_string(),
            _ => value.to_string(),
        }),
        _ => None,
    }
}

/// JavaScript's `Number(value)` for numbers and numeric strings.
pub(crate) fn number_of(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(text) => text.trim().parse().ok().filter(|n: &f64| n.is_finite()),
        _ => None,
    }
}

/// Whether a session started in `dir` belongs to `project`: the same folder however it was written, or a folder that
/// no longer exists but carries the project's name (the project was moved or renamed).
fn in_project(dir: Option<&str>, project: Option<&Path>) -> bool {
    let Some(project) = project
        .map(|project| project.to_string_lossy())
        .filter(|project| !project.is_empty())
    else {
        return true;
    };
    dir.is_some_and(|dir| {
        dir_key(dir) == dir_key(&project) || (base_name(dir) == base_name(&project) && !Path::new(dir).exists())
    })
}

/// A folder as text that is equal for the same place however it was written: either separator, a trailing one, `.` and
/// `..` segments, the `\\?\` prefix, and (for Windows paths) letter case do not matter.
fn dir_key(dir: &str) -> String {
    let dir = validate::simplify(PathBuf::from(dir)).to_string_lossy().into_owned();
    let windows = dir.as_bytes().get(1) == Some(&b':') || dir.starts_with(r"\\");
    let mut parts: Vec<&str> = Vec::new();
    for part in dir.split(if windows { &['/', '\\'][..] } else { &['/'][..] }) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let key = format!("{}{}", if dir.starts_with('/') { "/" } else { "" }, parts.join("/"));
    if windows { key.to_lowercase() } else { key }
}

/// The text of a message's content: a string, or the `text` of each block joined by spaces.
pub fn extract_session_text(content: &Value) -> String {
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

/// A chat's sidebar name when it has none: its first prompt on one line, 58 characters at most.
pub fn session_title(text: &str) -> Option<String> {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    if one_line.chars().count() > 58 {
        let head: String = one_line.chars().take(57).collect();
        return Some(format!("{}…", head.trim_end()));
    }
    Some(one_line)
}

/// When a user message was sent: the message's own time, else its record's.
fn prompt_time(record: &Value) -> Option<f64> {
    if record["type"] != "message" || record["message"]["role"] != "user" {
        return None;
    }
    match record["message"].get("timestamp").and_then(Value::as_f64) {
        Some(at) if at.is_finite() => Some(at),
        _ => record["timestamp"].as_str().and_then(parse_iso_ms),
    }
}

/// Opening a chat appends model, thinking-level and state records, so a file's modification time moves a chat to the top
/// just for being viewed. Chats are ordered by the last prompt the user sent instead.
pub fn last_prompt_at(records: &[Value]) -> Option<f64> {
    records.iter().rev().find_map(prompt_time)
}

/// Milliseconds since the Unix epoch for an ISO 8601 time (`2026-09-30T10:01:00.000Z`, a date alone, `+02:00` offsets).
/// A time with no offset is read as UTC.
pub fn parse_iso_ms(text: &str) -> Option<f64> {
    static SHAPE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"^(\d{4})-(\d{2})-(\d{2})(?:[Tt ](\d{2}):(\d{2})(?::(\d{2})(?:[.,](\d+))?)?)?\s*(?:([Zz])|([+-])(\d{2}):?(\d{2}))?$",
        )
        .unwrap()
    });
    let caps = SHAPE.captures(text.trim())?;
    let number = |index: usize| caps.get(index).map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));
    let (year, month, day, hour, minute, second) = (number(1), number(2), number(3), number(4), number(5), number(6));
    let days = days_from_civil(year, month, day);
    if !(1..=12).contains(&month)
        || civil_from_days(days) != (year, month, day)
        || hour > 24
        || minute > 59
        || second > 59
    {
        return None;
    }
    let millis = caps.get(7).map_or(0, |m| {
        format!("{:0<3}", &m.as_str()[..m.as_str().len().min(3)])
            .parse::<i64>()
            .unwrap_or(0)
    });
    let offset = match (caps.get(9), number(10), number(11)) {
        (Some(sign), hours, minutes) if hours < 24 && minutes < 60 => {
            (hours * 60 + minutes) * if sign.as_str() == "-" { -1 } else { 1 }
        }
        (Some(_), ..) => return None,
        _ => 0,
    };
    Some((((days * 24 + hour) * 60 + minute - offset) * 60 + second) as f64 * 1000.0 + millis as f64)
}

/// The UTC date, `YYYY-MM-DD`, of a time in milliseconds since the Unix epoch.
pub fn utc_day(ms: f64) -> Option<String> {
    if !ms.is_finite() {
        return None;
    }
    let (year, month, day) = civil_from_days((ms / 86_400_000.0).floor() as i64);
    (0..=9999)
        .contains(&year)
        .then(|| format!("{year:04}-{month:02}-{day:02}"))
}

pub fn now_ms() -> f64 {
    epoch_ms(SystemTime::now())
}

// Howard Hinnant's civil-calendar algorithms: days since 1970-01-01 and back, for the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// The id a session file carries in its header. Only the first line is read: session files can be many megabytes.
pub fn read_session_id(path: &Path) -> Option<String> {
    let mut head = Vec::new();
    File::open(path).ok()?.take(16_384).read_to_end(&mut head).ok()?;
    let header: Value = serde_json::from_slice(head.split(|byte| *byte == b'\n').next()?).ok()?;
    let id = header
        .get("id")
        .filter(|id| !id.is_null())
        .or_else(|| header.get("sessionId"))?;
    id.as_str().filter(|id| !id.is_empty()).map(str::to_string)
}

/// The session file for a session id, when one exists.
pub fn session_file_for(session_id: &str) -> Option<PathBuf> {
    session_file_in(&sessions_dir(), session_id)
}

fn session_file_in(dir: &Path, session_id: &str) -> Option<PathBuf> {
    // The id comes from the daemon, but it names a file here, so it must not be able to leave the folder.
    if session_id.is_empty() || session_id.contains(['/', '\\', '\0', ':']) {
        return None;
    }
    let named = dir.join(format!("{session_id}.jsonl"));
    if named.is_file() {
        return Some(named);
    }
    // A file the daemon named differently: the listing knows which id it holds.
    cache()
        .iter()
        .find(|(path, cached)| {
            path.parent() == Some(dir) && cached.summary.as_ref().is_some_and(|s| s.session_id == session_id)
        })
        .map(|(path, _)| path.clone())
        .filter(|path| path.is_file())
}

/// A session file the renderer named, checked to be one of the project's saved chats. The daemon opens or deletes
/// exactly this path, so a path outside the sessions folder, or another project's chat, is refused. Returns the path as
/// the listing spells it (what `Manager::delete_session` compares against, and what the pins are keyed by).
pub async fn checked_session(manager: &Manager, agent_id: &str, path: &str) -> Result<String> {
    let target = validate::existing_file("session path", path)?;
    let project = manager.project_path(agent_id);
    let listed = tokio::task::spawn_blocking(move || list_sessions(project.as_deref())).await?;
    listed
        .into_iter()
        .map(|session| session.session_file)
        .find(|file| validate::resolve(Path::new(file)).is_ok_and(|resolved| resolved == target))
        .ok_or_else(|| anyhow!("Session does not belong to this project"))
}

/// `agent:sessions`: the chats of the agent's project, or all of them for an agent the manager does not know.
pub async fn agent_sessions(manager: &Manager, agent_id: Option<&str>) -> Result<Value> {
    let project = agent_id.and_then(|id| manager.project_path(id));
    let listed = tokio::task::spawn_blocking(move || list_sessions(project.as_deref())).await?;
    Ok(serde_json::to_value(listed)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write(dir: &Path, name: &str, records: &[Value]) -> PathBuf {
        let path = dir.join(name);
        let mut file = File::create(&path).unwrap();
        for record in records {
            writeln!(file, "{record}").unwrap();
        }
        path
    }

    fn header(id: &str, cwd: &str) -> Value {
        json!({"type": "session", "id": id, "timestamp": "2026-09-30T10:00:00.000Z", "cwd": cwd})
    }

    fn user(text: &str, at: u64) -> Value {
        json!({"type": "message", "timestamp": "2026-09-30T10:01:00.000Z", "message": {"role": "user", "content": text, "timestamp": at}})
    }

    #[test]
    fn chats_are_ordered_by_the_last_prompt_not_by_later_bookkeeping() {
        let records = [
            json!({"type": "session", "timestamp": "2026-09-30T10:00:00.000Z"}),
            json!({"type": "message", "timestamp": "2026-09-30T10:01:00.000Z", "message": {"role": "user", "timestamp": 1000}}),
            json!({"type": "message", "timestamp": "2026-09-30T10:02:00.000Z", "message": {"role": "assistant", "timestamp": 2000}}),
            json!({"type": "message", "timestamp": "2026-09-30T10:03:00.000Z", "message": {"role": "user", "timestamp": 3000}}),
            json!({"type": "model_change", "timestamp": "2026-10-02T09:00:00.000Z"}),
            json!({"type": "session_state", "timestamp": "2026-10-02T09:00:00.000Z"}),
        ];
        assert_eq!(last_prompt_at(&records), Some(3000.0));
        let by_record_time =
            [json!({"type": "message", "timestamp": "2026-09-30T10:01:00.000Z", "message": {"role": "user"}})];
        assert_eq!(
            last_prompt_at(&by_record_time),
            parse_iso_ms("2026-09-30T10:01:00.000Z")
        );
        assert_eq!(last_prompt_at(&[json!({"type": "session"})]), None);
        // A prompt with no usable time defers to the one before it.
        let records = [
            json!({"type": "message", "message": {"role": "user", "timestamp": 500}}),
            json!({"type": "message", "message": {"role": "user", "timestamp": "soon"}, "timestamp": "never"}),
        ];
        assert_eq!(last_prompt_at(&records), Some(500.0));
    }

    #[test]
    fn iso_times_parse_like_javascript_and_bad_ones_do_not() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_iso_ms("2026-10-16T16:35:11.816Z"), Some(1_792_168_511_816.0));
        assert_eq!(parse_iso_ms("2026-10-16T16:35:11.8Z"), Some(1_792_168_511_800.0));
        assert_eq!(parse_iso_ms("2026-10-16T16:35:11.123456Z"), Some(1_792_168_511_123.0));
        assert_eq!(parse_iso_ms("2026-10-16T18:35:11+02:00"), Some(1_792_168_511_000.0));
        assert_eq!(parse_iso_ms("2026-10-16T12:05:11-0430"), Some(1_792_168_511_000.0));
        assert_eq!(parse_iso_ms("2026-10-16"), Some(1_792_108_800_000.0));
        assert_eq!(parse_iso_ms("2024-02-29T00:00:00Z"), Some(1_709_164_800_000.0));
        assert_eq!(parse_iso_ms("1969-12-31T23:59:59Z"), Some(-1000.0));
        for bad in [
            "",
            "soon",
            "2026-02-30",
            "2025-02-29",
            "2026-13-01",
            "2026-10-16T25:00:00Z",
            "2026-10-16T10:61:00Z",
            "2026-10-16T10:00:00+25:00",
            "1792168511816",
        ] {
            assert_eq!(parse_iso_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(utc_day(0.0).as_deref(), Some("1970-01-01"));
        assert_eq!(utc_day(1_792_168_511_816.0).as_deref(), Some("2026-10-16"));
        assert_eq!(utc_day(1_709_164_799_999.0).as_deref(), Some("2024-02-28"));
        assert_eq!(utc_day(-1.0).as_deref(), Some("1969-12-31"));
        assert_eq!(utc_day(f64::NAN), None);
        assert_eq!(utc_day(1e18), None);
    }

    #[test]
    fn titles_are_one_line_and_clipped() {
        assert_eq!(session_title(""), None);
        assert_eq!(session_title("  \n\t "), None);
        assert_eq!(
            session_title("fix\n the   csv\tparser").as_deref(),
            Some("fix the csv parser")
        );
        let title = session_title(&"ab ".repeat(40)).unwrap();
        assert_eq!(
            title.chars().count(),
            57,
            "56 characters and the ellipsis, after trimming the space"
        );
        assert!(title.ends_with("ab…"));
        let exact = "x".repeat(58);
        assert_eq!(session_title(&exact).as_deref(), Some(exact.as_str()));
        let over = "x".repeat(59);
        assert_eq!(session_title(&over).unwrap(), format!("{}…", "x".repeat(57)));
        assert_eq!(
            session_title(&"é".repeat(70)).unwrap().chars().count(),
            58,
            "clipped by characters, not bytes"
        );
    }

    #[test]
    fn message_text_comes_from_strings_and_blocks() {
        assert_eq!(extract_session_text(&json!("  hi  ")), "hi");
        assert_eq!(
            extract_session_text(&json!([{"type": "text", "text": "a"}, {"type": "image"}, {"text": "b"}, "c", null])),
            "a  b"
        );
        assert_eq!(extract_session_text(&json!({"text": "x"})), "");
        assert_eq!(extract_session_text(&Value::Null), "");
    }

    #[test]
    fn a_listing_names_each_chat_and_skips_the_empty_ones() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.jsonl",
            &[header("sid-a", "/work/proj"), user("  build the\nthing  ", 2000)],
        );
        write(
            dir.path(),
            "b.jsonl",
            &[
                header("sid-b", "/work/proj"),
                user("hello", 3000),
                json!({"type": "session_name_change", "name": "First"}),
                json!({"type": "session_info", "name": "Renamed"}),
                json!({"type": "session_info", "name": "   "}),
                json!({"type": "model_change", "name": "ignored", "timestamp": "2026-10-02T09:00:00.000Z"}),
            ],
        );
        // The daemon's start-up session: a header and bookkeeping, no prompt, no name.
        write(
            dir.path(),
            "empty.jsonl",
            &[header("sid-empty", "/work/proj"), json!({"type": "session_state"})],
        );
        write(
            dir.path(),
            "named-only.jsonl",
            &[header("sid-n", "/work/proj"), json!({"sessionName": "Just a name"})],
        );
        std::fs::write(dir.path().join("notes.txt"), "not a session").unwrap();
        std::fs::create_dir(dir.path().join("dir.jsonl")).unwrap();

        let all = list_in(dir.path(), None);
        let names: Vec<_> = all
            .iter()
            .map(|s| (s.session_id.as_str(), s.name.as_deref(), s.message_count))
            .collect();
        assert_eq!(
            names,
            [
                ("sid-n", Some("Just a name"), 0),
                ("sid-b", Some("Renamed"), 1),
                ("sid-a", Some("build the thing"), 1)
            ],
            "newest first (a chat with no prompt is dated by its file); a rename beats the prompt; the latest rename wins; \
             blank renames are ignored"
        );
        assert_eq!(all[1].mtime, 3000.0);
        assert_eq!(all[1].working_directory.as_deref(), Some("/work/proj"));
        assert_eq!(all[1].session_file, dir.path().join("b.jsonl").to_string_lossy());
        let json = serde_json::to_value(&all[2]).unwrap();
        assert_eq!(
            json,
            json!({"sessionFile": dir.path().join("a.jsonl").to_string_lossy(), "sessionId": "sid-a", "messageCount": 1,
                   "workingDirectory": "/work/proj", "mtime": 2000.0, "name": "build the thing"})
        );
        assert!(list_in(&dir.path().join("missing"), None).is_empty());
    }

    #[test]
    fn a_chat_with_no_message_time_is_dated_by_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "x.jsonl",
            &[json!({"type": "message", "message": {"role": "user", "content": "hi"}})],
        );
        let listed = list_in(dir.path(), None);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, "x", "no header: the file name is the id");
        let modified = epoch_ms(std::fs::metadata(&path).unwrap().modified().unwrap());
        assert_eq!(listed[0].mtime, modified);
        assert_eq!(listed[0].working_directory, None);
    }

    #[test]
    fn header_fields_fall_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "old.jsonl",
            &[
                json!({"type": "session", "sessionId": 42, "messageCount": "7", "entries": [{"workingDirectory": "/from/entry"}]}),
                json!({"type": "session", "id": "ignored-second-header", "cwd": "/ignored"}),
                json!({"type": "message", "message": {"role": "assistant", "content": "I speak first"}}),
                json!({"type": "message", "message": {"role": "user", "content": [{"text": "from"}, {"text": "blocks"}]}}),
            ],
        );
        let listed = list_in(dir.path(), None);
        assert_eq!(listed[0].session_id, "42");
        assert_eq!(listed[0].working_directory.as_deref(), Some("/from/entry"));
        assert_eq!(listed[0].message_count, 2, "message records beat the header's count");
        assert_eq!(listed[0].name.as_deref(), Some("from blocks"));
        write(
            dir.path(),
            "count.jsonl",
            &[
                json!({"type": "session", "id": "c", "messageCount": 9, "cwd": ""}),
                json!({"sessionName": "n"}),
            ],
        );
        let counted = list_in(dir.path(), None)
            .into_iter()
            .find(|s| s.session_id == "c")
            .unwrap();
        assert_eq!(counted.message_count, 9);
        assert_eq!(counted.working_directory, None, "an empty cwd is no cwd");
    }

    #[test]
    fn a_half_written_last_line_does_not_hide_the_chat() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "live.jsonl", &[header("live", "/p"), user("hello", 10)]);
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"{\"type\":\"message\",\"message\":{\"role\":\"assist")
            .unwrap();
        file.write_all(b"\n\xff\xfe not utf8 \n").unwrap();
        let listed = list_in(dir.path(), None);
        assert_eq!((listed.len(), listed[0].message_count), (1, 1));
    }

    #[test]
    fn a_listing_reuses_a_summary_until_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "c.jsonl", &[header("sid", "/p"), user("aaaa", 1)]);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(list_in(dir.path(), None)[0].name.as_deref(), Some("aaaa"));
        // Same size, same time: the cached summary is served even though the content differs.
        write(dir.path(), "c.jsonl", &[header("sid", "/p"), user("bbbb", 1)]);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before)
            .unwrap();
        assert_eq!(
            list_in(dir.path(), None)[0].name.as_deref(),
            Some("aaaa"),
            "served from the cache"
        );
        // A different size is a change.
        write(dir.path(), "c.jsonl", &[header("sid", "/p"), user("bbbbbb", 1)]);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before)
            .unwrap();
        assert_eq!(list_in(dir.path(), None)[0].name.as_deref(), Some("bbbbbb"));
        // A deleted file leaves the list and the cache.
        std::fs::remove_file(&path).unwrap();
        assert!(list_in(dir.path(), None).is_empty());
        assert!(!cache().contains_key(&path));
    }

    #[test]
    fn a_listing_keeps_to_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("here");
        std::fs::create_dir(&here).unwrap();
        let here_text = here.to_string_lossy();
        write(dir.path(), "1.jsonl", &[header("here", &here_text), user("a", 1)]);
        write(
            dir.path(),
            "2.jsonl",
            &[header("elsewhere", "/somewhere/else"), user("b", 2)],
        );
        write(
            dir.path(),
            "3.jsonl",
            &[json!({"type": "message", "message": {"role": "user", "content": "no cwd"}})],
        );
        // The project was moved: the old folder is gone, the folder name is the same.
        write(
            dir.path(),
            "4.jsonl",
            &[header("moved", "/old/place/here"), user("c", 3)],
        );
        // A folder with the same name that still exists is a different project.
        write(
            dir.path(),
            "5.jsonl",
            &[header("twin", &dir.path().to_string_lossy()), user("d", 4)],
        );
        let ids = |project: Option<&Path>| {
            let mut ids: Vec<_> = list_in(dir.path(), project).into_iter().map(|s| s.session_id).collect();
            ids.sort();
            ids
        };
        assert_eq!(ids(Some(&here)), ["here", "moved"]);
        assert_eq!(
            ids(Some(Path::new(&format!("{here_text}/")))),
            ["here", "moved"],
            "a trailing separator changes nothing"
        );
        assert_eq!(ids(Some(&here.join("sub").join(".."))), ["here"], "`..` is resolved");
        assert_eq!(ids(None).len(), 5);
        assert_eq!(ids(Some(Path::new(""))).len(), 5, "no project means every chat");
        assert_eq!(ids(Some(Path::new("/nowhere"))), Vec::<String>::new());
    }

    #[test]
    fn folders_compare_the_same_however_they_are_written() {
        let same = |a: &str, b: &str| dir_key(a) == dir_key(b);
        assert!(same("/a/b", "/a/b/"));
        assert!(same("/a/b", "/a/./b"));
        assert!(same("/a/b/c/..", "/a/b"));
        assert!(!same("/a/b", "a/b"), "a relative path is not the absolute one");
        assert!(!same("/a/b", "/a/B"), "POSIX paths are case-sensitive");
        assert!(same(r"C:\Users\Ada\proj", "c:/users/ada/proj/"));
        assert!(same(r"\\?\C:\Users\Ada\proj", r"C:\Users\Ada\proj"));
        assert!(same(r"C:\Users\Ada\proj\..\proj", r"C:\Users\Ada\proj"));
        assert!(!same(r"C:\Users\Ada\proj", r"D:\Users\Ada\proj"));
        assert!(same(r"\\server\share\proj", r"\\SERVER\share\proj\"));
        assert_eq!(dir_key("/"), "/");
        assert!(in_project(
            Some(r"C:\Users\Ada\proj"),
            Some(Path::new("c:/users/ada/proj"))
        ));
        assert!(
            in_project(Some(r"D:\gone\proj"), Some(Path::new(r"C:\Users\Ada\proj"))),
            "moved, same name, no longer there"
        );
        assert!(!in_project(None, Some(Path::new("/p"))));
    }

    #[test]
    fn a_session_id_is_read_from_the_header_line_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.jsonl", &[header("the-id", "/p"), json!({"id": "later"})]);
        assert_eq!(read_session_id(&path).as_deref(), Some("the-id"));
        let by_session_id = write(dir.path(), "b.jsonl", &[json!({"sessionId": "sid"})]);
        assert_eq!(read_session_id(&by_session_id).as_deref(), Some("sid"));
        let numeric = write(dir.path(), "c.jsonl", &[json!({"id": 7, "sessionId": "ignored"})]);
        assert_eq!(
            read_session_id(&numeric),
            None,
            "a non-text id is no id, and does not fall through"
        );
        let empty = write(dir.path(), "d.jsonl", &[json!({"id": ""})]);
        assert_eq!(read_session_id(&empty), None);
        std::fs::write(dir.path().join("e.jsonl"), "not json\n{\"id\":\"x\"}\n").unwrap();
        assert_eq!(read_session_id(&dir.path().join("e.jsonl")), None);
        std::fs::write(
            dir.path().join("f.jsonl"),
            format!("{{\"id\":\"{}\"}}\n", "x".repeat(20_000)),
        )
        .unwrap();
        assert_eq!(
            read_session_id(&dir.path().join("f.jsonl")),
            None,
            "a header past 16 KiB is not read"
        );
        std::fs::write(dir.path().join("g.jsonl"), b"{\"id\":\"crlf\"}\r\n").unwrap();
        assert_eq!(read_session_id(&dir.path().join("g.jsonl")).as_deref(), Some("crlf"));
        assert_eq!(read_session_id(&dir.path().join("missing.jsonl")), None);
        assert_eq!(read_session_id(dir.path()), None, "a folder is not a session");
    }

    #[test]
    fn a_session_file_is_found_by_name_or_by_what_the_listing_learned() {
        let dir = tempfile::tempdir().unwrap();
        let named = write(dir.path(), "by-name.jsonl", &[header("by-name", "/p"), user("x", 1)]);
        assert_eq!(session_file_in(dir.path(), "by-name"), Some(named));
        let odd = write(
            dir.path(),
            "2026_odd-name.jsonl",
            &[header("odd-id", "/p"), user("x", 1)],
        );
        assert_eq!(
            session_file_in(dir.path(), "odd-id"),
            None,
            "unknown until a listing reads it"
        );
        list_in(dir.path(), None);
        assert_eq!(session_file_in(dir.path(), "odd-id"), Some(odd.clone()));
        std::fs::remove_file(&odd).unwrap();
        assert_eq!(
            session_file_in(dir.path(), "odd-id"),
            None,
            "a deleted file is not returned"
        );
        for hostile in ["", "../x", "a/b", r"a\b", "x\0y", "C:x", "C:..\\x"] {
            assert_eq!(session_file_in(dir.path(), hostile), None, "{hostile:?}");
        }
    }
}

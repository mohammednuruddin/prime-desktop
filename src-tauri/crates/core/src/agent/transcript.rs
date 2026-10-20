//! Reading a saved session file into the messages the renderer shows.
//!
//! Prime Agent's `get_messages` returns the model's context: after each compaction it holds only a summary plus the
//! messages kept since, so a long task loses the user's prompt and the early work. The session file keeps every
//! entry, so this walks the current branch the way Prime Agent does (leaf, then `parentId`, up to the root) but keeps
//! the messages before each compaction and marks where the compaction happened.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use serde_json::{Map, Value, json};

/// What the window needs to draw a message, not what the model needs to resume. In a long run most bytes are
/// reasoning signatures (opaque provider blobs, never shown) and tool-result details, of which the chat reads only
/// `reasoning`; tool text is shown up to this many characters. Dropping the rest cut an 11 MB session's transcript from
/// 9.4 MB to a few MB. The session file and the daemon's context are untouched.
const TOOL_TEXT_LIMIT: usize = 12_000;

/// The visible history of the current branch of a session, from its parsed entries.
pub fn transcript_from_entries(entries: &[Value]) -> Vec<Value> {
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|entry| Some((entry.get("id")?.as_str()?, entry)))
        .collect();
    // Session files are append-only, so the newest entry is the current leaf. The walk stops at a repeated id, which
    // only a damaged file has.
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut current = entries.last();
    while let Some(entry) = current {
        if let Some(id) = entry.get("id").and_then(Value::as_str)
            && !seen.insert(id)
        {
            break;
        }
        path.push(entry);
        current = entry
            .get("parentId")
            .and_then(Value::as_str)
            .and_then(|parent| by_id.get(parent).copied());
    }
    path.reverse();

    let mut messages = Vec::new();
    for entry in path {
        let timestamp = millis(entry.get("timestamp"));
        let kind = entry.get("type").and_then(Value::as_str);
        if kind == Some("message") {
            if let Some(message) = entry.get("message").and_then(Value::as_object) {
                messages.push(for_display(message));
            }
        } else if kind == Some("custom_message") {
            let mut custom = Map::new();
            custom.insert("role".into(), "custom".into());
            for key in ["customType", "content", "display", "details"] {
                if let Some(value) = entry.get(key) {
                    custom.insert(key.into(), value.clone());
                }
            }
            custom.insert("timestamp".into(), timestamp);
            messages.push(Value::Object(custom));
        } else if kind == Some("branch_summary") {
            if let Some(summary) = entry
                .get("summary")
                .and_then(Value::as_str)
                .filter(|summary| !summary.is_empty())
            {
                messages.push(json!({
                    "role": "system",
                    "id": format!("sys-branch_summary-{}", id_text(entry, &timestamp)),
                    "content": format!("Branch summary: {summary}"),
                    "timestamp": timestamp
                }));
            }
        } else if kind == Some("compaction") {
            // Rendered as the chat's "Context compacted" card at the point it happened.
            messages.push(json!({
                "role": "system",
                "id": format!("sys-compaction_outcome-{}", id_text(entry, &timestamp)),
                "compaction": true,
                "content": "Context automatically compacted",
                "timestamp": timestamp
            }));
        }
    }
    messages
}

/// The entry's id for a synthetic message id, or its timestamp when it has none.
fn id_text(entry: &Value, timestamp: &Value) -> String {
    match entry.get("id") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => timestamp.to_string(),
    }
}

fn for_display(message: &Map<String, Value>) -> Value {
    let mut message = message.clone();
    match message.get("role").and_then(Value::as_str) {
        Some("assistant") => without_signatures(&mut message),
        Some("toolResult") => compact_tool_result(&mut message),
        _ => {}
    }
    Value::Object(message)
}

fn without_signatures(message: &mut Map<String, Value>) {
    if let Some(Value::Array(blocks)) = message.get_mut("content") {
        for block in blocks.iter_mut().filter_map(Value::as_object_mut) {
            block.shift_remove("thinkingSignature");
        }
    }
}

fn compact_tool_result(message: &mut Map<String, Value>) {
    if message
        .get("details")
        .is_some_and(|details| details.is_object() || details.is_array())
    {
        // The chat reads only `reasoning` from a result's details.
        let reasoning = message["details"]
            .get("reasoning")
            .and_then(Value::as_str)
            .map(str::to_string);
        match reasoning {
            Some(mut reasoning) => {
                trim_text(&mut reasoning);
                message.insert("details".into(), json!({ "reasoning": reasoning }));
            }
            None => {
                message.shift_remove("details");
            }
        }
    }
    match message.get_mut("content") {
        Some(Value::String(text)) => trim_text(text),
        Some(Value::Array(blocks)) => {
            for block in blocks.iter_mut() {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(Value::String(text)) = block.get_mut("text")
                {
                    trim_text(text);
                }
            }
        }
        _ => {}
    }
}

fn trim_text(text: &mut String) {
    if let Some((cut, _)) = text.char_indices().nth(TOOL_TEXT_LIMIT) {
        text.truncate(cut);
        text.push_str("\n… tool output truncated …");
    }
}

/// An entry's timestamp as milliseconds: a number stays as it is, an ISO string is parsed, anything else is 0.
fn millis(value: Option<&Value>) -> Value {
    match value {
        Some(number @ Value::Number(_)) => number.clone(),
        Some(Value::String(text)) => parse_iso_millis(text).unwrap_or(0).into(),
        _ => 0.into(),
    }
}

/// `Date.parse` for the ISO 8601 forms the agent writes: `YYYY[-MM[-DD]]`, then optionally `T` or a space, `HH:MM`,
/// `:SS`, `.fraction`, and `Z` or `±HH:MM`. Anything else is `None`.
// ponytail: a time with no zone is read as UTC (JavaScript reads it as local time). The agent always writes `Z`; add
// a time crate if sessions from elsewhere ever matter.
fn parse_iso_millis(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once(['T', ' ']) {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let mut date = date.splitn(3, '-');
    let year = digits(date.next()?, 4, 4)?;
    let month = date.next().map_or(Some(1), |month| digits(month, 2, 2))?;
    let day = date.next().map_or(Some(1), |day| digits(day, 2, 2))?;
    if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    let (mut seconds_of_day, mut millis, mut offset_minutes) = (0, 0, 0);
    if let Some(time) = time {
        let zone_at = time.find(['Z', 'z', '+', '-']).unwrap_or(time.len());
        let (clock, zone) = time.split_at(zone_at);
        let mut clock = clock.splitn(3, ':');
        let hours = digits(clock.next()?, 2, 2)?;
        let minutes = digits(clock.next()?, 2, 2)?;
        let (seconds, fraction) = match clock.next() {
            Some(rest) => rest.split_once('.').unwrap_or((rest, "")),
            None => ("00", ""),
        };
        let seconds = digits(seconds, 2, 2)?;
        let fraction_ok = fraction.bytes().all(|byte| byte.is_ascii_digit());
        if hours > 24 || minutes > 59 || seconds > 59 || !fraction_ok || (hours == 24 && minutes + seconds > 0) {
            return None;
        }
        // The first three digits are the milliseconds; the rest is finer than a millisecond and is dropped.
        millis = format!("{:0<3}", &fraction[..fraction.len().min(3)])
            .parse::<i64>()
            .ok()?;
        seconds_of_day = hours * 3600 + minutes * 60 + seconds;
        offset_minutes = match zone.as_bytes() {
            b"" | b"Z" | b"z" => 0,
            [sign @ (b'+' | b'-'), ..] => {
                let zone = zone[1..].replace(':', "");
                let (hh, mm) = (digits(zone.get(..2)?, 2, 2)?, digits(zone.get(2..)?, 2, 2)?);
                if hh > 23 || mm > 59 {
                    return None;
                }
                (hh * 60 + mm) * if *sign == b'-' { -1 } else { 1 }
            }
            _ => return None,
        };
    }
    let days = days_from_civil(year, month, day);
    Some(((days * 86_400 + seconds_of_day) - offset_minutes * 60) * 1000 + millis)
}

fn digits(text: &str, min: usize, max: usize) -> Option<i64> {
    let ok = (min..=max).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit());
    if ok { text.parse().ok() } else { None }
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// One parsed file is kept so reopening the same chat does not re-read megabytes.
struct Cached {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
    messages: Arc<Vec<Value>>,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// The transcript of a saved session file, or `None` when the file cannot be read. A partly written last line (the
/// daemon is appending to the file) is skipped.
pub fn read_session_transcript(path: &Path) -> Option<Arc<Vec<Value>>> {
    let info = fs::metadata(path).ok()?;
    let (modified, size) = (info.modified().ok(), info.len());
    {
        let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        let hit = cache
            .as_ref()
            .filter(|cached| cached.path == path && cached.modified == modified && cached.size == size);
        if let Some(cached) = hit {
            return Some(cached.messages.clone());
        }
    }
    let mut entries = Vec::new();
    for line in BufReader::new(File::open(path).ok()?).split(b'\n') {
        let line = line.ok()?;
        let line = String::from_utf8_lossy(&line);
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry @ Value::Object(_)) = serde_json::from_str::<Value>(&line) {
            entries.push(entry);
        }
    }
    let messages = Arc::new(transcript_from_entries(&entries));
    let cached = Cached {
        path: path.to_path_buf(),
        modified,
        size,
        messages: messages.clone(),
    };
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some(cached);
    Some(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn msg(id: &str, parent: Option<&str>, role: &str, text: &str) -> Value {
        let stamp: i64 = id
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(1);
        json!({
            "type": "message", "id": id, "parentId": parent, "timestamp": "2026-10-08T10:00:00.000Z",
            "message": { "role": role, "content": text, "timestamp": stamp }
        })
    }

    fn contents(messages: &[Value]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| message["content"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn keeps_messages_from_before_a_compaction_and_marks_where_it_happened() {
        let entries = vec![
            json!({ "type": "session", "id": "s" }),
            msg("m1", None, "user", "Design a binder for EGFR"),
            msg("m2", Some("m1"), "assistant", "Early work"),
            msg("m3", Some("m2"), "assistant", "More early work"),
            json!({ "type": "compaction", "id": "c1", "parentId": "m3", "firstKeptEntryId": "m3", "summary": "summary",
                    "timestamp": "2026-10-08T11:00:00.000Z" }),
            json!({ "type": "custom_message", "id": "x1", "parentId": "c1", "customType": "refinement_outcome",
                    "content": "Refined", "display": true, "timestamp": "2026-10-08T11:01:00.000Z" }),
            msg("m4", Some("x1"), "assistant", "Later work"),
        ];
        let messages = transcript_from_entries(&entries);
        assert_eq!(
            contents(&messages),
            [
                "Design a binder for EGFR",
                "Early work",
                "More early work",
                "Context automatically compacted",
                "Refined",
                "Later work"
            ]
        );
        assert_eq!(messages[3]["role"], "system");
        assert_eq!(messages[3]["compaction"], true);
        assert_eq!(messages[3]["id"], "sys-compaction_outcome-c1");
        assert_eq!(messages[3]["timestamp"], 1_791_457_200_000_i64);
        assert_eq!(messages[4]["role"], "custom");
        assert_eq!(messages[4]["customType"], "refinement_outcome");
        assert_eq!(messages[4]["display"], true);
        assert_eq!(messages[4]["timestamp"], 1_791_457_260_000_i64);
        assert!(messages[4].get("details").is_none());
    }

    #[test]
    fn follows_only_the_current_branch_after_a_fork() {
        let entries = vec![
            msg("m1", None, "user", "Start"),
            msg("m2", Some("m1"), "assistant", "Abandoned branch"),
            msg("m3", Some("m1"), "assistant", "Current branch"),
        ];
        assert_eq!(
            contents(&transcript_from_entries(&entries)),
            ["Start", "Current branch"]
        );
    }

    #[test]
    fn trims_oversized_tool_output_but_keeps_short_results_untouched() {
        let huge = "x".repeat(50_000);
        let entries = vec![
            msg("m1", None, "user", "Run it"),
            json!({ "type": "message", "id": "r1", "parentId": "m1",
                    "message": { "role": "toolResult", "toolCallId": "t1", "content": [{ "type": "text", "text": huge }] } }),
            json!({ "type": "message", "id": "r2", "parentId": "r1",
                    "message": { "role": "toolResult", "toolCallId": "t2", "content": [{ "type": "text", "text": "short" }] } }),
        ];
        let messages = transcript_from_entries(&entries);
        let text = messages[1]["content"][0]["text"].as_str().unwrap();
        assert!(text.chars().count() < 12_100);
        assert!(text.contains("tool output truncated"));
        assert!(text.starts_with(&"x".repeat(12_000)));
        assert_eq!(messages[2]["content"], entries[2]["message"]["content"]);
    }

    #[test]
    fn trims_string_tool_output_and_never_splits_a_character() {
        let text = "é".repeat(12_001);
        let entries = vec![json!({ "type": "message", "id": "r", "parentId": null,
                                   "message": { "role": "toolResult", "content": text } })];
        let shown = transcript_from_entries(&entries)[0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(shown.starts_with(&"é".repeat(12_000)));
        assert!(shown.ends_with("\n… tool output truncated …"));
        assert_eq!(shown.chars().filter(|c| *c == 'é').count(), 12_000);
        let exact = vec![
            json!({ "type": "message", "id": "r", "message": { "role": "toolResult", "content": "é".repeat(12_000) } }),
        ];
        assert_eq!(transcript_from_entries(&exact)[0]["content"], "é".repeat(12_000));
    }

    #[test]
    fn drops_reasoning_signatures_and_unused_tool_details_from_the_display_copy() {
        let entries = vec![
            json!({ "type": "message", "id": "a1", "parentId": null, "message": { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "plan", "thinkingSignature": "x".repeat(1000) },
                { "type": "text", "text": "Hi" }, "stray", null
            ] } }),
            json!({ "type": "message", "id": "r1", "parentId": "a1", "message": { "role": "toolResult", "toolCallId": "t",
                    "content": "ok", "details": { "reasoning": "why", "bigTrace": "y".repeat(5000) } } }),
        ];
        let messages = transcript_from_entries(&entries);
        assert_eq!(
            messages[0]["content"][0],
            json!({ "type": "thinking", "thinking": "plan" })
        );
        assert_eq!(messages[0]["content"][1], json!({ "type": "text", "text": "Hi" }));
        assert_eq!(messages[1]["details"], json!({ "reasoning": "why" }));
        // The source entries are not touched.
        assert!(entries[0]["message"]["content"][0].get("thinkingSignature").is_some());
    }

    #[test]
    fn tool_details_without_a_reasoning_string_are_removed() {
        for details in [
            json!({ "trace": 1 }),
            json!({ "reasoning": 5 }),
            json!([1, 2]),
            json!({}),
        ] {
            let entries = vec![
                json!({ "type": "message", "id": "r", "message": { "role": "toolResult", "content": "ok", "details": details } }),
            ];
            assert!(
                transcript_from_entries(&entries)[0].get("details").is_none(),
                "{details}"
            );
        }
        // A details value that is not an object is left alone.
        let entries = vec![
            json!({ "type": "message", "id": "r", "message": { "role": "toolResult", "content": "ok", "details": "text" } }),
        ];
        assert_eq!(transcript_from_entries(&entries)[0]["details"], "text");
        let long = vec![
            json!({ "type": "message", "id": "r", "message": { "role": "toolResult", "content": "ok",
                                "details": { "reasoning": "r".repeat(13_000) } } }),
        ];
        let reasoning = transcript_from_entries(&long)[0]["details"]["reasoning"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(reasoning.ends_with("tool output truncated …") && reasoning.chars().count() < 12_100);
    }

    #[test]
    fn other_roles_pass_through_unchanged() {
        let user = json!({ "role": "user", "content": [{ "type": "text", "thinkingSignature": "keep", "text": "hi" }], "x": 1 });
        let entries = vec![json!({ "type": "message", "id": "u", "message": user })];
        assert_eq!(transcript_from_entries(&entries)[0], user);
    }

    #[test]
    fn branch_summaries_become_system_notes_and_empty_ones_are_skipped() {
        let entries = vec![
            msg("m1", None, "user", "Start"),
            json!({ "type": "branch_summary", "id": "b1", "parentId": "m1", "summary": "went left", "timestamp": "2026-10-08T10:00:00.000Z" }),
            json!({ "type": "branch_summary", "id": "b2", "parentId": "b1", "summary": "", "timestamp": 5 }),
            json!({ "type": "branch_summary", "id": "b3", "parentId": "b2", "summary": 7 }),
            json!({ "type": "branch_summary", "parentId": "b3", "summary": "no id", "timestamp": 1234 }),
        ];
        let messages = transcript_from_entries(&entries);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"], "Branch summary: went left");
        assert_eq!(messages[1]["id"], "sys-branch_summary-b1");
        assert_eq!(messages[1]["role"], "system");
        assert_eq!(messages[2]["id"], "sys-branch_summary-1234");
    }

    #[test]
    fn a_compaction_without_an_id_or_timestamp_still_gets_an_id() {
        let entries = vec![json!({ "type": "compaction" })];
        let messages = transcript_from_entries(&entries);
        assert_eq!(messages[0]["id"], "sys-compaction_outcome-0");
        assert_eq!(messages[0]["timestamp"], 0);
    }

    #[test]
    fn a_damaged_file_cannot_loop_the_walk() {
        let entries = vec![
            msg("a", Some("b"), "user", "one"),
            msg("b", Some("a"), "assistant", "two"),
        ];
        // b -> a -> b: the repeat ends the walk with each entry shown once.
        assert_eq!(contents(&transcript_from_entries(&entries)), ["one", "two"]);
        let self_parent = vec![msg("a", Some("a"), "user", "one")];
        assert_eq!(contents(&transcript_from_entries(&self_parent)), ["one"]);
    }

    #[test]
    fn a_missing_parent_ends_the_chain_and_the_last_duplicate_id_wins() {
        let entries = vec![
            msg("a", None, "user", "old a"),
            msg("a", None, "user", "new a"),
            msg("b", Some("a"), "assistant", "reply"),
            msg("c", Some("gone"), "assistant", "orphan"),
        ];
        assert_eq!(contents(&transcript_from_entries(&entries)), ["orphan"]);
        assert_eq!(contents(&transcript_from_entries(&entries[..3])), ["new a", "reply"]);
        assert!(transcript_from_entries(&[]).is_empty());
    }

    #[test]
    fn entries_of_unknown_or_odd_shape_add_nothing() {
        let entries = vec![
            json!({ "type": "session", "id": "s" }),
            json!({ "type": "message", "id": "m", "parentId": "s", "message": "not an object" }),
            json!({ "type": "message", "id": "n", "parentId": "m" }),
            json!(42),
            json!(null),
        ];
        assert!(transcript_from_entries(&entries).is_empty());
    }

    #[test]
    fn iso_timestamps_parse_like_date_parse() {
        let cases: [(&str, Option<i64>); 17] = [
            ("2026-10-08T11:01:00.000Z", Some(1_791_457_260_000)),
            ("2026-10-08T11:01:00.5+02:00", Some(1_791_450_060_500)),
            ("2026-10-08T11:01:00+0200", Some(1_791_450_060_000)),
            ("2026-10-08T11:01:00-05:30", Some(1_791_457_260_000 + 19_800_000)),
            ("2026-10-08T11:01:00.123456789Z", Some(1_791_457_260_123)),
            ("2026-10-08T11:01:00.1Z", Some(1_791_457_260_100)),
            ("2026-10-08T11:01Z", Some(1_791_457_260_000)),
            ("2026-10-08 11:01:00Z", Some(1_791_457_260_000)),
            ("2026-10-08", Some(1_791_417_600_000)),
            ("2026-10", Some(1_790_812_800_000)),
            ("2026", Some(1_767_225_600_000)),
            ("1970-01-01T00:00:00Z", Some(0)),
            ("2024-02-29T12:00:00Z", Some(1_709_208_000_000)),
            ("2026-10-08T24:00:00Z", Some(1_791_504_000_000)),
            ("2026-02-30T00:00:00Z", None),
            ("yesterday", None),
            ("2026-13-01", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_iso_millis(text), expected, "{text}");
        }
        for bad in [
            "",
            "2026-10-08T25:00:00Z",
            "2026-10-08T10:61:00Z",
            "2026-10-08T10:00:00Q",
            "2026-10-08T10:00:00+9",
            "2026-10-08Tx",
        ] {
            assert_eq!(parse_iso_millis(bad), None, "{bad}");
        }
        assert_eq!(parse_iso_millis("1969-12-31T23:59:59.999Z"), Some(-1));
    }

    #[test]
    fn timestamp_parsing_never_panics_on_odd_text() {
        let alphabet: Vec<char> = "0123456789-:.TZz+ \u{fc}\u{1f600}x".chars().collect();
        let mut state = 12345_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        for _ in 0..30_000 {
            let text: String = (0..next() % 30).map(|_| alphabet[next() % alphabet.len()]).collect();
            let _ = parse_iso_millis(&text);
        }
    }

    #[test]
    fn timestamps_may_be_numbers_strings_or_missing() {
        let entries = vec![
            json!({ "type": "custom_message", "id": "a", "timestamp": 1_700_000_000_123_i64, "content": "n" }),
            json!({ "type": "custom_message", "id": "b", "parentId": "a", "timestamp": "garbage", "content": "s" }),
            json!({ "type": "custom_message", "id": "c", "parentId": "b", "content": "m", "details": null }),
        ];
        let messages = transcript_from_entries(&entries);
        assert_eq!(messages[0]["timestamp"], 1_700_000_000_123_i64);
        assert_eq!(messages[1]["timestamp"], 0);
        assert_eq!(messages[2]["timestamp"], 0);
        assert_eq!(messages[2]["details"], Value::Null);
    }

    fn write_session(path: &Path, lines: &[Value]) {
        let mut file = File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    #[test]
    fn reads_a_session_file_and_skips_blank_broken_and_non_object_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "{}", json!({ "type": "session", "id": "s" })).unwrap();
        write!(file, "\r\n   \r\n").unwrap();
        writeln!(file, "{}\r", msg("m1", Some("s"), "user", "hello")).unwrap();
        writeln!(file, "42").unwrap();
        writeln!(file, "not json").unwrap();
        writeln!(file, "{}", msg("m2", Some("m1"), "assistant", "second")).unwrap();
        // The daemon is mid-append: the last line is cut short.
        write!(file, "{{\"type\":\"message\",\"id\":\"m9\",\"parentId\":\"m2\",\"mess").unwrap();
        drop(file);
        assert_eq!(contents(&read_session_transcript(&path).unwrap()), ["hello", "second"]);
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let line = b"{\"type\":\"message\",\"id\":\"m\",\"message\":{\"role\":\"user\",\"content\":\"caf\xff\"}}\n";
        fs::write(&path, line).unwrap();
        assert_eq!(contents(&read_session_transcript(&path).unwrap()), ["caf\u{fffd}"]);
    }

    #[test]
    fn a_missing_or_unreadable_path_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_session_transcript(&dir.path().join("nope.jsonl")).is_none());
        assert!(read_session_transcript(dir.path()).is_none());
    }

    #[test]
    fn an_unchanged_file_is_served_from_the_cache_and_a_changed_one_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write_session(&path, &[msg("m1", None, "user", "first")]);
        let one = read_session_transcript(&path).unwrap();
        let again = read_session_transcript(&path).unwrap();
        assert!(Arc::ptr_eq(&one, &again));
        // The daemon appends: the size changes, so the file is parsed again.
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", msg("m2", Some("m1"), "assistant", "second")).unwrap();
        drop(file);
        let grown = read_session_transcript(&path).unwrap();
        assert_eq!(contents(&grown), ["first", "second"]);
        assert!(!Arc::ptr_eq(&one, &grown));
        // Another file replaces the cached one and the first is read fresh afterwards.
        let other = dir.path().join("t.jsonl");
        write_session(&other, &[msg("x1", None, "user", "other")]);
        assert_eq!(contents(&read_session_transcript(&other).unwrap()), ["other"]);
        assert_eq!(contents(&read_session_transcript(&path).unwrap()), ["first", "second"]);
    }

    #[test]
    fn reads_the_real_session_shape_the_agent_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("real.jsonl");
        write_session(
            &path,
            &[
                json!({ "type": "session", "version": 3, "id": "s1", "timestamp": "2026-10-13T22:13:32.211Z", "cwd": "/p" }),
                json!({ "type": "thinking_level_change", "id": "74bf80f0", "parentId": null, "timestamp": "2026-10-13T22:13:32.213Z", "thinkingLevel": "off" }),
                json!({ "type": "session_state", "id": "999c5ea4", "parentId": "74bf80f0", "timestamp": "2026-10-13T22:13:32.213Z", "state": {} }),
                json!({ "type": "message", "id": "5ab8e3db", "parentId": "999c5ea4", "timestamp": "2026-10-13T22:13:32.283Z",
                        "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 1_791_929_612_283_i64 } }),
            ],
        );
        let messages = read_session_transcript(&path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }
}

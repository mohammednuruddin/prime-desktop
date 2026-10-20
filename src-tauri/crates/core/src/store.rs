//! The app state file: open projects, the selected tab, session pins, preview files and settings.
//!
//! One [`Store`] per state file. The file is read once at open and cached, as the Electron build did; every change is
//! applied to a copy, written atomically, and only then becomes the cached state, so a failed write changes nothing
//! and two writers can never lose each other's change.

use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};

const STATE_VERSION: u32 = 2;
/// Files the user picked through the preview "open file" dialog: the only out-of-project files the renderer may read.
const PREVIEW_FILES_LIMIT: usize = 50;
const PREVIEW_PATH_MAX: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectTab {
    pub id: String,
    pub path: String,
    pub name: String,
}

/// What `state:initial` and the tab, pin and settings calls read. `settings` stays a JSON object: it keeps keys this
/// build does not know (a newer build wrote them), and the renderer gets exactly what is stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredState {
    pub version: u32,
    pub settings: Map<String, Value>,
    pub tabs: Vec<ProjectTab>,
    pub active_tab_id: Option<String>,
    pub model: Option<String>,
    pub pinned_session_files: Vec<String>,
    pub preview_files: Vec<String>,
}

impl StoredState {
    pub fn empty() -> Self {
        Self {
            version: STATE_VERSION,
            settings: default_settings(),
            tabs: Vec::new(),
            active_tab_id: None,
            model: None,
            pinned_session_files: Vec::new(),
            preview_files: Vec::new(),
        }
    }
}

// The theme constants are `src/shared/themes.ts`.
fn default_settings() -> Map<String, Value> {
    let fonts = json!({ "code": "Geist Mono", "ui": "Geist" });
    let Value::Object(settings) = json!({
        "notifications": true,
        "checkpoints": true,
        "dockBadge": false,
        "autoNameSessions": true,
        "thinkingLevel": "medium",
        "showReasoning": true,
        "autoCompaction": true,
        "autoRetry": true,
        "model": null,
        "rlmMaxDepth": 1,
        "transport": "auto",
        "autonomous": {
            "enabled": false,
            "gates": [],
            "gateRetries": 2,
            "maxContinuations": 8,
            "maxTurns": 30,
            "maxTokens": 100000,
            "maxSeconds": 3600
        },
        "themeMode": "system",
        "codeThemeId": "codex",
        "lightTheme": {
            "accent": "#c97b76",
            "contrast": 42,
            "fonts": fonts,
            "ink": "#292832",
            "opaqueWindows": true,
            "semanticColors": { "diffAdded": "#2da44e", "diffRemoved": "#cf222e", "skill": "#c4846a" },
            "surface": "#ffffff"
        },
        "darkTheme": {
            "accent": "#339cff",
            "contrast": 60,
            "fonts": fonts,
            "ink": "#ffffff",
            "opaqueWindows": false,
            "semanticColors": { "diffAdded": "#40c977", "diffRemoved": "#fa423e", "skill": "#ad7bf9" },
            "surface": "#00000e"
        }
    }) else {
        unreachable!("the defaults are an object")
    };
    settings
}

/// Where the Electron build kept its state. The TypeScript hard-coded the macOS path on every system, so on Windows
/// and Linux the file is under that same odd folder in the home directory.
pub fn electron_state_file() -> PathBuf {
    crate::platform::home_dir()
        .join("Library")
        .join("Application Support")
        .join("PrimeDesktop")
        .join("state.json")
}

/// Turns whatever a state file holds into a valid state: old files gain new fields, wrong types fall back to the
/// defaults, unknown settings survive.
pub fn migrate_state(raw: &Value) -> StoredState {
    let field = |key: &str| raw.get(key);
    let tabs: Vec<ProjectTab> = field("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tab| {
            let text = |key: &str| tab.get(key).and_then(Value::as_str).map(str::to_string);
            Some(ProjectTab {
                id: text("id")?,
                path: text("path")?,
                name: text("name")?,
            })
        })
        .collect();
    let wanted = field("activeTabId").and_then(Value::as_str);
    let active_tab_id = match wanted {
        Some(id) if tabs.iter().any(|tab| tab.id == id) => Some(id.to_string()),
        _ => tabs.first().map(|tab| tab.id.clone()),
    };
    let strings = |key: &str| -> Vec<String> {
        field(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    };
    let mut preview_files = unique(
        strings("previewFiles")
            .into_iter()
            .filter(|path| {
                !path.is_empty() && path.encode_utf16().count() <= PREVIEW_PATH_MAX && Path::new(path).is_absolute()
            })
            .collect(),
    );
    preview_files.truncate(PREVIEW_FILES_LIMIT);
    StoredState {
        version: STATE_VERSION,
        settings: migrate_settings(field("settings")),
        tabs,
        active_tab_id,
        model: field("model").and_then(Value::as_str).map(str::to_string),
        pinned_session_files: strings("pinnedSessionFiles"),
        preview_files,
    }
}

fn migrate_settings(raw: Option<&Value>) -> Map<String, Value> {
    let defaults = default_settings();
    let raw = raw.and_then(Value::as_object);
    let field = |key: &str| raw.and_then(|raw| raw.get(key));
    let mut settings = defaults.clone();
    for (key, value) in raw.into_iter().flatten() {
        settings.insert(key.clone(), value.clone());
    }
    settings.insert(
        "autonomous".into(),
        Value::Object(overlay(&defaults["autonomous"], field("autonomous"))),
    );
    for key in ["lightTheme", "darkTheme"] {
        settings.insert(key.into(), merge_theme(&defaults[key], field(key)));
    }
    settings.insert("rlmMaxDepth".into(), integer_or(1, field("rlmMaxDepth"), 0, 10).into());
    let mode = field("themeMode")
        .and_then(Value::as_str)
        .filter(|mode| ["light", "dark"].contains(mode));
    settings.insert("themeMode".into(), mode.unwrap_or("system").into());
    let transport = field("transport")
        .and_then(Value::as_str)
        .filter(|transport| ["sse", "websocket", "websocket-cached"].contains(transport));
    settings.insert("transport".into(), transport.unwrap_or("auto").into());
    settings
}

/// `base` with the keys of `over` laid on top. Anything that is not a plain object adds nothing.
fn overlay(base: &Value, over: Option<&Value>) -> Map<String, Value> {
    let mut merged = base.as_object().cloned().unwrap_or_default();
    for (key, value) in over.and_then(Value::as_object).into_iter().flatten() {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// A theme with its fonts and semantic colours merged one level down, so a theme that sets one colour keeps the rest.
fn merge_theme(base: &Value, patch: Option<&Value>) -> Value {
    let Some(patch) = patch.filter(|patch| patch.is_object()) else {
        return base.clone();
    };
    let mut theme = overlay(base, Some(patch));
    for key in ["fonts", "semanticColors"] {
        theme.insert(key.into(), Value::Object(overlay(&base[key], patch.get(key))));
    }
    Value::Object(theme)
}

/// A whole number in `min..=max`, else `fallback`. JSON `2.0` counts as 2, as it did when the TypeScript parsed it.
fn integer_or(fallback: i64, value: Option<&Value>, min: i64, max: i64) -> i64 {
    let whole = value.and_then(|value| {
        value.as_i64().or_else(|| {
            value
                .as_f64()
                .filter(|float| float.fract() == 0.0 && float.abs() < 1e15)
                .map(|f| f as i64)
        })
    });
    whole.filter(|number| (min..=max).contains(number)).unwrap_or(fallback)
}

/// The values once each, in first-seen order.
fn unique(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values.into_iter().filter(|value| seen.insert(value.clone())).collect()
}

pub struct Store {
    path: PathBuf,
    state: Mutex<StoredState>,
}

impl Store {
    /// Opens the state file. A missing file is an empty state and is not created until the first change. A file that
    /// cannot be parsed is moved aside to `state.json.corrupt` (it is the user's tabs and pins) and the state starts
    /// empty.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let state = match fs::read(&path) {
            Ok(bytes) => {
                let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
                match serde_json::from_slice::<Value>(json) {
                    Ok(raw) if raw.is_object() => migrate_state(&raw),
                    _ => {
                        quarantine(&path);
                        StoredState::empty()
                    }
                }
            }
            Err(_) => StoredState::empty(),
        };
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    fn lock(&self) -> MutexGuard<'_, StoredState> {
        // A change is applied to a copy and swapped in only after the write, so a panic elsewhere cannot leave this
        // half-changed and the state is safe to keep using.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn state(&self) -> StoredState {
        self.lock().clone()
    }

    /// Reads, changes and writes the state as one step. Host code that needs a read-modify-write (adding a tab) uses
    /// this rather than `state()` then a setter, which another call could slip between.
    pub fn update<R>(&self, change: impl FnOnce(&mut StoredState) -> R) -> Result<R> {
        let mut state = self.lock();
        let mut next = state.clone();
        let result = change(&mut next);
        self.save(&next)?;
        *state = next;
        Ok(result)
    }

    fn save(&self, state: &StoredState) -> Result<()> {
        let text = serde_json::to_string_pretty(state)?;
        write_atomic(&self.path, text.as_bytes(), false)
            .with_context(|| format!("Could not save {}", self.path.display()))
    }

    /// Merges `patch` over the settings, one level deep, as `settings:set` always has. Returns the settings.
    pub fn set_settings(&self, patch: &Value) -> Result<Map<String, Value>> {
        let Some(patch) = patch.as_object() else {
            bail!("Settings must be an object");
        };
        self.update(|state| {
            for (key, value) in patch {
                state.settings.insert(key.clone(), value.clone());
            }
            state.settings.clone()
        })
    }

    pub fn set_tabs(&self, tabs: Vec<ProjectTab>, active_tab_id: Option<String>) -> Result<()> {
        self.update(|state| {
            state.tabs = tabs;
            state.active_tab_id = active_tab_id;
        })
    }

    pub fn set_model(&self, model: Option<String>) -> Result<()> {
        self.update(|state| state.model = model)
    }

    pub fn set_session_pinned(&self, session_file: &str, pinned: bool) -> Result<Vec<String>> {
        self.update(|state| {
            let mut files = unique(std::mem::take(&mut state.pinned_session_files));
            if pinned {
                if !files.iter().any(|file| file == session_file) {
                    files.push(session_file.to_string());
                }
            } else {
                files.retain(|file| file != session_file);
            }
            state.pinned_session_files = files.clone();
            files
        })
    }

    /// Remembers a file the user picked, newest first. It is stored in the resolved form `validate::existing_file`
    /// returns, because that is what the later "may the renderer read this" check compares it with.
    pub fn add_preview_file(&self, file: &Path) -> Result<Vec<String>> {
        let resolved = crate::validate::resolve(file).or_else(|_| std::path::absolute(file))?;
        let path = resolved.to_string_lossy().into_owned();
        self.update(|state| {
            state.preview_files.retain(|item| *item != path);
            state.preview_files.insert(0, path);
            state.preview_files.truncate(PREVIEW_FILES_LIMIT);
            state.preview_files.clone()
        })
    }

    /// One-time import of the Electron build's state (settings, tabs, pins, preview files). It happens only while this
    /// build has no state file of its own; any change this build saves creates one, which ends the import for good.
    /// Returns whether anything was imported.
    pub fn import_electron_state(&self, from: &Path) -> Result<bool> {
        let mut state = self.lock();
        if self.path.try_exists()? {
            return Ok(false);
        }
        let text = match fs::read_to_string(from) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).with_context(|| format!("Could not read {}", from.display())),
        };
        let raw: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .with_context(|| format!("{} is not valid JSON", from.display()))?;
        if !raw.is_object() {
            bail!("{} must hold a JSON object", from.display());
        }
        let imported = migrate_state(&raw);
        self.save(&imported)?;
        *state = imported;
        Ok(true)
    }
}

/// Moves an unreadable data file out of the way instead of letting the next save overwrite it. Best effort: if it
/// cannot be moved the caller still carries on with an empty state.
pub(crate) fn quarantine(path: &Path) {
    let mut aside = path.as_os_str().to_owned();
    aside.push(".corrupt");
    let _ = fs::rename(path, aside);
}

/// Writes `bytes` to `path` so a reader sees the old file or the new one and never half of it: a temp file in the
/// same folder, flushed, then renamed over the target. `private` creates it 0600 on Unix, so a key is never readable
/// by others, not even before the rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = dir.join(format!(
        "{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = write_file(&temp, bytes, private).and_then(|()| rename_over(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

fn write_file(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    if private {
        owner_only(&mut options);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn owner_only(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn owner_only(_: &mut fs::OpenOptions) {}

/// Windows refuses to replace a file another process has open, which a virus scanner or the Prime CLI reading the
/// file does for a moment. A few short retries ride that out; elsewhere the first failure is final.
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Err(_) if cfg!(windows) && attempt < 5 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(30 * attempt));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// An absolute path that is valid on this system.
    fn abs(name: &str) -> String {
        if cfg!(windows) {
            format!(r"C:\tmp\{name}")
        } else {
            format!("/tmp/{name}")
        }
    }

    fn store_in(dir: &Path) -> Store {
        Store::open(dir.join("state.json"))
    }

    fn on_disk(dir: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(dir.join("state.json")).unwrap()).unwrap()
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let names = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned());
        names.filter(|name| name.ends_with(".tmp")).collect()
    }

    #[test]
    fn older_state_files_get_empty_preview_files() {
        assert!(migrate_state(&json!({})).preview_files.is_empty());
        assert!(
            migrate_state(&json!({ "previewFiles": "nope" }))
                .preview_files
                .is_empty()
        );
    }

    #[test]
    fn preview_files_keep_only_unique_absolute_paths_capped_at_fifty() {
        let many: Vec<Value> = (0..60).map(|i| json!(abs(&format!("file-{i}.txt")))).collect();
        let mut files = vec![
            json!(abs("a.txt")),
            json!("relative.txt"),
            json!(42),
            json!(abs("a.txt")),
            json!(""),
        ];
        files.extend(many);
        let migrated = migrate_state(&json!({ "previewFiles": files }));
        assert_eq!(migrated.preview_files[0], abs("a.txt"));
        assert!(!migrated.preview_files.contains(&"relative.txt".to_string()));
        assert_eq!(migrated.preview_files.len(), 50);
        assert_eq!(migrated.preview_files.iter().collect::<HashSet<_>>().len(), 50);
    }

    #[test]
    fn preview_paths_over_the_length_cap_are_dropped() {
        let long = format!("{}{}", abs(""), "x".repeat(PREVIEW_PATH_MAX));
        let ok = format!("{}{}", abs(""), "y".repeat(100));
        let migrated = migrate_state(&json!({ "previewFiles": [long, ok] }));
        assert_eq!(migrated.preview_files, vec![ok]);
    }

    #[test]
    fn an_empty_state_has_the_documented_defaults() {
        let state = migrate_state(&json!({}));
        assert_eq!(state, StoredState::empty());
        assert_eq!(state.version, 2);
        assert_eq!(state.settings["thinkingLevel"], "medium");
        assert_eq!(state.settings["lightTheme"]["accent"], "#c97b76");
        assert_eq!(state.settings["darkTheme"]["surface"], "#00000e");
        assert_eq!(state.settings["autonomous"]["maxSeconds"], 3600);
    }

    #[test]
    fn settings_migrate_with_defaults_filled_in_and_unknown_keys_kept() {
        let raw = json!({ "settings": {
            "notifications": false,
            "somethingNewer": { "a": 1 },
            "autonomous": { "enabled": true, "maxTurns": 5 },
            "lightTheme": { "accent": "#000000", "fonts": { "ui": "Inter" }, "semanticColors": { "skill": "#111111" } },
            "darkTheme": "broken",
            "rlmMaxDepth": 4,
            "themeMode": "dark",
            "transport": "sse"
        } });
        let settings = migrate_state(&raw).settings;
        assert_eq!(settings["notifications"], false);
        assert_eq!(settings["somethingNewer"], json!({ "a": 1 }));
        assert_eq!(settings["autonomous"]["enabled"], true);
        assert_eq!(settings["autonomous"]["maxTurns"], 5);
        assert_eq!(settings["autonomous"]["gateRetries"], 2);
        assert_eq!(settings["lightTheme"]["accent"], "#000000");
        assert_eq!(
            settings["lightTheme"]["fonts"],
            json!({ "code": "Geist Mono", "ui": "Inter" })
        );
        assert_eq!(settings["lightTheme"]["semanticColors"]["skill"], "#111111");
        assert_eq!(settings["lightTheme"]["semanticColors"]["diffAdded"], "#2da44e");
        assert_eq!(settings["lightTheme"]["contrast"], 42);
        assert_eq!(settings["darkTheme"], default_settings()["darkTheme"]);
        assert_eq!(settings["rlmMaxDepth"], 4);
        assert_eq!(settings["themeMode"], "dark");
        assert_eq!(settings["transport"], "sse");
    }

    #[test]
    fn invalid_enum_and_range_settings_fall_back() {
        for bad in [json!(11), json!(-1), json!(1.5), json!("2"), json!(null), json!(1e300)] {
            let settings = migrate_state(&json!({ "settings": { "rlmMaxDepth": bad } })).settings;
            assert_eq!(settings["rlmMaxDepth"], 1, "{bad}");
        }
        let whole_float: Value = serde_json::from_str(r#"{ "settings": { "rlmMaxDepth": 3.0 } }"#).unwrap();
        assert_eq!(migrate_state(&whole_float).settings["rlmMaxDepth"], 3);
        let odd =
            migrate_state(&json!({ "settings": { "themeMode": "sepia", "transport": "carrier-pigeon" } })).settings;
        assert_eq!(odd["themeMode"], "system");
        assert_eq!(odd["transport"], "auto");
        for transport in ["sse", "websocket", "websocket-cached", "auto"] {
            let kept = migrate_state(&json!({ "settings": { "transport": transport } })).settings;
            assert_eq!(kept["transport"], transport);
        }
    }

    #[test]
    fn garbage_settings_become_the_defaults() {
        for settings in [json!([1, 2]), json!("x"), json!(null), json!(7)] {
            assert_eq!(
                migrate_state(&json!({ "settings": settings })).settings,
                default_settings()
            );
        }
        assert_eq!(migrate_state(&json!(null)), StoredState::empty());
        assert_eq!(migrate_state(&json!([])), StoredState::empty());
    }

    #[test]
    fn tabs_without_all_three_strings_are_dropped_and_the_active_tab_stays_valid() {
        let raw = json!({
            "tabs": [
                { "id": "a", "path": "/p/a", "name": "a" },
                { "id": 2, "path": "/p/b", "name": "b" },
                { "id": "c", "path": "/p/c" },
                null,
                "x",
                { "id": "d", "path": "/p/d", "name": "d", "extra": true }
            ],
            "activeTabId": "d"
        });
        let state = migrate_state(&raw);
        assert_eq!(
            state.tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(),
            ["a", "d"]
        );
        assert_eq!(state.active_tab_id.as_deref(), Some("d"));
        let gone = migrate_state(&json!({ "tabs": raw["tabs"], "activeTabId": "zzz" }));
        assert_eq!(gone.active_tab_id.as_deref(), Some("a"));
        let none = migrate_state(&json!({ "tabs": "no", "activeTabId": "a" }));
        assert!(none.tabs.is_empty() && none.active_tab_id.is_none());
    }

    #[test]
    fn model_and_pins_are_type_checked() {
        let state = migrate_state(&json!({ "model": 5, "pinnedSessionFiles": ["a", 1, null, "b"] }));
        assert_eq!(state.model, None);
        assert_eq!(state.pinned_session_files, ["a", "b"]);
        assert_eq!(migrate_state(&json!({ "model": "p/m" })).model.as_deref(), Some("p/m"));
        assert!(
            migrate_state(&json!({ "pinnedSessionFiles": "no" }))
                .pinned_session_files
                .is_empty()
        );
    }

    #[test]
    fn the_state_serializes_with_the_renderer_field_names() {
        let value = serde_json::to_value(StoredState::empty()).unwrap();
        let keys: HashSet<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        let expected = [
            "version",
            "settings",
            "tabs",
            "activeTabId",
            "model",
            "pinnedSessionFiles",
            "previewFiles",
        ];
        assert_eq!(keys, HashSet::from(expected));
        assert_eq!(value["activeTabId"], Value::Null);
        assert_eq!(value["model"], Value::Null);
    }

    #[test]
    fn a_missing_file_is_an_empty_state_and_is_not_created_by_opening() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        assert_eq!(store.state(), StoredState::empty());
        assert!(!dir.path().join("state.json").exists());
    }

    #[test]
    fn changes_are_saved_and_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let tab = ProjectTab {
            id: "t1".into(),
            path: abs("p"),
            name: "p".into(),
        };
        store.set_tabs(vec![tab.clone()], Some("t1".into())).unwrap();
        store.set_model(Some("anthropic/x".into())).unwrap();
        store.set_session_pinned(&abs("s.jsonl"), true).unwrap();
        let settings = store
            .set_settings(&json!({ "notifications": false, "thinkingLevel": "high" }))
            .unwrap();
        assert_eq!(settings["notifications"], false);
        assert_eq!(settings["autoRetry"], true);

        let reopened = store_in(dir.path()).state();
        assert_eq!(reopened.tabs, vec![tab]);
        assert_eq!(reopened.active_tab_id.as_deref(), Some("t1"));
        assert_eq!(reopened.model.as_deref(), Some("anthropic/x"));
        assert_eq!(reopened.pinned_session_files, [abs("s.jsonl")]);
        assert_eq!(reopened.settings["thinkingLevel"], "high");
        assert_eq!(on_disk(dir.path())["version"], 2);
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn the_file_is_pretty_printed_with_two_spaces() {
        let dir = tempfile::tempdir().unwrap();
        store_in(dir.path()).set_model(None).unwrap();
        let text = fs::read_to_string(dir.path().join("state.json")).unwrap();
        assert!(text.starts_with("{\n  \"version\": 2,\n  \"settings\": {"), "{text}");
    }

    #[test]
    fn settings_patches_replace_whole_top_level_values_and_must_be_objects() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let settings = store
            .set_settings(&json!({ "autonomous": { "enabled": true }, "extra": 1 }))
            .unwrap();
        // One level deep: the patched `autonomous` replaces the old one entirely, as in the TypeScript.
        assert_eq!(settings["autonomous"], json!({ "enabled": true }));
        assert_eq!(settings["extra"], 1);
        for bad in [json!(null), json!([1]), json!("x"), json!(3)] {
            assert!(store.set_settings(&bad).is_err());
        }
        assert_eq!(store.state().settings["extra"], 1);
    }

    #[test]
    fn pinning_is_idempotent_ordered_and_reversible() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        assert_eq!(store.set_session_pinned("a", true).unwrap(), ["a"]);
        assert_eq!(store.set_session_pinned("b", true).unwrap(), ["a", "b"]);
        assert_eq!(store.set_session_pinned("a", true).unwrap(), ["a", "b"]);
        assert_eq!(store.set_session_pinned("a", false).unwrap(), ["b"]);
        assert_eq!(store.set_session_pinned("missing", false).unwrap(), ["b"]);
        assert_eq!(store.state().pinned_session_files, ["b"]);
    }

    #[test]
    fn pinning_collapses_duplicates_found_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("state.json"),
            json!({ "pinnedSessionFiles": ["a", "b", "a"] }).to_string(),
        )
        .unwrap();
        let store = store_in(dir.path());
        assert_eq!(store.state().pinned_session_files, ["a", "b", "a"]);
        assert_eq!(store.set_session_pinned("c", true).unwrap(), ["a", "b", "c"]);
        assert_eq!(store.set_session_pinned("a", false).unwrap(), ["b", "c"]);
    }

    #[test]
    fn a_picked_file_goes_to_the_front_without_duplicates_and_the_list_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let files: Vec<PathBuf> = (0..52).map(|i| dir.path().join(format!("f{i}.txt"))).collect();
        for file in &files {
            fs::write(file, "x").unwrap();
        }
        for file in &files[..3] {
            store.add_preview_file(file).unwrap();
        }
        let after = store.add_preview_file(&files[0]).unwrap();
        let names: Vec<_> = after
            .iter()
            .map(|path| Path::new(path).file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["f0.txt", "f2.txt", "f1.txt"]);
        for file in &files[3..] {
            store.add_preview_file(file).unwrap();
        }
        assert_eq!(store.state().preview_files.len(), 50);
        assert!(store.state().preview_files[0].ends_with("f51.txt"));
    }

    #[test]
    fn a_picked_file_is_stored_in_the_form_the_validators_return() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        fs::write(&real, "x").unwrap();
        let store = store_in(dir.path());
        let via_dots = dir.path().join("sub").join("..").join("real.txt");
        fs::create_dir(dir.path().join("sub")).unwrap();
        let stored = store.add_preview_file(&via_dots).unwrap();
        let checked = crate::validate::existing_file("path", via_dots.to_str().unwrap()).unwrap();
        assert_eq!(stored, [checked.to_string_lossy().into_owned()]);
        #[cfg(unix)]
        {
            let link = dir.path().join("link.txt");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let stored = store.add_preview_file(&link).unwrap();
            assert_eq!(stored[0], checked.to_string_lossy());
            assert_eq!(stored.len(), 1);
        }
    }

    #[test]
    fn a_file_that_does_not_exist_yet_is_still_remembered_as_an_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let stored = store.add_preview_file(&dir.path().join("later.txt")).unwrap();
        assert!(Path::new(&stored[0]).is_absolute());
        assert!(stored[0].ends_with("later.txt"));
    }

    #[test]
    fn concurrent_changes_never_lose_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(store_in(dir.path()));
        let workers: Vec<_> = (0..16)
            .map(|worker| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for n in 0..5 {
                        store.set_session_pinned(&format!("w{worker}-{n}"), true).unwrap();
                        store.set_settings(&json!({ format!("k{worker}-{n}"): n })).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let saved = on_disk(dir.path());
        assert_eq!(saved["pinnedSessionFiles"].as_array().unwrap().len(), 80);
        for worker in 0..16 {
            for n in 0..5 {
                assert_eq!(saved["settings"][format!("k{worker}-{n}")], n);
            }
        }
        assert_eq!(store.state().pinned_session_files.len(), 80);
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn a_failed_save_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // The parent of the state file is a regular file, so it can never be created.
        fs::write(dir.path().join("blocker"), "x").unwrap();
        let store = Store::open(dir.path().join("blocker").join("state.json"));
        assert!(store.set_model(Some("x/y".into())).is_err());
        assert_eq!(store.state().model, None);
        assert!(store.set_settings(&json!({ "notifications": false })).is_err());
        assert_eq!(store.state().settings["notifications"], true);
    }

    #[test]
    fn a_corrupt_file_is_kept_aside_and_the_state_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        fs::write(&file, "{ \"tabs\": [").unwrap();
        let store = Store::open(&file);
        assert_eq!(store.state(), StoredState::empty());
        assert_eq!(
            fs::read_to_string(dir.path().join("state.json.corrupt")).unwrap(),
            "{ \"tabs\": ["
        );
        assert!(!file.exists());
        store.set_model(Some("a/b".into())).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("state.json.corrupt")).unwrap(),
            "{ \"tabs\": ["
        );
        // A file that parses but is not an object is just as unusable.
        fs::write(&file, "[1]").unwrap();
        assert_eq!(Store::open(&file).state(), StoredState::empty());
    }

    #[test]
    fn a_file_that_is_not_text_is_kept_aside_too() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("state.json"), b"{\"model\":\"\xff\xfe\"}").unwrap();
        assert_eq!(store_in(dir.path()).state(), StoredState::empty());
        assert!(dir.path().join("state.json.corrupt").exists());
    }

    #[test]
    fn a_byte_order_mark_does_not_make_the_file_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("state.json"), "\u{feff}{\"model\":\"a/b\"}").unwrap();
        assert_eq!(store_in(dir.path()).state().model.as_deref(), Some("a/b"));
        assert!(!dir.path().join("state.json.corrupt").exists());
    }

    #[test]
    fn the_electron_state_is_imported_once_and_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let electron = dir.path().join("PrimeDesktop").join("state.json");
        fs::create_dir_all(electron.parent().unwrap()).unwrap();
        let old = json!({
            "version": 1,
            "settings": { "thinkingLevel": "low", "rlmMaxDepth": 99 },
            "tabs": [{ "id": "t1", "path": abs("p"), "name": "p" }, { "id": 5 }],
            "activeTabId": "t1",
            "model": "openai/gpt",
            "pinnedSessionFiles": [abs("s.jsonl")],
            "previewFiles": [abs("a.txt"), "relative"]
        });
        fs::write(&electron, old.to_string()).unwrap();

        let new_dir = dir.path().join("new");
        let store = store_in(&new_dir);
        assert!(store.import_electron_state(&electron).unwrap());
        let state = store.state();
        assert_eq!(state.version, 2);
        assert_eq!(state.settings["thinkingLevel"], "low");
        assert_eq!(state.settings["rlmMaxDepth"], 1);
        assert_eq!(state.tabs.len(), 1);
        assert_eq!(state.active_tab_id.as_deref(), Some("t1"));
        assert_eq!(state.model.as_deref(), Some("openai/gpt"));
        assert_eq!(state.pinned_session_files, [abs("s.jsonl")]);
        assert_eq!(state.preview_files, [abs("a.txt")]);
        assert_eq!(on_disk(&new_dir)["model"], "openai/gpt");

        // The import is one-time: a later change to the old file is ignored, and the old file is left alone.
        fs::write(&electron, json!({ "model": "other/m" }).to_string()).unwrap();
        assert!(!store.import_electron_state(&electron).unwrap());
        assert!(!store_in(&new_dir).import_electron_state(&electron).unwrap());
        assert_eq!(store_in(&new_dir).state().model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn nothing_is_imported_when_there_is_no_electron_state_or_this_build_already_has_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir.path().join("new"));
        assert!(!store.import_electron_state(&dir.path().join("nope.json")).unwrap());
        assert!(!dir.path().join("new").join("state.json").exists());

        let electron = dir.path().join("e.json");
        fs::write(&electron, json!({ "model": "e/m" }).to_string()).unwrap();
        store.set_model(Some("mine/m".into())).unwrap();
        assert!(!store.import_electron_state(&electron).unwrap());
        assert_eq!(store.state().model.as_deref(), Some("mine/m"));
    }

    #[test]
    fn an_unreadable_electron_state_is_an_error_and_imports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir.path().join("new"));
        for bad in ["{ nope", "[1, 2]", "42"] {
            let electron = dir.path().join("e.json");
            fs::write(&electron, bad).unwrap();
            assert!(store.import_electron_state(&electron).is_err(), "{bad}");
        }
        assert_eq!(store.state(), StoredState::empty());
        assert!(!dir.path().join("new").join("state.json").exists());
        // A directory where the file should be is an error too, not "no Electron state".
        let folder = dir.path().join("folder");
        fs::create_dir(&folder).unwrap();
        assert!(store.import_electron_state(&folder).is_err());
    }

    #[test]
    fn the_electron_location_is_the_hard_coded_macos_one() {
        let path = electron_state_file();
        assert!(path.starts_with(crate::platform::home_dir()));
        assert!(
            path.ends_with(
                Path::new("Library")
                    .join("Application Support")
                    .join("PrimeDesktop")
                    .join("state.json")
            )
        );
    }

    #[test]
    fn atomic_writes_replace_the_target_and_can_be_private() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("deep").join("er").join("f.json");
        write_atomic(&target, b"one", true).unwrap();
        write_atomic(&target, b"two", true).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "two");
        assert!(leftovers(target.parent().unwrap()).is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn a_failed_atomic_write_leaves_the_old_file_and_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.json");
        fs::write(&target, "old").unwrap();
        // A directory cannot be replaced by a file, so the rename fails after the temp file was written.
        let blocked = dir.path().join("blocked");
        fs::create_dir(&blocked).unwrap();
        assert!(write_atomic(&blocked, b"new", false).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        assert!(leftovers(dir.path()).is_empty());
    }
}

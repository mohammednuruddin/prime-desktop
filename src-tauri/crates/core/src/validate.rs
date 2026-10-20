//! Argument checks for every command that crosses from the renderer: strings, paths, URLs and agent commands.
//!
//! Tauri's serde decoding checks types; it does not check length, range, existence or containment. These do, and
//! every command calls them before it touches the disk, a process or the daemon.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde_json::Value;

pub const DEFAULT_MAX_TEXT: usize = 200_000;
const MAX_PATH_TEXT: usize = 4_096;
const MAX_URL_TEXT: usize = 8_192;

/// A string of at most `max` characters.
pub fn text<'a>(name: &str, value: &'a str, max: usize) -> Result<&'a str> {
    if value.chars().count() > max {
        bail!("{name} must be a string of at most {max} characters");
    }
    Ok(value)
}

/// A non-blank string of at most `max` characters, trimmed.
pub fn non_empty(name: &str, value: &str, max: usize) -> Result<String> {
    let trimmed = text(name, value, max)?.trim();
    if trimmed.is_empty() {
        bail!("{name} is required");
    }
    Ok(trimmed.to_string())
}

/// A finite number inside optional bounds.
pub fn number(name: &str, value: f64, min: Option<f64>, max: Option<f64>) -> Result<f64> {
    if !value.is_finite() {
        bail!("{name} must be a finite number");
    }
    if let Some(min) = min.filter(|min| value < *min) {
        bail!("{name} must be at least {min}");
    }
    if let Some(max) = max.filter(|max| value > *max) {
        bail!("{name} must be at most {max}");
    }
    Ok(value)
}

/// An absolute path to an existing directory, in its resolved form.
pub fn existing_dir(name: &str, value: &str) -> Result<PathBuf> {
    let path = absolute_path(name, value)?;
    if !path.is_dir() {
        bail!("{name} is not an existing directory");
    }
    resolve(&path)
}

/// An absolute path to an existing file, in its resolved form.
pub fn existing_file(name: &str, value: &str) -> Result<PathBuf> {
    let path = absolute_path(name, value)?;
    if !path.is_file() {
        bail!("{name} is not an existing file");
    }
    resolve(&path)
}

fn absolute_path(name: &str, value: &str) -> Result<PathBuf> {
    let value = non_empty(name, value, MAX_PATH_TEXT)?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        bail!("{name} must be an absolute path");
    }
    Ok(path)
}

/// The canonical form of an existing path, without the `\\?\` prefix Windows adds, so it reads and compares like the
/// path the user chose.
pub fn resolve(path: &Path) -> Result<PathBuf> {
    Ok(simplify(std::fs::canonicalize(path)?))
}

/// `\\?\C:\x` becomes `C:\x`. Only the drive form is rewritten: `\\?\UNC\...` and device paths keep their prefix,
/// because dropping it would change what they name.
pub fn simplify(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') && !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

/// Only plain web links may be opened outside the app.
pub fn safe_external_url(value: &str) -> Result<String> {
    let raw = non_empty("url", value, MAX_URL_TEXT)?;
    let Ok(url) = url::Url::parse(&raw) else {
        bail!("Invalid external URL");
    };
    if url.scheme() != "https" && url.scheme() != "http" {
        bail!("Only HTTP(S) external URLs are allowed");
    }
    Ok(url.to_string())
}

/// Whether `candidate` is `root` or inside it. Both are resolved first, so a link that points out of the root does not
/// count as inside it, and a sibling that merely shares a name prefix does not either. A candidate that does not exist
/// yet is resolved through its nearest existing ancestor: `root/link/new.txt` with `link` pointing outside is outside.
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    match (comparable(root), comparable(candidate)) {
        (Some(root), Some(candidate)) => candidate.starts_with(&root),
        _ => false,
    }
}

/// The resolved form of a path, for comparison. The part that does not exist yet is appended to the resolved nearest
/// existing ancestor; a `..` in that part cannot be resolved without the disk, so such a path has no form (`None`).
fn comparable(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    let resolved = loop {
        match resolve(existing) {
            Ok(resolved) => break resolved,
            Err(_) => {
                // Something is there but will not resolve: a link whose target is missing. Writing through it would
                // create the target wherever it points, so it has no resolved form either.
                if std::fs::symlink_metadata(existing).is_ok() {
                    return None;
                }
                // No name means `..` or a root that cannot be resolved: the disk has to answer, and it did not.
                missing.push(existing.file_name()?);
                existing = existing.parent()?;
            }
        }
    };
    let joined = missing.iter().rev().fold(resolved, |acc, name| acc.join(name));
    Some(if cfg!(windows) {
        PathBuf::from(joined.to_string_lossy().to_lowercase())
    } else {
        joined
    })
}

const AGENT_COMMANDS: &[&str] = &[
    "prompt",
    "steer",
    "follow_up",
    "abort",
    "compact",
    "new_session",
    "fork",
    "clone",
    "set_model",
    "set_thinking_level",
    "set_auto_compaction",
    "set_auto_retry",
    "set_session_name",
    "get_available_models",
    "get_commands",
    "bash",
    "refine",
    "get_last_assistant_text",
    "get_fork_messages",
    "get_session_stats",
    "set_heartbeat",
    "abort_retry",
];

/// A change to the app's settings. Most settings are the page's own business (themes, layout). The ones the agent manager
/// reads must be of the kind it expects, since a wrong one would silently reset all of them; the sandbox and autonomous
/// mode have their own commands, which prepare what they change.
pub fn settings_patch(patch: &Value) -> Result<()> {
    let Some(patch) = patch.as_object() else {
        bail!("Settings must be an object");
    };
    for (key, value) in patch {
        match key.as_str() {
            "sandbox" => bail!("The sandbox setting is changed with the sandbox control."),
            "autonomous" => bail!("Autonomous mode is changed with its own control."),
            "autoCompaction" | "autoRetry" | "checkpoints" | "notifications" | "autoNameSessions" | "dockBadge"
                if !value.is_boolean() =>
            {
                bail!("{key} must be true or false")
            }
            "thinkingLevel" | "transport" => {
                text(key, value.as_str().unwrap_or(""), 64)?;
                if !value.is_string() {
                    bail!("{key} must be text");
                }
            }
            "model" if !(value.is_null() || value.is_string()) => bail!("model must be text"),
            "rlmMaxDepth" if !value.as_u64().is_some_and(|depth| depth <= 10) => {
                bail!("rlmMaxDepth must be a whole number from 0 to 10")
            }
            _ => {}
        }
    }
    Ok(())
}

/// An agent command the renderer may send: a known `type`, and for commands that carry text, non-blank text.
pub fn agent_command(value: &Value) -> Result<&Value> {
    let Some(command) = value.as_object() else {
        bail!("Invalid agent command");
    };
    let kind = command.get("type").and_then(Value::as_str);
    let Some(kind) = kind.filter(|kind| AGENT_COMMANDS.contains(kind)) else {
        bail!("Unsupported agent command");
    };
    if ["prompt", "steer", "follow_up", "bash"].contains(&kind) {
        let body = command
            .get("message")
            .or_else(|| command.get("command"))
            .and_then(Value::as_str);
        non_empty("command text", body.unwrap_or(""), DEFAULT_MAX_TEXT)?;
    }
    if kind == "set_heartbeat" {
        for field in ["schedule", "prompt"] {
            let text = command.get(field).and_then(Value::as_str).unwrap_or("");
            non_empty(field, text, DEFAULT_MAX_TEXT)?;
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn urls_accept_web_links_and_refuse_everything_else() {
        assert_eq!(
            safe_external_url("https://example.com/path").unwrap(),
            "https://example.com/path"
        );
        assert!(
            safe_external_url("javascript:alert(1)")
                .unwrap_err()
                .to_string()
                .contains("HTTP(S)")
        );
        assert!(
            safe_external_url("file:///etc/passwd")
                .unwrap_err()
                .to_string()
                .contains("HTTP(S)")
        );
        assert!(safe_external_url("mailto:a@b.c").is_err());
        assert!(safe_external_url("not a url").is_err());
        assert!(safe_external_url("   ").is_err());
    }

    #[test]
    fn containment_has_no_prefix_confusion() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let other = dir.path().join("project-other");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        assert!(is_within(&project, &project.join("src")));
        assert!(is_within(&project, &project));
        assert!(!is_within(&project, &other));
        assert!(!is_within(&project, &project.join("..").join("project-other")));
        assert!(is_within(&project, &project.join("not-yet").join("made")));
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_does_not_exist_yet_is_judged_through_its_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, project.join("shortcut")).unwrap();
        // Written through the link, the new file would land outside the project.
        assert!(!is_within(&project, &project.join("shortcut").join("new.txt")));
        assert!(!is_within(
            &project,
            &project.join("shortcut").join("deep").join("er").join("new.txt")
        ));
        // A link whose target does not exist yet: writing through it would create the target outside.
        std::os::unix::fs::symlink(outside.join("not-yet"), project.join("dangling")).unwrap();
        assert!(!is_within(&project, &project.join("dangling")));
        assert!(!is_within(&project, &project.join("dangling").join("child.txt")));
        // A `..` in the part that does not exist cannot be resolved, so it is never inside.
        assert!(!is_within(
            &project,
            &project.join("missing").join("..").join("..").join("outside")
        ));
        assert!(!is_within(
            &project,
            &project.join("missing").join("..").join("kept.txt")
        ));
        assert!(is_within(&project, &project.join("missing").join("kept.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_root_is_not_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let secret = dir.path().join("secret");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("key"), "k").unwrap();
        std::os::unix::fs::symlink(&secret, project.join("shortcut")).unwrap();
        assert!(!is_within(&project, &project.join("shortcut").join("key")));
    }

    #[test]
    fn paths_must_be_absolute_and_exist() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(existing_dir("folder", dir.path().to_str().unwrap()).is_ok());
        assert!(existing_dir("folder", file.to_str().unwrap()).is_err());
        assert!(existing_file("file", file.to_str().unwrap()).is_ok());
        assert!(existing_file("file", dir.path().to_str().unwrap()).is_err());
        assert!(
            existing_dir("folder", "relative/path")
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        assert!(existing_dir("folder", "").unwrap_err().to_string().contains("required"));
        assert!(existing_dir("folder", &"x".repeat(5000)).is_err());
    }

    #[test]
    fn the_verbatim_prefix_is_dropped_only_for_drive_paths() {
        assert_eq!(simplify(PathBuf::from(r"\\?\C:\Users\a")), PathBuf::from(r"C:\Users\a"));
        assert_eq!(
            simplify(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\?\UNC\server\share")
        );
        assert_eq!(simplify(PathBuf::from(r"\\?\pipe\x")), PathBuf::from(r"\\?\pipe\x"));
        assert_eq!(simplify(PathBuf::from("/usr/bin")), PathBuf::from("/usr/bin"));
    }

    #[test]
    fn text_and_numbers_are_bounded() {
        assert!(text("t", "abc", 3).is_ok());
        assert!(text("t", "abcd", 3).is_err());
        assert_eq!(non_empty("t", "  hi  ", 10).unwrap(), "hi");
        assert!(non_empty("t", "   ", 10).is_err());
        assert!(number("n", f64::NAN, None, None).is_err());
        assert!(number("n", 5.0, Some(6.0), None).is_err());
        assert!(number("n", 5.0, None, Some(4.0)).is_err());
        assert_eq!(number("n", 5.0, Some(0.0), Some(10.0)).unwrap(), 5.0);
    }

    #[test]
    fn agent_commands_are_an_allow_list() {
        assert!(agent_command(&json!({"type": "prompt", "message": "hi"})).is_ok());
        assert!(agent_command(&json!({"type": "abort"})).is_ok());
        assert!(agent_command(&json!({"type": "bash", "command": "ls"})).is_ok());
        assert!(agent_command(&json!({"type": "prompt", "message": "  "})).is_err());
        assert!(agent_command(&json!({"type": "prompt"})).is_err());
        assert!(
            agent_command(&json!({"type": "rm_rf"}))
                .unwrap_err()
                .to_string()
                .contains("Unsupported")
        );
        assert!(agent_command(&json!({"message": "no type"})).is_err());
        assert!(agent_command(&json!("prompt")).is_err());
        assert!(agent_command(&json!([1])).is_err());
    }

    #[test]
    fn only_commands_the_page_needs_are_allowed_and_resuming_or_exporting_is_not_one() {
        for kind in [
            "get_last_assistant_text",
            "get_fork_messages",
            "get_session_stats",
            "abort_retry",
            "refine",
        ] {
            assert!(agent_command(&json!({"type": kind})).is_ok(), "{kind}");
        }
        // These take a file path and have their own checked commands.
        for kind in ["switch_session", "export_html", "import_jsonl", "kill", "shutdown"] {
            assert!(agent_command(&json!({"type": kind})).is_err(), "{kind}");
        }
        assert!(agent_command(&json!({"type": "set_heartbeat", "schedule": "5m", "prompt": "check"})).is_ok());
        assert!(agent_command(&json!({"type": "set_heartbeat", "schedule": "5m"})).is_err());
    }

    #[test]
    fn a_settings_patch_cannot_lower_the_sandbox_or_break_what_the_manager_reads() {
        assert!(
            settings_patch(&json!({"themeMode": "dark", "darkTheme": {"surface": "#000000"}, "model": null})).is_ok()
        );
        assert!(settings_patch(&json!({"rlmMaxDepth": 3, "transport": "auto", "checkpoints": false})).is_ok());
        for bad in [
            json!({"sandbox": "unprotected"}),
            json!({"autonomous": {"gates": ["x"]}}),
            json!({"checkpoints": "no"}),
            json!({"thinkingLevel": 5}),
            json!({"rlmMaxDepth": 11}),
            json!({"rlmMaxDepth": -1}),
            json!({"model": 3}),
            json!([1]),
        ] {
            assert!(settings_patch(&bad).is_err(), "{bad} was accepted");
        }
    }
}

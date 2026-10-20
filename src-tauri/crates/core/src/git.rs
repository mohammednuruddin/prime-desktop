//! The git the app runs: status, diffs, stage, commit, checkpoints and restore. Never runs a command a repository names
//! on the read side; the write side cannot be fully closed (see [`write`]).
//!
//! Checkpoints are commits on a side ref the app owns (`refs/prime-desktop/checkpoints/<sha>`). The ref names, the
//! commit message and the JSON files that remember them match the Electron build, so its checkpoints are still found.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::{platform, validate};

// ---------- Shapes the renderer reads (src/shared/types.ts) ----------

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitChange {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub is_repo: bool,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub changes: Vec<GitChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub sha: String,
    pub summary: String,
    pub status: GitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub status: FileStatus,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: String,
    pub created_at: u64,
    pub label: String,
    pub agent_id: String,
    pub dirty_files: Vec<String>,
}

// ---------- Running git ----------

// A repository can carry its own config and attributes, and an agent can write them. Git runs the commands they name
// (the fsmonitor hook, an external diff, a textconv filter) as the user, outside any sandbox. Every call turns the
// fsmonitor off; every diff turns the other two off.
pub const GIT_HARDENING: &[&str] = &["-c", "core.fsmonitor=false"];
pub const GIT_DIFF_HARDENING: &[&str] = &["--no-ext-diff", "--no-textconv"];

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
const SLOW_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_OUTPUT: usize = 20 * 1024 * 1024;
const MAX_PATH_TEXT: usize = 4_096;

/// What the caller may change per call: extra environment and how long git gets.
#[derive(Clone, Copy)]
struct Opts<'a> {
    env: &'a [(&'a str, &'a str)],
    timeout: Duration,
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Opts {
            env: &[],
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// A failed git call, with what git printed. `Display` is the text the renderer shows: stderr, else stdout (git commit
/// reports "nothing to commit" there), else the reason it could not run.
#[derive(Debug, Default)]
struct Failure {
    /// git (or the working folder) does not exist, so nothing ran.
    missing: bool,
    stdout: String,
    stderr: String,
    message: String,
}

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        Failure {
            message: message.into(),
            ..Default::default()
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = [&self.stderr, &self.stdout, &self.message]
            .into_iter()
            .map(|part| part.trim())
            .find(|part| !part.is_empty())
            .unwrap_or("git failed");
        f.write_str(text)
    }
}

impl std::error::Error for Failure {}

/// The subcommand: the first argument that is not a global option.
fn verb<'a>(args: &[&'a str]) -> &'a str {
    args.iter().find(|arg| !arg.starts_with('-')).copied().unwrap_or("")
}

/// Subcommands that change the repository: its index, objects, refs or working tree.
const WRITE_VERBS: &[&str] = &[
    "add",
    "rm",
    "restore",
    "reset",
    "commit",
    "read-tree",
    "write-tree",
    "commit-tree",
    "update-ref",
    "checkout-index",
];

/// A call that only reads. In debug builds (so in every test) a write verb routed here panics, so a new write-side
/// call cannot skip [`write`] by accident.
async fn read(dir: &Path, args: &[&str], opts: Opts<'_>) -> Result<String, Failure> {
    debug_assert!(
        !WRITE_VERBS.contains(&verb(args)),
        "write-side git must go through write(): {args:?}"
    );
    run(dir, args, opts).await
}

/// The one place write-side git runs: add, rm, restore, reset, commit and the checkpoint plumbing.
///
/// This is the known weak point. Those commands run the hooks and the clean and smudge filters the repository names,
/// as the user and outside any sandbox, and git has no switch to stop them (the filter names are arbitrary, so a `-c`
/// override cannot list them). The Electron build accepts that. A trust gate (refuse, or ask, when the repository's own
/// config names a hook or filter) belongs here, before `run`, so no caller changes. It returns `Result` for that.
async fn write(dir: &Path, args: &[&str], opts: Opts<'_>) -> Result<String, Failure> {
    debug_assert!(
        WRITE_VERBS.contains(&verb(args)),
        "read-only git must go through read(): {args:?}"
    );
    run(dir, args, opts).await
}

/// Runs `git` in `dir` and returns its stdout. Never reads stdin, never opens a console window, gives up after
/// `opts.timeout` and refuses more than [`MAX_OUTPUT`] bytes on either stream.
async fn run(dir: &Path, args: &[&str], opts: Opts<'_>) -> Result<String, Failure> {
    let mut command = Command::new("git");
    platform::quiet_async(&mut command);
    command
        .args(GIT_HARDENING)
        .args(args)
        .current_dir(dir)
        .envs(opts.env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A timed-out or abandoned call must not leave git running.
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| Failure {
        // Windows reports a working folder that is gone as "not a directory", not "not found".
        missing: error.kind() == std::io::ErrorKind::NotFound || !dir.is_dir(),
        message: error.to_string(),
        ..Default::default()
    })?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(Failure::new("git has no output pipes"));
    };
    let io = async {
        // try_join: if one stream overflows, stop at once instead of waiting for a git that is blocked writing it.
        let (out, err) = tokio::try_join!(read_capped(stdout, MAX_OUTPUT), read_capped(stderr, MAX_OUTPUT))?;
        let status = child.wait().await.map_err(|error| Failure::new(error.to_string()))?;
        Ok::<_, Failure>((status, out, err))
    };
    let done = match tokio::time::timeout(opts.timeout, io).await {
        Ok(Ok(done)) => done,
        Ok(Err(failure)) => {
            stop(&mut child).await;
            return Err(failure);
        }
        Err(_) => {
            stop(&mut child).await;
            return Err(Failure::new(format!(
                "git {} timed out after {:?}",
                verb(args),
                opts.timeout
            )));
        }
    };
    let (status, out, err) = done;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    );
    if status.success() {
        Ok(stdout)
    } else {
        Err(Failure {
            stdout,
            stderr,
            message: format!("git {} failed ({status})", verb(args)),
            missing: false,
        })
    }
}

/// Stops a git that ran too long. SIGTERM first: git removes its lock files on it, while a killed `git add` leaves
/// `index.lock` behind and blocks every later git command, the user's own included. Killed outright if it has not
/// exited two seconds later, or where there is no SIGTERM (Windows, where this is the same as Node's `kill`).
async fn stop(child: &mut tokio::process::Child) {
    if cfg!(unix)
        && let Some(pid) = child.id()
    {
        let mut kill = Command::new("kill");
        platform::quiet_async(&mut kill);
        let asked = kill.args(["-TERM", &pid.to_string()]).status().await;
        if asked.is_ok_and(|status| status.success())
            && tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
}

async fn read_capped(stream: impl AsyncRead + Unpin, max: usize) -> Result<Vec<u8>, Failure> {
    let mut buffer = Vec::new();
    stream
        .take(max as u64 + 1)
        .read_to_end(&mut buffer)
        .await
        .map_err(|error| Failure::new(error.to_string()))?;
    if buffer.len() > max {
        return Err(Failure::new(format!("git printed more than {} MiB", max / 1024 / 1024)));
    }
    Ok(buffer)
}

fn pathspecs(paths: &[String]) -> Result<Vec<&str>> {
    paths
        .iter()
        .map(|path| validate::text("path", path, MAX_PATH_TEXT))
        .collect()
}

// ---------- Repository, status ----------

pub async fn is_git_repo(dir: &Path) -> bool {
    // Inside the .git folder or a bare repository git answers "false" and still exits 0.
    matches!(read(dir, &["rev-parse", "--is-inside-work-tree"], Opts::default()).await, Ok(out) if out.trim() == "true")
}

/// `(status, path)` per changed file, the status trimmed ("M", "A", "R", "??"). `-z` so a name with non-ASCII or special
/// characters comes back as it is on disk rather than quoted, and a rename names the new path.
async fn status_short(dir: &Path) -> Vec<(String, String)> {
    let args = [
        "--no-optional-locks",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    ];
    let Ok(out) = read(dir, &args, Opts::default()).await else {
        return Vec::new();
    };
    let mut records = out.split('\0');
    let mut changes = Vec::new();
    while let Some(record) = records.next() {
        let (Some(code), Some(path)) = (record.get(..2), record.get(3..)) else {
            continue;
        };
        if code.contains(['R', 'C']) {
            records.next(); // the path it came from
        }
        changes.push((code.trim().to_string(), path.to_string()));
    }
    changes
}

pub async fn changed_files(dir: &Path) -> Vec<String> {
    status_short(dir).await.into_iter().map(|(_, path)| path).collect()
}

fn not_a_repo() -> GitStatus {
    GitStatus {
        is_repo: false,
        branch: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        changes: Vec::new(),
    }
}

/// Parses `git status --porcelain=v2 --branch -z`. One process reports the branch, upstream, ahead/behind counts and
/// every change, and the app polls it every few seconds, so it must not grow back into several git calls.
pub fn parse_status(output: &str) -> GitStatus {
    let (mut oid, mut head, mut upstream, mut ahead, mut behind) = ("", "", None, 0, 0);
    let mut changes = Vec::new();
    let records: Vec<&str> = output.split('\0').collect();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" => oid = value,
                "branch.head" => head = value,
                "branch.upstream" => upstream = Some(value.to_string()),
                "branch.ab" => {
                    let count = |part: Option<&str>| part.and_then(|n| n.trim_start_matches(['+', '-']).parse().ok());
                    let mut parts = value.split(' ');
                    ahead = count(parts.next()).unwrap_or(0);
                    behind = count(parts.next()).unwrap_or(0);
                }
                _ => {}
            }
            continue;
        }
        match record.chars().next() {
            Some('?') => changes.push(GitChange {
                path: record.get(2..).unwrap_or_default().to_string(),
                original_path: None,
                index_status: "?".into(),
                worktree_status: "?".into(),
                staged: false,
                unstaged: true,
            }),
            // Fields before the path: 8 for ordinary changes, 9 for renames and copies, 10 for conflicts.
            Some(kind @ ('1' | '2' | 'u')) => {
                let before_path = match kind {
                    '1' => 8,
                    '2' => 9,
                    _ => 10,
                };
                let fields: Vec<&str> = record.splitn(before_path + 1, ' ').collect();
                if fields.len() <= before_path {
                    continue;
                }
                let mut code = fields[1].chars().map(|c| if c == '.' { ' ' } else { c });
                let (index_status, worktree_status) = (code.next().unwrap_or(' '), code.next().unwrap_or(' '));
                let original_path = if kind == '2' {
                    index += 1;
                    records
                        .get(index - 1)
                        .filter(|path| !path.is_empty())
                        .map(|path| path.to_string())
                } else {
                    None
                };
                changes.push(GitChange {
                    path: fields[before_path].to_string(),
                    original_path,
                    index_status: index_status.to_string(),
                    worktree_status: worktree_status.to_string(),
                    staged: index_status != ' ',
                    unstaged: worktree_status != ' ',
                });
            }
            _ => {}
        }
    }
    let branch = if head == "(detached)" {
        oid.chars().take(7).collect()
    } else {
        head.to_string()
    };
    GitStatus {
        is_repo: true,
        branch: Some(if branch.is_empty() { "HEAD".into() } else { branch }),
        upstream,
        ahead,
        behind,
        changes,
    }
}

pub async fn git_status(dir: &Path) -> Result<GitStatus> {
    // Polling must not take index.lock away from the user's own git commands. LC_ALL=C because the "not a repository"
    // check below reads git's English message.
    let args = [
        "--no-optional-locks",
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
        "--untracked-files=all",
    ];
    let opts = Opts {
        env: &[("LC_ALL", "C")],
        ..Opts::default()
    };
    match read(dir, &args, opts).await {
        Ok(out) => Ok(parse_status(&out)),
        Err(failure) => {
            // A timeout or a damaged index is an error to show, not proof that this is no repository.
            let text = failure.to_string().to_lowercase();
            if failure.missing || text.contains("not a git repository") || text.contains("must be run in a work tree") {
                return Ok(not_a_repo());
            }
            Err(failure.into())
        }
    }
}

// ---------- Diffs ----------

/// `git diff` of one file. --literal-pathspecs: the path is a file name from the status list, never a pattern.
fn diff_args(path: &str, cached: bool) -> Vec<&str> {
    let mut args = vec!["--literal-pathspecs", "diff"];
    if cached {
        args.push("--cached");
    }
    args.extend(GIT_DIFF_HARDENING);
    args.extend(["--", path]);
    args
}

/// The diff of one file, staged or not. A file git does not track yet is shown as all added.
pub async fn git_file_diff(dir: &Path, path: &str, staged: bool) -> Result<String> {
    validate::text("path", path, MAX_PATH_TEXT)?;
    // The TypeScript left --no-textconv off here, so a textconv filter could run for the file view.
    let diff = read(dir, &diff_args(path, staged), Opts::default()).await?;
    if !diff.is_empty() || staged {
        return Ok(diff);
    }
    let tracked = ["--literal-pathspecs", "ls-files", "--error-unmatch", "--", path];
    if read(dir, &tracked, Opts::default()).await.is_ok() {
        return Ok(diff);
    }
    // --no-index reads whatever file it is given, so it only gets files inside the project.
    if !validate::is_within(dir, &dir.join(path)) {
        bail!("{path} is outside the project");
    }
    let mut args = vec!["diff", "--no-index"];
    args.extend(GIT_DIFF_HARDENING);
    args.extend(["--", "/dev/null", path]);
    // --no-index exits 1 when the files differ; the diff is still on stdout.
    Ok(read(dir, &args, Opts::default())
        .await
        .unwrap_or_else(|failure| failure.stdout))
}

/// The unstaged diff of a file, or the staged one when there is none. Errors read as "no diff".
async fn diff_for_file(dir: &Path, path: &str) -> String {
    let unstaged = read(dir, &diff_args(path, false), Opts::default())
        .await
        .unwrap_or_default();
    if !unstaged.trim().is_empty() {
        return unstaged;
    }
    read(dir, &diff_args(path, true), Opts::default())
        .await
        .unwrap_or_default()
}

fn file_status(code: &str) -> FileStatus {
    match code.chars().next() {
        Some('D') => FileStatus::Deleted,
        Some('A') => FileStatus::Added,
        Some('R') => FileStatus::Renamed,
        _ => FileStatus::Modified,
    }
}

/// The changed files of a project with their diffs: from git, or from file times when the folder is not a repository.
pub async fn diff_files(dir: &Path) -> Result<Vec<FileDiff>> {
    if !is_git_repo(dir).await {
        let root = dir.to_path_buf();
        return Ok(tokio::task::spawn_blocking(move || snapshot_file_changes(&root)).await?);
    }
    let mut out = Vec::new();
    for (code, path) in status_short(dir).await {
        let diff = diff_for_file(dir, &path).await;
        out.push(FileDiff {
            status: file_status(&code),
            path,
            diff,
        });
    }
    Ok(out)
}

// ---------- Changes in a folder that is not a repository ----------

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    ".next",
    ".cache",
    ".turbo",
    ".prime",
];
const SKIP_EXT: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".woff", ".woff2", ".ttf", ".lockb", ".exe", ".dll", ".dylib",
    ".so", ".bin",
];
const SNAPSHOT_MAX_FILES: usize = 5000;

/// The modification times of the previous walk of each folder.
static SNAPSHOTS: LazyLock<Mutex<HashMap<PathBuf, HashMap<String, f64>>>> = LazyLock::new(Default::default);

/// Relative path to modification time in milliseconds, for the files the app cares about.
fn walk_project(root: &Path) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while out.len() < SNAPSHOT_MAX_FILES {
        let Some(dir) = stack.pop() else { break };
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            // A link to a file counts as that file. A link to a folder is not entered: it can loop, or leave the project.
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let extension = name.rfind('.').map_or("", |dot| &name[dot..]).to_lowercase();
            if !meta.is_file() || SKIP_EXT.contains(&extension.as_str()) {
                continue;
            }
            let millis = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0.0, |age| age.as_secs_f64() * 1000.0);
            if let Ok(relative) = path.strip_prefix(root) {
                out.insert(relative.to_string_lossy().into_owned(), millis);
            }
        }
    }
    out
}

/// What changed in `root` since this was last called for it. The first call lists every file as modified, with no
/// diff; after that a diff is the new modification time, because there is no history to compare against.
pub fn snapshot_file_changes(root: &Path) -> Vec<FileDiff> {
    let current = walk_project(root);
    let previous = SNAPSHOTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(root.to_path_buf(), current.clone());
    let mut out = Vec::new();
    let mut push = |path: &String, status, diff: String| {
        out.push(FileDiff {
            path: path.clone(),
            status,
            diff,
        })
    };
    match previous {
        None => current
            .keys()
            .for_each(|path| push(path, FileStatus::Modified, String::new())),
        Some(previous) => {
            for (path, mtime) in &current {
                match previous.get(path) {
                    None => push(path, FileStatus::Added, format!("+{mtime}")),
                    Some(before) if before != mtime => push(path, FileStatus::Modified, mtime.to_string()),
                    _ => {}
                }
            }
            for path in previous.keys().filter(|path| !current.contains_key(*path)) {
                push(path, FileStatus::Deleted, String::new());
            }
        }
    }
    // A walk visits folders in whatever order the disk gives them; sorted, the list does not jump between polls.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

// ---------- Stage, unstage, commit ----------

async fn has_head(dir: &Path) -> bool {
    read(dir, &["rev-parse", "--verify", "HEAD"], Opts::default())
        .await
        .is_ok()
}

/// Each of these returns the status after the change, which is what the renderer asks for next.
pub async fn stage_files(dir: &Path, paths: &[String]) -> Result<GitStatus> {
    if !paths.is_empty() {
        let mut args = vec!["--literal-pathspecs", "add", "--"];
        args.extend(pathspecs(paths)?);
        write(dir, &args, Opts::default()).await?;
    }
    git_status(dir).await
}

pub async fn unstage_files(dir: &Path, paths: &[String]) -> Result<GitStatus> {
    if !paths.is_empty() {
        // Before the first commit there is no HEAD to restore from; the index is simply emptied of these files. -f
        // because rm --cached otherwise refuses a file edited since it was staged; it never touches the working tree.
        let mut args = if has_head(dir).await {
            vec!["--literal-pathspecs", "restore", "--staged", "--"]
        } else {
            vec!["--literal-pathspecs", "rm", "--cached", "-f", "-q", "--"]
        };
        args.extend(pathspecs(paths)?);
        write(dir, &args, Opts::default()).await?;
    }
    git_status(dir).await
}

pub async fn stage_all(dir: &Path) -> Result<GitStatus> {
    write(dir, &["add", "-A"], Opts::default()).await?;
    git_status(dir).await
}

pub async fn unstage_all(dir: &Path) -> Result<GitStatus> {
    let args: &[&str] = if has_head(dir).await {
        &["reset", "-q", "HEAD", "--", "."]
    } else {
        &["rm", "--cached", "-r", "-f", "-q", "--", "."]
    };
    write(dir, args, Opts::default()).await?;
    git_status(dir).await
}

pub async fn commit_staged(dir: &Path, message: &str) -> Result<CommitResult> {
    let message = message.trim();
    if message.is_empty() {
        bail!("Enter a commit message.");
    }
    validate::text("message", message, validate::DEFAULT_MAX_TEXT)?;
    let summary = write(dir, &["commit", "-m", message], Opts::default())
        .await?
        .trim()
        .to_string();
    let sha = read(dir, &["rev-parse", "--short", "HEAD"], Opts::default())
        .await?
        .trim()
        .to_string();
    Ok(CommitResult {
        sha,
        summary,
        status: git_status(dir).await?,
    })
}

// ---------- Checkpoints ----------

pub const CHECKPOINT_REF_PREFIX: &str = "refs/prime-desktop/checkpoints/";
const MAX_CHECKPOINTS: usize = 50;
// commit-tree refuses to run without an identity; only used as a fallback.
const FALLBACK_EMAIL: &str = "prime-desktop@localhost";

fn fallback_identity() -> [(&'static str, &'static str); 4] {
    let name = crate::product::product().name.as_str();
    [
        ("GIT_AUTHOR_NAME", name),
        ("GIT_AUTHOR_EMAIL", FALLBACK_EMAIL),
        ("GIT_COMMITTER_NAME", name),
        ("GIT_COMMITTER_EMAIL", FALLBACK_EMAIL),
    ]
}

/// Where the checkpoint list is remembered between runs: one `<sha>.json` per checkpoint. The Electron build kept these
/// under its own app folder; pass that folder instead to carry its checkpoints over.
pub fn checkpoint_dir() -> PathBuf {
    platform::app_data_dir().join("checkpoints")
}

/// A commit id as `git commit-tree` prints it (SHA-1 or SHA-256), the only form accepted as a checkpoint.
pub fn is_checkpoint_id(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn checkpoint_ref(sha: &str) -> String {
    format!("{CHECKPOINT_REF_PREFIX}{sha}")
}

async fn rev_parse(dir: &Path, rev: &str) -> Option<String> {
    let out = read(dir, &["rev-parse", "--verify", "--quiet", rev], Opts::default())
        .await
        .ok()?;
    Some(out.trim().to_string()).filter(|value| !value.is_empty())
}

/// An index file of our own, so a snapshot never touches the user's staging area. Removed when dropped.
struct TempIndex(PathBuf);

impl TempIndex {
    async fn new(dir: &Path) -> Result<Self, Failure> {
        let name = format!(
            "prime-desktop-checkpoint-{}-{}.index",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        // Relative to dir; a linked worktree keeps its index elsewhere than `.git`.
        let path = read(dir, &["rev-parse", "--git-path", &name], Opts::default()).await?;
        Ok(TempIndex(dir.join(path.trim())))
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Snapshots the project's working tree as a commit under [`CHECKPOINT_REF_PREFIX`] and returns its id; `None` when
/// the folder is not a repository, or when nothing changed since `previous`. Paths are relative to `dir` and limited to
/// it, so a project opened in a subfolder of a larger repository only snapshots that subfolder. HEAD, the branch and the
/// real index are not touched.
pub async fn create_checkpoint_snapshot(dir: &Path, message: &str, previous: Option<&str>) -> Result<Option<String>> {
    if !is_git_repo(dir).await {
        return Ok(None);
    }
    let head = rev_parse(dir, "HEAD^{commit}").await;
    let tree = {
        let index = TempIndex::new(dir).await?;
        let file = index.0.to_string_lossy();
        let env = [("GIT_INDEX_FILE", file.as_ref())];
        let quick = Opts {
            env: &env,
            ..Opts::default()
        };
        if let Some(head) = &head {
            write(dir, &["read-tree", head], quick).await?;
        }
        write(
            dir,
            &["add", "-A", "--", "."],
            Opts {
                timeout: SLOW_TIMEOUT,
                ..quick
            },
        )
        .await?;
        write(dir, &["write-tree"], quick).await?.trim().to_string()
    };
    if let Some(previous) = previous.filter(|previous| is_checkpoint_id(previous))
        && rev_parse(dir, &format!("{previous}^{{tree}}")).await.as_deref() == Some(tree.as_str())
    {
        return Ok(None);
    }
    let mut args = vec!["commit-tree", "--no-gpg-sign", &tree];
    if let Some(head) = &head {
        args.extend(["-p", head]);
    }
    args.extend(["-m", message]);
    let created = match write(dir, &args, Opts::default()).await {
        Ok(out) => out,
        Err(_) => {
            write(
                dir,
                &args,
                Opts {
                    env: &fallback_identity(),
                    ..Opts::default()
                },
            )
            .await?
        }
    };
    let sha = created.trim().to_string();
    write(dir, &["update-ref", &checkpoint_ref(&sha), &sha], Opts::default()).await?;
    Ok(Some(sha))
}

pub async fn checkpoint_exists(dir: &Path, sha: &str) -> bool {
    is_checkpoint_id(sha)
        && rev_parse(dir, &format!("{}^{{commit}}", checkpoint_ref(sha)))
            .await
            .is_some()
}

pub async fn delete_checkpoint_ref(dir: &Path, sha: &str) {
    if is_checkpoint_id(sha) {
        let _ = write(dir, &["update-ref", "-d", &checkpoint_ref(sha)], Opts::default()).await;
    }
}

/// A repository-relative path that stays inside the folder it is joined to.
fn stays_inside(path: &str) -> bool {
    !path.ends_with('/')
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// Deletes the files in `current` that `snapshot` does not have, then the folders they leave empty.
fn remove_files_not_in(root: &Path, current: &[String], snapshot: &HashSet<String>) {
    let mut seen = HashSet::new();
    for path in current {
        // Nested repositories show up as "dir/"; never remove those. ls-files can list a path once per merge stage.
        if snapshot.contains(path) || !stays_inside(path) || !seen.insert(path) {
            continue;
        }
        let file = root.join(path);
        let removed = match std::fs::remove_file(&file) {
            // Windows removes a link to a folder as a folder.
            Err(_) if cfg!(windows) && file.is_dir() => std::fs::remove_dir(&file),
            other => other,
        };
        if removed.is_err_and(|error| error.kind() != std::io::ErrorKind::NotFound) {
            continue;
        }
        for parent in file.ancestors().skip(1).take_while(|parent| *parent != root) {
            if std::fs::remove_dir(parent).is_err() {
                break;
            }
        }
    }
}

/// Restores file contents without moving HEAD or touching the real index. Non-ignored files that did not exist in the
/// snapshot are removed; ignored files are left alone.
pub async fn restore_checkpoint_snapshot(dir: &Path, sha: &str) -> Result<()> {
    if !is_checkpoint_id(sha) {
        bail!("Invalid checkpoint id");
    }
    let Some(commit) = rev_parse(dir, &format!("{}^{{commit}}", checkpoint_ref(sha))).await else {
        bail!("Checkpoint not found in this repository");
    };
    let names = |out: String| {
        out.split('\0')
            .filter(|name| !name.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let snapshot: HashSet<String> =
        names(read(dir, &["ls-tree", "-r", "-z", "--name-only", &commit], Opts::default()).await?)
            .into_iter()
            .collect();
    let current = names(
        read(
            dir,
            &["ls-files", "-z", "-c", "-o", "--exclude-standard"],
            Opts::default(),
        )
        .await?,
    );
    let root = dir.to_path_buf();
    tokio::task::spawn_blocking(move || remove_files_not_in(&root, &current, &snapshot)).await?;
    let index = TempIndex::new(dir).await?;
    let file = index.0.to_string_lossy();
    let env = [("GIT_INDEX_FILE", file.as_ref())];
    write(
        dir,
        &["read-tree", &commit],
        Opts {
            env: &env,
            ..Opts::default()
        },
    )
    .await?;
    write(
        dir,
        &["checkout-index", "-a", "-f"],
        Opts {
            env: &env,
            timeout: SLOW_TIMEOUT,
        },
    )
    .await?;
    Ok(())
}

// ---------- The project's checkpoint list ----------
//
// The caller (the agent manager) keeps one `Vec<Checkpoint>` per project, newest last, and passes it in; `agent_id` is
// the project's primary chat. `store` is [`checkpoint_dir`].

fn stored_path(store: &Path, id: &str) -> PathBuf {
    store.join(format!("{id}.json"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |age| age.as_millis() as u64)
}

/// Takes a checkpoint, adds it to `history` and remembers it in `store`; keeps the last [`MAX_CHECKPOINTS`], never
/// dropping `keep`. `Ok(None)` when the folder is not a repository or nothing changed since the last checkpoint.
async fn try_create_checkpoint(
    project: &Path,
    agent_id: &str,
    label: &str,
    keep: Option<&str>,
    history: &mut Vec<Checkpoint>,
    store: &Path,
) -> Result<Option<Checkpoint>> {
    let previous = history.last().map(|checkpoint| checkpoint.id.as_str());
    let message = format!("[prime-desktop] {label} checkpoint");
    let Some(sha) = create_checkpoint_snapshot(project, &message, previous).await? else {
        return Ok(None);
    };
    let checkpoint = Checkpoint {
        id: sha,
        created_at: now_millis(),
        label: label.to_string(),
        agent_id: agent_id.to_string(),
        dirty_files: changed_files(project).await,
    };
    history.push(checkpoint.clone());
    // The ref is the checkpoint; this file only lets a later run know which project it belongs to. Losing it must not
    // lose the checkpoint, so a failure here is not an error.
    let _ = tokio::fs::create_dir_all(store).await;
    if let Ok(json) = serde_json::to_vec_pretty(&checkpoint) {
        let _ = tokio::fs::write(stored_path(store, &checkpoint.id), json).await;
    }
    while history.len() > MAX_CHECKPOINTS {
        let Some(at) = history.iter().position(|item| Some(item.id.as_str()) != keep) else {
            break;
        };
        let dropped = history.remove(at);
        delete_checkpoint_ref(project, &dropped.id).await;
        let _ = tokio::fs::remove_file(stored_path(store, &dropped.id)).await;
    }
    Ok(Some(checkpoint))
}

/// [`try_create_checkpoint`] for the automatic checkpoint before a prompt: a failure means no checkpoint, not an error.
pub async fn create_checkpoint(
    project: &Path,
    agent_id: &str,
    label: &str,
    keep: Option<&str>,
    history: &mut Vec<Checkpoint>,
    store: &Path,
) -> Option<Checkpoint> {
    try_create_checkpoint(project, agent_id, label, keep, history, store)
        .await
        .ok()
        .flatten()
}

/// The checkpoints remembered in `store` for an agent that is not connected, newest first.
pub async fn list_stored_checkpoints(store: &Path, agent_id: &str) -> Vec<Checkpoint> {
    let Ok(mut entries) = tokio::fs::read_dir(store).await else {
        return Vec::new();
    };
    let mut found = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        if let Some(checkpoint) = read_stored(&entry.path())
            .await
            .filter(|checkpoint| checkpoint.agent_id == agent_id)
        {
            found.push(checkpoint);
        }
    }
    found.sort_by_key(|checkpoint| std::cmp::Reverse(checkpoint.created_at));
    found
}

async fn read_stored(path: &Path) -> Option<Checkpoint> {
    serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()
}

/// Whether `sha` is a checkpoint of this project: in its list, or remembered in `store` for this agent.
pub async fn is_known_checkpoint(history: &[Checkpoint], store: &Path, agent_id: &str, sha: &str) -> bool {
    if history.iter().any(|checkpoint| checkpoint.id == sha) {
        return true;
    }
    // The id becomes part of a file name, so it must be a plain commit id first.
    if !is_checkpoint_id(sha) {
        return false;
    }
    read_stored(&stored_path(store, sha))
        .await
        .is_some_and(|stored| stored.id == sha && stored.agent_id == agent_id)
}

/// Puts the project's files back to a checkpoint. Restoring deletes files created since the snapshot, so the current
/// state is checkpointed first; if that fails, nothing is restored (the TypeScript went on without a backup).
pub async fn restore_checkpoint(
    project: &Path,
    agent_id: &str,
    sha: &str,
    history: &mut Vec<Checkpoint>,
    store: &Path,
) -> Result<()> {
    if !is_checkpoint_id(sha) {
        bail!("Invalid checkpoint id");
    }
    if !is_known_checkpoint(history, store, agent_id, sha).await || !checkpoint_exists(project, sha).await {
        bail!("Unknown checkpoint for this project");
    }
    if let Err(error) = try_create_checkpoint(project, agent_id, "before-restore", Some(sha), history, store).await {
        bail!("Could not save the current state before restoring, so nothing was restored: {error}");
    }
    restore_checkpoint_snapshot(project, sha).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;
    use tempfile::TempDir;

    // ---------- helpers ----------

    /// Keeps the developer's global git config (signing, hooks, identity) out of every test. Set once, before any test
    /// starts git; the variables never change afterwards.
    fn isolate() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
            // SAFETY: runs once, first, in every test that starts git; nothing writes the environment afterwards.
            unsafe {
                std::env::set_var("GIT_CONFIG_GLOBAL", null);
                std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            }
        });
    }

    fn try_git(cwd: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
        isolate();
        let mut command = std::process::Command::new("git");
        platform::quiet(&mut command);
        command.args(args).current_dir(cwd).output()
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let out = try_git(cwd, args).unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn git_fails(cwd: &Path, args: &[&str]) -> bool {
        !try_git(cwd, args).unwrap().status.success()
    }

    fn put(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn get(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }

    /// A repository with no commits, an identity of its own and `main` as the branch.
    fn empty_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        dir
    }

    /// One commit holding `.gitignore`, `tracked.txt` and `sub/keep.txt`.
    fn repo() -> TempDir {
        let dir = empty_repo();
        put(dir.path(), ".gitignore", "ignored.log\n");
        put(dir.path(), "tracked.txt", "v1\n");
        put(dir.path(), "sub/keep.txt", "keep\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "initial"]);
        dir
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn change<'a>(status: &'a GitStatus, path: &str) -> &'a GitChange {
        status
            .changes
            .iter()
            .find(|change| change.path == path)
            .unwrap_or_else(|| panic!("{path} not in {status:?}"))
    }

    const OID: &str = "1234567890abcdef1234567890abcdef12345678";

    fn record(fields: &[&str]) -> String {
        fields.join("\0") + "\0"
    }

    // ---------- status parsing (tests/git-status.test.ts) ----------

    #[test]
    fn status_headers_give_the_branch_upstream_and_counts() {
        let status = parse_status(&record(&[
            &format!("# branch.oid {OID}"),
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
        ]));
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(
            (status.is_repo, status.ahead, status.behind, status.changes.len()),
            (true, 2, 1, 0)
        );
    }

    #[test]
    fn a_detached_head_shows_the_short_commit_id() {
        let status = parse_status(&record(&[&format!("# branch.oid {OID}"), "# branch.head (detached)"]));
        assert_eq!(status.branch.as_deref(), Some("1234567"));
        assert_eq!((status.upstream, status.ahead, status.behind), (None, 0, 0));
    }

    #[test]
    fn a_status_with_no_branch_line_is_head() {
        assert_eq!(parse_status("").branch.as_deref(), Some("HEAD"));
    }

    #[test]
    fn entries_map_to_the_shape_the_renderer_reads_and_paths_keep_their_spaces() {
        let sha = "a".repeat(40);
        let status = parse_status(&record(&[
            "# branch.head main",
            &format!("1 .M N... 100644 100644 100644 {sha} {sha} src/my file.ts"),
            &format!("1 A. N... 000000 100644 100644 {sha} {sha} new.ts"),
            &format!("2 R. N... 100644 100644 100644 {sha} {sha} R100 moved name.ts"),
            "old name.ts",
            &format!("u UU N... 100644 100644 100644 100644 {sha} {sha} {sha} clash.ts"),
            "? notes today.md",
        ]));
        let entry = |path: &str, original: Option<&str>, index: &str, worktree: &str, staged, unstaged| GitChange {
            path: path.into(),
            original_path: original.map(String::from),
            index_status: index.into(),
            worktree_status: worktree.into(),
            staged,
            unstaged,
        };
        assert_eq!(
            status.changes,
            vec![
                entry("src/my file.ts", None, " ", "M", false, true),
                entry("new.ts", None, "A", " ", true, false),
                entry("moved name.ts", Some("old name.ts"), "R", " ", true, false),
                entry("clash.ts", None, "U", "U", true, true),
                entry("notes today.md", None, "?", "?", false, true),
            ]
        );
    }

    #[test]
    fn a_malformed_record_is_skipped_not_a_panic() {
        let status = parse_status(&record(&[
            "# branch.head main",
            "1 M",
            "2 R. N...",
            "u",
            "?",
            "garbage",
            "# branch.ab x y",
        ]));
        assert_eq!((status.ahead, status.behind), (0, 0));
        assert_eq!(status.changes.len(), 1); // the bare "?" is an untracked entry with an empty path
    }

    #[test]
    fn the_status_serializes_the_way_the_renderer_expects() {
        let status = parse_status(&record(&["# branch.head main", "? a.txt"]));
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "isRepo": true, "branch": "main", "upstream": null, "ahead": 0, "behind": 0,
                "changes": [{"path": "a.txt", "indexStatus": "?", "worktreeStatus": "?", "staged": false, "unstaged": true}]
            })
        );
        assert_eq!(
            serde_json::to_value(not_a_repo()).unwrap(),
            serde_json::json!({"isRepo": false, "branch": null, "upstream": null, "ahead": 0, "behind": 0, "changes": []})
        );
        let diff = FileDiff {
            path: "a".into(),
            status: FileStatus::Renamed,
            diff: String::new(),
        };
        assert_eq!(
            serde_json::to_value(diff).unwrap(),
            serde_json::json!({"path": "a", "status": "renamed", "diff": ""})
        );
    }

    // ---------- status against a real repository ----------

    #[tokio::test]
    async fn a_clean_repository_has_no_changes() {
        let dir = repo();
        let status = git_status(dir.path()).await.unwrap();
        assert_eq!(
            status,
            GitStatus {
                branch: Some("main".into()),
                is_repo: true,
                ..not_a_repo()
            }
        );
    }

    #[tokio::test]
    async fn renames_edits_and_untracked_files_come_from_one_call() {
        let dir = repo();
        let root = dir.path();
        std::fs::rename(root.join("tracked.txt"), root.join("renamed file.txt")).unwrap();
        git(root, &["add", "-A"]);
        put(root, "renamed file.txt", "two\n");
        put(root, "dir/new.txt", "x");
        let status = git_status(root).await.unwrap();
        assert_eq!(
            *change(&status, "renamed file.txt"),
            GitChange {
                path: "renamed file.txt".into(),
                original_path: Some("tracked.txt".into()),
                index_status: "R".into(),
                worktree_status: "M".into(),
                staged: true,
                unstaged: true
            }
        );
        let untracked = change(&status, "dir/new.txt");
        assert_eq!(
            (untracked.index_status.as_str(), untracked.staged, untracked.unstaged),
            ("?", false, true)
        );
    }

    #[tokio::test]
    async fn a_folder_outside_any_repository_is_not_a_repo() {
        let outside = tempfile::tempdir().unwrap();
        assert_eq!(git_status(outside.path()).await.unwrap(), not_a_repo());
        assert!(!is_git_repo(outside.path()).await);
    }

    #[tokio::test]
    async fn a_folder_that_does_not_exist_is_not_a_repo() {
        let gone = tempfile::tempdir().unwrap().path().join("gone");
        assert_eq!(git_status(&gone).await.unwrap(), not_a_repo());
    }

    #[tokio::test]
    async fn a_damaged_index_is_an_error_not_proof_of_no_repository() {
        let dir = repo();
        std::fs::write(dir.path().join(".git/index"), "not an index").unwrap();
        assert!(git_status(dir.path()).await.is_err());
    }

    #[tokio::test]
    async fn a_repository_with_no_commits_still_has_a_branch() {
        let dir = empty_repo();
        put(dir.path(), "a.txt", "a");
        let status = git_status(dir.path()).await.unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.changes.len(), 1);
        assert!(is_git_repo(dir.path()).await);
    }

    #[tokio::test]
    async fn detached_head_and_upstream_counts_from_real_git() {
        let dir = repo();
        let root = dir.path();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let remote_url = remote.path().to_str().unwrap();
        git(root, &["remote", "add", "origin", remote_url]);
        git(root, &["push", "-q", "-u", "origin", "main"]);
        // Someone else pushes one commit; a local one is not pushed.
        let other = tempfile::tempdir().unwrap();
        git(other.path(), &["clone", "-q", remote_url, "c"]);
        let clone = other.path().join("c");
        put(&clone, "other.txt", "o");
        git(&clone, &["add", "-A"]);
        git(
            &clone,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "other",
            ],
        );
        git(&clone, &["push", "-q"]);
        git(root, &["fetch", "-q"]);
        put(root, "mine.txt", "m");
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "mine"]);
        let status = git_status(root).await.unwrap();
        assert_eq!(
            (status.upstream.as_deref(), status.ahead, status.behind),
            (Some("origin/main"), 1, 1)
        );

        git(root, &["checkout", "-q", "--detach"]);
        let short = git(root, &["rev-parse", "--short=7", "HEAD"]);
        assert_eq!(git_status(root).await.unwrap().branch.as_deref(), Some(short.trim()));
    }

    // ---------- what a repository can make git run (tests/git-hardening.test.ts) ----------

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_str().unwrap().to_string()
    }

    /// A repository whose config names a fsmonitor, a textconv filter and an external diff, each of which leaves a
    /// marker file when git runs it. Returns the folder that holds the markers and the repository.
    #[cfg(unix)]
    fn trapped_repo() -> (TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "test"]);
        put(&root, "a.txt", "one\n");
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-q", "-m", "init"]);
        put(&root, "a.txt", "two\n");
        put(&root, ".gitattributes", "*.txt diff=evil\n");
        let marker = |name: &str| tmp.path().join(name).display().to_string();
        let fsmonitor = script(
            tmp.path(),
            "fsmonitor.sh",
            &format!("touch \"{}\"", marker("fsmonitor")),
        );
        let textconv = script(
            tmp.path(),
            "textconv.sh",
            &format!("touch \"{}\"; cat \"$1\"", marker("textconv")),
        );
        let external = script(tmp.path(), "external.sh", &format!("touch \"{}\"", marker("external")));
        git(&root, &["config", "core.fsmonitor", &fsmonitor]);
        git(&root, &["config", "diff.evil.textconv", &textconv]);
        git(&root, &["config", "diff.external", &external]);
        (tmp, root)
    }

    #[cfg(unix)]
    fn ran(tmp: &TempDir) -> Vec<&'static str> {
        ["fsmonitor", "textconv", "external"]
            .into_iter()
            .filter(|name| tmp.path().join(name).exists())
            .collect()
    }

    #[cfg(unix)]
    fn reset(tmp: &TempDir) {
        for name in ["fsmonitor", "textconv", "external"] {
            let _ = std::fs::remove_file(tmp.path().join(name));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_trap_is_real_plain_git_runs_all_three_commands() {
        let (tmp, root) = trapped_repo();
        git(&root, &["status"]);
        git(&root, &["diff"]);
        // An external diff replaces the built-in one, so textconv only shows once that is switched off.
        git(&root, &["diff", "--no-ext-diff"]);
        assert_eq!(ran(&tmp), ["fsmonitor", "textconv", "external"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn status_and_every_diff_run_none_of_them() {
        let (tmp, root) = trapped_repo();
        reset(&tmp);
        git_status(&root).await.unwrap();
        diff_for_file(&root, "a.txt").await;
        git_file_diff(&root, "a.txt", false).await.unwrap();
        diff_files(&root).await.unwrap();
        assert_eq!(ran(&tmp), Vec::<&str>::new());
        // The staged file view had no --no-textconv in the TypeScript; it runs textconv in plain git.
        git(&root, &["add", "a.txt"]);
        reset(&tmp);
        git_file_diff(&root, "a.txt", true).await.unwrap();
        assert_eq!(ran(&tmp), Vec::<&str>::new());
        git(&root, &["diff", "--cached", "--no-ext-diff"]);
        assert!(ran(&tmp).contains(&"textconv"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staging_and_checkpoints_do_not_run_the_fsmonitor() {
        let (tmp, root) = trapped_repo();
        reset(&tmp);
        stage_all(&root).await.unwrap();
        create_checkpoint_snapshot(&root, "[prime-desktop] x checkpoint", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ran(&tmp), Vec::<&str>::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_calls_still_report_the_change() {
        let (_tmp, root) = trapped_repo();
        let status = git_status(&root).await.unwrap();
        assert!(status.changes.iter().any(|change| change.path == "a.txt"));
        assert!(diff_for_file(&root, "a.txt").await.contains("+two"));
        assert!(git_file_diff(&root, "a.txt", false).await.unwrap().contains("+two"));
        assert_eq!(
            diff_files(&root)
                .await
                .unwrap()
                .iter()
                .filter(|file| file.path == "a.txt")
                .count(),
            1
        );
    }

    // The weak point named on `write()`: a hook the repository names runs on commit. This pins today's behaviour so the
    // day a trust gate is added, this test is the one to change.
    #[cfg(unix)]
    #[tokio::test]
    async fn known_gap_commit_runs_the_hooks_the_repository_names() {
        let dir = repo();
        let root = dir.path();
        let marker = root.join("hook-ran");
        script(
            &root.join(".git/hooks"),
            "pre-commit",
            &format!("touch \"{}\"", marker.display()),
        );
        put(root, "a.txt", "a");
        stage_all(root).await.unwrap();
        commit_staged(root, "with a hook").await.unwrap();
        assert!(marker.exists());
    }

    // ---------- the gate ----------

    #[cfg(debug_assertions)]
    #[tokio::test]
    #[should_panic(expected = "must go through write()")]
    async fn a_write_verb_cannot_take_the_read_path() {
        let _ = read(Path::new("."), &["add", "-A"], Opts::default()).await;
    }

    #[cfg(debug_assertions)]
    #[tokio::test]
    #[should_panic(expected = "must go through read()")]
    async fn a_read_verb_cannot_take_the_write_path() {
        let _ = write(Path::new("."), &["status"], Opts::default()).await;
    }

    #[test]
    fn the_subcommand_is_found_past_global_options() {
        assert_eq!(verb(&["--no-optional-locks", "status", "--porcelain=v2"]), "status");
        assert_eq!(verb(&["--literal-pathspecs", "add", "--", "-x"]), "add");
        assert_eq!(verb(&[]), "");
    }

    // ---------- running git ----------

    #[tokio::test]
    async fn a_missing_folder_is_reported_as_nothing_ran() {
        let gone = tempfile::tempdir().unwrap().path().join("gone");
        let failure = read(&gone, &["status"], Opts::default()).await.unwrap_err();
        assert!(failure.missing);
    }

    #[tokio::test]
    async fn the_failure_text_is_stderr_then_stdout_then_the_reason() {
        let failure = |stdout: &str, stderr: &str, message: &str| {
            Failure {
                stdout: stdout.into(),
                stderr: stderr.into(),
                message: message.into(),
                missing: false,
            }
            .to_string()
        };
        assert_eq!(failure("out", " err \n", "msg"), "err");
        assert_eq!(failure("out\n", "", "msg"), "out");
        assert_eq!(failure("", "  ", "msg"), "msg");
        assert_eq!(failure("", "", ""), "git failed");
        let dir = repo();
        let text = git_file_diff(dir.path(), "x", false).await.map(|_| String::new());
        assert!(text.is_ok()); // an unknown file is an empty diff, not an error
        let error = commit_staged(dir.path(), "nothing").await.unwrap_err().to_string();
        assert!(error.contains("nothing to commit"), "{error}");
    }

    #[tokio::test]
    async fn output_over_the_cap_is_refused() {
        use tokio::io::AsyncWriteExt;
        let (mut tx, rx) = tokio::io::duplex(64);
        tx.write_all(&[b'x'; 20]).await.unwrap();
        drop(tx);
        assert_eq!(read_capped(rx, 20).await.unwrap().len(), 20);
        let (mut tx, rx) = tokio::io::duplex(64);
        tx.write_all(&[b'x'; 21]).await.unwrap();
        drop(tx);
        assert!(read_capped(rx, 20).await.unwrap_err().to_string().contains("more than"));
    }

    // A clean filter that sleeps makes `git status` hang (git runs the filter on a file whose time changed), which is a
    // call that never finishes by itself. The filter records its parent's pid: that is the git that must be killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_that_hangs_is_killed_at_its_timeout() {
        let dir = repo();
        let root = dir.path();
        let pid_file = root.join(".git/git-pid");
        let filter = script(
            &root.join(".git"),
            "slow.sh",
            &format!("echo $PPID > \"{}\"; sleep 5; cat", pid_file.display()),
        );
        put(root, ".gitattributes", "*.txt filter=slow\n");
        git(root, &["config", "filter.slow.clean", &filter]);
        put(root, "tracked.txt", "v1\n"); // same content, new time
        let started = std::time::Instant::now();
        let opts = Opts {
            timeout: Duration::from_millis(500),
            ..Opts::default()
        };
        let failure = read(root, &["status", "--porcelain"], opts).await.unwrap_err();
        assert!(failure.to_string().contains("timed out"), "{failure}");
        assert!(started.elapsed() < Duration::from_secs(4));

        let pid = std::fs::read_to_string(pid_file).unwrap().trim().to_string();
        let alive = || {
            std::process::Command::new("kill")
                .args(["-0", &pid])
                .output()
                .unwrap()
                .status
                .success()
        };
        for _ in 0..20 {
            if !alive() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("git (pid {pid}) is still running after its call timed out");
    }

    // A write that is stopped must not leave git's lock behind: that would block every later git command.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stopped_write_leaves_no_lock_file() {
        let dir = repo();
        let root = dir.path();
        let filter = script(&root.join(".git"), "slow.sh", "sleep 5; cat");
        put(root, ".gitattributes", "*.txt filter=slow\n");
        git(root, &["config", "filter.slow.clean", &filter]);
        put(root, "new.txt", "new\n");
        let opts = Opts {
            timeout: Duration::from_millis(500),
            ..Opts::default()
        };
        let failure = write(root, &["add", "-A"], opts).await.unwrap_err();
        assert!(failure.to_string().contains("timed out"), "{failure}");
        assert!(
            !root.join(".git/index.lock").exists(),
            "git was killed without the chance to clean up"
        );
        git(root, &["status", "--porcelain"]);
    }

    // ---------- diffs ----------

    #[tokio::test]
    async fn a_file_diff_is_staged_or_not_as_asked() {
        let dir = repo();
        let root = dir.path();
        put(root, "tracked.txt", "v2\n");
        assert!(git_file_diff(root, "tracked.txt", false).await.unwrap().contains("+v2"));
        assert_eq!(git_file_diff(root, "tracked.txt", true).await.unwrap(), "");
        git(root, &["add", "tracked.txt"]);
        assert_eq!(git_file_diff(root, "tracked.txt", false).await.unwrap(), "");
        assert!(git_file_diff(root, "tracked.txt", true).await.unwrap().contains("+v2"));
    }

    #[tokio::test]
    async fn a_tracked_file_without_changes_has_an_empty_diff() {
        let dir = repo();
        assert_eq!(git_file_diff(dir.path(), "sub/keep.txt", false).await.unwrap(), "");
    }

    #[tokio::test]
    async fn an_untracked_file_is_shown_as_all_added() {
        let dir = repo();
        put(dir.path(), "notes today.md", "hello\nworld\n");
        let diff = git_file_diff(dir.path(), "notes today.md", false).await.unwrap();
        assert!(
            diff.contains("new file mode") && diff.contains("+hello") && diff.contains("+world"),
            "{diff}"
        );
        // Nothing to show for a staged diff of a file that is not staged.
        assert_eq!(git_file_diff(dir.path(), "notes today.md", true).await.unwrap(), "");
    }

    #[tokio::test]
    async fn a_file_that_does_not_exist_has_an_empty_diff() {
        let dir = repo();
        assert_eq!(git_file_diff(dir.path(), "missing.txt", false).await.unwrap(), "");
    }

    #[tokio::test]
    async fn an_untracked_path_outside_the_project_is_refused() {
        let dir = repo();
        let secret = tempfile::tempdir().unwrap();
        put(secret.path(), "secret.txt", "TOPSECRET\n");
        let absolute = secret.path().join("secret.txt");
        let error = git_file_diff(dir.path(), absolute.to_str().unwrap(), false)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("TOPSECRET"));
        assert!(
            error.contains("outside the project") || error.contains("outside repository"),
            "{error}"
        );
        assert!(git_file_diff(dir.path(), "../secret.txt", false).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_out_of_the_project_is_not_read() {
        let dir = repo();
        let secret = tempfile::tempdir().unwrap();
        put(secret.path(), "secret.txt", "TOPSECRET\n");
        std::os::unix::fs::symlink(secret.path(), dir.path().join("link")).unwrap();
        let error = git_file_diff(dir.path(), "link/secret.txt", false).await;
        assert!(!matches!(error, Ok(diff) if diff.contains("TOPSECRET")));
    }

    #[tokio::test]
    async fn an_oversized_path_is_refused() {
        let dir = repo();
        let long = "a".repeat(MAX_PATH_TEXT + 1);
        assert!(git_file_diff(dir.path(), &long, false).await.is_err());
        assert!(stage_files(dir.path(), &[long]).await.is_err());
    }

    #[tokio::test]
    async fn changed_files_are_listed_with_their_diffs() {
        let dir = repo();
        let root = dir.path();
        put(root, "tracked.txt", "v2\n"); // modified
        put(root, "added.txt", "a\n");
        git(root, &["add", "added.txt"]); // added
        std::fs::remove_file(root.join("sub/keep.txt")).unwrap(); // deleted
        put(root, "untracked.txt", "u\n"); // untracked: listed, no diff
        let files = diff_files(root).await.unwrap();
        let by = |path: &str| {
            files
                .iter()
                .find(|file| file.path == path)
                .unwrap_or_else(|| panic!("{path} in {files:?}"))
        };
        assert_eq!(by("tracked.txt").status, FileStatus::Modified);
        assert!(by("tracked.txt").diff.contains("+v2"));
        assert_eq!(by("added.txt").status, FileStatus::Added);
        assert!(by("added.txt").diff.contains("+a"));
        assert_eq!(by("sub/keep.txt").status, FileStatus::Deleted);
        assert!(by("sub/keep.txt").diff.contains("-keep"));
        assert_eq!(
            (by("untracked.txt").status, by("untracked.txt").diff.as_str()),
            (FileStatus::Modified, "")
        );
    }

    #[tokio::test]
    async fn a_rename_names_the_new_path_and_a_name_with_non_ascii_characters_keeps_its_diff() {
        let dir = repo();
        let root = dir.path();
        git(root, &["mv", "tracked.txt", "moved name.txt"]);
        put(root, "caf\u{e9} x.txt", "unicode\n"); // not the same bytes as tracked.txt, or git calls it the rename
        git(root, &["add", "-A"]);
        put(root, "caf\u{e9} x.txt", "v2\n");
        let files = diff_files(root).await.unwrap();
        let moved = files.iter().find(|file| file.path == "moved name.txt").unwrap();
        assert_eq!(moved.status, FileStatus::Renamed);
        assert_eq!(changed_files(root).await.len(), 2);
        let unicode = files.iter().find(|file| file.path == "caf\u{e9} x.txt").unwrap();
        assert!(unicode.diff.contains("+v2"), "{unicode:?}");
    }

    #[test]
    fn file_status_follows_the_first_letter() {
        let cases = [
            ("D", FileStatus::Deleted),
            ("AM", FileStatus::Added),
            ("R", FileStatus::Renamed),
        ];
        for (code, expected) in cases {
            assert_eq!(file_status(code), expected);
        }
        assert_eq!(file_status("??"), FileStatus::Modified);
        assert_eq!(file_status("M"), FileStatus::Modified);
        assert_eq!(file_status(""), FileStatus::Modified);
    }

    // ---------- changes in a folder that is not a repository ----------

    fn set_mtime(path: &Path, seconds: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds)).unwrap();
    }

    #[tokio::test]
    async fn the_first_walk_lists_every_file_as_modified_and_skips_what_the_app_ignores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "b.txt", "b");
        put(root, "src/a.ts", "a");
        put(root, "node_modules/x/index.js", "x");
        put(root, ".git/config", "c");
        put(root, "dist/out.js", "o");
        put(root, "logo.PNG", "p");
        put(root, "font.woff2", "f");
        put(root, ".png", "dotfile with an ignored extension");
        let files = diff_files(root).await.unwrap();
        let listed: Vec<_> = files
            .iter()
            .map(|file| (file.path.replace('\\', "/"), file.status, file.diff.clone()))
            .collect();
        assert_eq!(
            listed,
            [
                ("b.txt".to_string(), FileStatus::Modified, String::new()),
                ("src/a.ts".to_string(), FileStatus::Modified, String::new())
            ]
        );
    }

    #[test]
    fn later_walks_report_added_modified_and_deleted_files_and_no_change_when_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "same.txt", "s");
        put(root, "edit.txt", "e");
        put(root, "gone.txt", "g");
        for name in ["same.txt", "edit.txt", "gone.txt"] {
            set_mtime(&root.join(name), 1_700_000_000);
        }
        snapshot_file_changes(root);
        assert!(snapshot_file_changes(root).is_empty());

        put(root, "new.txt", "n");
        set_mtime(&root.join("new.txt"), 1_700_000_100);
        set_mtime(&root.join("edit.txt"), 1_700_000_200);
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        let changes = snapshot_file_changes(root);
        let expected = [
            ("edit.txt", FileStatus::Modified, "1700000200000"),
            ("gone.txt", FileStatus::Deleted, ""),
            ("new.txt", FileStatus::Added, "+1700000100000"),
        ];
        let got: Vec<_> = changes
            .iter()
            .map(|file| (file.path.as_str(), file.status, file.diff.as_str()))
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn a_walk_stops_after_the_folder_that_reaches_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        for folder in 0..6 {
            for file in 0..1000 {
                put(dir.path(), &format!("d{folder}/f{file}"), "");
            }
        }
        assert_eq!(walk_project(dir.path()).len(), SNAPSHOT_MAX_FILES);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_file_counts_but_a_link_to_a_folder_is_not_entered() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        put(outside.path(), "leak.txt", "x");
        put(dir.path(), "real.txt", "r");
        std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("alias.txt")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let mut found: Vec<_> = walk_project(dir.path()).into_keys().collect();
        found.sort();
        assert_eq!(found, ["alias.txt", "real.txt"]);
    }

    // ---------- stage, unstage, commit ----------

    #[tokio::test]
    async fn staging_and_unstaging_files_with_odd_names() {
        let dir = repo();
        let root = dir.path();
        for name in ["has space.txt", "app/[id]/page.tsx", "-dash.txt", "caf\u{e9}.txt"] {
            put(root, name, "x");
        }
        let names = strings(&["has space.txt", "app/[id]/page.tsx", "-dash.txt", "caf\u{e9}.txt"]);
        let status = stage_files(root, &names).await.unwrap();
        for name in &names {
            assert!(change(&status, name).staged, "{name}");
        }
        let status = unstage_files(root, &names).await.unwrap();
        for name in &names {
            let entry = change(&status, name);
            assert_eq!((entry.staged, entry.index_status.as_str()), (false, "?"), "{name}");
        }
    }

    #[tokio::test]
    async fn a_path_is_a_file_name_never_a_pattern() {
        let dir = repo();
        let root = dir.path();
        put(root, "a1.txt", "1");
        put(root, "a2.txt", "2");
        assert!(stage_files(root, &strings(&["a*.txt"])).await.is_err());
        assert!(
            git_status(root)
                .await
                .unwrap()
                .changes
                .iter()
                .all(|entry| !entry.staged)
        );
    }

    // A name that is itself a pattern works when it exists (Windows cannot have such a name).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_named_like_a_pattern_is_staged_alone() {
        let dir = repo();
        let root = dir.path();
        put(root, "a1.txt", "1");
        put(root, "a*.txt", "star");
        assert!(change(&stage_files(root, &strings(&["a*.txt"])).await.unwrap(), "a*.txt").staged);
        assert!(!change(&git_status(root).await.unwrap(), "a1.txt").staged);
    }

    #[tokio::test]
    async fn an_empty_list_changes_nothing_and_still_returns_the_status() {
        let dir = repo();
        put(dir.path(), "a.txt", "a");
        let status = stage_files(dir.path(), &[]).await.unwrap();
        assert!(!change(&status, "a.txt").staged);
        assert!(!change(&unstage_files(dir.path(), &[]).await.unwrap(), "a.txt").staged);
    }

    #[tokio::test]
    async fn a_path_with_a_nul_byte_is_an_error_not_a_panic() {
        let dir = repo();
        assert!(stage_files(dir.path(), &strings(&["a\0b"])).await.is_err());
    }

    #[tokio::test]
    async fn unstaging_keeps_the_edit_in_the_working_tree() {
        let dir = repo();
        let root = dir.path();
        put(root, "tracked.txt", "v2\n");
        stage_files(root, &strings(&["tracked.txt"])).await.unwrap();
        let status = unstage_files(root, &strings(&["tracked.txt"])).await.unwrap();
        let entry = change(&status, "tracked.txt");
        assert_eq!((entry.staged, entry.unstaged), (false, true));
        assert_eq!(get(root, "tracked.txt"), "v2\n");
    }

    #[tokio::test]
    async fn before_the_first_commit_unstaging_empties_the_index_of_those_files() {
        let dir = empty_repo();
        let root = dir.path();
        put(root, "a.txt", "a");
        put(root, "b.txt", "b");
        stage_files(root, &strings(&["a.txt", "b.txt"])).await.unwrap();
        let status = unstage_files(root, &strings(&["a.txt"])).await.unwrap();
        assert_eq!(
            (change(&status, "a.txt").staged, change(&status, "b.txt").staged),
            (false, true)
        );
        // Edited after staging: git rm --cached refuses this without -f, and the app must still be able to unstage it.
        put(root, "b.txt", "changed");
        let status = unstage_files(root, &strings(&["b.txt"])).await;
        assert!(status.is_ok(), "{status:?}");
        assert_eq!(get(root, "b.txt"), "changed");
    }

    #[tokio::test]
    async fn stage_all_and_unstage_all_with_and_without_a_commit() {
        for committed in [true, false] {
            let dir = if committed { repo() } else { empty_repo() };
            let root = dir.path();
            put(root, "one.txt", "1");
            put(root, "dir/two.txt", "2");
            let status = stage_all(root).await.unwrap();
            assert!(change(&status, "one.txt").staged && change(&status, "dir/two.txt").staged);
            let status = unstage_all(root).await.unwrap();
            assert!(
                status.changes.iter().all(|entry| !entry.staged),
                "committed={committed} {status:?}"
            );
            assert_eq!(get(root, "dir/two.txt"), "2");
        }
    }

    #[tokio::test]
    async fn commit_returns_the_short_id_the_summary_and_the_clean_status() {
        let dir = repo();
        let root = dir.path();
        put(root, "a.txt", "a");
        stage_all(root).await.unwrap();
        let result = commit_staged(root, "  add a: it's \"quoted\"\n\nbody \u{1f600}  ")
            .await
            .unwrap();
        assert_eq!(result.sha, git(root, &["rev-parse", "--short", "HEAD"]).trim());
        assert!(result.summary.contains("add a"), "{}", result.summary);
        assert!(result.status.changes.is_empty());
        assert_eq!(
            git(root, &["log", "-1", "--format=%B"]).trim(),
            "add a: it's \"quoted\"\n\nbody \u{1f600}"
        );
        let json = serde_json::to_value(&result).unwrap();
        assert!(json["sha"].is_string() && json["summary"].is_string() && json["status"]["isRepo"].is_boolean());
    }

    #[tokio::test]
    async fn commit_refuses_a_blank_message_and_an_empty_index_and_works_before_the_first_commit() {
        let dir = empty_repo();
        let root = dir.path();
        let blank = commit_staged(root, " \n\t ").await.unwrap_err();
        assert_eq!(blank.to_string(), "Enter a commit message.");
        put(root, "a.txt", "a");
        let error = commit_staged(root, "nothing staged").await.unwrap_err().to_string();
        assert!(error.contains("nothing") || error.contains("untracked"), "{error}");
        stage_all(root).await.unwrap();
        commit_staged(root, "-first").await.unwrap(); // a message that starts like an option
        assert_eq!(git(root, &["log", "-1", "--format=%s"]).trim(), "-first");
    }

    // ---------- checkpoints (tests/checkpoints.test.ts) ----------

    fn repo_state(root: &Path) -> Vec<String> {
        vec![
            git(root, &["rev-parse", "HEAD"]),
            git(root, &["symbolic-ref", "HEAD"]),
            git(root, &["log", "--oneline", "main"]),
            git(root, &["ls-files", "-s"]),
            git(root, &["diff", "--cached", "--name-status"]),
        ]
    }

    async fn snapshot(root: &Path, label: &str, previous: Option<&str>) -> Option<String> {
        create_checkpoint_snapshot(root, &format!("[prime-desktop] {label} checkpoint"), previous)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_snapshot_leaves_the_branch_and_index_alone_and_a_restore_puts_the_files_back() {
        let dir = repo();
        let root = dir.path();
        put(root, "tracked.txt", "v2\n");
        put(root, "new.txt", "new\n");
        put(root, "ignored.log", "ign1\n");
        put(root, "staged.txt", "staged\n");
        git(root, &["add", "staged.txt"]);
        let before = repo_state(root);
        let status = git(root, &["status", "--porcelain"]);

        let sha = snapshot(root, "before-prompt", None).await.unwrap();
        assert_eq!(repo_state(root), before);
        assert_eq!(git(root, &["status", "--porcelain"]), status);
        assert_eq!(git(root, &["rev-parse", &checkpoint_ref(&sha)]).trim(), sha);
        assert!(checkpoint_exists(root, &sha).await);
        assert_eq!(git(root, &["show", &format!("{sha}:tracked.txt")]), "v2\n");
        assert_eq!(git(root, &["show", &format!("{sha}:new.txt")]), "new\n");
        assert!(git_fails(root, &["cat-file", "-e", &format!("{sha}:ignored.log")]));
        assert_eq!(git(root, &["rev-parse", &format!("{sha}^")]), before[0]);
        assert_eq!(
            git(root, &["log", "-1", "--format=%s", &sha]).trim(),
            "[prime-desktop] before-prompt checkpoint"
        );

        // Nothing changed since the last snapshot: no new checkpoint.
        assert_eq!(snapshot(root, "again", Some(&sha)).await, None);
        // Nothing but the temp index was ever written into .git.
        let leftovers: Vec<_> = std::fs::read_dir(root.join(".git"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains("prime-desktop-checkpoint"))
            .collect();
        assert!(leftovers.is_empty());

        put(root, "tracked.txt", "v3\n");
        put(root, "another.txt", "another\n");
        put(root, "newdir/deep/file.txt", "deep\n");
        put(root, "ignored.log", "ign2\n");
        std::fs::remove_file(root.join("new.txt")).unwrap();
        std::fs::remove_file(root.join("sub/keep.txt")).unwrap();

        restore_checkpoint_snapshot(root, &sha).await.unwrap();

        assert_eq!(get(root, "tracked.txt"), "v2\n");
        assert_eq!(get(root, "new.txt"), "new\n");
        assert_eq!(get(root, "sub/keep.txt"), "keep\n");
        assert_eq!(get(root, "staged.txt"), "staged\n");
        assert!(!root.join("another.txt").exists());
        assert!(!root.join("newdir").exists());
        assert_eq!(get(root, "ignored.log"), "ign2\n");
        assert_eq!(repo_state(root), before);
        assert_eq!(git(root, &["status", "--porcelain"]), status);
    }

    #[tokio::test]
    async fn unknown_or_malformed_ids_are_refused() {
        let dir = repo();
        let root = dir.path();
        for bad in ["HEAD", "--all", "", "abc", "ABCDEF1", "../../x", &"a".repeat(65)] {
            let error = restore_checkpoint_snapshot(root, bad).await.unwrap_err().to_string();
            assert!(error.contains("Invalid checkpoint"), "{bad}: {error}");
            assert!(!checkpoint_exists(root, bad).await);
        }
        let head = git(root, &["rev-parse", "HEAD"]).trim().to_string();
        let error = restore_checkpoint_snapshot(root, &head).await.unwrap_err().to_string();
        assert!(error.contains("not found"), "{error}");
        assert!(!checkpoint_exists(root, &head).await); // a real commit, but not under the checkpoint ref
    }

    #[test]
    fn checkpoint_ids_are_lowercase_hex_of_a_commit_id_length() {
        assert!(is_checkpoint_id("abcdef1"));
        assert!(is_checkpoint_id(&"0".repeat(40)));
        assert!(is_checkpoint_id(&"f".repeat(64))); // SHA-256 repositories
        assert!(!is_checkpoint_id("abcdef")); // 6
        assert!(!is_checkpoint_id(&"a".repeat(65)));
        assert!(!is_checkpoint_id("abcdefg"));
        assert!(!is_checkpoint_id("ABCDEF1"));
        assert!(!is_checkpoint_id("abcdef1\n")); // JavaScript's $ does not allow it either
        assert_eq!(checkpoint_ref("abc"), "refs/prime-desktop/checkpoints/abc");
    }

    #[tokio::test]
    async fn a_project_opened_in_a_subfolder_only_snapshots_and_restores_that_subfolder() {
        let dir = repo();
        let root = dir.path();
        let project = root.join("sub");
        let sha = snapshot(&project, "sub", None).await.unwrap();
        put(root, "outside.txt", "outside\n");
        put(root, "tracked.txt", "changed outside\n");
        put(&project, "inside.txt", "inside\n");
        put(&project, "keep.txt", "changed\n");

        restore_checkpoint_snapshot(&project, &sha).await.unwrap();

        assert!(!project.join("inside.txt").exists());
        assert_eq!(get(&project, "keep.txt"), "keep\n");
        assert_eq!(get(root, "outside.txt"), "outside\n");
        assert_eq!(get(root, "tracked.txt"), "changed outside\n");
    }

    #[tokio::test]
    async fn a_repository_without_commits_can_be_checkpointed() {
        let dir = empty_repo();
        let root = dir.path();
        put(root, "a.txt", "a\n");
        let sha = snapshot(root, "first", None).await.unwrap();
        assert!(git_fails(root, &["rev-parse", "--verify", "--quiet", "HEAD"]));
        put(root, "a.txt", "changed\n");
        put(root, "b.txt", "b\n");
        restore_checkpoint_snapshot(root, &sha).await.unwrap();
        assert_eq!(get(root, "a.txt"), "a\n");
        assert!(!root.join("b.txt").exists());
    }

    #[tokio::test]
    async fn a_folder_that_is_not_a_repository_has_no_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "a.txt", "a");
        assert_eq!(snapshot(dir.path(), "x", None).await, None);
        let store = tempfile::tempdir().unwrap();
        let mut history = Vec::new();
        assert!(
            create_checkpoint(dir.path(), "agent", "x", None, &mut history, store.path())
                .await
                .is_none()
        );
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn a_repository_that_cannot_name_a_committer_still_gets_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.useConfigOnly", "true"]); // no identity anywhere: commit-tree must fail
        put(root, "a.txt", "a");
        assert!(git_fails(
            root,
            &["commit-tree", "-m", "x", git(root, &["write-tree"]).trim()]
        ));
        let sha = snapshot(root, "anon", None).await.unwrap();
        assert_eq!(
            git(root, &["log", "-1", "--format=%an <%ae>", &sha]).trim(),
            format!("{} <{FALLBACK_EMAIL}>", crate::product::product().name)
        );
    }

    #[tokio::test]
    async fn snapshots_taken_at_the_same_time_do_not_disturb_each_other() {
        let dir = repo();
        let root = dir.path();
        put(root, "a.txt", "a");
        let (one, two, three, four) = tokio::join!(
            create_checkpoint_snapshot(root, "m", None),
            create_checkpoint_snapshot(root, "m", None),
            create_checkpoint_snapshot(root, "m", None),
            create_checkpoint_snapshot(root, "m", None)
        );
        let ids: HashSet<_> = [one, two, three, four]
            .into_iter()
            .map(|result| result.unwrap().unwrap())
            .collect();
        // Same tree, parent and message: one commit, or two when the clock ticks over between them.
        assert!((1..=2).contains(&ids.len()), "{ids:?}");
        for id in &ids {
            assert!(checkpoint_exists(root, id).await);
        }
        let leftovers = std::fs::read_dir(root.join(".git"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.file_name().to_string_lossy().ends_with(".index.lock")
                    || entry.file_name().to_string_lossy().contains("checkpoint")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn a_path_from_git_that_could_leave_the_folder_is_never_removed() {
        for ok in ["a", "a/b.txt", "dir/with space/x"] {
            assert!(stays_inside(ok), "{ok}");
        }
        for bad in ["a/", "nested/", "../x", "a/../../x", "/etc/passwd", "./../x"] {
            assert!(!stays_inside(bad), "{bad}");
        }
        if cfg!(windows) {
            for bad in [r"..\x", r"C:\x", "C:/x", r"\\server\share\x", r"\x"] {
                assert!(!stays_inside(bad), "{bad}");
            }
        } else {
            assert!(stays_inside(r"a\b")); // a legal file name off Windows
        }
    }

    #[tokio::test]
    async fn a_nested_repository_survives_a_restore() {
        let dir = repo();
        let root = dir.path();
        let sha = snapshot(root, "before", None).await.unwrap();
        git(root, &["init", "-q", "nested"]);
        put(root, "nested/work.txt", "mine");
        restore_checkpoint_snapshot(root, &sha).await.unwrap();
        assert_eq!(get(root, "nested/work.txt"), "mine");
    }

    // ---------- the project's checkpoint list ----------

    #[tokio::test]
    async fn a_checkpoint_is_remembered_in_the_shape_the_electron_build_wrote() {
        let dir = repo();
        let root = dir.path();
        let store = tempfile::tempdir().unwrap();
        put(root, "tracked.txt", "v2\n");
        put(root, "new.txt", "n\n");
        let mut history = Vec::new();

        let checkpoint = create_checkpoint(root, "agent-1", "before-prompt", None, &mut history, store.path())
            .await
            .unwrap();

        assert_eq!(history, std::slice::from_ref(&checkpoint));
        assert_eq!(
            (checkpoint.label.as_str(), checkpoint.agent_id.as_str()),
            ("before-prompt", "agent-1")
        );
        assert_eq!(checkpoint.dirty_files, ["tracked.txt", "new.txt"]); // git lists tracked changes first
        assert!(checkpoint.created_at > 1_700_000_000_000);
        assert_eq!(
            git(root, &["log", "-1", "--format=%s", &checkpoint.id]).trim(),
            "[prime-desktop] before-prompt checkpoint"
        );
        assert_eq!(
            git(root, &["for-each-ref", "--format=%(refname)", "refs/prime-desktop/"]).trim(),
            format!("refs/prime-desktop/checkpoints/{}", checkpoint.id)
        );
        let text = std::fs::read_to_string(store.path().join(format!("{}.json", checkpoint.id))).unwrap();
        assert!(text.starts_with("{\n  \"id\": "), "{text}"); // JSON.stringify(cp, null, 2)
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["agentId", "createdAt", "dirtyFiles", "id", "label"]);
        assert_eq!(json["dirtyFiles"], serde_json::json!(["tracked.txt", "new.txt"]));
    }

    #[tokio::test]
    async fn nothing_changed_since_the_last_checkpoint_means_no_new_one() {
        let dir = repo();
        let store = tempfile::tempdir().unwrap();
        let mut history = Vec::new();
        put(dir.path(), "a.txt", "a");
        assert!(
            create_checkpoint(dir.path(), "a", "one", None, &mut history, store.path())
                .await
                .is_some()
        );
        assert!(
            create_checkpoint(dir.path(), "a", "two", None, &mut history, store.path())
                .await
                .is_none()
        );
        assert_eq!(history.len(), 1);
        put(dir.path(), "a.txt", "b");
        assert!(
            create_checkpoint(dir.path(), "a", "three", None, &mut history, store.path())
                .await
                .is_some()
        );
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    async fn the_list_keeps_fifty_and_never_drops_the_one_to_keep() {
        let dir = repo();
        let root = dir.path();
        let store = tempfile::tempdir().unwrap();
        let mut history = Vec::new();
        let mut first = String::new();
        for n in 0..52 {
            put(root, "n.txt", &format!("{n}\n"));
            let keep = (n > 0).then_some(first.as_str());
            let made = create_checkpoint(root, "a", &format!("c{n}"), keep, &mut history, store.path())
                .await
                .unwrap();
            if n == 0 {
                first = made.id;
            }
        }
        assert_eq!(history.len(), 50);
        assert_eq!(history[0].id, first); // the oldest, protected
        assert_eq!(history[1].label, "c3"); // c1 and c2 went
        let refs = git(root, &["for-each-ref", "--format=%(refname)", "refs/prime-desktop/"]);
        assert_eq!(refs.lines().count(), 50);
        assert_eq!(std::fs::read_dir(store.path()).unwrap().count(), 50);
        assert!(refs.contains(&first));
    }

    #[tokio::test]
    async fn checkpoints_of_an_agent_that_is_not_connected_are_listed_newest_first() {
        let store = tempfile::tempdir().unwrap();
        let write_json = |name: &str, body: &str| std::fs::write(store.path().join(name), body).unwrap();
        let stored = |id: &str, at: u64, agent: &str| {
            format!(r#"{{"id":"{id}","createdAt":{at},"label":"l","agentId":"{agent}","dirtyFiles":["a.txt"]}}"#)
        };
        write_json("aaaaaaa.json", &stored("aaaaaaa", 100, "me"));
        write_json("bbbbbbb.json", &stored("bbbbbbb", 300, "me"));
        write_json("ccccccc.json", &stored("ccccccc", 200, "me"));
        write_json("ddddddd.json", &stored("ddddddd", 400, "someone-else"));
        write_json("eeeeeee.json", "{ not json");
        write_json("fffffff.json", r#"{"id":"fffffff","agentId":"me"}"#); // missing fields
        write_json("notes.txt", &stored("ggggggg", 500, "me"));

        let list = list_stored_checkpoints(store.path(), "me").await;

        assert_eq!(
            list.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["bbbbbbb", "ccccccc", "aaaaaaa"]
        );
        assert!(
            list_stored_checkpoints(&store.path().join("missing"), "me")
                .await
                .is_empty()
        );
        let json = serde_json::to_value(&list[0]).unwrap();
        assert_eq!(json["createdAt"], 300);
    }

    #[tokio::test]
    async fn a_checkpoint_is_known_from_the_list_or_from_the_store_for_the_same_agent() {
        let store = tempfile::tempdir().unwrap();
        let stored = r#"{"id":"abcdef1","createdAt":1,"label":"l","agentId":"me","dirtyFiles":[]}"#;
        std::fs::write(store.path().join("abcdef1.json"), stored).unwrap();
        // A file that claims another id than its name does not count.
        std::fs::write(store.path().join("abcdef2.json"), stored).unwrap();
        let listed = Checkpoint {
            id: "1234567".into(),
            created_at: 1,
            label: "l".into(),
            agent_id: "me".into(),
            dirty_files: vec![],
        };
        let history = [listed];

        assert!(is_known_checkpoint(&history, store.path(), "me", "1234567").await);
        assert!(is_known_checkpoint(&history, store.path(), "me", "abcdef1").await);
        assert!(!is_known_checkpoint(&history, store.path(), "someone-else", "abcdef1").await);
        assert!(!is_known_checkpoint(&history, store.path(), "me", "abcdef2").await);
        assert!(!is_known_checkpoint(&history, store.path(), "me", "abcdef3").await);
        // The id becomes a file name; one that leaves the folder is not even looked up.
        std::fs::write(store.path().parent().unwrap().join("outside.json"), stored).unwrap();
        assert!(!is_known_checkpoint(&[], store.path(), "me", "../outside").await);
        let _ = std::fs::remove_file(store.path().parent().unwrap().join("outside.json"));
    }

    #[tokio::test]
    async fn restoring_saves_the_current_state_first_and_can_be_undone() {
        let dir = repo();
        let root = dir.path();
        let store = tempfile::tempdir().unwrap();
        let mut history = Vec::new();
        put(root, "tracked.txt", "one\n");
        let one = create_checkpoint(root, "a", "one", None, &mut history, store.path())
            .await
            .unwrap();
        put(root, "tracked.txt", "two\n");
        put(root, "later.txt", "later\n");

        restore_checkpoint(root, "a", &one.id, &mut history, store.path())
            .await
            .unwrap();

        assert_eq!(get(root, "tracked.txt"), "one\n");
        assert!(!root.join("later.txt").exists());
        let saved = history.last().unwrap().clone();
        assert_eq!(saved.label, "before-restore");
        assert_eq!(saved.dirty_files, ["tracked.txt", "later.txt"]);
        restore_checkpoint(root, "a", &saved.id, &mut history, store.path())
            .await
            .unwrap();
        assert_eq!(get(root, "tracked.txt"), "two\n");
        assert_eq!(get(root, "later.txt"), "later\n");
    }

    #[tokio::test]
    async fn restoring_something_that_is_not_this_projects_checkpoint_is_refused() {
        let dir = repo();
        let root = dir.path();
        let store = tempfile::tempdir().unwrap();
        let mut history = Vec::new();
        put(root, "a.txt", "a");
        let cp = create_checkpoint(root, "a", "x", None, &mut history, store.path())
            .await
            .unwrap();
        let head = git(root, &["rev-parse", "--short", "HEAD"]).trim().to_string();

        let refused = |error: anyhow::Error| error.to_string();
        let bad = restore_checkpoint(root, "a", "HEAD", &mut history, store.path())
            .await
            .unwrap_err();
        assert_eq!(refused(bad), "Invalid checkpoint id");
        let unlisted = restore_checkpoint(root, "a", &head, &mut history, store.path())
            .await
            .unwrap_err();
        assert_eq!(refused(unlisted), "Unknown checkpoint for this project");
        // Listed, but another project's: the ref is not in this repository.
        let mut foreign = history.clone();
        foreign[0].id = "abcdef1".into();
        let other = restore_checkpoint(root, "a", "abcdef1", &mut foreign, store.path())
            .await
            .unwrap_err();
        assert_eq!(refused(other), "Unknown checkpoint for this project");
        // The ref is gone: unknown, and nothing was changed.
        delete_checkpoint_ref(root, &cp.id).await;
        put(root, "a.txt", "edited");
        let gone = restore_checkpoint(root, "a", &cp.id, &mut history, store.path())
            .await
            .unwrap_err();
        assert_eq!(refused(gone), "Unknown checkpoint for this project");
        assert_eq!(get(root, "a.txt"), "edited");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_restore_that_cannot_save_the_current_state_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        // Puts the permissions back so the folder can be removed, even when an assertion fails.
        struct Unlock(PathBuf);
        impl Drop for Unlock {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let dir = repo();
        let root = dir.path();
        let store = tempfile::tempdir().unwrap();
        let mut history = Vec::new();
        put(root, "tracked.txt", "one\n");
        let one = create_checkpoint(root, "a", "one", None, &mut history, store.path())
            .await
            .unwrap();
        put(root, "tracked.txt", "two\n");
        put(root, "later.txt", "later\n");

        // Git cannot write the new files to the object store now.
        let objects = root.join(".git/objects");
        let _unlock = Unlock(objects.clone());
        std::fs::set_permissions(&objects, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::create_dir(objects.join("probe")).is_ok() {
            return; // running as root: permissions do not bind
        }

        let error = restore_checkpoint(root, "a", &one.id, &mut history, store.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nothing was restored"), "{error}");
        assert_eq!(get(root, "tracked.txt"), "two\n");
        assert_eq!(get(root, "later.txt"), "later\n");
    }

    #[tokio::test]
    async fn a_checkpoint_made_by_another_run_is_found_by_its_ref_and_its_file() {
        // What the Electron build leaves behind: the ref, and `<sha>.json` as JSON.stringify writes it.
        let dir = repo();
        let root = dir.path();
        put(root, "tracked.txt", "from before\n");
        let sha = snapshot(root, "before-prompt", None).await.unwrap();
        let store = tempfile::tempdir().unwrap();
        let json = format!(
            "{{\n  \"id\": \"{sha}\",\n  \"createdAt\": 1760000000123,\n  \"label\": \"before-prompt\",\n  \"agentId\": \"prime-tab-1\",\n  \"dirtyFiles\": [\n    \"tracked.txt\"\n  ]\n}}"
        );
        std::fs::write(store.path().join(format!("{sha}.json")), json).unwrap();
        put(root, "tracked.txt", "now\n");

        let listed = list_stored_checkpoints(store.path(), "prime-tab-1").await;
        assert_eq!(
            (listed.len(), listed[0].created_at, listed[0].dirty_files.clone()),
            (1, 1_760_000_000_123, strings(&["tracked.txt"]))
        );
        let mut history = Vec::new();
        restore_checkpoint(root, "prime-tab-1", &sha, &mut history, store.path())
            .await
            .unwrap();
        assert_eq!(get(root, "tracked.txt"), "from before\n");
    }
}

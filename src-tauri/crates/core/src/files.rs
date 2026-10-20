//! Project file listing and the previews of files an agent produced or the user picked.
//!
//! The host composes these pieces for the renderer's calls; the project roots and the picked files come from the app
//! state as parameters, never as globals here:
//!
//! | Call                   | Host does                                                                          |
//! |------------------------|------------------------------------------------------------------------------------|
//! | `artifactRead`         | `read_preview(&project_file(project, path)?)`                                      |
//! | `artifactReadFile`     | `read_preview(&previewable_file(roots, picked, path)?)`                            |
//! | `artifactReveal`       | `reveal(&project_file(project, path)?)`, then `true`                               |
//! | `revealInFinder`       | `reveal(&previewable_file(roots, picked, path)?)`                                  |
//! | `projectFiles`         | `project_files(project)`, or `{files: [], truncated: false}` with no project       |
//! | `fileMenu`             | `file_menu(project, path)`, then the native menu                                   |
//! | `artifactChoose`       | the native dialog, then the state's `addPreviewFile`                               |
//!
//! All of these block on the disk; the host runs them off the async runtime.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;

use crate::{platform, validate};

pub const FILE_LIMIT: usize = 3000;
pub const MAX_DEPTH: usize = 8;
const MAX_PATH_TEXT: usize = 4_096;

// Folders that hold dependencies, caches or build output rather than the user's own files.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "env",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".next",
    ".nuxt",
    ".cache",
    ".turbo",
    ".parcel-cache",
    "dist",
    "build",
    "out",
    "target",
    ".idea",
    ".vscode",
    ".gradle",
    "Pods",
    "DerivedData",
    ".ipynb_checkpoints",
    ".snakemake",
    "work",
    ".nextflow",
];
const SKIP_FILES: &[&str] = &[".DS_Store", "Thumbs.db"];

#[derive(Debug, Clone, Serialize)]
pub struct ProjectFile {
    /// Relative to the project root, with forward slashes.
    pub path: String,
    pub size: u64,
    /// Milliseconds since the epoch, like JavaScript's `mtimeMs`.
    pub mtime: f64,
}

#[derive(Debug, Serialize)]
pub struct ProjectFiles {
    pub files: Vec<ProjectFile>,
    pub truncated: bool,
}

pub fn project_files(root: &Path) -> ProjectFiles {
    list_project_files(root, FILE_LIMIT, MAX_DEPTH)
}

/// Files in a project, most recently modified first. Walks breadth-first so a huge tree still yields its top-level
/// files before the limit is reached. Links are skipped, not followed: a loop cannot trap the walk and a link cannot
/// list files from outside the project.
pub fn list_project_files(root: &Path, limit: usize, max_depth: usize) -> ProjectFiles {
    let mut files = Vec::new();
    let mut truncated = false;
    let mut level = vec![root.to_path_buf()];
    let mut depth = 0;
    while depth <= max_depth && !level.is_empty() && !truncated {
        let mut next = Vec::new();
        'dirs: for dir in &level {
            let Ok(read) = std::fs::read_dir(dir) else {
                continue; // unreadable folder
            };
            // Directory order is arbitrary; sorting makes it deterministic which files a truncated listing keeps.
            let mut entries: Vec<_> = read.flatten().collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let Ok(kind) = entry.file_type() else { continue };
                let name = entry.file_name().to_string_lossy().into_owned();
                if kind.is_dir() {
                    if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                        next.push(entry.path());
                    }
                    continue;
                }
                if !kind.is_file() || SKIP_FILES.contains(&name.as_str()) {
                    continue;
                }
                if files.len() >= limit {
                    truncated = true;
                    break 'dirs;
                }
                // Deleted or unreadable between the listing and here.
                let Ok(info) = entry.metadata() else { continue };
                files.push(ProjectFile {
                    path: relative(root, &entry.path()),
                    size: info.len(),
                    mtime: info.modified().map(epoch_millis).unwrap_or(0.0),
                });
            }
        }
        level = next;
        depth += 1;
    }
    // ponytail: ASCII-case-insensitive tie-break, not ICU's `localeCompare`; it only orders files with equal mtimes.
    files.sort_by(|a, b| {
        b.mtime
            .total_cmp(&a.mtime)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
            .then_with(|| a.path.cmp(&b.path))
    });
    ProjectFiles { files, truncated }
}

fn relative(root: &Path, full: &Path) -> String {
    let relative = full.strip_prefix(root).unwrap_or(full);
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    parts.join("/")
}

fn epoch_millis(time: std::time::SystemTime) -> f64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs_f64() * 1000.0,
        Err(before) => -before.duration().as_secs_f64() * 1000.0,
    }
}

const MIB: u64 = 1024 * 1024;
// Large complexes routinely ship as tens of MB of mmCIF text; 3Dmol handles them, so structures get a higher ceiling
// than other text files.
const STRUCTURE_EXTENSIONS: &[&str] = &["pdb", "pdbqt", "cif", "mmcif", "sdf", "mol", "mol2", "xyz"];
const IMAGE_MIME: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
];
const DOCUMENT_MIME: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xls", "application/vnd.ms-excel"),
];

/// What the file panel shows for a file: text as `content`, images and office documents as a base64 `dataUrl`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub path: String,
    pub size: u64,
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_url: Option<String>,
}

/// Reads a file for preview. `file` has already passed [`project_file`] or [`previewable_file`].
pub fn read_preview(file: &Path) -> Result<Preview> {
    let extension = file
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mime_of = |table: &[(&'static str, &'static str)]| {
        table
            .iter()
            .find(|(known, _)| *known == extension)
            .map(|(_, mime)| *mime)
    };
    let (image, document) = (mime_of(IMAGE_MIME), mime_of(DOCUMENT_MIME));
    let max = if document.is_some() {
        32 * MIB
    } else if STRUCTURE_EXTENSIONS.contains(&extension.as_str()) {
        64 * MIB
    } else {
        8 * MIB
    };

    let handle = File::open(file).with_context(|| format!("Could not open {}", file.display()))?;
    let size = handle.metadata()?.len();
    if size > max {
        return Err(too_large(size, max));
    }
    // Bounded by the ceiling, not by the size seen above: a file an agent is still writing can grow in between.
    let mut data = Vec::with_capacity(size as usize);
    handle.take(max + 1).read_to_end(&mut data)?;
    if data.len() as u64 > max {
        return Err(too_large(data.len() as u64, max));
    }

    let path = file.to_string_lossy().into_owned();
    if let Some(mime) = image.or(document) {
        let data_url = format!("data:{mime};base64,{}", STANDARD.encode(&data));
        return Ok(Preview {
            path,
            size,
            mime: mime.into(),
            content: None,
            data_url: Some(data_url),
        });
    }
    let content = String::from_utf8(data).unwrap_or_else(|bad| String::from_utf8_lossy(bad.as_bytes()).into_owned());
    Ok(Preview {
        path,
        size,
        mime: "text/plain".into(),
        content: Some(content),
        data_url: None,
    })
}

fn too_large(size: u64, max: u64) -> anyhow::Error {
    let megabytes = (size as f64 / MIB as f64).round();
    anyhow::anyhow!(
        "File is too large to preview ({megabytes} MB; the limit is {} MB)",
        max / MIB
    )
}

/// `path.resolve(base, requested)`: joined, with `.` and `..` folded away, no disk access. Containment is judged on
/// this path and the file is opened through it, so what was checked is what is read.
fn resolve_lexical(base: &Path, requested: &str) -> PathBuf {
    let mut resolved = PathBuf::new();
    for part in base.join(requested).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// A file named relative to a project (or absolute) that is inside it and exists. Backs `artifactRead` and
/// `artifactReveal`.
pub fn project_file(project: &Path, requested: &str) -> Result<PathBuf> {
    let requested = validate::non_empty("artifact path", requested, MAX_PATH_TEXT)?;
    let path = resolve_lexical(project, &requested);
    if !validate::is_within(project, &path) {
        bail!("Artifact path is outside the project");
    }
    validate::existing_file("artifact path", &path.to_string_lossy())
}

/// A file the renderer may read without an agent: inside an open project, or one the user picked in the native
/// dialog. Links are resolved on both sides, so a link inside a project that points out of it does not count as
/// inside (the TypeScript compared paths as text and let it through).
pub fn previewable_file<P: AsRef<Path>, F: AsRef<Path>>(
    projects: &[P],
    picked: &[F],
    requested: &str,
) -> Result<PathBuf> {
    let file = validate::existing_file("path", requested)?;
    if projects.iter().any(|root| validate::is_within(root.as_ref(), &file)) {
        return Ok(file);
    }
    // A picked file that has since gone cannot resolve, and cannot be `file` either.
    if picked
        .iter()
        .any(|path| validate::resolve(path.as_ref()).is_ok_and(|path| path == file))
    {
        return Ok(file);
    }
    bail!("File is not in an open project or a file you opened")
}

/// What the file context menu needs; the host builds the native menu and handles the clicks.
#[derive(Debug, PartialEq)]
pub struct FileMenu {
    pub full_path: PathBuf,
    /// Set when the file is inside the project (the "Copy Relative Path" item).
    pub relative_path: Option<String>,
    /// Whether "Show in ..." is enabled: only a file that exists inside the project.
    pub revealable: bool,
    pub reveal_label: &'static str,
}

pub fn file_menu(project: Option<&Path>, requested: &str) -> Result<FileMenu> {
    let requested = validate::non_empty("path", requested, MAX_PATH_TEXT)?;
    let base = project
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let full_path = resolve_lexical(&base, &requested);
    let inside = project.filter(|project| validate::is_within(project, &full_path));
    let relative_path = inside.and_then(|project| {
        let project = resolve_lexical(project, "");
        full_path
            .strip_prefix(project)
            .ok()
            .map(|rest| rest.to_string_lossy().into_owned())
    });
    let reveal_label = if cfg!(target_os = "macos") {
        "Show in Finder"
    } else if cfg!(windows) {
        "Show in Explorer"
    } else {
        "Show in File Manager"
    };
    Ok(FileMenu {
        revealable: inside.is_some() && full_path.exists(),
        relative_path,
        full_path,
        reveal_label,
    })
}

/// Shows a file in the system file manager: selected in Explorer or Finder, its folder elsewhere.
pub fn reveal(file: &Path) -> Result<()> {
    let mut command = reveal_command(file);
    platform::quiet(&mut command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!("Could not open the file manager: {error}"))?;
    // Explorer reports failure in its exit code even when it worked, so the status is not checked. The wait only
    // reaps the process.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(windows)]
fn reveal_command(file: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("explorer");
    // Explorer wants `/select,"path"` as one argument with its own quotes; Rust's quoting would wrap all of it.
    command.raw_arg(format!("/select,\"{}\"", file.display()));
    command
}

#[cfg(target_os = "macos")]
fn reveal_command(file: &Path) -> Command {
    let mut command = Command::new("open");
    command.arg("-R").arg(file);
    command
}

#[cfg(not(any(windows, target_os = "macos")))]
fn reveal_command(file: &Path) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(file.parent().unwrap_or(file));
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn touch(path: &Path, content: impl AsRef<[u8]>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn paths(listing: &ProjectFiles) -> Vec<&str> {
        listing.files.iter().map(|file| file.path.as_str()).collect()
    }

    #[test]
    fn lists_files_newest_first_and_skips_dependency_and_hidden_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("README.md"), "#");
        touch(&root.join("data/cells.csv"), "a,b");
        touch(&root.join("node_modules/x/index.js"), "");
        touch(&root.join(".git/HEAD"), "");
        touch(&root.join(".hidden/file.txt"), "");
        touch(&root.join(".DS_Store"), "");
        touch(&root.join("data/Thumbs.db"), "");
        touch(&root.join(".gitignore"), "x"); // hidden files are listed; only hidden folders are skipped
        let old = File::options().write(true).open(root.join("README.md")).unwrap();
        old.set_modified(UNIX_EPOCH + Duration::from_millis(1000)).unwrap();
        let old = File::options().write(true).open(root.join(".gitignore")).unwrap();
        old.set_modified(UNIX_EPOCH + Duration::from_millis(500)).unwrap();

        let listing = list_project_files(root, FILE_LIMIT, MAX_DEPTH);
        assert_eq!(paths(&listing), ["data/cells.csv", "README.md", ".gitignore"]);
        assert!(!listing.truncated);
        assert_eq!(listing.files[0].size, 3);
        assert_eq!(listing.files[1].mtime, 1000.0);
        assert!(listing.files[0].mtime > 1.0e12, "mtime is in milliseconds");
        assert!(list_project_files(root, 1, MAX_DEPTH).truncated);
    }

    #[test]
    fn the_limit_truncates_only_when_a_file_would_be_dropped() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("a.txt"), "");
        touch(&dir.path().join("b.txt"), "");
        let exact = list_project_files(dir.path(), 2, MAX_DEPTH);
        assert!(!exact.truncated);
        assert_eq!(exact.files.len(), 2);
        let cut = list_project_files(dir.path(), 1, MAX_DEPTH);
        assert!(cut.truncated);
        assert_eq!(cut.files.len(), 1);
        assert_eq!(list_project_files(dir.path(), 0, MAX_DEPTH).files.len(), 0);
    }

    #[test]
    fn a_truncated_walk_keeps_the_top_level_files_first() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..5 {
            touch(&dir.path().join(format!("deep/{index}.txt")), "");
        }
        touch(&dir.path().join("top.txt"), "");
        let listing = list_project_files(dir.path(), 3, MAX_DEPTH);
        assert!(listing.truncated);
        assert!(paths(&listing).contains(&"top.txt"));
    }

    #[test]
    fn depth_is_bounded_and_paths_use_forward_slashes() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("a/b/c.txt"), "");
        touch(&dir.path().join("a/b/c/d/e/f.txt"), "");
        let listing = list_project_files(dir.path(), FILE_LIMIT, 2);
        assert_eq!(paths(&listing).len(), 1);
        assert_eq!(listing.files[0].path, "a/b/c.txt");
        assert_eq!(list_project_files(dir.path(), FILE_LIMIT, 0).files.len(), 0);
        assert_eq!(list_project_files(dir.path(), FILE_LIMIT, 5).files.len(), 2);
    }

    #[test]
    fn a_missing_or_unreadable_root_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let listing = list_project_files(&dir.path().join("nope"), FILE_LIMIT, MAX_DEPTH);
        assert!(listing.files.is_empty() && !listing.truncated);
        let file = dir.path().join("f.txt");
        touch(&file, "x");
        assert!(list_project_files(&file, FILE_LIMIT, MAX_DEPTH).files.is_empty());
    }

    #[test]
    fn equal_times_order_by_path_and_times_before_the_epoch_sort_last() {
        let dir = tempfile::tempdir().unwrap();
        // Where "A.txt" and "a.txt" are the same file (Windows, default macOS), the case tie-break cannot be tested.
        let names = if cfg!(windows) {
            ["c.txt", "B.txt", "a.txt", "old.txt"]
        } else {
            ["b.txt", "A.txt", "a.txt", "old.txt"]
        };
        for name in names {
            touch(&dir.path().join(name), "");
            let time = if name == "old.txt" {
                UNIX_EPOCH - Duration::from_secs(5)
            } else {
                UNIX_EPOCH + Duration::from_secs(9)
            };
            File::options()
                .write(true)
                .open(dir.path().join(name))
                .unwrap()
                .set_modified(time)
                .unwrap();
        }
        let listing = list_project_files(dir.path(), FILE_LIMIT, MAX_DEPTH);
        let expected = if cfg!(windows) {
            ["a.txt", "B.txt", "c.txt", "old.txt"]
        } else {
            ["A.txt", "a.txt", "b.txt", "old.txt"]
        };
        assert_eq!(paths(&listing), expected);
        assert_eq!(listing.files[3].mtime, -5000.0);
    }

    #[test]
    fn the_listing_serializes_as_the_renderer_expects() {
        let json = serde_json::to_value(ProjectFiles {
            files: vec![ProjectFile {
                path: "a/b.txt".into(),
                size: 3,
                mtime: 1000.0,
            }],
            truncated: false,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"files": [{"path": "a/b.txt", "size": 3, "mtime": 1000.0}], "truncated": false})
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_are_neither_followed_nor_listed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let outside = dir.path().join("outside");
        touch(&root.join("real.txt"), "");
        touch(&outside.join("secret.txt"), "");
        std::os::unix::fs::symlink(&outside, root.join("shortcut")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        assert_eq!(paths(&list_project_files(&root, FILE_LIMIT, MAX_DEPTH)), ["real.txt"]);
    }

    #[test]
    fn previews_text_as_content_and_replaces_bytes_that_are_not_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        touch(&text, "héllo\nworld");
        let preview = read_preview(&text).unwrap();
        assert_eq!((preview.mime.as_str(), preview.size), ("text/plain", 12));
        assert_eq!(preview.content.as_deref(), Some("héllo\nworld"));
        assert!(preview.data_url.is_none());
        assert_eq!(preview.path, text.to_string_lossy());

        let binary = dir.path().join("blob");
        touch(&binary, [b'a', 0xFF, b'b']);
        assert_eq!(read_preview(&binary).unwrap().content.as_deref(), Some("a\u{FFFD}b"));
        touch(&dir.path().join("empty.csv"), "");
        assert_eq!(
            read_preview(&dir.path().join("empty.csv")).unwrap().content.as_deref(),
            Some("")
        );
    }

    #[test]
    fn previews_images_and_documents_as_data_urls() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            ("a.PNG", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.svg", "image/svg+xml"),
            ("a.pdf", "application/pdf"),
            (
                "a.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            (
                "a.pptx",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            ),
            (
                "a.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            ("a.xls", "application/vnd.ms-excel"),
        ];
        for (name, mime) in cases {
            let file = dir.path().join(name);
            touch(&file, [0u8, 1, 2, 250, 255]);
            let preview = read_preview(&file).unwrap();
            assert_eq!(preview.mime, mime, "{name}");
            assert_eq!(
                preview.data_url.as_deref(),
                Some(format!("data:{mime};base64,AAEC+v8=").as_str()),
                "{name}"
            );
            assert!(preview.content.is_none());
        }
    }

    #[test]
    fn the_preview_serializes_with_only_the_field_that_applies() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("a.txt");
        touch(&text, "x");
        let json = serde_json::to_value(read_preview(&text).unwrap()).unwrap();
        assert_eq!(json["content"], "x");
        assert!(json.get("dataUrl").is_none());
        let image = dir.path().join("a.png");
        touch(&image, "x");
        let json = serde_json::to_value(read_preview(&image).unwrap()).unwrap();
        assert!(json.get("content").is_none());
        assert!(json["dataUrl"].as_str().unwrap().starts_with("data:image/png;base64,"));
    }

    /// A file of `size` bytes without writing them.
    fn sparse(path: &Path, size: u64) {
        File::create(path).unwrap().set_len(size).unwrap();
    }

    #[test]
    fn size_ceilings_depend_on_the_kind_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let join = |name: &str| dir.path().join(name);
        let message = |name: &str| read_preview(&join(name)).unwrap_err().to_string();

        sparse(&join("big.txt"), 9 * MIB);
        assert_eq!(
            message("big.txt"),
            "File is too large to preview (9 MB; the limit is 8 MB)"
        );
        sparse(&join("exact.txt"), 8 * MIB);
        assert!(read_preview(&join("exact.txt")).is_ok());
        sparse(&join("over.txt"), 8 * MIB + 1);
        assert!(message("over.txt").contains("(8 MB; the limit is 8 MB)"));
        sparse(&join("huge.png"), 9 * MIB); // images are held to the text ceiling
        assert!(message("huge.png").contains("limit is 8 MB"));

        sparse(&join("report.pdf"), 33 * MIB);
        assert_eq!(
            message("report.pdf"),
            "File is too large to preview (33 MB; the limit is 32 MB)"
        );
        sparse(&join("ok.pdf"), 9 * MIB);
        assert!(read_preview(&join("ok.pdf")).is_ok());

        sparse(&join("complex.cif"), 65 * MIB);
        assert_eq!(
            message("complex.cif"),
            "File is too large to preview (65 MB; the limit is 64 MB)"
        );
        sparse(&join("ok.PDB"), 9 * MIB);
        assert!(read_preview(&join("ok.PDB")).is_ok());

        sparse(&join("half.txt"), 9 * MIB + MIB / 2);
        assert!(message("half.txt").contains("(10 MB;"), "rounds like Math.round");
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_preview(&dir.path().join("gone.txt")).is_err());
        assert!(read_preview(dir.path()).is_err(), "a folder is not a file");
    }

    #[test]
    fn lexical_resolution_folds_dots_and_stays_at_the_root() {
        let base = Path::new("/p/q");
        assert_eq!(resolve_lexical(base, "a/./b/../c.txt"), Path::new("/p/q/a/c.txt"));
        assert_eq!(resolve_lexical(base, "../x"), Path::new("/p/x"));
        assert_eq!(resolve_lexical(base, "/etc/passwd"), Path::new("/etc/passwd"));
        assert_eq!(resolve_lexical(base, "../../../../x"), Path::new("/x"));
        assert_eq!(resolve_lexical(Path::new("/p/q/"), ""), Path::new("/p/q"));
    }

    #[cfg(windows)]
    #[test]
    fn lexical_resolution_understands_drives_and_both_separators() {
        let base = Path::new(r"C:\proj");
        assert_eq!(resolve_lexical(base, r"sub\..\a.txt"), Path::new(r"C:\proj\a.txt"));
        assert_eq!(resolve_lexical(base, "sub/a.txt"), Path::new(r"C:\proj\sub\a.txt"));
        assert_eq!(resolve_lexical(base, r"D:\other\x"), Path::new(r"D:\other\x"));
        assert_eq!(resolve_lexical(base, r"..\..\..\x"), Path::new(r"C:\x"));
    }

    #[test]
    fn project_files_must_be_inside_the_project_and_exist() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        touch(&project.join("results/plot.png"), "x");
        touch(&dir.path().join("project-other/secret.txt"), "x");
        touch(&dir.path().join("outside.txt"), "x");
        let message = |requested: &str| project_file(&project, requested).unwrap_err().to_string();
        let expected = validate::resolve(&project.join("results/plot.png")).unwrap();

        assert_eq!(project_file(&project, "results/plot.png").unwrap(), expected);
        assert_eq!(project_file(&project, "  results/plot.png  ").unwrap(), expected);
        assert_eq!(
            project_file(&project, "results/../results/./plot.png").unwrap(),
            expected
        );
        assert_eq!(project_file(&project, expected.to_str().unwrap()).unwrap(), expected);

        assert_eq!(message("../outside.txt"), "Artifact path is outside the project");
        assert_eq!(
            message("../project-other/secret.txt"),
            "Artifact path is outside the project"
        );
        assert_eq!(
            message("results/../../outside.txt"),
            "Artifact path is outside the project"
        );
        assert_eq!(
            message(dir.path().join("outside.txt").to_str().unwrap()),
            "Artifact path is outside the project"
        );
        assert_eq!(message("results/missing.png"), "artifact path is not an existing file");
        assert_eq!(message("results"), "artifact path is not an existing file");
        assert_eq!(message(""), "artifact path is required");
        assert_eq!(message("   "), "artifact path is required");
        assert!(message(&"a".repeat(5000)).contains("at most 4096"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_project_is_outside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        touch(&project.join("inside.txt"), "x");
        touch(&dir.path().join("secret/key"), "k");
        std::os::unix::fs::symlink(dir.path().join("secret"), project.join("shortcut")).unwrap();
        std::os::unix::fs::symlink(project.join("inside.txt"), project.join("alias.txt")).unwrap();
        assert_eq!(
            project_file(&project, "shortcut/key").unwrap_err().to_string(),
            "Artifact path is outside the project"
        );
        // A link that stays inside is fine.
        assert!(project_file(&project, "alias.txt").is_ok());
        // `link/..` is folded before the disk is asked, so it cannot climb out through the link.
        assert!(project_file(&project, "shortcut/../inside.txt").is_ok());
    }

    #[test]
    fn previewable_files_are_in_an_open_project_or_were_picked() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        touch(&one.join("a.txt"), "x");
        touch(&two.join("deep/b.txt"), "x");
        touch(&dir.path().join("picked.txt"), "x");
        touch(&dir.path().join("other.txt"), "x");
        touch(&dir.path().join("one-two/c.txt"), "x");
        let text = |path: PathBuf| path.to_str().unwrap().to_string();
        let roots = [one.clone(), two.clone()];
        let picked = [
            text(dir.path().join("picked.txt")),
            text(dir.path().join("vanished.txt")),
        ];
        let ask = |path: PathBuf| previewable_file(&roots, &picked, &text(path));

        assert!(ask(one.join("a.txt")).is_ok());
        assert!(ask(two.join("deep/b.txt")).is_ok());
        assert!(ask(dir.path().join("picked.txt")).is_ok());
        let denied = "File is not in an open project or a file you opened";
        assert_eq!(ask(dir.path().join("other.txt")).unwrap_err().to_string(), denied);
        assert_eq!(ask(dir.path().join("one-two/c.txt")).unwrap_err().to_string(), denied);
        assert_eq!(
            ask(dir.path().join("vanished.txt")).unwrap_err().to_string(),
            "path is not an existing file"
        );
        assert!(
            previewable_file(&roots, &picked, "relative/a.txt")
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        assert!(previewable_file::<PathBuf, String>(&[], &[], &text(one.join("a.txt"))).is_err());
        // With nothing open and nothing picked, nothing is readable.
        assert!(previewable_file(&[] as &[PathBuf], &[] as &[String], &text(one.join("a.txt"))).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_picked_file_matches_through_a_link_and_a_link_out_of_a_project_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        touch(&dir.path().join("data/real.txt"), "x");
        std::fs::create_dir_all(&project).unwrap();
        std::os::unix::fs::symlink(dir.path().join("data"), project.join("data")).unwrap();
        let through_link = project.join("data/real.txt").to_str().unwrap().to_string();
        let denied = previewable_file(std::slice::from_ref(&project), &[] as &[String], &through_link);
        assert!(denied.is_err(), "a link that leaves the project is not inside it");
        // Picked once under either spelling, readable under both.
        let picked = [dir.path().join("data/real.txt")];
        assert!(previewable_file(&[] as &[PathBuf], &picked, &through_link).is_ok());
        let picked = [project.join("data/real.txt")];
        let real = dir.path().join("data/real.txt").to_str().unwrap().to_string();
        assert!(previewable_file(&[] as &[PathBuf], &picked, &real).is_ok());
    }

    #[test]
    fn the_file_menu_describes_a_file_inside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        touch(&project.join("a/b.txt"), "x");
        touch(&dir.path().join("outside.txt"), "x");

        let menu = file_menu(Some(&project), "a/b.txt").unwrap();
        assert_eq!(menu.full_path, project.join("a/b.txt"));
        assert_eq!(
            menu.relative_path.as_deref(),
            Some(Path::new("a").join("b.txt").to_str().unwrap())
        );
        assert!(menu.revealable);

        // Mentioned in chat but not written yet: still has a path, cannot be revealed.
        let missing = file_menu(Some(&project), "a/new.txt").unwrap();
        assert!(!missing.revealable);
        assert!(missing.relative_path.is_some());

        let outside = file_menu(Some(&project), "../outside.txt").unwrap();
        assert_eq!(outside.full_path, dir.path().join("outside.txt"));
        assert!((outside.relative_path.as_ref(), outside.revealable) == (None, false));

        let loose = file_menu(None, dir.path().join("outside.txt").to_str().unwrap()).unwrap();
        assert!((loose.relative_path.as_ref(), loose.revealable) == (None, false));
        assert_eq!(loose.full_path, dir.path().join("outside.txt"));

        assert!(file_menu(Some(&project), "  ").is_err());
        let label = menu.reveal_label;
        assert!(["Show in Finder", "Show in Explorer", "Show in File Manager"].contains(&label));
    }

    #[test]
    fn revealing_selects_or_opens_the_folder_per_system() {
        let command = reveal_command(Path::new("/p/q/file.txt"));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        if cfg!(target_os = "macos") {
            assert_eq!(
                (command.get_program().to_str(), args),
                (Some("open"), vec!["-R".into(), "/p/q/file.txt".into()])
            );
        } else if cfg!(windows) {
            assert_eq!(command.get_program().to_str(), Some("explorer"));
        } else {
            assert_eq!(
                (command.get_program().to_str(), args),
                (Some("xdg-open"), vec!["/p/q".into()])
            );
        }
    }

    #[test]
    fn mtime_converts_both_sides_of_the_epoch() {
        assert_eq!(epoch_millis(UNIX_EPOCH + Duration::from_millis(1500)), 1500.0);
        assert_eq!(epoch_millis(UNIX_EPOCH - Duration::from_millis(1500)), -1500.0);
        assert!(epoch_millis(SystemTime::now()) > 1.0e12);
    }
}

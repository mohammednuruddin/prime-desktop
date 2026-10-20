use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

const RESERVED_DEVICE_NAMES: [&str; 28] = [
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "CONIN$",
    "CONOUT$",
    "CLOCK$",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
];

/// Why a path string is unsafe on Windows, or `None` when it is fine. It looks at the text only, so it
/// runs on every platform, and the policy applies it on Windows. Each rule closes a way to name a file
/// that a plain prefix check on the string would not notice: device and verbatim namespaces, network
/// shares, alternate data streams, trailing dots and spaces (which Win32 drops), and device names.
pub fn windows_path_problem(raw: &str) -> Option<&'static str> {
    let unified = raw.replace('/', "\\");
    if unified.starts_with("\\\\.\\") {
        return Some("device paths (\\\\.\\) are not allowed");
    }
    if let Some(rest) = unified.strip_prefix("\\\\?\\") {
        let mut chars = rest.chars();
        let drive_form = matches!((chars.next(), chars.next(), chars.next()), (Some(d), Some(':'), Some('\\')) if d.is_ascii_alphabetic());
        if !drive_form {
            return Some("only \\\\?\\<drive>:\\ verbatim paths are allowed");
        }
    } else if unified.starts_with("\\\\") {
        return Some("network share paths are not supported");
    }
    // Skip a leading drive such as `C:` or `\\?\C:`, then no segment may use a colon.
    let body = unified.strip_prefix("\\\\?\\").unwrap_or(&unified);
    let body = match body.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => &body[2..],
        _ => body,
    };
    for segment in body.split('\\').filter(|s| !s.is_empty()) {
        if segment.contains(':') {
            return Some("alternate data streams (a colon in a name) are not allowed");
        }
        if segment != "." && segment != ".." && (segment.ends_with('.') || segment.ends_with(' ')) {
            return Some("names ending in a dot or a space are not allowed");
        }
        let stem = segment.split('.').next().unwrap_or(segment).trim_end();
        if RESERVED_DEVICE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(stem)) {
            return Some("reserved device names (CON, NUL, COM1 ...) are not allowed");
        }
    }
    None
}

/// A `..` in a path a host sends. Hosts must resolve those first. Unix follows links before applying `..`
/// and Win32 applies it to the text first, so the same string can name two different files.
pub fn has_parent_segment(path: &Path) -> bool {
    // Compare the text, not `Component::ParentDir`: on a verbatim path (`\\?\C:\a`) std reports `..` as an
    // ordinary name, and Win32 still resolves it when the path is opened.
    path.components().any(|c| c.as_os_str() == "..")
}

/// Resolves symlinks and junctions in the longest part that exists, then appends the rest as written.
/// A path that does not exist yet (a `.git` that `git init` will create) still gets a stable, comparable
/// form. A `..` in the part that does not exist is refused, because it could climb back out later.
pub fn canonical_with_missing_tail(path: &Path) -> io::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(base) => {
                let mut out = base;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return Ok(out);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
                    return Err(error);
                };
                if name == ".." {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a .. segment in a missing part of the path",
                    ));
                }
                tail.push(name);
                if !existing.pop() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn folded(path: &Path) -> PathBuf {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// True when `target` is `root` or lies under it. It compares whole components, so `/work` does not
/// contain `/workshop`. Windows and macOS volumes ignore case by default, so they ignore it here too;
/// a spurious match there only ever denies a path the kernel would have allowed. Both paths must
/// already be canonical.
pub fn within(root: &Path, target: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        within_ignoring_case(root, target)
    } else {
        target.starts_with(root)
    }
}

/// Like [`within`], but always ignores case. Denials use it on every platform: a case variant of a denied
/// name (`.GIT`) is the same directory on a case-insensitive volume, and on a case-sensitive one a
/// spurious match only over-denies.
pub fn within_ignoring_case(root: &Path, target: &Path) -> bool {
    folded(target).starts_with(folded(root))
}

/// True when any segment of `relative` is one of `names`, compared without case. Used for protected names
/// such as `.git` and `.env`, which must hold at every depth: a nested repository's `.git/hooks` runs code
/// just as the top one does.
pub fn has_protected_segment(relative: &Path, names: &[String]) -> bool {
    relative
        .components()
        .any(|c| names.contains(&c.as_os_str().to_string_lossy().to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_traps_are_named() {
        let bad = [
            r"\\.\C:\x",
            r"\\?\GLOBALROOT\Device\x",
            r"\\server\share\x",
            r"C:\work\file.txt:hidden",
            r"C:\work\name. ",
            r"C:\work\trailing.",
            r"C:\work\NUL",
            r"C:\work\nul.txt",
            r"C:\work\COM1.log",
            "C:/work/aux",
        ];
        for path in bad {
            assert!(windows_path_problem(path).is_some(), "{path} should be refused");
        }
    }

    #[test]
    fn ordinary_windows_paths_pass() {
        for path in [
            r"C:\Users\A B\project",
            r"C:\work\.git",
            r"\\?\C:\work",
            "C:/work/src/main.rs",
            r"C:\work\console",
        ] {
            assert_eq!(windows_path_problem(path), None, "{path} should pass");
        }
    }

    #[test]
    fn within_compares_whole_components() {
        let root = Path::new("/work");
        assert!(within(root, Path::new("/work")));
        assert!(within(root, Path::new("/work/src")));
        assert!(!within(root, Path::new("/workshop")));
        assert!(!within(root, Path::new("/")));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn within_ignores_case_where_the_volume_does() {
        assert!(within(Path::new("/Work"), Path::new("/WORK/Src")));
    }

    #[test]
    fn denials_ignore_case_everywhere() {
        assert!(within_ignoring_case(
            Path::new("/work/.git"),
            Path::new("/work/.GIT/hooks")
        ));
        assert!(!within_ignoring_case(
            Path::new("/work/.git"),
            Path::new("/work/.gitignore")
        ));
    }

    #[test]
    fn protected_segments_match_at_any_depth_by_whole_name() {
        let names = vec![".git".to_string(), ".env".to_string()];
        assert!(has_protected_segment(Path::new("sub/.git/hooks/pre-commit"), &names));
        assert!(has_protected_segment(Path::new("app/.ENV"), &names));
        assert!(!has_protected_segment(Path::new("src/.gitignore"), &names));
        assert!(!has_protected_segment(Path::new("src/not.git"), &names));
    }

    #[test]
    fn missing_tail_is_kept() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let resolved = canonical_with_missing_tail(&dir.join("not-yet").join(".git")).unwrap();
        assert_eq!(resolved, dir.join("not-yet").join(".git"));
    }

    // Only Unix refuses here: Win32 folds `..` into the text before it looks at the disk, so the same
    // string is a different, valid path there. `has_parent_segment` is what keeps both platforms equal.
    #[cfg(unix)]
    #[test]
    fn dotdot_in_a_missing_part_is_refused() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        assert!(canonical_with_missing_tail(&dir.join("not-yet").join("..").join("x")).is_err());
    }

    #[test]
    fn parent_segments_are_found() {
        assert!(has_parent_segment(Path::new("/a/../b")));
        assert!(!has_parent_segment(Path::new("/a/b..c/..d")));
    }

    #[cfg(windows)]
    #[test]
    fn parent_segments_are_found_in_verbatim_paths() {
        assert!(has_parent_segment(Path::new(r"\\?\C:\work\..\outside")));
    }
}

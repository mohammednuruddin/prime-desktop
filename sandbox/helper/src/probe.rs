//! The import checks the agent runs before it starts a kernel.
//!
//! The agent asks its kernel interpreter, one launch at a time, whether it can import each package it needs: a runtime check,
//! about a dozen packages, and every Python skill. Each launch through the wrapper starts a sandbox, which takes about a
//! second on Windows, so a new chat spent half a minute on them. A check that passed inside the sandbox is remembered in the
//! helper's own state folder (the sandboxed program cannot write there) and answered from it while the interpreter and its
//! packages are as they were. Only a check that is nothing but imports can be remembered: anything else runs every time.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The agent's runtime check starts like this. It only imports and asserts.
const RUNTIME_CHECK_START: &str = "import inspect; import rlm;";

/// The code of an interpreter call that is `-c <imports only>`, which is safe to remember.
pub fn cacheable(args: &[String]) -> Option<&str> {
    let [flag, code] = args else { return None };
    if flag != "-c" {
        return None;
    }
    let plain_import = code.starts_with("import ")
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ',' | ' '));
    (plain_import || code.starts_with(RUNTIME_CHECK_START)).then_some(code.as_str())
}

fn stamp(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return format!("{}:missing", path.display());
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |age| age.as_nanos());
    format!("{}:{}:{modified}", path.display(), meta.len())
}

/// Where the packages of the interpreter's environment are, for the key: an installed or removed package changes the
/// modification time of its folder.
fn package_dirs(python: &Path, python_path: &[PathBuf]) -> Vec<PathBuf> {
    if !python_path.is_empty() {
        return python_path.to_vec();
    }
    let Some(root) = python.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    let mut dirs = vec![root.join("Lib").join("site-packages")];
    dirs.extend(
        std::fs::read_dir(root.join("lib"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("site-packages")),
    );
    dirs
}

/// Identifies one check on one state of the environment.
pub fn key(code: &str, python: &Path, python_path: &[PathBuf]) -> String {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    stamp(python).hash(&mut hasher);
    for dir in package_dirs(python, python_path) {
        stamp(&dir).hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn file(state_dir: &Path, key: &str) -> PathBuf {
    state_dir.join("probe-cache").join(key)
}

pub fn passed_before(state_dir: &Path, key: &str) -> bool {
    file(state_dir, key).is_file()
}

pub fn remember(state_dir: &Path, key: &str) {
    let path = file(state_dir, key);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, b"passed");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder that goes away with the test.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn only_checks_that_do_nothing_but_import_are_remembered() {
        assert!(cacheable(&args(&["-c", "import edit"])).is_some());
        assert!(cacheable(&args(&["-c", "import yaml, dill"])).is_some());
        assert!(cacheable(&args(&["-c", "import inspect; import rlm; assert callable(rlm.spawn)"])).is_some());
        for refused in [
            args(&["-c", "import os; os.system('x')"]),
            args(&["-c", "import os\nprint(1)"]),
            args(&["-c", "open('f', 'w')"]),
            args(&["-m", "rlm.repl"]),
            args(&["-c", "import edit", "--"]),
            args(&["-c"]),
            args(&[]),
        ] {
            assert!(cacheable(&refused).is_none(), "{refused:?}");
        }
    }

    #[test]
    fn a_check_is_answered_from_memory_until_the_environment_changes() {
        let scratch = std::env::temp_dir().join(format!("desktop-probe-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let dir = Scratch(scratch);
        let python = dir.0.join("venv").join("bin").join("python");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        let packages = dir.0.join("venv").join("lib").join("python3.11").join("site-packages");
        std::fs::create_dir_all(&packages).unwrap();
        let state = dir.0.join("state");
        let first = key("import edit", &python, &[]);
        assert!(!passed_before(&state, &first));
        remember(&state, &first);
        assert!(passed_before(&state, &first));
        assert_eq!(
            first,
            key("import edit", &python, &[]),
            "the same check and environment give the same key"
        );
        assert_ne!(first, key("import goal", &python, &[]));
        // Installing a package changes the folder's time.
        std::fs::create_dir_all(packages.join("newpkg")).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::open(&packages).unwrap().set_modified(later).unwrap();
        assert_ne!(
            first,
            key("import edit", &python, &[]),
            "a changed environment asks again"
        );
    }
}

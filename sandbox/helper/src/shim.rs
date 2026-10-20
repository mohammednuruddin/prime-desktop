//! The startup shim a Windows kernel needs inside the container (see `shim/sitecustomize.py` for what and why).

use std::path::{Path, PathBuf};

const SITECUSTOMIZE: &str = include_str!("shim/sitecustomize.py");

/// Writes the shim under `state_dir` when it is missing or different, and returns the folder to put on `PYTHONPATH`.
/// The folder is given to the sandbox read-only; the contained program cannot change what runs at its own start-up.
pub fn install(state_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = state_dir.join("kernel-shim");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("sitecustomize.py");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(SITECUSTOMIZE) {
        std::fs::write(&file, SITECUSTOMIZE)?;
    }
    dir.canonicalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!("desktop-shim-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_shim_is_written_once_and_rewritten_if_it_changes() {
        let dir = Scratch::new("once");
        let shim = install(&dir.0).unwrap();
        let file = shim.join("sitecustomize.py");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), SITECUSTOMIZE);
        std::fs::write(&file, "tampered").unwrap();
        install(&dir.0).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            SITECUSTOMIZE,
            "a changed shim is put back"
        );
    }

    #[test]
    fn the_shim_is_valid_python_when_a_python_is_at_hand() {
        let Some(python) = ["python3", "python"].into_iter().find(|name| {
            std::process::Command::new(name)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        }) else {
            return;
        };
        let dir = Scratch::new("syntax");
        let file = install(&dir.0).unwrap().join("sitecustomize.py");
        let status = std::process::Command::new(python)
            .args([
                "-c",
                "import sys; compile(open(sys.argv[1]).read(), sys.argv[1], 'exec')",
            ])
            .arg(&file)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn off_windows_the_shim_does_nothing_even_when_run() {
        let Some(python) = ["python3", "python"].into_iter().find(|name| {
            std::process::Command::new(name)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        }) else {
            return;
        };
        if cfg!(windows) {
            return;
        }
        let dir = Scratch::new("inert");
        let shim = install(&dir.0).unwrap();
        let out = std::process::Command::new(python)
            .env("PYTHONPATH", &shim)
            .args([
                "-c",
                "import os; print(os.environ.get('PRIME_AGENT_KERNEL_OWNER_PID', 'untouched'))",
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "untouched");
    }
}

//! Where things live and how child processes start on each system.

use std::path::{Path, PathBuf};

use crate::product::product;

/// The current user's home folder.
pub fn home_dir() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Where the app keeps its own files: state, logs, the installed Prime Agent, the daemon socket. `PRIME_DESKTOP_DATA_DIR`
/// overrides it, which is how tests stay out of the real one.
pub fn app_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PRIME_DESKTOP_DATA_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    if cfg!(windows) {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
        base.join(&product().app_id)
    } else if cfg!(target_os = "macos") {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join(product().mac_dir())
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local").join("share"))
            .join(product().slug())
    }
}

pub fn state_file() -> PathBuf {
    app_data_dir().join("state.json")
}

pub fn logs_dir() -> PathBuf {
    app_data_dir().join("logs")
}

/// Where the pinned Prime Agent is installed.
pub fn agent_install_dir() -> PathBuf {
    app_data_dir().join("agent")
}

/// Prime Agent's own folder (auth, models, settings, sessions). Shared with the Prime CLI on purpose: it is the same
/// agent, so the same keys and the same saved sessions. `PRIME_AGENT_CODING_AGENT_DIR` moves it.
pub fn prime_agent_dir() -> PathBuf {
    match std::env::var_os("PRIME_AGENT_CODING_AGENT_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home_dir().join(".prime").join("agent"),
    }
}

/// The daemon's socket. Windows daemons listen on named pipes, which are not file paths. Elsewhere it sits in Prime Agent's
/// own folder, because `prime-agent status` and `shutdown` only see daemons whose socket is there.
pub fn daemon_socket() -> PathBuf {
    if cfg!(windows) {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        PathBuf::from(format!(r"\\.\pipe\{}-{user}", product().slug()))
    } else {
        prime_agent_dir().join(format!("{}.sock", product().slug()))
    }
}

pub fn is_named_pipe(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with(r"\\.\pipe\") || text.starts_with(r"\\?\pipe\")
}

/// Stops a child process opening a console window on Windows. Every process the app starts goes through this; a
/// forgotten one flashes a terminal at the user. No effect elsewhere.
pub fn quiet(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// [`quiet`] for tokio's command type.
pub fn quiet_async(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts a process that outlives the app: no console window, no parent console, its own process group, so closing the
/// app (or Ctrl+C in a terminal that launched it) does not take the Prime Agent daemon down with it.
pub fn detached(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// The program and arguments that open `exe` in a terminal window of its own, one per terminal worth trying.
fn terminal_commands(exe: &Path, os: &str) -> Vec<(String, Vec<String>)> {
    let path = exe.to_string_lossy().into_owned();
    let strings = |list: &[&str]| list.iter().map(|part| part.to_string()).collect::<Vec<_>>();
    match os {
        "windows" => {
            let mut args = strings(&["/C", "start", "", "powershell.exe", "-NoExit", "-Command"]);
            args.push(format!("& '{}'", path.replace('\'', "''")));
            vec![("cmd".into(), args)]
        }
        "macos" => {
            let quoted = format!("'{}'", path.replace('\'', "'\\''"));
            vec![(
                "osascript".into(),
                vec![
                    "-e".into(),
                    format!(
                        "tell application \"Terminal\" to do script \"{}\"",
                        quoted.replace('\\', "\\\\").replace('"', "\\\"")
                    ),
                    "-e".into(),
                    "tell application \"Terminal\" to activate".into(),
                ],
            )]
        }
        _ => ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"]
            .iter()
            .map(|terminal| {
                let separator = if *terminal == "gnome-terminal" { "--" } else { "-e" };
                (terminal.to_string(), vec![separator.to_string(), path.clone()])
            })
            .collect(),
    }
}

/// Opens a program in a terminal window, for the few screens that live in Prime Agent's own interface (signing in).
pub fn open_in_terminal(exe: &Path) -> anyhow::Result<()> {
    let mut last = None;
    for (program, args) in terminal_commands(exe, std::env::consts::OS) {
        // A window is wanted here, so this does not go through `quiet`.
        match std::process::Command::new(&program).args(&args).spawn() {
            Ok(_) => return Ok(()),
            Err(error) => last = Some(error),
        }
    }
    anyhow::bail!(
        "could not open a terminal: {}",
        last.map_or_else(|| "none to try".to_string(), |error| error.to_string())
    )
}

/// The shell for the terminal panel and its arguments.
pub fn terminal_shell() -> (String, Vec<String>) {
    if cfg!(windows) {
        return ("powershell.exe".into(), vec!["-NoLogo".into()]);
    }
    if let Some(shell) = std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/') && Path::new(shell).exists())
    {
        return (shell, vec!["-l".into()]);
    }
    for shell in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if Path::new(shell).exists() {
            return (shell.into(), vec!["-l".into()]);
        }
    }
    ("/bin/sh".into(), vec!["-l".into()])
}

/// PATH for child processes. Apps started from the macOS Dock do not inherit the login shell's PATH, so the usual
/// install folders are added there.
pub fn with_extra_path(path: Option<&str>) -> String {
    if cfg!(windows) {
        return path.unwrap_or_default().to_string();
    }
    let separator = ':';
    let extra = [
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        home_dir().join(".local/bin").to_string_lossy().into_owned(),
    ];
    let mut parts: Vec<String> = extra.to_vec();
    parts.push(path.unwrap_or("/usr/bin:/bin").to_string());
    parts.join(&separator.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_pipes_are_not_file_paths() {
        assert!(is_named_pipe(Path::new(r"\\.\pipe\prime-desktop-ada")));
        assert!(is_named_pipe(Path::new(r"\\?\pipe\x")));
        assert!(!is_named_pipe(Path::new("/tmp/prime-agent.sock")));
        assert!(!is_named_pipe(Path::new(r"C:\pipe\x")));
    }

    #[test]
    fn a_program_opens_in_a_terminal_on_each_system() {
        let exe = Path::new("/opt/prime agent/it's");
        let windows = terminal_commands(exe, "windows");
        assert_eq!(windows[0].0, "cmd");
        assert_eq!(
            windows[0].1.last().unwrap(),
            "& '/opt/prime agent/it''s'",
            "a single quote in the path is doubled for PowerShell"
        );
        let mac = terminal_commands(exe, "macos");
        assert_eq!(mac[0].0, "osascript");
        assert!(mac[0].1[1].contains("do script"));
        let linux = terminal_commands(exe, "linux");
        assert!(
            linux
                .iter()
                .any(|(program, args)| program == "gnome-terminal" && args[0] == "--")
        );
        assert!(
            linux
                .iter()
                .all(|(_, args)| args.last().unwrap() == "/opt/prime agent/it's"),
            "the path is one argument, never split"
        );
    }

    #[test]
    fn the_data_dir_can_be_moved_for_tests() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module are the only ones that touch this variable.
        unsafe { std::env::set_var("PRIME_DESKTOP_DATA_DIR", dir.path()) };
        assert_eq!(app_data_dir(), dir.path());
        assert_eq!(state_file(), dir.path().join("state.json"));
        assert_eq!(agent_install_dir(), dir.path().join("agent"));
        unsafe { std::env::remove_var("PRIME_DESKTOP_DATA_DIR") };
    }

    #[test]
    fn extra_path_entries_come_first_off_windows() {
        let joined = with_extra_path(Some("/usr/bin"));
        if cfg!(windows) {
            assert_eq!(joined, "/usr/bin");
        } else {
            assert!(joined.starts_with("/opt/homebrew/bin:/usr/local/bin:"));
            assert!(joined.ends_with(":/usr/bin"));
        }
    }
}

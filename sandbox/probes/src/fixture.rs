use std::fs;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A throwaway directory tree that each probe runs against.
///
/// ```text
/// ws/             write root          outside/        in no root
///   src/ok.txt                          secret.txt
///   .git/config     protected           agent-copy    a program the policy does not allow
///   sub/.git/       protected, nested
///   app/.env        protected, nested
///   link_out  ->  outside              (directory link, if the host allows it)
/// bin/            read root, holds the probe program
/// ```
pub struct Fixture {
    pub root: PathBuf,
    pub ws: PathBuf,
    pub outside: PathBuf,
    pub agent: PathBuf,
    pub outside_agent: PathBuf,
    pub links_ok: bool,
    pub listener: TcpListener,
}

/// A path as a host would send it: no `\\?\` prefix.
pub fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// A junction on Windows, which needs no privilege, and a symlink elsewhere.
pub fn dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|out| out.status.success())
    }
}

impl Fixture {
    pub fn create(agent_binary: &Path) -> io::Result<Self> {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("desktop-probe-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base)?;
        let root = plain(&base.canonicalize()?);
        let (ws, outside, bin) = (root.join("ws"), root.join("outside"), root.join("bin"));
        for dir in [
            ws.join("src"),
            ws.join(".git").join("hooks"),
            ws.join("sub").join(".git"),
            ws.join("app"),
            outside.clone(),
            bin.clone(),
        ] {
            fs::create_dir_all(dir)?;
        }
        fs::write(ws.join("src").join("ok.txt"), "ok")?;
        fs::write(ws.join(".git").join("config"), "git-secret")?;
        fs::write(ws.join("app").join(".env"), "TOKEN=secret")?;
        fs::write(outside.join("secret.txt"), "top-secret")?;

        let name = agent_binary.file_name().unwrap_or_default();
        let agent = bin.join(name);
        let outside_agent = outside.join(name);
        fs::copy(agent_binary, &agent)?;
        fs::copy(agent_binary, &outside_agent)?;

        let links_ok = dir_link(&outside, &ws.join("link_out"));
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            root,
            ws,
            outside,
            agent,
            outside_agent,
            links_ok,
            listener,
        })
    }

    pub fn listener_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("a bound listener has an address")
    }

    /// Whether anything connected to the listener since it was created.
    pub fn saw_connection(&self) -> bool {
        self.listener.accept().is_ok()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A directory link must go first: removing the tree must never follow it into `outside`.
        let link = self.ws.join("link_out");
        let _ = if cfg!(unix) {
            fs::remove_file(&link)
        } else {
            fs::remove_dir(&link)
        };
        let _ = fs::remove_dir_all(&self.root);
    }
}

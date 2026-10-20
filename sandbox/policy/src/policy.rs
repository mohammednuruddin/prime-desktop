use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::path::{
    Access, canonical_with_missing_tail, has_parent_segment, has_protected_segment, windows_path_problem, within,
    within_ignoring_case,
};

/// Bump when a field changes meaning. A host and a helper that disagree must fail at startup, not
/// quietly run with different semantics.
pub const SCHEMA_VERSION: u32 = 1;

/// Names under every root that no policy can open, unless the host sets `protectedNames` itself.
/// Credentials and repository metadata: an agent that can write `.git/hooks` can run code later, outside
/// any sandbox.
const DEFAULT_PROTECTED_NAMES: [&str; 5] = [".git", ".ssh", ".env", ".aws", ".gnupg"];

const TIMEOUT_MS: (u64, u64, u64) = (1_000, 30 * 60_000, 2 * 60_000);
const OUTPUT_BYTES: (u64, u64, u64) = (4_096, 64 * 1024 * 1024, 1024 * 1024);
const PROCESSES: (u32, u32, u32) = (1, 256, 8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    /// No network at all, including DNS.
    None,
    /// Only a host-supplied filtering proxy is reachable.
    ProxyOnly,
    /// An allowlist of destinations enforced by the backend.
    Restricted,
    /// Anything the host can reach. Workspace data can leave the machine, so it is only valid with an
    /// expiring grant (`grantExpiresAt`).
    Open,
}

/// What a host sends. Everything optional has a safe default, and nothing is clamped: a value outside its
/// range is an error, because a silently adjusted limit hides a bug in the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyInput {
    pub schema_version: u32,
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub read_roots: Vec<PathBuf>,
    #[serde(default)]
    pub write_roots: Vec<PathBuf>,
    #[serde(default)]
    pub denied_paths: Vec<PathBuf>,
    /// Names that are denied under every root. `None` means the default list.
    #[serde(default)]
    pub protected_names: Option<Vec<String>>,
    pub network: NetworkMode,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    pub max_processes: Option<u32>,
    #[serde(default)]
    pub allow_pty: bool,
    /// Windows only. Lets the child use the GUI subsystem (Win32k). Node, PowerShell and other programs that
    /// import USER32 cannot start without it, and it widens the kernel surface the child can reach, so it is
    /// off unless the host asks for it.
    #[serde(default)]
    pub allow_gui: bool,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
    /// Epoch milliseconds. `None` means the policy does not expire.
    #[serde(default)]
    pub grant_expires_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyError {
    pub field: String,
    pub message: String,
}

impl PolicyError {
    pub(crate) fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid `{}`: {}", self.field, self.message)
    }
}

impl std::error::Error for PolicyError {}

/// A validated policy. Every path is canonical, so a symlink or junction was followed when it was
/// resolved and cannot point somewhere else later in the checks. `denied_paths` holds those the host
/// listed plus each protected name at the top of every root, in both its written and its resolved form.
/// `protected_names` are denied at every depth under a root; a backend that can only deny by path prefix
/// cannot hold that by itself and must say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPolicy {
    pub id: String,
    pub label: String,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub denied_paths: Vec<PathBuf>,
    /// Roots as the host wrote them where that differs from the resolved path, such as a folder reached through a
    /// symlink name. A backend that binds folders into a private view needs the written name to exist there, or a
    /// link that points through it dangles. It names the same directory, so it grants nothing more.
    pub root_spellings: Vec<PathBuf>,
    /// Lower case. Matched without regard to case.
    pub protected_names: Vec<String>,
    pub network: NetworkMode,
    pub env_allowlist: BTreeSet<String>,
    pub max_processes: u32,
    pub allow_pty: bool,
    pub allow_gui: bool,
    pub timeout: Duration,
    pub max_output_bytes: u64,
    pub grant_expires_at: Option<u64>,
}

fn env_key(name: &str) -> String {
    // Windows environment names ignore case; elsewhere they do not.
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

pub(crate) fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn bounded<T: PartialOrd + Copy + fmt::Display>(
    field: &str,
    value: Option<T>,
    (min, max, default): (T, T, T),
) -> Result<T, PolicyError> {
    let value = value.unwrap_or(default);
    if value < min || value > max {
        return Err(PolicyError::new(
            field,
            format!("must be between {min} and {max}, got {value}"),
        ));
    }
    Ok(value)
}

fn resolve_path(field: &str, raw: &Path, must_exist: bool) -> Result<PathBuf, PolicyError> {
    let text = raw.to_string_lossy();
    if text.trim().is_empty() {
        return Err(PolicyError::new(field, "must not be empty"));
    }
    if text.contains('\0') {
        return Err(PolicyError::new(field, "must not contain a NUL"));
    }
    if cfg!(windows)
        && let Some(problem) = windows_path_problem(&text)
    {
        return Err(PolicyError::new(field, problem));
    }
    if !raw.is_absolute() {
        return Err(PolicyError::new(
            field,
            format!("must be an absolute path, got `{text}`"),
        ));
    }
    if has_parent_segment(raw) {
        return Err(PolicyError::new(field, "must not contain `..`; send the resolved path"));
    }
    let resolved = if must_exist {
        raw.canonicalize()
    } else {
        canonical_with_missing_tail(raw)
    };
    resolved.map_err(|error| PolicyError::new(field, format!("cannot resolve `{text}`: {error}")))
}

fn resolve_list(field: &str, raw: &[PathBuf], must_exist: bool) -> Result<Vec<PathBuf>, PolicyError> {
    Ok(resolve_list_with_spellings(field, raw, must_exist)?.0)
}

/// Like [`resolve_list`], and also returns the paths as written where they differ from the resolved ones.
fn resolve_list_with_spellings(
    field: &str,
    raw: &[PathBuf],
    must_exist: bool,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), PolicyError> {
    let (mut out, mut spellings): (Vec<PathBuf>, Vec<PathBuf>) = (Vec::new(), Vec::new());
    for (i, path) in raw.iter().enumerate() {
        let resolved = resolve_path(&format!("{field}[{i}]"), path, must_exist)?;
        if &resolved != path && !spellings.contains(path) {
            spellings.push(path.clone());
        }
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok((out, spellings))
}

impl PolicyInput {
    pub fn from_json(json: &str) -> Result<Self, PolicyError> {
        serde_json::from_str(json).map_err(|error| PolicyError::new("policy", error.to_string()))
    }

    /// Validates the input against the file system as it is now. `now_ms` is epoch milliseconds, passed in
    /// so a test can fix the clock.
    pub fn resolve(self, now_ms: u64) -> Result<ResolvedPolicy, PolicyError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(PolicyError::new(
                "schemaVersion",
                format!(
                    "this helper speaks version {SCHEMA_VERSION}, got {}",
                    self.schema_version
                ),
            ));
        }
        if self.id.trim().is_empty() {
            return Err(PolicyError::new("id", "must not be empty"));
        }
        if self.label.trim().is_empty() {
            return Err(PolicyError::new("label", "must not be empty"));
        }
        if self.grant_expires_at.is_some_and(|expires| expires <= now_ms) {
            return Err(PolicyError::new("grantExpiresAt", "must be in the future"));
        }
        if self.network == NetworkMode::Open && self.grant_expires_at.is_none() {
            return Err(PolicyError::new(
                "network",
                "open needs an expiring grant: set grantExpiresAt",
            ));
        }

        let (read_roots, read_spellings) = resolve_list_with_spellings("readRoots", &self.read_roots, true)?;
        let (write_roots, write_spellings) = resolve_list_with_spellings("writeRoots", &self.write_roots, true)?;
        let root_spellings: Vec<PathBuf> = read_spellings.into_iter().chain(write_spellings).collect();

        let names: Vec<String> = match &self.protected_names {
            Some(names) => names.clone(),
            None => DEFAULT_PROTECTED_NAMES.iter().map(|n| n.to_string()).collect(),
        };
        for name in &names {
            if name.is_empty() || name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
                return Err(PolicyError::new(
                    "protectedNames",
                    format!("`{name}` must be a single path segment"),
                ));
            }
        }
        let names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let mut denied = resolve_list("deniedPaths", &self.denied_paths, false)?;
        for root in read_roots.iter().chain(&write_roots) {
            for name in &names {
                // The written form is for backends that deny by path text. The resolved form follows a
                // `.git` that is itself a link, so access through the link target is denied as well.
                let written = root.join(name);
                let resolved = canonical_with_missing_tail(&written)
                    .map_err(|error| PolicyError::new("protectedNames", format!("cannot resolve `{name}`: {error}")))?;
                for path in [written, resolved] {
                    if !denied.contains(&path) {
                        denied.push(path);
                    }
                }
            }
        }

        let mut env_allowlist = BTreeSet::new();
        for (i, name) in self.env_allowlist.iter().enumerate() {
            if !valid_env_name(name) {
                return Err(PolicyError::new(
                    format!("envAllowlist[{i}]"),
                    format!("`{name}` is not a valid variable name"),
                ));
            }
            env_allowlist.insert(env_key(name));
        }

        Ok(ResolvedPolicy {
            id: self.id,
            label: self.label,
            read_roots,
            write_roots,
            denied_paths: denied,
            root_spellings,
            protected_names: names,
            network: self.network,
            env_allowlist,
            max_processes: bounded("maxProcesses", self.max_processes, PROCESSES)?,
            allow_pty: self.allow_pty,
            allow_gui: self.allow_gui,
            timeout: Duration::from_millis(bounded("timeoutMs", self.timeout_ms, TIMEOUT_MS)?),
            max_output_bytes: bounded("maxOutputBytes", self.max_output_bytes, OUTPUT_BYTES)?,
            grant_expires_at: self.grant_expires_at,
        })
    }
}

impl ResolvedPolicy {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.grant_expires_at.is_some_and(|expires| expires <= now_ms)
    }

    pub fn env_allowed(&self, name: &str) -> bool {
        self.env_allowlist.contains(&env_key(name))
    }

    /// Whether the policy lets a process touch `target`. Write roots are readable too. A denied path or a
    /// protected name at any depth wins over every root. This is a check on the policy, used for the working
    /// directory and by tests; the backend enforces the same rule in the kernel. It cannot see a hard link,
    /// which names the same file under two paths: only the kernel can refuse to create one.
    pub fn allows(&self, target: &Path, access: Access) -> bool {
        if !target.is_absolute() || has_parent_segment(target) {
            return false;
        }
        if cfg!(windows) && windows_path_problem(&target.to_string_lossy()).is_some() {
            return false;
        }
        let Ok(target) = canonical_with_missing_tail(target) else {
            return false;
        };
        if self
            .denied_paths
            .iter()
            .any(|denied| within_ignoring_case(denied, &target))
        {
            return false;
        }
        let roots: Vec<&PathBuf> = match access {
            Access::Write => self.write_roots.iter().collect(),
            Access::Read => self.read_roots.iter().chain(&self.write_roots).collect(),
        };
        roots.into_iter().any(|root| {
            within(root, &target) && {
                let below: PathBuf = target.components().skip(root.components().count()).collect();
                !has_protected_segment(&below, &self.protected_names)
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    pub(crate) const NOW: u64 = 1_800_000_000_000;

    pub(crate) struct Sandbox {
        pub root: PathBuf,
    }

    impl Sandbox {
        pub fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("desktop-policy-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("work").join("src")).unwrap();
            fs::create_dir_all(root.join("outside")).unwrap();
            Self {
                root: root.canonicalize().unwrap(),
            }
        }
        pub fn work(&self) -> PathBuf {
            self.root.join("work")
        }
        pub fn outside(&self) -> PathBuf {
            self.root.join("outside")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Joins by text. `PathBuf::join` folds a `..` away on a verbatim Windows path, which would hide the very
    /// input these tests need to send.
    pub(crate) fn raw_join(base: &Path, parts: &[&str]) -> PathBuf {
        let mut text = base.to_string_lossy().into_owned();
        for part in parts {
            text.push(std::path::MAIN_SEPARATOR);
            text.push_str(part);
        }
        PathBuf::from(text)
    }

    pub(crate) fn input(box_: &Sandbox) -> PolicyInput {
        PolicyInput {
            schema_version: SCHEMA_VERSION,
            id: "workspace".into(),
            label: "Workspace".into(),
            read_roots: vec![],
            write_roots: vec![box_.work()],
            denied_paths: vec![],
            protected_names: None,
            network: NetworkMode::None,
            env_allowlist: vec!["PATH".into(), "HOME".into()],
            max_processes: None,
            allow_pty: false,
            allow_gui: false,
            timeout_ms: None,
            max_output_bytes: None,
            grant_expires_at: None,
        }
    }

    #[test]
    fn defaults_apply_and_protected_names_are_denied_under_every_root() {
        let sb = Sandbox::new("defaults");
        let policy = input(&sb).resolve(NOW).unwrap();
        assert_eq!(policy.max_processes, 8);
        assert_eq!(policy.timeout, Duration::from_secs(120));
        assert_eq!(policy.max_output_bytes, 1024 * 1024);
        assert!(policy.denied_paths.contains(&sb.work().join(".git")));
        assert!(policy.denied_paths.contains(&sb.work().join(".ssh")));
        assert!(policy.protected_names.contains(&".env".to_string()));
    }

    #[test]
    fn protected_names_hold_at_every_depth_and_in_any_case() {
        let sb = Sandbox::new("nested");
        let policy = input(&sb).resolve(NOW).unwrap();
        for denied in [
            "sub/.git/hooks/pre-commit",
            "app/.env",
            "app/.ENV",
            ".GIT/config",
            "a/b/c/.ssh/id_rsa",
        ] {
            let path = sb.work().join(denied);
            assert!(
                !policy.allows(&path, Access::Write),
                "{denied} must be denied for write"
            );
            assert!(!policy.allows(&path, Access::Read), "{denied} must be denied for read");
        }
        for fine in [
            "src/.gitignore",
            "src/not.git",
            ".github/workflows/ci.yml",
            "env/notes.txt",
        ] {
            assert!(
                policy.allows(&sb.work().join(fine), Access::Write),
                "{fine} must stay allowed"
            );
        }
    }

    #[test]
    fn a_protected_dir_that_is_itself_a_link_is_denied_through_its_target_too() {
        let sb = Sandbox::new("gitlink");
        let real = sb.work().join("real-git");
        fs::create_dir_all(&real).unwrap();
        if !make_dir_link(&real, &sb.work().join(".git")) {
            panic!("could not create a directory link, so the linked .git case was not tested");
        }
        let policy = input(&sb).resolve(NOW).unwrap();
        assert!(
            !policy.allows(&real.join("hooks").join("pre-commit"), Access::Write),
            "the target of a linked .git"
        );
        assert!(!policy.allows(&sb.work().join(".git").join("config"), Access::Read));
        assert!(policy.allows(&sb.work().join("src").join("a.rs"), Access::Write));
    }

    #[test]
    fn explicit_denied_paths_ignore_case_on_every_platform() {
        let sb = Sandbox::new("case");
        let mut raw = input(&sb);
        raw.denied_paths = vec![sb.work().join("Private")];
        let policy = raw.resolve(NOW).unwrap();
        assert!(!policy.allows(&sb.work().join("PRIVATE").join("x"), Access::Read));
        assert!(!policy.allows(&sb.work().join("private").join("x"), Access::Read));
        assert!(policy.allows(&sb.work().join("privateer"), Access::Read));
    }

    #[test]
    fn deny_wins_over_roots_and_write_roots_are_readable() {
        let sb = Sandbox::new("deny");
        let policy = input(&sb).resolve(NOW).unwrap();
        assert!(policy.allows(&sb.work().join("src").join("a.rs"), Access::Write));
        assert!(policy.allows(&sb.work().join("src"), Access::Read));
        assert!(!policy.allows(&sb.work().join(".git").join("hooks").join("pre-commit"), Access::Write));
        assert!(!policy.allows(&sb.work().join(".git"), Access::Read));
        assert!(!policy.allows(&sb.outside().join("x"), Access::Read));
        assert!(
            !policy.allows(&sb.root, Access::Read),
            "a parent of a root is not inside it"
        );
    }

    #[test]
    fn a_read_root_does_not_grant_write() {
        let sb = Sandbox::new("readonly");
        let mut raw = input(&sb);
        raw.write_roots = vec![];
        raw.read_roots = vec![sb.work()];
        let policy = raw.resolve(NOW).unwrap();
        assert!(policy.allows(&sb.work().join("src"), Access::Read));
        assert!(!policy.allows(&sb.work().join("src"), Access::Write));
    }

    #[test]
    fn rejects_what_a_host_must_not_send() {
        let sb = Sandbox::new("reject");
        type Case = (&'static str, Box<dyn Fn(&mut PolicyInput)>);
        let cases: Vec<Case> = vec![
            ("schemaVersion", Box::new(|p| p.schema_version = 2)),
            ("id", Box::new(|p| p.id = " ".into())),
            (
                "writeRoots[0]",
                Box::new(|p| p.write_roots = vec![PathBuf::from("relative/dir")]),
            ),
            (
                "writeRoots[0]",
                Box::new(|p| p.write_roots = vec![p.write_roots[0].join("does-not-exist")]),
            ),
            (
                "writeRoots[0]",
                Box::new(|p| p.write_roots = vec![raw_join(&p.write_roots[0], &["..", "outside"])]),
            ),
            (
                "deniedPaths[0]",
                Box::new(|p| p.denied_paths = vec![raw_join(&p.write_roots[0], &["a", "..", "b"])]),
            ),
            (
                "envAllowlist[0]",
                Box::new(|p| p.env_allowlist = vec!["BAD NAME".into()]),
            ),
            (
                "envAllowlist[0]",
                Box::new(|p| p.env_allowlist = vec!["1STARTS_WITH_DIGIT".into()]),
            ),
            (
                "protectedNames",
                Box::new(|p| p.protected_names = Some(vec!["a/b".into()])),
            ),
            ("timeoutMs", Box::new(|p| p.timeout_ms = Some(0))),
            ("timeoutMs", Box::new(|p| p.timeout_ms = Some(u64::MAX))),
            ("maxOutputBytes", Box::new(|p| p.max_output_bytes = Some(10))),
            ("maxProcesses", Box::new(|p| p.max_processes = Some(0))),
            ("grantExpiresAt", Box::new(|p| p.grant_expires_at = Some(NOW))),
        ];
        for (field, mutate) in cases {
            let mut raw = input(&sb);
            mutate(&mut raw);
            let error = raw.resolve(NOW).expect_err(field);
            assert_eq!(error.field, field, "{error}");
        }
    }

    #[test]
    fn json_with_unknown_fields_or_wrong_names_is_refused() {
        assert!(
            PolicyInput::from_json(r#"{"schemaVersion":1,"id":"a","label":"A","network":"none","extra":1}"#).is_err()
        );
        assert!(PolicyInput::from_json(r#"{"schemaVersion":1,"id":"a","label":"A","network":"everything"}"#).is_err());
        assert!(PolicyInput::from_json(r#"{"schemaVersion":1,"id":"a","label":"A","network":"proxy-only"}"#).is_ok());
    }

    #[test]
    fn a_root_reached_through_a_link_keeps_the_name_it_was_given() {
        let sb = Sandbox::new("spelling");
        let link = sb.root.join("alias");
        if !make_dir_link(&sb.outside(), &link) {
            panic!("could not create a directory link, so the spelling case was not tested");
        }
        let mut raw = input(&sb);
        raw.read_roots = vec![link.clone(), sb.work()];
        let policy = raw.resolve(NOW).unwrap();
        assert!(
            policy.read_roots.contains(&sb.outside()),
            "resolved to the real directory"
        );
        assert_eq!(
            policy.root_spellings,
            vec![link],
            "only the written alias differs from the resolved path"
        );
    }

    #[test]
    fn open_network_needs_an_expiring_grant() {
        let sb = Sandbox::new("open");
        let mut raw = input(&sb);
        raw.network = NetworkMode::Open;
        assert_eq!(raw.clone().resolve(NOW).unwrap_err().field, "network");
        raw.grant_expires_at = Some(NOW + 60_000);
        assert_eq!(raw.resolve(NOW).unwrap().network, NetworkMode::Open);
    }

    #[test]
    fn grant_expiry_is_checked_at_resolve_and_later() {
        let sb = Sandbox::new("grant");
        let mut raw = input(&sb);
        raw.grant_expires_at = Some(NOW + 1_000);
        let policy = raw.resolve(NOW).unwrap();
        assert!(!policy.is_expired(NOW + 999));
        assert!(policy.is_expired(NOW + 1_000));
    }

    #[test]
    fn env_names_follow_the_platform_rule() {
        let sb = Sandbox::new("env");
        let policy = input(&sb).resolve(NOW).unwrap();
        assert!(policy.env_allowed("PATH"));
        assert_eq!(policy.env_allowed("path"), cfg!(windows));
        assert!(!policy.env_allowed("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn a_link_out_of_the_root_resolves_to_its_target() {
        let sb = Sandbox::new("link");
        let link = sb.work().join("escape");
        if !make_dir_link(&sb.outside(), &link) {
            panic!("could not create a directory link, so the symlink case was not tested");
        }
        let policy = input(&sb).resolve(NOW).unwrap();
        assert!(
            !policy.allows(&link.join("secret.txt"), Access::Read),
            "the link must resolve to its target outside the root"
        );
        assert!(!policy.allows(&link.join("secret.txt"), Access::Write));
        // A root given as a link is resolved to where it really points.
        let mut raw = input(&sb);
        raw.write_roots = vec![link];
        let policy = raw.resolve(NOW).unwrap();
        assert_eq!(policy.write_roots, vec![sb.outside()]);
    }

    #[cfg(unix)]
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    // A junction needs no privilege, unlike a directory symlink on Windows.
    #[cfg(windows)]
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|out| out.status.success())
    }
}

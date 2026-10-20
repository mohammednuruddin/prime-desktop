use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use desktop_sandbox_policy::NetworkMode;
use serde::Serialize;

use crate::fixture::Fixture;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Expect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }
}

/// How a run is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The probe program's own report: did the operation succeed?
    Op,
    /// The helper must have stopped the child at the timeout.
    Timeout,
    /// The helper must have cut the output at the cap.
    OutputCap,
    /// The operation must have succeeded and the fixture's listener must have seen the connection.
    Connection,
    /// The program must have received `args[1]` exactly: no expansion, no quoting damage.
    ArgvIntact,
    /// The program ran, and whatever it reported, the real file is unchanged. For a protected path that a backend
    /// hides behind an empty mount, a write can succeed into the mount and vanish; what matters is the real file.
    HostUnchanged,
    /// A connection to the public internet. The runner first checks that the test host itself can make it, and
    /// reports the case unavailable on an offline machine instead of failing.
    External,
}

pub struct Plan {
    pub args: Vec<String>,
    pub network: NetworkMode,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
    pub max_processes: Option<u32>,
    /// Sent in the request, so the policy must allow each name.
    pub request_env: BTreeMap<String, String>,
    /// Set on the helper's own process, to see whether it leaks into the child.
    pub host_env: Vec<(String, String)>,
    /// Paths that must not exist afterwards, whatever the operation reported.
    pub must_not_exist: Vec<PathBuf>,
    pub needs_links: bool,
    pub check: Check,
}

impl Plan {
    fn new(args: &[&str]) -> Self {
        Plan {
            args: args.iter().map(|a| a.to_string()).collect(),
            network: NetworkMode::None,
            timeout_ms: None,
            max_output_bytes: None,
            max_processes: None,
            request_env: BTreeMap::new(),
            host_env: Vec::new(),
            must_not_exist: Vec::new(),
            needs_links: false,
            check: Check::Op,
        }
    }
    fn op(op: &str, paths: &[&Path]) -> Self {
        let mut args = vec![op.to_string()];
        args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
        let mut plan = Plan::new(&[]);
        plan.args = args;
        plan
    }
}

pub struct Case {
    pub id: &'static str,
    pub category: &'static str,
    pub expect: Expect,
    /// True when even an unsandboxed run satisfies a deny case, because the helper itself holds it (the
    /// environment is built from the request alone). Every other deny case is a boundary only the OS can
    /// hold, and the control must break it: that is how the suite shows it can see an open door.
    pub control_holds: bool,
    pub description: &'static str,
    /// Empty means every platform.
    pub only_on: &'static [Os],
    /// A backend that fails this case but lists this text among its limitations is reporting a known gap, not
    /// a failure. A backend that holds the case passes it as usual.
    pub known_gap: Option<&'static str>,
    pub build: fn(&Fixture) -> Plan,
}

const WIN_MAC: &[Os] = &[Os::Windows, Os::Macos];
const WIN: &[Os] = &[Os::Windows];
const ANY: &[Os] = &[];

fn case(
    id: &'static str,
    category: &'static str,
    expect: Expect,
    control_holds: bool,
    only_on: &'static [Os],
    description: &'static str,
    build: fn(&Fixture) -> Plan,
) -> Case {
    Case {
        id,
        category,
        expect,
        control_holds,
        description,
        only_on,
        known_gap: None,
        build,
    }
}

pub const ALLOWED_VAR: &str = "PRIME_DESKTOP_PROBE_ALLOWED";
pub const SECRET_VAR: &str = "PRIME_DESKTOP_PROBE_SECRET";
/// Cloudflare's public resolver. Only a TCP connection is made: nothing is sent.
pub const EXTERNAL_PROBE_ADDR: &str = "1.1.1.1:443";

pub fn cases() -> Vec<Case> {
    use Expect::{Allow, Deny};
    vec![
        // Files: the boundary of the roots.
        case(
            "fs.workspace.read.allowed",
            "filesystem",
            Allow,
            false,
            ANY,
            "Reading inside a root works.",
            |f| Plan::op("read", &[&f.ws.join("src").join("ok.txt")]),
        ),
        case(
            "fs.outside-root.read.denied",
            "filesystem",
            Deny,
            false,
            ANY,
            "Reading outside every root is refused.",
            |f| Plan::op("read", &[&f.outside.join("secret.txt")]),
        ),
        case(
            "fs.workspace.write.allowed",
            "filesystem",
            Allow,
            false,
            ANY,
            "Writing inside a write root works.",
            |f| Plan::op("write", &[&f.ws.join("src").join("new.txt")]),
        ),
        case(
            "fs.outside-root.write.denied",
            "filesystem",
            Deny,
            false,
            ANY,
            "Writing outside every root is refused.",
            |f| {
                let target = f.outside.join("new.txt");
                let mut plan = Plan::op("write", &[&target]);
                plan.must_not_exist.push(target);
                plan
            },
        ),
        case(
            "fs.rename-out.denied",
            "filesystem",
            Deny,
            false,
            ANY,
            "Moving a file out of the root is refused.",
            |f| {
                let target = f.outside.join("moved.txt");
                let mut plan = Plan::op("rename", &[&f.ws.join("src").join("ok.txt"), &target]);
                plan.must_not_exist.push(target);
                plan
            },
        ),
        case(
            "fs.hardlink-out.denied",
            "filesystem",
            Deny,
            false,
            ANY,
            "Hard-linking an outside file into the root is refused.",
            |f| {
                let target = f.ws.join("src").join("hard.txt");
                let mut plan = Plan::op("hardlink", &[&f.outside.join("secret.txt"), &target]);
                plan.must_not_exist.push(target);
                plan
            },
        ),
        // Protected names, at the top and nested.
        case(
            "fs.carve-out.git-read.denied",
            "protected-names",
            Deny,
            false,
            ANY,
            "Reading .git inside the root is refused.",
            |f| Plan::op("read", &[&f.ws.join(".git").join("config")]),
        ),
        case(
            "fs.carve-out.git-write.denied",
            "protected-names",
            Deny,
            false,
            ANY,
            "Writing a git hook is refused.",
            |f| {
                let target = f.ws.join(".git").join("hooks").join("pre-commit");
                let mut plan = Plan::op("write", &[&target]);
                plan.must_not_exist.push(target);
                plan.check = Check::HostUnchanged;
                plan
            },
        ),
        case(
            "fs.carve-out.nested-git-write.denied",
            "protected-names",
            Deny,
            false,
            ANY,
            "Writing inside a nested repository's .git is refused.",
            |f| {
                let target = f.ws.join("sub").join(".git").join("config");
                let mut plan = Plan::op("write", &[&target]);
                plan.must_not_exist.push(target);
                plan.check = Check::HostUnchanged;
                plan
            },
        ),
        case(
            "fs.carve-out.nested-env-read.denied",
            "protected-names",
            Deny,
            false,
            ANY,
            "Reading a nested .env file is refused.",
            |f| Plan::op("read", &[&f.ws.join("app").join(".env")]),
        ),
        case(
            "fs.carve-out.case-variant.denied",
            "protected-names",
            Deny,
            false,
            WIN_MAC,
            "A case variant of a protected name is the same denial.",
            |f| Plan::op("read", &[&f.ws.join(".GIT").join("config")]),
        ),
        case(
            "fs.carve-out.sibling-name.allowed",
            "protected-names",
            Allow,
            false,
            ANY,
            "A name that only starts like a protected one stays usable.",
            |f| Plan::op("write", &[&f.ws.join("src").join(".gitignore")]),
        ),
        // Path-based enforcement cannot see a name that does not exist yet. A backend that cannot hold this lists the
        // limitation, and the suite then reports a known gap instead of a failure.
        Case {
            known_gap: Some("not where it creates them later"),
            ..case(
                "fs.carve-out.created-later.denied",
                "protected-names",
                Deny,
                false,
                ANY,
                "A .git created after the command started is still protected.",
                |f| {
                    let target = f.ws.join("fresh").join(".git").join("config");
                    let mut plan = Plan::op("mkgit", &[&f.ws.join("fresh")]);
                    plan.must_not_exist.push(target);
                    plan
                },
            )
        },
        // Links.
        case(
            "fs.symlink.read-escape.denied",
            "links",
            Deny,
            false,
            ANY,
            "Reading through a link that leaves the root is refused.",
            |f| {
                let mut plan = Plan::op("read", &[&f.ws.join("link_out").join("secret.txt")]);
                plan.needs_links = true;
                plan
            },
        ),
        case(
            "fs.symlink.write-escape.denied",
            "links",
            Deny,
            false,
            ANY,
            "Writing through a link that leaves the root is refused.",
            |f| {
                let mut plan = Plan::op("write", &[&f.ws.join("link_out").join("new.txt")]);
                plan.must_not_exist.push(f.outside.join("new.txt"));
                plan.needs_links = true;
                plan
            },
        ),
        // Windows-only ways to name a file.
        case(
            "fs.windows.ads-on-protected.denied",
            "windows-names",
            Deny,
            false,
            WIN,
            "An alternate data stream of a protected file is refused.",
            |f| {
                let target = format!("{}:evil", f.ws.join(".git").join("config").display());
                Plan::new(&["write", &target])
            },
        ),
        case(
            "fs.windows.verbatim-prefix.denied",
            "windows-names",
            Deny,
            false,
            WIN,
            "The \\\\?\\ form of an outside path is refused.",
            |f| {
                let target = format!(r"\\?\{}", f.outside.join("secret.txt").display());
                Plan::new(&["read", &target])
            },
        ),
        // Processes.
        case(
            "proc.child.allowed-binary.allowed",
            "processes",
            Allow,
            false,
            ANY,
            "A program in a read root can be started.",
            |f| Plan::op("exec", &[&f.agent]).with_extra(&["env", ALLOWED_VAR]),
        ),
        case(
            "proc.child.denied-binary.denied",
            "processes",
            Deny,
            false,
            ANY,
            "A program outside every root cannot be started.",
            |f| Plan::op("exec", &[&f.outside_agent]).with_extra(&["env", ALLOWED_VAR]),
        ),
        case(
            "proc.grandchild.workspace-write.allowed",
            "processes",
            Allow,
            false,
            ANY,
            "A grandchild can do what its parent may.",
            |f| {
                Plan::op("via-child", &[])
                    .with_extra(&["write"])
                    .with_path(&f.ws.join("src").join("grand.txt"))
            },
        ),
        case(
            "proc.grandchild.escape.denied",
            "processes",
            Deny,
            false,
            ANY,
            "A grandchild is held to the same policy.",
            |f| {
                let target = f.outside.join("grand.txt");
                let mut plan = Plan::op("via-child", &[]).with_extra(&["write"]).with_path(&target);
                plan.must_not_exist.push(target);
                plan
            },
        ),
        case(
            "proc.shell-metacharacters.inert",
            "processes",
            Allow,
            false,
            ANY,
            "Shell syntax in an argument stays text.",
            |f| {
                // Characters that are legal in a file name on every platform. If anything re-parsed the argument as a
                // command, a `pwned` file would appear in the working directory or its parent.
                let target = f.ws.join("src").join("a; touch pwned && echo $(id) `id`");
                let mut plan = Plan::op("write", &[&target]);
                plan.must_not_exist = vec![
                    f.ws.join("pwned"),
                    f.ws.join("src").join("pwned"),
                    f.outside.join("pwned"),
                ];
                plan
            },
        ),
        case(
            "proc.argv.arrives-intact",
            "processes",
            Allow,
            false,
            ANY,
            "Quotes, spaces, backslashes and % arrive unchanged.",
            |_| {
                // The shapes that Windows command-line parsing and cmd.exe each treat specially.
                let mut plan = Plan::new(&["arg", r#"a b "c d" \\e\\ \"f %SystemRoot% %%x ^& $HOME ~ * ?"#]);
                plan.check = Check::ArgvIntact;
                plan
            },
        ),
        // The two cases bracket the limit exactly. With a limit of 6 the tree is the probe program plus five
        // children, so five more may start and a sixth may not.
        case(
            "proc.process-limit.within.allowed",
            "processes",
            Allow,
            false,
            ANY,
            "A tree up to the policy's process limit can start.",
            |_| {
                let mut plan = Plan::new(&["fanout", "5"]);
                plan.max_processes = Some(6);
                plan
            },
        ),
        Case {
            known_gap: Some("is not limited on this platform"),
            ..case(
                "proc.process-limit.enforced",
                "processes",
                Deny,
                false,
                ANY,
                "One process past the limit cannot start.",
                |_| {
                    let mut plan = Plan::new(&["fanout", "6"]);
                    plan.max_processes = Some(6);
                    plan
                },
            )
        },
        // Environment.
        case(
            "env.allowlisted-var.passthrough",
            "environment",
            Allow,
            false,
            ANY,
            "A variable the policy allows reaches the child.",
            |_| {
                let mut plan = Plan::new(&["env", ALLOWED_VAR]);
                plan.request_env.insert(ALLOWED_VAR.into(), "yes".into());
                plan
            },
        ),
        case(
            "env.disallowed-var.scrubbed",
            "environment",
            Deny,
            true,
            ANY,
            "The helper's own secrets never reach the child.",
            |_| {
                let mut plan = Plan::new(&["env", SECRET_VAR]);
                plan.host_env.push((SECRET_VAR.into(), "leak".into()));
                plan
            },
        ),
        // Network.
        case(
            "net.dns.blocked",
            "network",
            Deny,
            false,
            ANY,
            "Name resolution is refused when the network is off.",
            |_| Plan::new(&["resolve", "example.com"]),
        ),
        case(
            "net.egress.blocked",
            "network",
            Deny,
            false,
            ANY,
            "A connection is refused when the network is off.",
            |f| {
                let mut plan = Plan::new(&["connect", &f.listener_addr().to_string()]);
                plan.check = Check::Connection;
                plan
            },
        ),
        case(
            "net.proxy-only.direct-egress.denied",
            "network",
            Deny,
            false,
            ANY,
            "A direct connection is refused in proxy-only mode.",
            |f| {
                let mut plan = Plan::new(&["connect", &f.listener_addr().to_string()]);
                plan.network = NetworkMode::ProxyOnly;
                plan.check = Check::Connection;
                plan
            },
        ),
        case(
            "net.open.allowed",
            "network",
            Allow,
            false,
            ANY,
            "With an explicit open-network grant, an outbound connection to the internet goes through.",
            |_| {
                let mut plan = Plan::new(&["connect", EXTERNAL_PROBE_ADDR]);
                plan.network = NetworkMode::Open;
                plan.check = Check::External;
                plan
            },
        ),
        case(
            "net.open.host-loopback.denied",
            "network",
            Deny,
            false,
            ANY,
            "Open means the internet, not services on this machine: the host's loopback stays closed.",
            |f| {
                let mut plan = Plan::new(&["connect", &f.listener_addr().to_string()]);
                plan.network = NetworkMode::Open;
                plan.check = Check::Connection;
                plan
            },
        ),
        // Limits.
        case(
            "limits.timeout.enforced",
            "limits",
            Deny,
            false,
            ANY,
            "A child that runs past the timeout is stopped.",
            |_| {
                let mut plan = Plan::new(&["spin"]);
                plan.timeout_ms = Some(1_000);
                plan.check = Check::Timeout;
                plan
            },
        ),
        case(
            "limits.output-cap.enforced",
            "limits",
            Deny,
            false,
            ANY,
            "Output past the cap is cut, not delivered whole.",
            |_| {
                let mut plan = Plan::new(&["flood", "1000000"]);
                plan.max_output_bytes = Some(4_096);
                plan.check = Check::OutputCap;
                plan
            },
        ),
    ]
}

impl Plan {
    fn with_extra(mut self, more: &[&str]) -> Self {
        self.args.extend(more.iter().map(|a| a.to_string()));
        self
    }
    fn with_path(mut self, path: &Path) -> Self {
        self.args.push(path.to_string_lossy().into_owned());
        self
    }
}

/// A short fingerprint of what the suite asks: every case id, its expectation and its known gap. Two runs with the
/// same fingerprint tested the same things, so an attestation can say which suite it passed.
pub fn fingerprint() -> String {
    // FNV-1a over a stable text form. Not a security hash: it only has to change when the suite does.
    let mut hash: u64 = 0xcbf29ce484222325;
    for case in cases() {
        let line = format!("{}|{:?}|{}\n", case.id, case.expect, case.known_gap.unwrap_or(""));
        for byte in line.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

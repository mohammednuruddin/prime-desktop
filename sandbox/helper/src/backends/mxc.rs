//! Microsoft Execution Containers (`mxc-sdk`): the OS-level backend.
//!
//! One backend, three kernels: a process container on Windows 11, bubblewrap on Linux and Seatbelt on macOS. The
//! policy, the checks and the result are shared; only what the kernel can hold differs, and each difference is
//! either handled below or listed in the result's `limitations`.

use desktop_sandbox_policy::{NetworkMode, ResolvedPolicy, ResolvedRequest};

use crate::backend::Backend;

pub struct Mxc;

const PROTECTED_WALK_DEPTH: usize = 8;
const PROTECTED_WALK_ENTRIES: usize = 100_000;

/// Text the probe suite matches to tell a known gap from a failure. Keep it in step with
/// `fs.carve-out.created-later.denied` and `proc.process-limit.enforced` in the catalog.
const LIMIT_CREATED_LATER: &str = "protected names such as .git and .env are denied where they exist when the command starts, not where it creates them later";
#[cfg_attr(windows, allow(dead_code))]
const LIMIT_PROCESSES: &str =
    "the number of processes the command may start is not limited on this platform, so a fork bomb is not stopped";

/// Existing entries named like a protected name, at any depth under the roots. MXC denies by path, so a
/// protected name is held only where it exists when the command starts. Links are not followed.
pub fn find_protected(roots: &[std::path::PathBuf], names: &[String]) -> (Vec<std::path::PathBuf>, bool) {
    let (mut found, mut visited, mut cut_short) = (Vec::new(), 0usize, false);
    let mut stack: Vec<(std::path::PathBuf, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            visited += 1;
            if visited > PROTECTED_WALK_ENTRIES {
                return (found, true);
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let Ok(kind) = entry.file_type() else { continue };
            if names.contains(&name) {
                found.push(entry.path());
            } else if kind.is_dir() && !kind.is_symlink() {
                if depth + 1 > PROTECTED_WALK_DEPTH {
                    cut_short = true;
                } else {
                    stack.push((entry.path(), depth + 1));
                }
            }
        }
    }
    (found, cut_short)
}

/// Processes outside the child tree that the job also counts: this helper. MXC adds none. The two
/// `proc.process-limit` probes fail if this drifts.
#[cfg(windows)]
const PROCESS_OVERHEAD: u32 = 1;

/// Puts this helper into a job that caps how many processes may exist and kills all of them when the helper
/// goes away, so a crashed host or an abandoned run cannot leave a tree behind. MXC has no process limit of its
/// own, and its containers are nested inside this job (nested jobs need Windows 8 or later). The handle is left
/// open on purpose: it closes when this process exits, and closing it is what kills the tree.
#[cfg(windows)]
fn limit_process_tree(max_processes: u32) -> Result<(), String> {
    use std::mem::{size_of, zeroed};
    use std::ptr::null;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: plain Win32 calls with a zeroed, correctly sized struct; each result is checked.
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.BasicLimitInformation.ActiveProcessLimit = max_processes + PROCESS_OVERHEAD;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if set == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}

/// The name MXC gives the backend this OS uses, and why it might be missing.
fn expected_backend() -> Option<(&'static str, &'static str)> {
    if cfg!(windows) {
        Some((
            "processcontainer",
            "MXC process containers are not available on this host (needs Windows 11 build 26100.9278 or later)",
        ))
    } else if cfg!(target_os = "linux") {
        Some((
            "bubblewrap",
            "bubblewrap is not usable on this host (install bubblewrap 0.5.0 or later and enable unprivileged user namespaces)",
        ))
    } else if cfg!(target_os = "macos") {
        Some((
            "seatbelt",
            "Seatbelt is not available on this host (needs macOS 15 or later)",
        ))
    } else {
        None
    }
}

/// The MXC request for `policy` and `request`, plus the limitations that apply. `bounded` is false for `exec`, where
/// the host owns the program's lifetime and the policy's timeout does not apply. A grant's expiry applies either way:
/// a program that outlives the grant that allowed it is stopped when the grant ends.
fn build(
    policy: &ResolvedPolicy,
    request: &ResolvedRequest,
    limitations: Vec<String>,
    bounded: bool,
) -> Result<(mxc_sdk::v1::ContainerRequest, Vec<String>), Box<desktop_sandbox_policy::ExecutionResult>> {
    use mxc_sdk::v1::{
        ContainerRequest, Containment, FilesystemPolicy, NetworkAction, NetworkEgressPolicy, NetworkIngressPolicy,
        NetworkPolicy,
    };

    use crate::exec::{now_ms, plain_path};

    let started_at = now_ms();
    let paths = |list: &[std::path::PathBuf]| -> Vec<String> { list.iter().map(|p| plain_path(p)).collect() };

    // Denied: what the host listed, plus protected names that exist now at any depth. A path that does not
    // exist cannot be denied by MXC, so it is left out here and named in the limitations.
    let roots: Vec<_> = policy.read_roots.iter().chain(&policy.write_roots).cloned().collect();
    let (nested, cut_short) = find_protected(&roots, &policy.protected_names);
    let (existing, missing): (Vec<_>, Vec<_>) = policy.denied_paths.iter().cloned().partition(|p| p.exists());
    let mut denied: Vec<std::path::PathBuf> = existing;
    for path in nested {
        if !denied.contains(&path) {
            denied.push(path);
        }
    }
    let mut limitations = limitations;
    if let Some(first) = missing.first() {
        limitations.push(format!(
            "{} denied path(s) do not exist yet (for example {}) and are not denied until they do",
            missing.len(),
            plain_path(first)
        ));
    }
    if cut_short {
        limitations.push(format!("protected names deeper than {PROTECTED_WALK_DEPTH} levels, or past {PROTECTED_WALK_ENTRIES} entries, were not searched for"));
    }

    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut env: Vec<(String, String)> = request.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    #[cfg(windows)]
    {
        // MXC refuses to start without these two variables. Only their presence matters, so give each a harmless value.
        let mut ensure = |name: &str, value: String| {
            if !env.iter().any(|(k, _)| k.eq_ignore_ascii_case(name)) {
                env.push((name.to_string(), value));
            }
        };
        ensure(
            "SystemRoot",
            std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()),
        );
        ensure("LOCALAPPDATA", plain_path(&request.cwd));
    }

    #[cfg(windows)]
    let command = crate::cmdline::windows(&request.argv);
    #[cfg(not(windows))]
    let command = crate::cmdline::posix(&request.argv);
    let mut container = ContainerRequest::new(command);
    container.working_directory = Some(plain_path(&request.cwd));
    // A grant that expires in ten seconds must not run a command for two minutes.
    let remaining = policy
        .grant_expires_at
        .map(|expires| std::time::Duration::from_millis(expires.saturating_sub(started_at)));
    let timeout = remaining.map_or(policy.timeout, |left| left.min(policy.timeout));
    if timeout.is_zero() {
        return Err(Box::new(crate::backend::unavailable(
            "mxc",
            "the grant has expired".into(),
        )));
    }
    let limit = if bounded { Some(timeout) } else { remaining };
    container.timeout_ms = limit.map(|limit| limit.as_millis().min(u32::MAX as u128) as u32);
    container.containment = if cfg!(windows) {
        #[cfg(windows)]
        {
            Containment::ProcessContainer(mxc_sdk::v1::configs::ProcessContainerConfig::default())
        }
        #[cfg(not(windows))]
        {
            Containment::Process
        }
    } else if cfg!(target_os = "macos") {
        #[cfg(target_os = "macos")]
        {
            Containment::Seatbelt(mxc_sdk::v1::configs::SeatbeltConfig::default())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Containment::Process
        }
    } else {
        Containment::Bubblewrap
    };
    // Bubblewrap builds a private view of the file system from bind mounts, so a folder the host reached through a
    // symlink name has to exist under that name too, or a link that points through it dangles inside.
    let mut readonly = paths(&policy.read_roots);
    if cfg!(target_os = "linux") {
        for spelling in paths(&policy.root_spellings) {
            if !readonly.contains(&spelling) {
                readonly.push(spelling);
            }
        }
    }
    container.filesystem = Some(FilesystemPolicy {
        readonly_paths: readonly,
        readwrite_paths: paths(&policy.write_roots),
        denied_paths: paths(&denied),
        clear_policy_on_exit: None,
    });
    container.network = Some(NetworkPolicy {
        egress: Some(NetworkEgressPolicy {
            default: Some(if policy.network == NetworkMode::Open {
                NetworkAction::Allow
            } else {
                NetworkAction::Deny
            }),
            allow: None,
            deny: None,
        }),
        ingress: Some(NetworkIngressPolicy {
            default: Some(NetworkAction::Deny),
            host_loopback: Some(NetworkAction::Deny),
        }),
        runtime_config: None,
    });
    #[cfg(windows)]
    {
        use mxc_sdk::v1::{ClipboardPolicy, UiPolicy};
        container.ui = Some(UiPolicy {
            disable: !policy.allow_gui,
            clipboard: ClipboardPolicy::None,
            allow_input_injection: false,
        });
    }
    container.environment = Some(env);
    container.inherit_default_environment = Some(false);

    Ok((container, limitations))
}

impl Backend for Mxc {
    fn name(&self) -> &'static str {
        "mxc"
    }

    fn host(&self) -> Result<(), String> {
        use mxc_sdk::v1::{BackendCapability, available_backends, platform_support};
        let Some((wanted, missing)) = expected_backend() else {
            return Err("this operating system has no sandbox backend".into());
        };
        let support = platform_support();
        if !support.is_supported {
            return Err(support
                .reason
                .unwrap_or_else(|| "MXC reports this host as unsupported".into()));
        }
        let Some(backend) = available_backends().into_iter().find(|b| b.backend == wanted) else {
            return Err(missing.into());
        };
        // Windows reports denied-path support because it depends on the OS build. Bubblewrap and Seatbelt always
        // have it and do not list it; the probe suite is what shows they hold it.
        if cfg!(windows) && !backend.capabilities.contains(&BackendCapability::FilesystemDeniedPaths) {
            return Err(format!(
                "the {wanted} backend on this host cannot deny paths inside a granted folder, which the policy relies on"
            ));
        }
        Ok(())
    }

    fn check(&self, policy: &ResolvedPolicy) -> Result<Vec<String>, String> {
        self.host()?;
        if !matches!(policy.network, NetworkMode::None | NetworkMode::Open) {
            return Err(format!(
                "network mode {:?} needs proxy or address enforcement, which this backend does not report here; only `none` and `open` can be held",
                policy.network
            ));
        }
        if policy.allow_pty {
            return Err(
                "a terminal inside the sandbox is not wired yet, so a policy that allows one cannot run".into(),
            );
        }
        let mut limitations = vec![LIMIT_CREATED_LATER.to_string()];
        if cfg!(windows) {
            limitations.push("processes that call GetFinalPathNameByHandle for a drive-letter path fail inside the container: native git and Node's realpath.native (a Windows limitation)".into());
            if policy.allow_gui {
                limitations.push("GUI access is on, which widens the kernel surface the child can reach".into());
            }
        } else {
            limitations.push(LIMIT_PROCESSES.into());
        }
        Ok(limitations)
    }

    fn run(
        &self,
        policy: &ResolvedPolicy,
        request: &ResolvedRequest,
        limitations: Vec<String>,
    ) -> desktop_sandbox_policy::ExecutionResult {
        use std::io::Read;

        use desktop_sandbox_policy::{Enforcement, ExecutionResult};
        use mxc_sdk::v1::{self, SpawnOptions, WaitResult};

        use crate::exec::{now_ms, read_capped, text};

        let started_at = now_ms();
        let (container, limitations) = match build(policy, request, limitations, true) {
            Ok(built) => built,
            Err(unavailable) => return *unavailable,
        };

        // From here every process this helper starts, and its descendants, counts against the limit.
        #[cfg(windows)]
        if let Err(e) = limit_process_tree(policy.max_processes) {
            return crate::backend::unavailable("mxc", format!("could not apply the process limit: {e}"));
        }
        let mut process = match v1::spawn(container, SpawnOptions::default()) {
            Ok(process) => process,
            Err(e) => {
                return crate::backend::unavailable(
                    "mxc",
                    format!("MXC could not start the process ({:?}): {}", e.code, e.message),
                );
            }
        };
        let out = process
            .take_stdout()
            .map(|s| read_capped(s as Box<dyn Read + Send>, policy.max_output_bytes));
        let err = process
            .take_stderr()
            .map(|s| read_capped(s as Box<dyn Read + Send>, policy.max_output_bytes));
        let outcome = process.wait();
        let (stdout, stdout_truncated) = out.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        let (stderr, stderr_truncated) = err.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        let (exit_code, timed_out) = match outcome {
            Ok(WaitResult::Exited(code)) => (Some(code), false),
            Ok(WaitResult::TimedOut) => (None, true),
            Err(_) => (None, false),
        };
        ExecutionResult {
            exit_code,
            timed_out,
            signal: None,
            backend: Some("mxc".into()),
            enforcement: Enforcement::Enforced,
            denials: Vec::new(),
            limitations,
            stdout: text(stdout),
            stderr: text(stderr),
            stdout_truncated,
            stderr_truncated,
            started_at,
            finished_at: Some(now_ms()),
        }
    }

    fn exec(&self, policy: &ResolvedPolicy, request: &ResolvedRequest) -> Result<i32, String> {
        use std::io::{Read, Write};

        use mxc_sdk::v1::{self, SpawnOptions, WaitResult};

        let (container, _limitations) = build(policy, request, Vec::new(), false).map_err(|result| {
            result
                .denials
                .first()
                .map(|d| d.message.clone())
                .unwrap_or_else(|| "the backend could not run this".into())
        })?;
        #[cfg(windows)]
        limit_process_tree(policy.max_processes).map_err(|e| format!("could not apply the process limit: {e}"))?;
        let mut process = v1::spawn(container, SpawnOptions::default())
            .map_err(|e| format!("MXC could not start the process ({:?}): {}", e.code, e.message))?;

        // Bridge the three streams. Standard input is copied on its own thread: it ends when the host closes it, and
        // the program's own exit is what ends everything else.
        let mut pumps = Vec::new();
        if let Some(mut child_in) = process.take_stdin() {
            std::thread::spawn(move || {
                let mut buffer = [0u8; 16 * 1024];
                let mut stdin = std::io::stdin().lock();
                while let Ok(n) = stdin.read(&mut buffer) {
                    if n == 0 || child_in.write_all(&buffer[..n]).and_then(|_| child_in.flush()).is_err() {
                        break;
                    }
                }
            });
        }
        let mut copy_out = |reader: Box<dyn Read + Send>, to_stderr: bool| {
            pumps.push(std::thread::spawn(move || {
                let mut reader = reader;
                let mut buffer = [0u8; 16 * 1024];
                while let Ok(n) = reader.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    let wrote = if to_stderr {
                        let mut e = std::io::stderr().lock();
                        e.write_all(&buffer[..n]).and_then(|_| e.flush())
                    } else {
                        let mut o = std::io::stdout().lock();
                        o.write_all(&buffer[..n]).and_then(|_| o.flush())
                    };
                    if wrote.is_err() {
                        break;
                    }
                }
            }));
        };
        if let Some(out) = process.take_stdout() {
            copy_out(out as Box<dyn Read + Send>, false);
        }
        if let Some(err) = process.take_stderr() {
            copy_out(err as Box<dyn Read + Send>, true);
        }
        let outcome = process.wait();
        for pump in pumps {
            let _ = pump.join();
        }
        match outcome {
            Ok(WaitResult::Exited(code)) => Ok(code),
            Ok(WaitResult::TimedOut) => Err("the program was stopped because its grant ended".into()),
            Err(e) => Err(format!("waiting for the program failed: {e}")),
        }
    }
}

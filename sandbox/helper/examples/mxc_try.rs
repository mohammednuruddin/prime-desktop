//! Exploration only: run one command under a ProcessContainer with the given paths and print what happened.
//! usage: mxc_try <cwd> <ro,ro,...> <rw,rw,...> <deny,deny,...> -- <command line>
use mxc_sdk::v1::configs::ProcessContainerConfig;
use mxc_sdk::v1::{self, ContainerRequest, Containment, FilesystemPolicy, WaitResult};

fn list(s: &str) -> Vec<String> {
    s.split(',').filter(|p| !p.is_empty()).map(String::from).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dash = args.iter().position(|a| a == "--").expect("-- before the command");
    let command = args[dash + 1..].join(" ");
    let mut request = ContainerRequest::new(command);
    request.working_directory = Some(args[0].clone());
    request.timeout_ms = Some(20_000);
    request.containment = Containment::ProcessContainer(ProcessContainerConfig::default());
    request.filesystem = Some(FilesystemPolicy {
        readonly_paths: list(&args[1]),
        readwrite_paths: list(&args[2]),
        denied_paths: list(&args[3]),
        clear_policy_on_exit: None,
    });
    request.environment = Some(vec![
        ("SystemRoot".into(), std::env::var("SystemRoot").unwrap_or_default()),
        ("LOCALAPPDATA".into(), args[0].clone()), // presence-only requirement; a harmless value
    ]);
    request.inherit_default_environment = Some(false);
    if std::env::var("MXC_UI").is_ok() {
        request.ui = Some(mxc_sdk::v1::UiPolicy {
            disable: false,
            ..Default::default()
        });
    }
    let started = std::time::Instant::now();
    match v1::run(request, Default::default()) {
        Ok(out) => {
            println!(
                "outcome={:?} in {:?}",
                out.outcome == WaitResult::TimedOut,
                started.elapsed()
            );
            println!("outcome_full={:?}", out.outcome);
            println!("warnings={:?}", out.warnings);
            println!("stdout={}", String::from_utf8_lossy(&out.stdout));
            println!("stderr={}", String::from_utf8_lossy(&out.stderr));
        }
        Err(e) => println!(
            "ERROR code={:?} op={:?} native={:?} remediation={:?}\n{}",
            e.code, e.operation, e.native_code, e.remediation, e.message
        ),
    }
}

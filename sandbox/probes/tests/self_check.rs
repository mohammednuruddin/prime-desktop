//! The suite must be able to fail. With no sandbox, every deny case that the OS is meant to hold has to
//! fail, otherwise that probe would also "pass" against a sandbox that does nothing.

use std::path::PathBuf;

#[test]
fn probes_fail_without_a_sandbox() {
    let agent = PathBuf::from(env!("CARGO_BIN_EXE_probe-agent"));
    let problems = desktop_sandbox_probes::runner::self_check(&agent);
    assert!(
        problems.is_empty(),
        "vacuous or broken probes:\n{}",
        problems.join("\n")
    );
}

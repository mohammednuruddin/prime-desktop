//! The sandbox wiring end to end: build the kernel environment, prepare the protected mode with the real helper, and see
//! that the state is `enforced` and the kernel starts inside. Slow, and needs uv, the network, a prime-agent release
//! extracted at `PRIME_DESKTOP_TEST_AGENT_DIR` and the built helper at `PRIME_DESKTOP_TEST_HELPER` (with `desktop-sandbox-probes` beside
//! it). Without them the test says so and passes.

use std::path::{Path, PathBuf};

use desktop_core::kernel_env::{self, KernelEnv};
use desktop_core::sandbox::{Mode, prepare};

#[test]
fn protected_mode_is_enforced_and_the_kernel_starts_inside() {
    let (Some(agent_dir), Some(helper)) = (
        std::env::var_os("PRIME_DESKTOP_TEST_AGENT_DIR"),
        std::env::var_os("PRIME_DESKTOP_TEST_HELPER"),
    ) else {
        eprintln!("SKIPPED: set PRIME_DESKTOP_TEST_AGENT_DIR and PRIME_DESKTOP_TEST_HELPER");
        return;
    };
    let Some(uv) = kernel_env::find_uv() else {
        eprintln!("SKIPPED: no uv");
        return;
    };
    let (agent_dir, helper) = (PathBuf::from(agent_dir), PathBuf::from(helper));
    let scratch = tempfile::tempdir().unwrap();
    let venv = scratch.path().join("venv");
    let env: KernelEnv = kernel_env::provision(&uv, &venv, &agent_dir.join("prime-agent-runtime"), &|step| {
        eprintln!("step: {step}")
    })
    .expect("the environment builds");

    // The agent's Python skills import, as the agent checks before it will offer them.
    let imports = std::process::Command::new(&env.python)
        .args([
            "-c",
            "import edit, goal, compact, refine, agent_message, attach_image, websearch",
        ])
        .output()
        .unwrap();
    assert!(
        imports.status.success(),
        "the skills are installed in the kernel environment: {}",
        String::from_utf8_lossy(&imports.stderr)
    );

    let sandbox_dir = scratch.path().join("sandbox");
    let prepared = prepare(Mode::Protected, &helper, &sandbox_dir, &env, &|step| {
        eprintln!("sandbox: {step}")
    });
    eprintln!("state: {:?}", prepared.state);
    assert_eq!(prepared.state.enforcement, "enforced", "{:?}", prepared.state.reason);
    assert_eq!(prepared.state.backend.as_deref(), Some("mxc"));
    assert!(
        !prepared.state.limitations.is_empty(),
        "the helper reports what it cannot hold"
    );

    // A second preparation finds the attestation and the wrapper already in place.
    let again = prepare(Mode::Protected, &helper, &sandbox_dir, &env, &|_| {});
    assert_eq!(again.state.enforcement, "enforced");

    // The kernel's own interpreter, through the wrapper, sees nothing of the user's home folder but the empty folders that
    // lead to the interpreter itself.
    let wrapper = Path::new(
        &prepared
            .env
            .iter()
            .find(|(key, _)| key == "PRIME_AGENT_KERNEL_PYTHON")
            .unwrap()
            .1,
    )
    .to_path_buf();
    let home = desktop_core::platform::home_dir();
    let work = sandbox_dir.join("work");
    let mut command = std::process::Command::new(&wrapper);
    command
        .args(["-c", &format!("import os, json\ntry:\n    print('LISTED', json.dumps(sorted(os.listdir({home:?}))))\nexcept Exception as e:\n    print('BLOCKED', type(e).__name__)")])
        .current_dir(&work)
        .envs(prepared.env.iter().map(|(key, value)| (key, value)))
        .env("PRIME_AGENT_SESSION_DIR", &work);
    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    if let Some(listed) = text.lines().find_map(|line| line.strip_prefix("LISTED ")) {
        let names: Vec<String> = serde_json::from_str(listed).unwrap();
        let on_the_way: Vec<String> = desktop_core::sandbox::interpreter_roots(&env)
            .iter()
            .filter_map(|root| root.strip_prefix(&home).ok())
            .filter_map(|rest| rest.components().next())
            .map(|first| first.as_os_str().to_string_lossy().into_owned())
            .collect();
        let real: Vec<String> = std::fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|name| on_the_way.contains(name)),
            "inside the sandbox the home folder shows {names:?}, more than the way to the interpreter {on_the_way:?}"
        );
        assert!(
            real.len() > names.len() || real.iter().all(|name| on_the_way.contains(name)),
            "the real home folder has more in it than the sandbox shows"
        );
    } else {
        assert!(
            text.contains("BLOCKED"),
            "{text} {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

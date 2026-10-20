//! `desktop-sandbox exec`: a program's input, output, exit code and environment through a real sandbox. Unix only for
//! now. It needs a host that can hold the policy, and says so when it cannot instead of passing.

#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_desktop-sandbox"))
}

fn host_can_sandbox() -> bool {
    let out = Command::new(helper())
        .arg("capabilities")
        .output()
        .expect("the helper runs");
    let text = String::from_utf8_lossy(&out.stdout);
    // The report is a list of objects with sorted keys, so `available` comes before `backend` inside one entry.
    let Some(at) = text.find("\"backend\": \"mxc\"") else {
        return false;
    };
    let entry = text[..at].rfind('{').unwrap_or(0);
    text[entry..at].contains("\"available\": true")
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("desktop-exec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

#[test]
fn input_output_exit_code_and_environment_cross_the_sandbox() {
    if !host_can_sandbox() {
        eprintln!("SKIPPED: this host cannot hold the policy, so exec was not exercised");
        return;
    }
    let work = temp("work");
    let policy = work.join("policy.json");
    std::fs::write(
        &policy,
        r#"{"schemaVersion":1,"id":"exec-test","label":"Exec test","network":"none","envAllowlist":["PRIME_DESKTOP_ALLOWED"]}"#,
    )
    .unwrap();
    let mut child = Command::new(helper())
        .args(["exec", "--policy"])
        .arg(&policy)
        .args(["--write-cwd", "--backend", "mxc", "--", "/bin/sh", "-c"])
        .arg("read line; echo \"got:$line\"; echo oops >&2; env | grep -c PRIME_DESKTOP_; echo \"allowed=$PRIME_DESKTOP_ALLOWED secret=${PRIME_DESKTOP_SECRET:-none}\"; exit 7")
        .current_dir(&work)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PRIME_DESKTOP_ALLOWED", "yes")
        .env("PRIME_DESKTOP_SECRET", "must-not-leak")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the helper starts");
    child.stdin.take().unwrap().write_all(b"hello\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        out.status.code(),
        Some(7),
        "the program's exit code comes back; stderr: {stderr}"
    );
    assert!(
        stdout.contains("got:hello"),
        "input reaches the program and output comes back: {stdout}"
    );
    assert!(stderr.contains("oops"), "the error stream is bridged too: {stderr}");
    assert!(
        stdout.contains("allowed=yes secret=none"),
        "the allowlist passes one variable and drops the other: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_policy_the_host_cannot_hold_starts_nothing() {
    let work = temp("refuse");
    let policy = work.join("policy.json");
    // Proxy-only networking is not held by any backend here, so the helper must refuse rather than run unconfined.
    std::fs::write(
        &policy,
        r#"{"schemaVersion":1,"id":"refuse","label":"Refuse","network":"proxy-only"}"#,
    )
    .unwrap();
    let marker = work.join("ran");
    let out = Command::new(helper())
        .args(["exec", "--policy"])
        .arg(&policy)
        .args(["--write-cwd", "--backend", "mxc", "--", "/bin/sh", "-c"])
        .arg(format!("touch {}", marker.display()))
        .current_dir(&work)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "exit 3 means nothing ran; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!marker.exists(), "the program must not have run");
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_program_that_outlives_its_grant_is_stopped() {
    if !host_can_sandbox() {
        eprintln!("SKIPPED: this host cannot hold the policy, so grant expiry was not exercised");
        return;
    }
    let work = temp("grant");
    let policy = work.join("policy.json");
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 2_500;
    std::fs::write(
        &policy,
        format!(r#"{{"schemaVersion":1,"id":"grant","label":"Grant","network":"none","grantExpiresAt":{expires}}}"#),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let out = Command::new(helper())
        .args(["exec", "--policy"])
        .arg(&policy)
        .args(["--write-cwd", "--backend", "mxc", "--", "/bin/sleep", "60"])
        .current_dir(&work)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        started.elapsed().as_secs() < 20,
        "it must stop near the grant's end, not run for a minute: {:?}",
        started.elapsed()
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "stopped by the grant ending; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn the_home_folder_cannot_be_made_writable_through_the_working_directory() {
    let work = temp("home");
    let policy = work.join("policy.json");
    std::fs::write(
        &policy,
        r#"{"schemaVersion":1,"id":"home","label":"Home","network":"none"}"#,
    )
    .unwrap();
    let out = Command::new(helper())
        .args(["exec", "--policy"])
        .arg(&policy)
        .args(["--write-cwd", "--backend", "mxc", "--", "/bin/true"])
        .current_dir(&work)
        .env("HOME", &work)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "a session on the home folder is refused; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to make"));
    let _ = std::fs::remove_dir_all(&work);
}

//! Drives the real Prime Agent daemon through Prime Agent's own Rust client, with its scripted fake model: no network,
//! no credentials. Set `PRIME_DESKTOP_TEST_AGENT` to a `prime-agent` executable to run it; without it the test says so and
//! passes, so a machine without the agent still runs the rest of the suite.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use pa_tui::daemon_client::{DaemonClient, DaemonClientEvent};
use pa_types::daemon::DaemonCommand;
use serde_json::{Value, json};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn socket(scratch: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\desktop-test-{}", std::process::id()))
    } else {
        scratch.join("d.sock")
    }
}

fn command(value: Value) -> DaemonCommand {
    serde_json::from_value(value).expect("a command the daemon's own types accept")
}

#[tokio::test]
async fn a_turn_runs_and_its_messages_are_saved() {
    let Some(exe) = std::env::var_os("PRIME_DESKTOP_TEST_AGENT") else {
        eprintln!("SKIPPED: set PRIME_DESKTOP_TEST_AGENT to a prime-agent executable");
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    // The sessions folder is created first: 0.10.0 on Windows loses a session's messages otherwise.
    std::fs::create_dir_all(home.join("agent").join("sessions")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let script = work.join("faux.json");
    std::fs::write(&script, json!({"responses": ["hello from the fake model"]}).to_string()).unwrap();

    let socket = socket(scratch.path());
    let _daemon = Daemon(
        Command::new(exe)
            .args(["--mode", "daemon", "--daemon-socket"])
            .arg(&socket)
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("PRIME_AGENT_CODING_AGENT_DIR", home.join("agent"))
            .env("PRIME_AGENT_TELEMETRY", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the agent starts"),
    );

    let (client, mut events) = DaemonClient::connect_with_retry(&socket)
        .await
        .expect("the daemon answers");
    assert_eq!(client.protocol().name, "prime-agent.daemon");
    assert_eq!(client.protocol().version, 7);

    let created = client
        .request_ok(command(json!({
            "type": "create",
            "lifecycle": "resident",
            "config": {"cwd": work, "script": script, "thinking": "medium"}
        })))
        .await
        .expect("a session is created");
    let id = created["activeSessionId"].as_str().expect("a session id").to_string();
    let file = PathBuf::from(created["sessionFile"].as_str().expect("a session file"));

    client
        .request_ok(command(json!({
            "type": "attach", "activeSessionId": id, "capabilities": ["attach_snapshot", "event_sequence"]
        })))
        .await
        .expect("the session attaches");
    client
        .request_ok(command(
            json!({"type": "prompt", "activeSessionId": id, "message": "hello"}),
        ))
        .await
        .expect("the prompt is accepted");

    // The turn ends with an agent_end event.
    let ended = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = events.recv().await {
            if let DaemonClientEvent::SessionEvent { event, .. } = event
                && event["type"] == "agent_end"
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("the turn finishes in time");
    assert!(ended, "the event stream ended before the turn did");

    let messages = client
        .request_ok(command(json!({"type": "get_messages", "activeSessionId": id})))
        .await
        .expect("messages come back");
    let list = messages["messages"].as_array().expect("a message list");
    assert_eq!(list.len(), 2, "user and assistant: {messages}");

    // And they reached the file, which is what a reload reads.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let saved = std::fs::read_to_string(&file).expect("the session file");
    assert!(saved.contains("\"role\":\"user\""), "the user message is saved");
    assert!(
        saved.contains("hello from the fake model"),
        "the assistant message is saved"
    );
}

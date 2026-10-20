//! The app's own session layer against the real daemon: start it through `DaemonLink`, open a session, run a turn on the
//! scripted fake model, and read the answers the renderer depends on. Set `PRIME_DESKTOP_TEST_AGENT` to a `prime-agent`
//! executable to run it.

use std::path::PathBuf;
use std::time::Duration;

use desktop_core::agent::link::{DaemonLink, SessionConfig, open_session};
use pa_tui::daemon_client::DaemonClientEvent;
use serde_json::{Value, json};

#[tokio::test]
async fn a_session_opens_runs_a_turn_and_answers_what_the_renderer_asks() {
    let Some(exe) = std::env::var_os("PRIME_DESKTOP_TEST_AGENT") else {
        eprintln!("SKIPPED: set PRIME_DESKTOP_TEST_AGENT to a prime-agent executable");
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(home.join("agent")).unwrap();
    let script = work.join("faux.json");
    std::fs::write(&script, json!({"responses": ["hello from the fake model"]}).to_string()).unwrap();
    // The link creates the sessions folder itself; it finds the agent folder through this variable.
    // SAFETY: this is the only test in this binary, so nothing else reads the environment meanwhile.
    unsafe { std::env::set_var("PRIME_AGENT_CODING_AGENT_DIR", home.join("agent")) };

    let socket = if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\desktop-session-test-{}", std::process::id()))
    } else {
        scratch.path().join("d.sock")
    };
    let env = vec![
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("USERPROFILE".to_string(), home.to_string_lossy().into_owned()),
        (
            "PRIME_AGENT_CODING_AGENT_DIR".to_string(),
            home.join("agent").to_string_lossy().into_owned(),
        ),
        ("PRIME_AGENT_TELEMETRY".to_string(), "0".to_string()),
    ];
    let link = DaemonLink::new(PathBuf::from(exe), socket, env);
    let config = SessionConfig {
        cwd: work.clone(),
        thinking: "medium".into(),
        model: None,
        autonomous: json!({"enabled": false}),
        rlm_max_depth: 1,
        transport: "auto".into(),
        extra: Some(json!({"script": script})),
    };
    let mut opened = open_session(&link, &config)
        .await
        .expect("a session opens (the daemon is started and gated)");
    let session = opened.session.clone();

    let state = session.send(&json!({"type": "get_state"})).await.expect("state");
    eprintln!("STATE {state}");
    assert!(
        state["sessionId"].is_string(),
        "the renderer reads sessionId from the state: {state}"
    );

    let commands = session.send(&json!({"type": "get_commands"})).await.expect("commands");
    assert!(commands["commands"].is_array(), "{commands}");

    session
        .send(&json!({"type": "prompt", "message": "hello"}))
        .await
        .expect("the prompt is accepted");
    let ended = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = opened.events.recv().await {
            if let DaemonClientEvent::SessionEvent { event, .. } = event
                && event["type"] == "agent_end"
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("the turn finishes");
    assert!(ended);

    let messages = session.send(&json!({"type": "get_messages"})).await.expect("messages");
    assert_eq!(messages["messages"].as_array().map(Vec::len), Some(2), "{messages}");

    let stats = session
        .send(&json!({"type": "get_session_stats"}))
        .await
        .expect("stats");
    eprintln!("STATS {stats}");
    assert!(stats.is_object());

    let listed = session
        .control(json!({"type": "list", "includeClientOwned": true}))
        .await
        .expect("list");
    eprintln!("LIST {listed}");
    assert!(listed["sessions"].is_array());

    assert!(
        session.send(&json!({"type": "rm_rf"})).await.is_err(),
        "an unknown command is refused here, not sent"
    );
    let _: Value = session
        .send(&json!({"type": "get_available_models"}))
        .await
        .expect("models");
}

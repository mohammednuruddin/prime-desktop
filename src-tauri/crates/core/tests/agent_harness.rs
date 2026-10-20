//! The harness actions against the real daemon, through the real manager and the scripted fake model. Set
//! `PRIME_DESKTOP_TEST_AGENT` to a `prime-agent` executable to run it.
//!
//! The test keeps clear of the machine's own Prime Agent: home, temp and agent folders are all under one scratch folder,
//! so the command-line actions (`status`, `doctor`, `shutdown`), which look at every daemon in the state root, only see
//! the daemon this test started. On Windows the daemons share one pipe namespace and cannot be scoped like that, so the
//! test does not run those actions there.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desktop_core::agent::harness;
use desktop_core::agent::manager::{Manager, ManagerConfig, Outbound, Settings, Sink, primary_agent_id};
use pa_tui::daemon_client::DaemonClient;
use serde_json::{Value, json};

type Log = Arc<Mutex<Vec<Outbound>>>;

fn events_of(log: &Log, kind: &str) -> Vec<Value> {
    let log = log.lock().unwrap();
    log.iter()
        .filter_map(|out| match out {
            Outbound::Event(event) if event["type"] == kind => Some(event.clone()),
            _ => None,
        })
        .collect()
}

async fn wait_for(what: &str, limit: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Asks the daemon on `socket` to exit, if one answers, and waits until nothing does: connecting to a daemon that is
/// still shutting down only fails later.
async fn stop_daemon(socket: &Path) {
    let Ok((client, _events)) = DaemonClient::connect(socket).await else {
        return;
    };
    if let Ok(command) = serde_json::from_value(json!({"type": "shutdown", "force": true})) {
        let _ = client.request_with_timeout(command, 5_000).await;
    }
    client.close();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && DaemonClient::connect(socket).await.is_ok() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Stops the test's daemon even when an assertion fails halfway.
struct StopOnDrop(PathBuf);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let socket = self.0.clone();
        let _ = std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                runtime.block_on(stop_daemon(&socket));
            }
        })
        .join();
    }
}

/// Where the command line looks for daemons: the socket folder of the state root.
#[cfg(unix)]
fn socket_in(tmp: &Path) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata(tmp).unwrap().uid();
    tmp.join(format!("prime-agent-{uid}")).join("app.sock")
}

#[cfg(windows)]
fn socket_in(_tmp: &Path) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\desktop-harness-test-{}", std::process::id()))
}

fn with_action(target: &Value, action: &str) -> Value {
    let mut input = target.clone();
    input["action"] = json!(action);
    input
}

fn text_of(rows: &Value, role: &str) -> Option<(String, String)> {
    rows["nodes"].as_array()?.iter().find_map(|row| {
        (row["role"] == role).then(|| {
            (
                row["entryId"].as_str().unwrap_or("").to_string(),
                row["text"].as_str().unwrap_or("").to_string(),
            )
        })
    })
}

/// One long async body in a debug build is a large future; the test thread's default stack is too small to poll it.
#[test]
fn the_harness_actions_answer_from_a_live_session() {
    let run = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(live_session());
        })
        .unwrap();
    if let Err(panic) = run.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn live_session() {
    let Some(exe) = std::env::var_os("PRIME_DESKTOP_TEST_AGENT") else {
        eprintln!("SKIPPED: set PRIME_DESKTOP_TEST_AGENT to a prime-agent executable");
        return;
    };
    let exe = PathBuf::from(exe);
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let agent_dir = home.join("agent");
    let tmp = scratch.path().join("tmp");
    let work = scratch.path().join("work");
    for dir in [&agent_dir, &tmp, &work] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // Every session the daemon creates answers from this script: replies to prompts, and a title for side questions.
    let script = work.join("faux.json");
    std::fs::write(
        &script,
        json!({
            "responses": ["I fixed the parser.", "Nothing more to add."],
            "sideQuestion": {"responses": ["Fix the CSV parser"]},
        })
        .to_string(),
    )
    .unwrap();
    // SAFETY: this is the only test in this binary, and nothing has been started yet, so no other thread reads the
    // environment meanwhile.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("TMPDIR", &tmp);
        std::env::set_var("PRIME_AGENT_CODING_AGENT_DIR", &agent_dir);
        std::env::set_var("PRIME_AGENT_TELEMETRY", "0");
        std::env::set_var("PRIME_AGENT_INTERNAL_DAEMON_WORKER_SCRIPT", &script);
    }

    let socket = socket_in(&tmp);
    let _stop = StopOnDrop(socket.clone());
    let binary = Arc::new(Mutex::new(exe.clone()));
    let config = ManagerConfig {
        binary: Arc::new({
            let binary = binary.clone();
            move || binary.lock().unwrap().clone()
        }),
        socket: socket.clone(),
        daemon_env: Arc::new(Vec::new),
        settings: Arc::new(Settings::default),
    };
    let log: Log = Arc::default();
    let sink: Sink = {
        let log = log.clone();
        Arc::new(move |out| log.lock().unwrap().push(out))
    };
    let manager = Manager::new(config, sink);
    manager.open_tab("t1", work.clone()).await.expect("the tab opens");
    let agent = primary_agent_id("t1");
    let act = |name: &'static str, input: Value| {
        let (manager, agent) = (manager.clone(), agent.clone());
        async move {
            let answer = harness::action(&manager, &agent, name, &input).await;
            let shown = format!("{answer:?}");
            eprintln!("{name} => {}", shown.chars().take(300).collect::<String>());
            answer
        }
    };
    let ok = |name: &'static str, input: Value| {
        let call = act(name, input);
        async move { call.await.unwrap_or_else(|error| panic!("{name}: {error:#}")) }
    };
    let err = |name: &'static str, input: Value| {
        let call = act(name, input);
        async move { call.await.expect_err(name).to_string() }
    };

    // ----- A session that has not run yet.
    let capabilities = ok("capabilities", json!({})).await;
    assert!(
        capabilities.as_object().unwrap().values().all(|on| on == true),
        "{capabilities}"
    );
    let queue = ok("queue", json!({})).await;
    assert_eq!(
        queue,
        json!({"steering": [], "followUp": [], "mutationSupported": true, "sendQueuedSupported": true})
    );
    assert!(ok("system_prompt", json!({})).await["text"].is_string());
    assert_eq!(ok("goal_state", json!({})).await["goal"]["status"], "idle");
    assert_eq!(ok("heartbeats", json!({})).await, json!({"heartbeats": []}));
    let catalog = ok("model_catalog", json!({})).await;
    assert!(
        !catalog["models"].as_array().unwrap().is_empty(),
        "the catalog lists models"
    );
    assert!(ok("resources", json!({})).await["skills"].is_array());
    assert_eq!(ok("family", json!({})).await, json!({"children": []}));
    assert_eq!(
        ok("family_cancel", json!({"childId": "nope"})).await,
        json!({"cancelled": false})
    );
    assert_eq!(ok("refinement_history", json!({})).await, json!({"history": []}));
    let tree = ok("get_tree", json!({})).await;
    assert!(tree["nodes"].is_array() && tree["leafId"].is_string(), "{tree}");

    // ----- Queue edits are checked here, before the daemon sees them.
    assert!(
        err("queue_mutate", json!({"mutation": {"type": "wipe"}}))
            .await
            .contains("Unknown queue change")
    );
    assert!(err("queue_mutate", json!({})).await.contains("Unknown queue change"));
    let nothing_there = ok(
        "queue_mutate",
        json!({"lane": "followUp", "index": 4, "expectedText": "x", "mutation": {"type": "delete"}}),
    )
    .await;
    assert_eq!(nothing_there["mutationSupported"], true);
    assert!(
        nothing_there["status"].is_string() && nothing_there["followUp"] == json!([]),
        "{nothing_there}"
    );
    assert_eq!(ok("queue_clear", json!({})).await["steering"], json!([]));
    assert_eq!(ok("queue_abort_clear", json!({})).await["followUp"], json!([]));

    // ----- Settings that live on the session.
    assert_eq!(ok("fast", json!({})).await, json!({"enabled": true}));
    assert_eq!(ok("fast", json!({})).await, json!({"enabled": false}));
    assert_eq!(
        ok("set_transport", json!({"transport": "sse"})).await,
        json!({"transport": "sse"})
    );
    assert_eq!(ok("set_transport", json!({})).await, json!({"transport": "auto"}));
    assert_eq!(ok("scoped_models", json!({"models": []})).await, json!({"saved": true}));
    assert_eq!(ok("reload", json!({})).await, json!({"reloaded": true}));

    // ----- Heartbeats: set one, see it listed, pause and stop it.
    let beat = ok(
        "heartbeat_set",
        json!({"schedule": "*/30 * * * *", "prompt": "check in", "deliveryMode": "follow_up"}),
    )
    .await;
    assert_eq!(
        (beat["status"].as_str(), beat["deliveryMode"].as_str()),
        (Some("active"), Some("follow_up"))
    );
    let listed = ok("heartbeats", json!({})).await;
    assert!(
        listed["heartbeats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["job"]["id"] == beat["id"]),
        "{listed}"
    );
    let target = json!({"activeSessionId": beat["activeSessionId"], "jobId": beat["id"]});
    let paused = ok("heartbeat_manage", with_action(&target, "pause")).await;
    assert_eq!(paused["status"], "paused", "{paused}");
    let resumed = ok("heartbeat_manage", with_action(&target, "resume")).await;
    assert_eq!(resumed["status"], "active", "{resumed}");
    let stopped = ok("heartbeat_manage", with_action(&target, "anything else")).await;
    assert_ne!(stopped["status"], "active", "any other verb stops it: {stopped}");
    assert!(
        err("heartbeat_set", json!({"schedule": "never o'clock", "prompt": "x"}))
            .await
            .len()
            > 5,
        "the daemon's refusal of a schedule reaches the caller"
    );

    // ----- Side questions: a blocking one, and one started and stopped.
    assert_eq!(
        ok("side_question", json!({"question": "What is this chat about?"})).await,
        json!({"answer": "Fix the CSV parser"})
    );
    assert!(
        err("side_question", json!({"question": ""}))
            .await
            .contains("Question cannot be empty"),
        "the daemon's refusal reaches the caller"
    );
    let started = ok(
        "side_question_start",
        json!({"id": "s1", "question": "And now?", "previousTurns": []}),
    )
    .await;
    assert_eq!(started, json!({"id": "s1", "started": true}));
    assert!(ok("side_question_abort", json!({"id": "s1"})).await["aborted"].is_boolean());
    assert_eq!(
        ok("side_question_abort", json!({"id": "unknown"})).await,
        json!({"aborted": false})
    );

    // ----- A turn on the scripted model; the manager names the chat once it ends.
    manager
        .run_command(
            &agent,
            json!({"type": "prompt", "message": "Please fix the CSV parser"}),
        )
        .await
        .expect("the prompt is accepted");
    wait_for("the run to end", Duration::from_secs(30), || {
        !events_of(&log, "agent_end").is_empty()
    })
    .await;
    wait_for("the chat to be named", Duration::from_secs(60), || {
        !events_of(&log, "session_renamed").is_empty()
    })
    .await;
    let renamed = events_of(&log, "session_renamed");
    assert_eq!(renamed.len(), 1, "named once: {renamed:?}");
    assert_eq!(renamed[0]["payload"]["name"], "Fix the CSV parser");
    let info = manager.all_infos();
    assert_eq!(info[0]["sessionName"], "Fix the CSV parser");
    // Naming again does nothing: it is named, and the model is not asked.
    let chat = manager.chat(&agent).unwrap();
    harness::auto_name(&manager, &chat).await;
    assert_eq!(events_of(&log, "session_renamed").len(), 1);

    // ----- The tree after the turn.
    let tree = ok("get_tree", json!({})).await;
    let (user_entry, user_text) = text_of(&tree, "user").expect("the prompt is in the tree");
    assert_eq!(user_text, "Please fix the CSV parser");
    assert_eq!(text_of(&tree, "assistant").unwrap().1, "I fixed the parser.");
    // The question that made the title is not part of the chat.
    let texts: Vec<&str> = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["text"].as_str())
        .collect();
    assert!(
        texts.iter().all(|text| !text.contains("name chat sessions")),
        "{texts:?}"
    );
    let full = ok("get_tree_full", json!({})).await;
    assert_eq!(full["leafId"], tree["leafId"]);
    assert!(full["tree"][0]["children"].is_array() && full["tree"][0]["entry"]["id"].is_string());
    let labelled = ok("label_tree", json!({"entryId": user_entry, "label": "start"})).await;
    assert!(labelled["tree"].is_array());
    let rows = ok("get_tree", json!({})).await;
    let label = rows["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["entryId"] == user_entry.as_str());
    assert_eq!(label.unwrap()["label"], "start");
    let unlabelled = ok("label_tree", json!({"entryId": user_entry})).await;
    assert!(unlabelled["tree"].is_array());

    // ----- Files: JSONL out and back in, HTML out, the saved sessions listed.
    let jsonl = scratch.path().join("export.jsonl");
    let exported = ok("export_jsonl", json!({"outputPath": jsonl})).await;
    assert!(
        jsonl.is_file() && std::fs::metadata(&jsonl).unwrap().len() > 0,
        "{exported}"
    );
    let html = scratch.path().join("export.html");
    let written = harness::export_html(&manager, &agent, Some(&html.to_string_lossy()))
        .await
        .expect("export");
    assert!(std::fs::read_to_string(&html).unwrap().contains("<html"), "{written}");
    assert_eq!(Path::new(&written), html.as_path());
    let relative = harness::export_html(&manager, &agent, Some("inside.html"))
        .await
        .expect("a relative path");
    assert_eq!(Path::new(&relative), work.join("inside.html").as_path());
    assert!(work.join("inside.html").is_file(), "a relative path is the project's");
    assert!(harness::export_html(&manager, "agent-nobody", None).await.is_err());
    let files = ok("trace_list", json!({})).await;
    let listed = files["files"].as_array().unwrap();
    assert!(!listed.is_empty(), "the chat's file is listed: {files}");
    let preview = ok("trace_preview", json!({"path": listed[0]["path"]})).await;
    assert!(
        preview["preview"]
            .as_str()
            .unwrap()
            .contains("Please fix the CSV parser"),
        "{preview}"
    );
    assert!(
        err("trace_preview", json!({"path": jsonl}))
            .await
            .contains("outside the session directory")
    );
    assert!(
        err("trace_preview", json!({"path": "/etc/passwd"}))
            .await
            .contains("outside the session directory")
    );
    assert!(
        err("import", json!({"path": "missing.jsonl"}))
            .await
            .contains("import path")
    );
    let imported = ok("import", json!({"path": jsonl})).await;
    eprintln!("IMPORTED {imported}");

    // ----- Sharing, with stand-ins for the GitHub CLI (the real one would publish).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fake = |name: &str, body: &str| {
            let path = scratch.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let seen = scratch.path().join("gh-saw");
        let copy = scratch.path().join("gh-copy.html");
        let works = fake(
            "gh-works",
            &format!(
                "[ \"$1\" = auth ] && exit 0\n[ \"$2\" = create ] && [ \"$3\" = --public=false ] || exit 9\n\
                 cp \"$4\" {copy}; stat -c '%a' \"$(dirname \"$4\")\" > {seen}; echo \"$4\" >> {seen}\n\
                 echo https://gist.github.com/someone/abc123",
                copy = copy.display(),
                seen = seen.display()
            ),
        );
        let shared = harness::share_session_via(&manager, &agent, &works)
            .await
            .expect("shared");
        assert_eq!(
            shared,
            json!({"gistUrl": "https://gist.github.com/someone/abc123", "previewUrl": "https://pi.dev/session/abc123"})
        );
        assert!(
            std::fs::read_to_string(&copy).unwrap().contains("<html"),
            "the page that was uploaded"
        );
        let saw = std::fs::read_to_string(&seen).unwrap();
        let mut lines = saw.lines();
        assert_eq!(
            lines.next(),
            Some("700"),
            "only this user can enter the export's folder"
        );
        let uploaded = PathBuf::from(lines.next().unwrap());
        assert!(
            !uploaded.exists() && !uploaded.parent().unwrap().exists(),
            "the export is deleted afterwards"
        );

        let logged_out = fake("gh-logged-out", "exit 1");
        let refused = harness::share_session_via(&manager, &agent, &logged_out)
            .await
            .unwrap_err();
        assert_eq!(
            refused.to_string(),
            "GitHub CLI is not logged in. Run gh auth login first."
        );
        let missing = scratch.path().join("no-gh-here");
        assert!(harness::share_session_via(&manager, &agent, &missing).await.is_err());
        let broken = fake(
            "gh-broken",
            "[ \"$1\" = auth ] && exit 0\necho 'name already taken' >&2; exit 1",
        );
        let failed = harness::share_session_via(&manager, &agent, &broken).await.unwrap_err();
        assert_eq!(failed.to_string(), "name already taken");
        let vague = fake("gh-vague", "[ \"$1\" = auth ] && exit 0\nexit 0");
        let nothing = harness::share_session_via(&manager, &agent, &vague).await.unwrap_err();
        assert_eq!(nothing.to_string(), "Failed to parse gist ID from gh output");
        assert!(
            harness::share_session_via(&manager, "agent-nobody", &works)
                .await
                .is_err()
        );
    }

    // ----- Going back to an earlier point of the chat.
    let moved = ok("navigate_tree", json!({"targetId": user_entry, "summarize": false})).await;
    eprintln!("NAVIGATED {moved}");

    // ----- Commands that go to the agent as prompts, and a refinement.
    assert_eq!(ok("goal_command", json!({})).await, json!({"accepted": true}));
    assert_eq!(
        ok("autonomous_command", json!({"mode": "status"})).await,
        json!({"accepted": true})
    );
    // The scripted session cannot refine; its refusal comes back as it was said.
    assert!(
        err("refine", json!({"instructions": "keep it short", "global": false}))
            .await
            .contains("does not support refinement")
    );
    let follow_up = ok(
        "side_question_start",
        json!({"id": "s2", "question": "And then?", "previousTurns": [{"question": "q", "answer": "a"}]}),
    )
    .await;
    assert_eq!(follow_up["started"], true);

    // ----- Prime Agent's own settings files, which need no session.
    let roles = ok("model_roles", json!({})).await;
    assert_eq!(roles["auxiliaryModel"], "");
    let set = ok(
        "model_role_set",
        json!({"key": "auxiliaryModel", "value": "openai/gpt-5"}),
    )
    .await;
    assert_eq!(set["auxiliaryModel"], "openai/gpt-5");
    assert!(
        err("model_role_set", json!({"key": "auxiliaryModel", "value": "gpt-5"}))
            .await
            .contains("provider and model ID")
    );
    assert_eq!(ok("traces", json!({})).await, json!({"enabled": false}));
    assert_eq!(ok("traces", json!({"mode": "on"})).await, json!({"enabled": true}));
    assert_eq!(ok("traces", json!({"mode": "off"})).await, json!({"enabled": false}));
    let servers = ok(
        "mcp_set",
        json!({"name": "colab", "type": "stdio", "commandLine": "uvx colab-mcp --flag 'a b'"}),
    )
    .await;
    assert_eq!(
        servers["servers"]["colab"]["args"],
        json!(["colab-mcp", "--flag", "a b"])
    );
    assert_eq!(ok("mcp_get", json!({})).await, servers);
    assert_eq!(
        ok("mcp_set", json!({"name": "colab", "remove": true})).await,
        json!({"servers": {}})
    );
    let catalog = ok("mcp_catalog", json!({})).await;
    assert!(
        catalog
            .as_array()
            .unwrap()
            .iter()
            .all(|service| service["id"].is_string() && service["auth"].is_string())
    );
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(agent_dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["auxiliaryModel"], "openai/gpt-5");

    // ----- Prime Agent's command line.
    let packages = ok("package", json!({"command": "list"})).await;
    assert_eq!(packages["ok"], true, "{packages}");
    assert!(
        err("package", json!({"command": "rm"}))
            .await
            .contains("Unsupported package action")
    );
    assert!(
        err("package", json!({"command": "install"}))
            .await
            .contains("source is required")
    );
    assert!(
        err("package", json!({"command": "install", "source": "--local"}))
            .await
            .contains("source is invalid")
    );
    assert!(
        err("package", json!({"command": "update", "source": "self"}))
            .await
            .contains("updated together")
    );
    assert!(err("update", json!({})).await.contains("0.10.0"));
    assert!(err("nope", json!({})).await.contains("Unknown harness action: nope"));
    assert!(
        harness::action(&manager, "agent-nobody", "queue", &json!({}))
            .await
            .is_err()
    );
    assert_eq!(
        harness::action(&manager, "", "model_roles", &json!({})).await.unwrap()["auxiliaryModel"],
        "openai/gpt-5",
        "settings actions need no chat"
    );

    #[cfg(unix)]
    {
        // ----- The daemon, from the command line.
        let status = ok("daemon_status", json!({})).await;
        assert_eq!(status["ok"], true, "{status}");
        assert!(
            status["output"].as_str().unwrap().contains("app.sock"),
            "the app's daemon is listed: {status}"
        );
        assert_eq!(ok("daemon_doctor", json!({})).await["ok"], true);

        // Shutting it down drops the chat's connection. A panel asking a read-only question then must not bring a
        // daemon back; recovering does.
        assert_eq!(ok("daemon_shutdown", json!({})).await["ok"], true);
        wait_for("the connection to drop", Duration::from_secs(20), || {
            chat.session().is_none()
        })
        .await;
        // They answer at once: reconnecting would take as long as starting a daemon.
        for name in ["get_tree", "heartbeats"] {
            let asked = tokio::time::timeout(Duration::from_secs(5), err(name, json!({})))
                .await
                .expect("a read-only question waited for a connection");
            assert!(asked.contains("Unknown or disconnected agent"), "{asked}");
        }
        assert!(
            DaemonClient::connect(&socket).await.is_err(),
            "a read-only question started a daemon"
        );

        // The manager does not start a second daemon within the first one's start window, which would be a mere
        // log line; a different path to the executable is a new start.
        let alias = scratch.path().join("prime-agent-b");
        std::os::unix::fs::symlink(&exe, &alias).unwrap();
        *binary.lock().unwrap() = alias;
        let recovered = ok("daemon_recover", json!({})).await;
        assert_eq!(recovered["ok"], true, "{recovered}");
        assert!(chat.session().is_some(), "recovering reconnects the chat");
        assert!(ok("get_tree", json!({})).await["nodes"].is_array());

        // A user action reconnects a chat whose daemon has gone.
        stop_daemon(&socket).await;
        wait_for("the connection to drop", Duration::from_secs(20), || {
            chat.session().is_none()
        })
        .await;
        let alias = scratch.path().join("prime-agent-c");
        std::os::unix::fs::symlink(&exe, &alias).unwrap();
        *binary.lock().unwrap() = alias;
        assert_eq!(ok("queue", json!({})).await["mutationSupported"], true);
        assert!(chat.session().is_some());
    }

    stop_daemon(&socket).await;
    manager.shutdown_all();
}

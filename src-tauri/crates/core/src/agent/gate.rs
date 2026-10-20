//! The version gate: whether the daemon that answered is the build the app was tested against.
//!
//! It runs on the daemon's first message before any session is created, and fails closed: a missing or oddly typed
//! field is a reason to refuse, because a daemon the app does not recognise is one whose behaviour it cannot vouch for.

use serde_json::Value;

use super::release::VERSION;

pub const PROTOCOL_NAME: &str = "prime-agent.daemon";
pub const PROTOCOL_VERSION: u64 = 7;

/// The oldest schema revision the app was tested with. A daemon older than this lacks fields the app reads.
pub const MIN_SCHEMA_REVISION: u64 = 30;

/// Everything the app calls that a Prime Agent without it would answer wrongly or not at all.
pub const REQUIRED_CAPABILITIES: &[&str] = &[
    "queue_message_mutation",
    "abort_and_send_queued",
    "model_catalog",
    "authoritative_child_roster",
    "heartbeat_catalog",
];

/// Why the app must not use this daemon, or `None` when it is the build the app was tested against.
pub fn hello_problem(hello: &Value, pinned_version: &str) -> Option<String> {
    let Some(fields) = hello.as_object() else {
        return Some("The daemon did not introduce itself.".into());
    };
    let protocol = fields.get("protocol");
    let name = protocol.and_then(|p| p.get("name")).and_then(Value::as_str);
    let version = protocol.and_then(|p| p.get("version")).and_then(Value::as_u64);
    if name != Some(PROTOCOL_NAME) || version != Some(PROTOCOL_VERSION) {
        return Some(format!(
            "The daemon speaks {} v{}, not {PROTOCOL_NAME} v{PROTOCOL_VERSION}.",
            name.unwrap_or("an unknown protocol"),
            version.map_or_else(|| "?".into(), |v| v.to_string())
        ));
    }
    let build = fields
        .get("runtime")
        .and_then(|r| r.get("buildId"))
        .and_then(Value::as_str);
    if !build.is_some_and(|build| build.starts_with("pa-daemon-rs-")) {
        return Some(format!(
            "The daemon is not a Rust Prime Agent build ({}).",
            build.unwrap_or("no build id")
        ));
    }
    let app_version = fields.get("appVersion").and_then(Value::as_str);
    if app_version != Some(pinned_version) {
        return Some(format!(
            "The daemon is Prime Agent {}; this app is tested with {pinned_version}.",
            app_version.unwrap_or("of an unknown version")
        ));
    }
    match fields.get("schemaRevision").and_then(Value::as_u64) {
        Some(revision) if revision >= MIN_SCHEMA_REVISION => {}
        other => {
            return Some(format!(
                "The daemon's schema revision {} is older than {MIN_SCHEMA_REVISION}.",
                other.map_or_else(|| "(missing)".into(), |r| r.to_string())
            ));
        }
    }
    let offered: Vec<&str> = fields
        .get("serverCapabilities")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = REQUIRED_CAPABILITIES
        .iter()
        .copied()
        .filter(|c| !offered.contains(c))
        .collect();
    if !missing.is_empty() {
        return Some(format!("The daemon lacks {}.", missing.join(", ")));
    }
    None
}

/// [`hello_problem`] against the version this build pins.
pub fn problem(hello: &Value) -> Option<String> {
    hello_problem(hello, VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The hello of Prime Agent 0.10.0, captured from its daemon on 2026-10-13 (trimmed to the fields the gate reads).
    fn real() -> Value {
        json!({
            "protocol": {"name": "prime-agent.daemon", "version": 7},
            "schemaRevision": 30,
            "appVersion": "0.10.0",
            "runtime": {"buildId": "pa-daemon-rs-0.10.0"},
            "serverCapabilities": [
                "abort_and_send_queued", "acp_mcp_servers", "agent_roster", "attach_snapshot", "authoritative_child_roster",
                "chunked_snapshot", "client_owned_sessions", "delete_rlm_subagent", "direct_peer_transport",
                "elide_snapshot_images", "event_sequence", "heartbeat_catalog", "heartbeat_management",
                "kernel_bash_activity", "model_catalog", "owned_prompt_cancellation", "owned_session_recovery_context",
                "prompt_admission_cancellation", "queue_message_mutation", "rlm_quiescence_barrier",
                "session_input_admission", "session_input_pause", "side_question_transcript", "slim_attach", "transient_bash"
            ]
        })
    }

    #[test]
    fn the_pinned_daemon_passes_exactly_as_it_introduced_itself() {
        assert_eq!(problem(&real()), None);
    }

    #[test]
    fn anything_else_is_refused_with_a_reason() {
        let mut other_protocol = real();
        other_protocol["protocol"]["version"] = json!(8);
        assert!(problem(&other_protocol).unwrap().contains("not prime-agent.daemon v7"));

        let mut npm = real();
        npm["runtime"]["buildId"] = json!("pa-daemon-ts-0.9.8");
        assert!(problem(&npm).unwrap().contains("not a Rust Prime Agent build"));

        let mut newer = real();
        newer["appVersion"] = json!("0.11.0");
        assert!(problem(&newer).unwrap().contains("tested with 0.10.0"));

        let mut old = real();
        old["schemaRevision"] = json!(29);
        assert!(problem(&old).unwrap().contains("older than 30"));

        let mut lacking = real();
        lacking["serverCapabilities"] = json!(["slim_attach"]);
        assert!(problem(&lacking).unwrap().contains("queue_message_mutation"));

        assert!(problem(&json!(null)).unwrap().contains("did not introduce itself"));
        assert!(problem(&json!({})).is_some());
        let mut mistyped = real();
        mistyped["schemaRevision"] = json!("30");
        assert!(
            problem(&mistyped).unwrap().contains("(missing)"),
            "a string where a number belongs fails closed"
        );
    }
}

//! Small helpers the renderer and the manager share with the TypeScript they replace: which messages are internal, how
//! a model list is read, what a model is called.

use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| block.get("content").and_then(Value::as_str))
                    .unwrap_or("")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Messages the agent writes for itself (restored interpreter state, harness digests). They are never shown.
pub fn is_internal_state_restore_message(message: &Value) -> bool {
    let Some(record) = message.as_object() else {
        return false;
    };
    let role = record.get("role").and_then(Value::as_str);
    if matches!(role, Some("user" | "toolResult")) {
        return false;
    }
    let custom_type = record.get("customType").and_then(Value::as_str);
    if record.get("display") == Some(&Value::Bool(false)) && (role == Some("custom") || custom_type.is_some()) {
        return true;
    }
    if matches!(
        custom_type,
        Some("ipython_state" | "refinement_notice" | "ipython_state_restored" | "harness_digest")
    ) {
        return true;
    }
    static TAGS: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (restore, digest) = TAGS.get_or_init(|| {
        (
            Regex::new(r"(?i)<ipython_state_restored(?:\s|>)").unwrap(),
            Regex::new(r"(?i)(?:^|\[)harness-digest\]|<harness_state>").unwrap(),
        )
    });
    let text = content_text(record.get("content").unwrap_or(&Value::Null));
    restore.is_match(&text) || digest.is_match(&text)
}

fn entries_of(raw: &Value) -> Vec<Value> {
    if let Some(list) = raw.as_array() {
        return list.clone();
    }
    let Some(object) = raw.as_object() else {
        return Vec::new();
    };
    if let Some(list) = object.get("models").and_then(Value::as_array) {
        return list.clone();
    }
    if let Some(list) = object
        .get("data")
        .and_then(|data| data.get("models"))
        .and_then(Value::as_array)
    {
        return list.clone();
    }
    let mut out = Vec::new();
    if let Some(providers) = object.get("providers").and_then(Value::as_object) {
        for (provider, config) in providers {
            let Some(models) = config.get("models").and_then(Value::as_array) else {
                continue;
            };
            for model in models {
                match model.as_object() {
                    Some(record) => {
                        let mut record = record.clone();
                        record.entry("provider").or_insert_with(|| json!(provider));
                        out.push(Value::Object(record));
                    }
                    None => out.push(model.clone()),
                }
            }
        }
    }
    out
}

/// A model list from any of the shapes the daemon and `models.json` use, as `{key, provider, id, name}` rows, once each.
pub fn parse_model_list(raw: &Value) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in entries_of(raw) {
        if let Some(text) = entry.as_str() {
            let id = text.trim();
            if id.is_empty() || !seen.insert(id.to_string()) {
                continue;
            }
            let (provider, name) = id.split_once('/').unwrap_or(("", id));
            out.push(json!({"key": id, "provider": provider, "id": name, "name": name}));
            continue;
        }
        let Some(record) = entry.as_object() else { continue };
        let pick = |keys: &[&str]| {
            keys.iter()
                .find_map(|key| record.get(*key).and_then(Value::as_str))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let id = pick(&["id", "modelId", "name"]);
        if id.is_empty() {
            continue;
        }
        let provider = record.get("provider").and_then(Value::as_str).unwrap_or("").to_string();
        let key = if provider.is_empty() {
            id.clone()
        } else {
            format!("{provider}/{id}")
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        let name = record
            .get("name")
            .and_then(Value::as_str)
            .map_or_else(|| id.clone(), str::to_string);
        out.push(json!({"key": key, "provider": provider, "id": id, "name": name}));
    }
    out
}

/// `provider/id` for the model a session state names, or the id alone, or the display name.
pub fn model_key_from_state(model: &Value) -> Option<String> {
    if let Some(text) = model.as_str() {
        return Some(text.to_string());
    }
    let record = model.as_object()?;
    let field = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| record.get(*key).and_then(Value::as_str))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let id = field(&["id", "modelId"]);
    let provider = record.get("provider").and_then(Value::as_str).unwrap_or("");
    if !provider.is_empty() && !id.is_empty() {
        return Some(format!("{provider}/{id}"));
    }
    if !id.is_empty() {
        return Some(id);
    }
    Some(field(&["name"])).filter(|name| !name.is_empty())
}

/// The last path segment, either separator.
pub fn base_name(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_messages_are_recognised_and_user_messages_never_are() {
        assert!(is_internal_state_restore_message(
            &json!({"role": "custom", "display": false})
        ));
        assert!(is_internal_state_restore_message(
            &json!({"customType": "ipython_state"})
        ));
        assert!(is_internal_state_restore_message(
            &json!({"role": "assistant", "content": "x <ipython_state_restored> y"})
        ));
        assert!(is_internal_state_restore_message(
            &json!({"role": "system", "content": [{"text": "[harness-digest] ok"}]})
        ));
        assert!(!is_internal_state_restore_message(
            &json!({"role": "user", "content": "<ipython_state_restored>"})
        ));
        assert!(!is_internal_state_restore_message(
            &json!({"role": "toolResult", "customType": "ipython_state"})
        ));
        assert!(!is_internal_state_restore_message(
            &json!({"role": "assistant", "content": "plain"})
        ));
        assert!(!is_internal_state_restore_message(&json!("text")));
    }

    #[test]
    fn model_lists_read_every_shape_once() {
        let rows = parse_model_list(
            &json!({"models": [{"id": "a", "provider": "p"}, {"modelId": "b"}, "q/c", "q/c", {"id": ""}]}),
        );
        let keys: Vec<&str> = rows.iter().map(|row| row["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["p/a", "b", "q/c"]);
        assert_eq!(rows[2]["provider"], "q");
        assert_eq!(rows[2]["id"], "c");
        let by_provider = parse_model_list(&json!({"providers": {"x": {"models": [{"id": "m"}]}}}));
        assert_eq!(by_provider[0]["key"], "x/m");
        assert_eq!(parse_model_list(&json!({"data": {"models": ["z"]}}))[0]["key"], "z");
        assert!(parse_model_list(&json!(null)).is_empty());
    }

    #[test]
    fn a_model_state_is_named_by_provider_and_id() {
        assert_eq!(
            model_key_from_state(&json!({"provider": "p", "id": "m"})).as_deref(),
            Some("p/m")
        );
        assert_eq!(model_key_from_state(&json!({"id": "m"})).as_deref(), Some("m"));
        assert_eq!(model_key_from_state(&json!({"name": "Nice"})).as_deref(), Some("Nice"));
        assert_eq!(model_key_from_state(&json!("p/m")).as_deref(), Some("p/m"));
        assert_eq!(model_key_from_state(&json!(null)), None);
    }

    #[test]
    fn base_names_read_both_separators() {
        assert_eq!(base_name("/a/b/c"), "c");
        assert_eq!(base_name(r"C:\Users\ada\proj\"), "proj");
        assert_eq!(base_name("plain"), "plain");
    }
}

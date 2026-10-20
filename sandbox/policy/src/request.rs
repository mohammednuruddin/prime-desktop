use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::path::{Access, has_parent_segment, windows_path_problem};
use crate::policy::{PolicyError, ResolvedPolicy, valid_env_name};

/// What a host asks to run. It is an argv array, never a shell string: there is no quoting helper here
/// on purpose, because escaping text that a shell will parse again only looks like safety.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestInput {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub pty: bool,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRequest {
    pub argv: Vec<String>,
    /// Canonical, and inside the policy's roots.
    pub cwd: PathBuf,
    pub pty: bool,
    /// Only variables the policy allows, under the names the host used.
    pub env: BTreeMap<String, String>,
}

impl RequestInput {
    pub fn from_json(json: &str) -> Result<Self, PolicyError> {
        serde_json::from_str(json).map_err(|error| PolicyError::new("request", error.to_string()))
    }

    pub fn resolve(self, policy: &ResolvedPolicy, now_ms: u64) -> Result<ResolvedRequest, PolicyError> {
        if policy.is_expired(now_ms) {
            return Err(PolicyError::new("policy", "the grant has expired"));
        }
        if self.argv.is_empty() {
            return Err(PolicyError::new("argv", "must not be empty"));
        }
        for (i, arg) in self.argv.iter().enumerate() {
            if arg.contains('\0') {
                return Err(PolicyError::new(format!("argv[{i}]"), "must not contain a NUL"));
            }
        }
        if self.argv[0].is_empty() {
            return Err(PolicyError::new("argv[0]", "must name a program"));
        }
        if !self.cwd.is_absolute() {
            return Err(PolicyError::new("cwd", "must be an absolute path chosen by the host"));
        }
        if has_parent_segment(&self.cwd) {
            return Err(PolicyError::new("cwd", "must not contain `..`; send the resolved path"));
        }
        if cfg!(windows)
            && let Some(problem) = windows_path_problem(&self.cwd.to_string_lossy())
        {
            return Err(PolicyError::new("cwd", problem));
        }
        let cwd = self
            .cwd
            .canonicalize()
            .map_err(|error| PolicyError::new("cwd", format!("cannot resolve: {error}")))?;
        if !policy.allows(&cwd, Access::Read) {
            return Err(PolicyError::new(
                "cwd",
                "is outside the policy roots or inside a denied path",
            ));
        }
        if self.pty && !policy.allow_pty {
            return Err(PolicyError::new("pty", "requested but the policy does not allow a PTY"));
        }
        for (name, value) in &self.env {
            if !valid_env_name(name) {
                return Err(PolicyError::new(format!("env.{name}"), "is not a valid variable name"));
            }
            if !policy.env_allowed(name) {
                return Err(PolicyError::new(
                    format!("env.{name}"),
                    "is not in the policy allowlist",
                ));
            }
            if value.contains('\0') {
                return Err(PolicyError::new(format!("env.{name}"), "must not contain a NUL"));
            }
        }
        Ok(ResolvedRequest {
            argv: self.argv,
            cwd,
            pty: self.pty,
            env: self.env,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::tests::{NOW, Sandbox, input, raw_join};

    fn request(cwd: PathBuf) -> RequestInput {
        RequestInput {
            argv: vec!["tool".into(), "--flag".into()],
            cwd,
            pty: false,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn a_request_inside_the_roots_resolves() {
        let sb = Sandbox::new("req-ok");
        let policy = input(&sb).resolve(NOW).unwrap();
        let mut raw = request(sb.work().join("src"));
        raw.env.insert("PATH".into(), "/bin".into());
        let resolved = raw.resolve(&policy, NOW).unwrap();
        assert_eq!(resolved.cwd, sb.work().join("src"));
        assert_eq!(resolved.argv, ["tool", "--flag"]);
    }

    #[test]
    fn shell_metacharacters_stay_inside_one_argument() {
        let sb = Sandbox::new("req-meta");
        let policy = input(&sb).resolve(NOW).unwrap();
        let mut raw = request(sb.work());
        raw.argv = vec!["tool".into(), "a; rm -rf / && $(id) | cat `x`".into()];
        let resolved = raw.resolve(&policy, NOW).unwrap();
        assert_eq!(
            resolved.argv.len(),
            2,
            "argv is passed through as data, never re-parsed"
        );
    }

    #[test]
    fn rejects_what_a_host_must_not_send() {
        let sb = Sandbox::new("req-bad");
        let policy = input(&sb).resolve(NOW).unwrap();
        type Case = (&'static str, Box<dyn Fn(&mut RequestInput)>);
        let cases: Vec<Case> = vec![
            ("argv", Box::new(|r| r.argv = vec![])),
            ("argv[1]", Box::new(|r| r.argv = vec!["tool".into(), "a\0b".into()])),
            ("argv[0]", Box::new(|r| r.argv = vec![String::new()])),
            ("cwd", Box::new(|r| r.cwd = PathBuf::from("relative"))),
            ("cwd", Box::new(|r| r.cwd = raw_join(&r.cwd, &["src", "..", ".."]))),
            ("cwd", Box::new(|r| r.cwd = std::env::temp_dir())),
            ("pty", Box::new(|r| r.pty = true)),
            (
                "env.SECRET_TOKEN",
                Box::new(|r| {
                    r.env.insert("SECRET_TOKEN".into(), "x".into());
                }),
            ),
            (
                "env.BAD NAME",
                Box::new(|r| {
                    r.env.insert("BAD NAME".into(), "x".into());
                }),
            ),
            (
                "env.PATH",
                Box::new(|r| {
                    r.env.insert("PATH".into(), "a\0b".into());
                }),
            ),
        ];
        for (field, mutate) in cases {
            let mut raw = request(sb.work());
            mutate(&mut raw);
            let error = raw.resolve(&policy, NOW).expect_err(field);
            assert_eq!(error.field, field, "{error}");
        }
    }

    #[test]
    fn a_denied_directory_is_not_a_valid_cwd() {
        let sb = Sandbox::new("req-denied");
        std::fs::create_dir_all(sb.work().join(".git")).unwrap();
        let policy = input(&sb).resolve(NOW).unwrap();
        let error = request(sb.work().join(".git")).resolve(&policy, NOW).unwrap_err();
        assert_eq!(error.field, "cwd");
    }

    #[test]
    fn an_expired_grant_runs_nothing() {
        let sb = Sandbox::new("req-expired");
        let mut raw = input(&sb);
        raw.grant_expires_at = Some(NOW + 10);
        let policy = raw.resolve(NOW).unwrap();
        assert!(request(sb.work()).resolve(&policy, NOW + 10).is_err());
    }
}

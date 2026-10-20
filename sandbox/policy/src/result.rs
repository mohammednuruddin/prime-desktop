use serde::{Deserialize, Serialize};

/// Whether the OS is actually holding the policy. `Enforced` is only ever reported on a host where the
/// behavioural probes have passed; a policy object existing proves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforcement {
    /// The backend held the policy and this host has passed the probe suite for it (see the helper's `attest`).
    Enforced,
    /// The backend applied the policy, but this host has not passed the probe suite for this backend and helper
    /// version, so nothing proves it. Show it as unverified, not as protected.
    Unattested,
    /// Ran on purpose without a sandbox, such as a full-access session. Always shown to the user.
    Unenforced,
    /// The host cannot hold this policy, so nothing was run.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Denial {
    pub code: String,
    pub message: String,
    /// Whether retrying inside the same policy could succeed.
    pub retryable_in_boundary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionResult {
    pub exit_code: Option<i32>,
    /// The helper stopped the child because the policy's timeout passed.
    pub timed_out: bool,
    pub signal: Option<String>,
    pub backend: Option<String>,
    pub enforcement: Enforcement,
    pub denials: Vec<Denial>,
    /// What the backend cannot hold about this policy even though it is enforcing the rest, in words a user can
    /// read. Empty means the policy is held as written.
    pub limitations: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub started_at: u64,
    pub finished_at: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforcement_uses_the_names_the_host_expects() {
        assert_eq!(
            serde_json::to_string(&Enforcement::Unavailable).unwrap(),
            "\"unavailable\""
        );
        assert_eq!(serde_json::to_string(&Enforcement::Enforced).unwrap(), "\"enforced\"");
    }
}

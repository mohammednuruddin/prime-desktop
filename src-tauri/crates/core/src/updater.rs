//! The update state machine the renderer shows.
//!
//! This build has no update feed: the release repository is private, so a feed cannot be read without a credential the app
//! must not carry (see the project notes on public releases). The state says so instead of claiming the app
//! is up to date, and installing does nothing. A feed would add `checking`, `downloading` and `ready` here without
//! changing what the renderer reads.

use std::sync::Mutex;

use serde::Serialize;

/// The renderer's `UpdateState` (`src/shared/appUpdate.ts`).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// `idle`, `checking`, `up-to-date`, `downloading`, `ready`, `error` or `unsupported`.
    pub status: &'static str,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub struct Updater {
    state: Mutex<UpdateState>,
}

impl Updater {
    pub fn new(current_version: &str) -> Self {
        Self {
            state: Mutex::new(UpdateState {
                status: "unsupported",
                current_version: current_version.to_string(),
                note: Some("Updates are installed by hand for now: this build has no update feed.".into()),
            }),
        }
    }

    pub fn state(&self) -> UpdateState {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// A check cannot reach a feed; the state stays as it is.
    pub fn check(&self) -> UpdateState {
        self.state()
    }

    pub fn install(&self) -> Result<(), String> {
        Err("There is no update to install.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_feed_the_app_says_so_and_never_claims_to_be_current() {
        let updater = Updater::new("0.1.1");
        let state = updater.check();
        assert_eq!(state.status, "unsupported");
        assert_eq!(state.current_version, "0.1.1");
        assert!(state.note.as_deref().unwrap().contains("by hand"));
        assert!(updater.install().is_err());
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["currentVersion"], "0.1.1");
    }
}

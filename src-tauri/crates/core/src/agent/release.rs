//! The pinned Prime Agent release: version, per-platform hashes, and where it is installed.
//!
//! The values come from the release's own `SHA256SUMS` and manifest, and the provenance of those files was checked with
//! `gh attestation verify`. Bumping the pin means re-running the contract checks on the new tag and committing its
//! hashes, never following `latest`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub const VERSION: &str = "0.10.0";
pub const REPO: &str = "PrimeIntellect-ai/prime-agent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    /// `<os>-<arch>` with the names the release uses: `win32`, `darwin`, `linux`; `x64`, `arm64`.
    pub key: &'static str,
    pub file: &'static str,
    /// Hash of the archive, checked before anything is unpacked.
    pub sha256: &'static str,
    /// Hash of the executable inside it, checked after unpacking and whenever the app asks "is this the pinned build".
    pub executable_sha256: &'static str,
}

pub const ASSETS: &[Asset] = &[
    Asset {
        key: "win32-x64",
        file: "prime-agent-0.10.0-win32-x64.tar.gz",
        sha256: "1b61708361f75336dbc1897b88a26a298bab354162122a9e9e34c2e7e4635f8b",
        executable_sha256: "e208889f647b275cd30d01f489e2b2bbe16e7ed59a113e06f079edf38afe755c",
    },
    Asset {
        key: "darwin-arm64",
        file: "prime-agent-0.10.0-darwin-arm64.tar.gz",
        sha256: "e418bdf62fcb0002777bf3ac43bcc136512b26763f2ab5c500792f26e37294e5",
        executable_sha256: "4e095433afad2b324148601133d4f4aacd1648a438bea051250414251c5baeb3",
    },
    Asset {
        key: "darwin-x64",
        file: "prime-agent-0.10.0-darwin-x64.tar.gz",
        sha256: "af4866b5ba82f3419b964290e3f023ddb9b99958a89e16ee856962b901ec0fc6",
        executable_sha256: "a9201e710147aee28bb2154cc61b5c89a7769782322e71dd44a4782b5c9b122a",
    },
    Asset {
        key: "linux-x64",
        file: "prime-agent-0.10.0-linux-x64.tar.gz",
        sha256: "c16bd2af5e77b53f49b914a44742c4cf6a67c5ed5000041430b78dbd4c3bcbec",
        executable_sha256: "546e71921bb0fa278c9d50dd8c61a27b835413552cb08a9359d0e320848e39d6",
    },
    Asset {
        key: "linux-arm64",
        file: "prime-agent-0.10.0-linux-arm64.tar.gz",
        sha256: "f3cab3530a4d7ca43dbef8321bf13f260d1ba05d8b5dde057165a20bd4f675c4",
        executable_sha256: "85887ad363dfb7a97ea038a80ca634ff61d05da29db4e1c7ba904b96914b2d62",
    },
];

/// The pinned asset for a system and CPU in Rust's names (`windows`/`macos`/`linux`, `x86_64`/`aarch64`), or `None`
/// where no build exists (Windows on ARM).
pub fn asset_for(os: &str, arch: &str) -> Option<&'static Asset> {
    let os = match os {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let key = format!("{os}-{arch}");
    ASSETS.iter().find(|asset| asset.key == key)
}

pub fn current_asset() -> Option<&'static Asset> {
    asset_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn release_url(asset: &Asset) -> String {
    format!("https://github.com/{REPO}/releases/download/v{VERSION}/{}", asset.file)
}

/// Where the verified install lives. The version is in the path, so a bump never reuses an old directory.
pub fn install_dir() -> PathBuf {
    crate::platform::agent_install_dir().join(VERSION)
}

/// Where the installer puts the agent it ships: an `agent` folder beside the app's own program. Not there in a development
/// run, which then downloads the pinned release instead.
pub fn bundled_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.join("agent");
    executable_in(&dir).is_file().then_some(dir)
}

pub fn executable_in(dir: &Path) -> PathBuf {
    dir.join(if cfg!(windows) {
        "prime-agent.exe"
    } else {
        "prime-agent"
    })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Why an executable cannot be trusted as the pinned build, or `None` when it is.
pub fn executable_problem(executable: &Path, asset: &Asset) -> Option<String> {
    match sha256_file(executable) {
        Err(error) => Some(error.to_string()),
        Ok(actual) if actual == asset.executable_sha256 => None,
        Ok(actual) => Some(format!(
            "{} is not the pinned build (sha256 {actual})",
            executable.display()
        )),
    }
}

/// Whether `binary` is exactly the pinned Prime Agent, by content and not by where it sits: a copy placed elsewhere
/// (through `PRIME_AGENT_PATH`, say) is the pinned agent if its hash says so. A 60 MB file is hashed once per path and
/// modification time.
pub fn is_pinned_agent(binary: &Path) -> bool {
    let Some(asset) = current_asset() else {
        return false;
    };
    let Ok(modified) = std::fs::metadata(binary).and_then(|meta| meta.modified()) else {
        return false;
    };
    static KNOWN: OnceLock<Mutex<HashMap<(PathBuf, SystemTime), bool>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    let key = (binary.to_path_buf(), modified);
    if let Some(answer) = known.lock().ok().and_then(|known| known.get(&key).copied()) {
        return answer;
    }
    let answer = executable_problem(binary, asset).is_none();
    if let Ok(mut known) = known.lock() {
        known.insert(key, answer);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_asset_is_named_for_its_key_and_carries_two_distinct_hashes() {
        assert_eq!(ASSETS.len(), 5);
        for asset in ASSETS {
            assert!(asset.file.contains(asset.key), "{}", asset.key);
            assert!(asset.file.contains(VERSION));
            for hash in [asset.sha256, asset.executable_sha256] {
                assert_eq!(hash.len(), 64, "{}", asset.key);
                assert!(hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
            }
            assert_ne!(asset.sha256, asset.executable_sha256);
        }
    }

    #[test]
    fn rust_names_map_to_the_release_names() {
        assert_eq!(asset_for("windows", "x86_64").unwrap().key, "win32-x64");
        assert_eq!(asset_for("macos", "aarch64").unwrap().key, "darwin-arm64");
        assert_eq!(asset_for("macos", "x86_64").unwrap().key, "darwin-x64");
        assert_eq!(asset_for("linux", "x86_64").unwrap().key, "linux-x64");
        assert_eq!(asset_for("linux", "aarch64").unwrap().key, "linux-arm64");
        assert!(
            asset_for("windows", "aarch64").is_none(),
            "no Windows on ARM build exists"
        );
        assert!(asset_for("freebsd", "x86_64").is_none());
    }

    #[test]
    fn the_url_names_the_tag_and_file() {
        let url = release_url(asset_for("linux", "x86_64").unwrap());
        assert_eq!(
            url,
            "https://github.com/PrimeIntellect-ai/prime-agent/releases/download/v0.10.0/prime-agent-0.10.0-linux-x64.tar.gz"
        );
    }

    #[test]
    fn a_file_is_pinned_only_when_its_hash_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("prime-agent");
        std::fs::write(&exe, b"not the agent").unwrap();
        let asset = Asset {
            key: "t",
            file: "t",
            sha256: "",
            executable_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        };
        assert!(
            executable_problem(&exe, &asset)
                .unwrap()
                .contains("not the pinned build")
        );
        let digest = hex::encode(Sha256::digest(b"not the agent"));
        let matching = Asset {
            key: "t",
            file: "t",
            sha256: "",
            executable_sha256: digest.as_str().to_string().leak(),
        };
        assert!(executable_problem(&exe, &matching).is_none());
        assert!(
            executable_problem(&dir.path().join("missing"), &matching)
                .unwrap()
                .contains("cannot read")
        );
        assert!(!is_pinned_agent(&exe), "the real hashes are for the real build");
        assert!(!is_pinned_agent(&dir.path().join("missing")));
    }
}

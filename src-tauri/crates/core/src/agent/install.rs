//! Hash-checked download and extraction of the pinned agent.
//!
//! The archive's hash is checked before anything is unpacked, the executable's after, and only then is the directory
//! moved into place, so a failure never leaves a half install behind. This does not run Prime Agent's own installer,
//! which also removes other Prime Agent installs.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::release::{self, Asset};

const MAX_ARCHIVE_BYTES: u64 = 120 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// GitHub serves release assets from its own CDN hosts after a redirect. Nothing else is followed.
const ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

pub fn is_allowed_download_url(value: &str) -> bool {
    url::Url::parse(value)
        .is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some_and(|host| ALLOWED_HOSTS.contains(&host)))
}

/// Where the archive comes from. The real one is HTTPS to GitHub; tests bring their own bytes.
pub trait Fetch {
    fn download(&self, url: &str, destination: &Path, max_bytes: u64, on_progress: &dyn Fn(f64)) -> Result<()>;
}

pub struct Https;

impl Fetch for Https {
    fn download(&self, url: &str, destination: &Path, max_bytes: u64, on_progress: &dyn Fn(f64)) -> Result<()> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .build()
            .into();
        let mut url = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            if !is_allowed_download_url(&url) {
                bail!("Refusing to download from {url}");
            }
            let mut response = agent
                .get(&url)
                .header("User-Agent", &crate::product::product().slug())
                .call()?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let Some(next) = response.headers().get("location").and_then(|value| value.to_str().ok()) else {
                    bail!("The download redirected without saying where");
                };
                url = url::Url::parse(&url)?.join(next)?.to_string();
                continue;
            }
            if status != 200 {
                bail!("The download failed with status {status}");
            }
            let total = response.body().content_length().unwrap_or(0);
            if total > max_bytes {
                bail!("The download is larger than the allowed limit");
            }
            let mut file = std::fs::File::create(destination)?;
            let mut reader = response.body_mut().as_reader();
            let (mut received, mut buffer) = (0u64, vec![0u8; 256 * 1024]);
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                received += n as u64;
                if received > max_bytes {
                    bail!("The download is larger than the allowed limit");
                }
                file.write_all(&buffer[..n])?;
                if total > 0 {
                    on_progress(received as f64 / total as f64);
                }
            }
            return Ok(file.sync_all()?);
        }
        bail!("The download redirected too many times")
    }
}

#[derive(Debug)]
pub struct InstallOutcome {
    pub executable: PathBuf,
    pub reused: bool,
}

/// Puts the pinned Prime Agent in `dir` and returns its executable. An install that already matches is left alone.
pub fn install_pinned(
    asset: &Asset,
    dir: &Path,
    fetch: &dyn Fetch,
    on_progress: &dyn Fn(f64),
) -> Result<InstallOutcome> {
    let executable = release::executable_in(dir);
    if release::executable_problem(&executable, asset).is_none() {
        return Ok(InstallOutcome {
            executable,
            reused: true,
        });
    }
    let work = PathBuf::from(format!("{}.partial-{}", dir.display(), std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let result = (|| -> Result<()> {
        let archive = work.join("archive.tar.gz");
        fetch.download(&release::release_url(asset), &archive, MAX_ARCHIVE_BYTES, on_progress)?;
        let actual = release::sha256_file(&archive)?;
        if actual != asset.sha256 {
            bail!("The downloaded archive does not match the pinned release (sha256 {actual}).");
        }
        let unpacked = work.join("unpacked");
        std::fs::create_dir_all(&unpacked)?;
        extract_tar_gz(&archive, &unpacked)?;
        let candidate = release::executable_in(&unpacked);
        if let Some(problem) = release::executable_problem(&candidate, asset) {
            bail!(problem);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755))?;
        }
        let _ = std::fs::remove_dir_all(dir);
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&unpacked, dir).with_context(|| format!("cannot move the install into {}", dir.display()))?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result?;
    Ok(InstallOutcome {
        executable,
        reused: false,
    })
}

/// Unpacks a `.tar.gz`. `unpack_in` refuses entries that would land outside `into` (`..`, absolute paths, links that
/// lead out), so a hostile archive cannot write elsewhere.
fn extract_tar_gz(archive: &Path, into: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.unpack_in(into)? {
            bail!(
                "The archive contains a path outside its own folder: {}",
                entry.path()?.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::cell::Cell;

    struct Bytes<'a> {
        data: &'a [u8],
        calls: Cell<usize>,
    }
    impl Fetch for Bytes<'_> {
        fn download(&self, _url: &str, destination: &Path, _max: u64, on_progress: &dyn Fn(f64)) -> Result<()> {
            self.calls.set(self.calls.get() + 1);
            std::fs::write(destination, self.data)?;
            on_progress(1.0);
            Ok(())
        }
    }

    fn leak(text: String) -> &'static str {
        text.leak()
    }

    /// A tar.gz holding one executable, and an asset whose hashes describe it.
    fn archive_with(exe_bytes: &[u8]) -> (Vec<u8>, Asset) {
        let name = release::executable_in(Path::new(""))
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(exe_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, &name, exe_bytes).unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_bytes).unwrap();
        let archive = gz.finish().unwrap();
        let asset = Asset {
            key: "test",
            file: "test.tar.gz",
            sha256: leak(hex::encode(Sha256::digest(&archive))),
            executable_sha256: leak(hex::encode(Sha256::digest(exe_bytes))),
        };
        (archive, asset)
    }

    /// Downloads the real release for this machine and installs it. Slow and needs the network:
    /// `cargo test --lib live_install -- --ignored`.
    #[test]
    #[ignore]
    fn live_install_of_the_pinned_release_verifies_and_runs() {
        let Some(asset) = release::current_asset() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(release::VERSION);
        let done = install_pinned(asset, &target, &Https, &|_| {}).unwrap();
        assert!(!done.reused);
        assert!(release::is_pinned_agent(&done.executable));
        let version = std::process::Command::new(&done.executable)
            .arg("--version")
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&version.stdout).contains(release::VERSION),
            "{version:?}"
        );
    }

    #[test]
    fn only_githubs_https_hosts_are_followed() {
        assert!(is_allowed_download_url(
            "https://github.com/PrimeIntellect-ai/prime-agent/releases/download/v0.10.0/x.tar.gz"
        ));
        assert!(is_allowed_download_url("https://objects.githubusercontent.com/abc"));
        assert!(!is_allowed_download_url("http://github.com/x"));
        assert!(!is_allowed_download_url("https://evil.example/github.com"));
        assert!(!is_allowed_download_url("https://github.com.evil.example/x"));
        assert!(!is_allowed_download_url("file:///etc/passwd"));
        assert!(!is_allowed_download_url("not a url"));
    }

    #[test]
    fn a_matching_archive_installs_and_a_second_call_reuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("agent").join("0.10.0");
        let (archive, asset) = archive_with(b"the pinned build");
        let fetch = Bytes {
            data: &archive,
            calls: Cell::new(0),
        };
        let done = install_pinned(&asset, &target, &fetch, &|_| {}).unwrap();
        assert!(!done.reused);
        assert_eq!(std::fs::read(&done.executable).unwrap(), b"the pinned build");
        assert!(
            !dir.path().join("agent").read_dir().unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("partial"))
        );
        let again = install_pinned(&asset, &target, &fetch, &|_| {}).unwrap();
        assert!(again.reused);
        assert_eq!(fetch.calls.get(), 1, "an install that matches is not downloaded again");
    }

    #[test]
    fn a_wrong_archive_or_wrong_executable_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("0.10.0");
        let (archive, mut asset) = archive_with(b"build");
        let wrong_archive = Asset {
            sha256: leak("0".repeat(64)),
            ..asset
        };
        let err = install_pinned(
            &wrong_archive,
            &target,
            &Bytes {
                data: &archive,
                calls: Cell::new(0),
            },
            &|_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not match the pinned release"), "{err}");
        assert!(!target.exists());

        asset.executable_sha256 = leak("1".repeat(64));
        let err = install_pinned(
            &asset,
            &target,
            &Bytes {
                data: &archive,
                calls: Cell::new(0),
            },
            &|_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the pinned build"), "{err}");
        assert!(!target.exists());
        assert_eq!(
            dir.path().read_dir().unwrap().count(),
            0,
            "no partial directory is left"
        );
    }

    #[test]
    fn a_bad_install_in_place_is_replaced_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("0.10.0");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(release::executable_in(&target), b"tampered").unwrap();
        let (archive, asset) = archive_with(b"the pinned build");
        let done = install_pinned(
            &asset,
            &target,
            &Bytes {
                data: &archive,
                calls: Cell::new(0),
            },
            &|_| {},
        )
        .unwrap();
        assert!(!done.reused);
        assert_eq!(std::fs::read(done.executable).unwrap(), b"the pinned build");
    }

    #[test]
    fn an_archive_cannot_write_outside_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        // A raw name with a parent segment, which the safe `append_data` path would refuse to write.
        header.as_old_mut().name[..12].copy_from_slice(b"../escaped.x");
        header.set_cksum();
        builder.append(&header, &b"evil"[..]).unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_bytes).unwrap();
        let archive = dir.path().join("a.tar.gz");
        std::fs::write(&archive, gz.finish().unwrap()).unwrap();
        let into = dir.path().join("into");
        std::fs::create_dir_all(&into).unwrap();
        assert!(extract_tar_gz(&archive, &into).is_err());
        assert!(!dir.path().join("escaped.x").exists());
    }
}

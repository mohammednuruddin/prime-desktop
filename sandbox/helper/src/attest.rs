//! Attestation: a record that the probe suite passed on this host, for this backend and this helper build.
//!
//! The helper reports `enforced` only when a matching record exists, and `unattested` otherwise. The record is a
//! guard against claiming protection that nothing has checked, and against a Windows update quietly changing what
//! the backend does: the host build is part of the key, so an update means attesting again. It is not a defence
//! against malware running as the user, which could write the file as easily as it could do anything else.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attestation {
    pub schema: u32,
    pub backend: String,
    pub helper_build: String,
    pub host: String,
    pub catalog: String,
    pub pass: usize,
    pub known_gaps: Vec<String>,
    pub unavailable: Vec<String>,
    pub at: u64,
}

const SCHEMA: u32 = 1;

pub(crate) fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PRIME_DESKTOP_SANDBOX_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_STATE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("Prime Desktop").join("sandbox")
}

fn file(backend: &str) -> PathBuf {
    state_dir().join(format!("attestation-{backend}.json"))
}

/// The operating system build. A Windows update changes it, which is the point.
#[cfg(windows)]
pub fn host_id() -> String {
    use std::ptr::null_mut;

    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};

    let key: Vec<u16> = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"
        .encode_utf16()
        .chain([0])
        .collect();
    let read = |name: &str, flags: u32, buffer: &mut [u16]| -> Option<usize> {
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: both strings are NUL-terminated and the buffer size is passed in bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        (status == 0).then_some(size as usize)
    };
    let mut build = [0u16; 32];
    let build_text = read("CurrentBuildNumber", RRF_RT_REG_SZ, &mut build)
        .map(|bytes| String::from_utf16_lossy(&build[..(bytes / 2).saturating_sub(1)]))
        .unwrap_or_else(|| "unknown".into());
    let mut ubr = [0u16; 2];
    let revision = read("UBR", RRF_RT_REG_DWORD, &mut ubr).map(|_| (ubr[0] as u32) | ((ubr[1] as u32) << 16));
    format!(
        "windows-{build_text}.{}",
        revision.map_or("?".to_string(), |r| r.to_string())
    )
}

/// The operating system build. A kernel update changes it, which is the point.
#[cfg(not(windows))]
pub fn host_id() -> String {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    format!("{}-{}", std::env::consts::OS, release.trim())
}

/// Identifies this exact helper executable. A rebuild is a different build, so the record cannot outlive the code
/// it vouched for. FNV-1a over the file: it only has to change when the file does.
pub fn helper_build() -> String {
    let bytes = std::env::current_exe().and_then(std::fs::read).unwrap_or_default();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in &bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{}-{hash:016x}", env!("CARGO_PKG_VERSION"))
}

pub fn read(backend: &str) -> Option<Attestation> {
    let text = std::fs::read_to_string(file(backend)).ok()?;
    let record: Attestation = serde_json::from_str(&text).ok()?;
    (record.schema == SCHEMA && record.backend == backend).then_some(record)
}

/// Whether the record on disk vouches for this backend, this host build and this helper build.
pub fn is_valid(backend: &str) -> bool {
    read(backend).is_some_and(|r| r.host == host_id() && r.helper_build == helper_build())
}

pub fn write(record: &Attestation) -> Result<(), String> {
    let path = file(&record.backend);
    std::fs::create_dir_all(path.parent().expect("a file has a parent")).map_err(|e| e.to_string())?;
    std::fs::write(&path, serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

pub fn clear(backend: &str) {
    let _ = std::fs::remove_file(file(backend));
}

pub fn new_record(
    backend: &str,
    catalog: String,
    pass: usize,
    known_gaps: Vec<String>,
    unavailable: Vec<String>,
) -> Attestation {
    Attestation {
        schema: SCHEMA,
        backend: backend.to_string(),
        helper_build: helper_build(),
        host: host_id(),
        catalog,
        pass,
        known_gaps,
        unavailable,
        at: crate::exec::now_ms(),
    }
}

use std::io::Read;
use std::thread::{self, JoinHandle};

/// Reads a stream, keeps the first `cap` bytes, and drains the rest so the child never blocks on a full pipe.
/// Returns what it kept and whether it dropped anything.
pub fn read_capped(stream: Box<dyn Read + Send>, cap: u64) -> JoinHandle<(Vec<u8>, bool)> {
    thread::spawn(move || {
        let mut stream = stream;
        let (mut kept, mut truncated) = (Vec::new(), false);
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = (cap as usize).saturating_sub(kept.len());
                    if n <= room {
                        kept.extend_from_slice(&chunk[..n]);
                    } else {
                        kept.extend_from_slice(&chunk[..room]);
                        truncated = true;
                    }
                }
            }
        }
        (kept, truncated)
    })
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// UTF-8 text from child output. A cut can land inside a character, so invalid bytes are replaced, not an error.
pub fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A path as a user and a native API expect it: without the `\\?\` prefix `canonicalize` adds on Windows.
pub fn plain_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => text.into_owned(),
    }
}

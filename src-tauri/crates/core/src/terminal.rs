//! Pseudo-terminals for the terminal panel, with a byte-offset ring so a reloaded page can re-attach.
//!
//! One shell per agent id, on a pseudo-terminal from `portable-pty` (forkpty on Unix, ConPTY on Windows). Each session
//! runs two threads: a reader blocked on the terminal, and a pump that decodes, coalesces and delivers the output.
//!
//! Offsets are byte offsets into the UTF-8 text the session has delivered so far (the TypeScript counted UTF-16 units).
//! The renderer slices an event's text by an offset difference only when an event straddles the snapshot it just
//! applied, and that cannot happen here: events and snapshots are cut at the same points, so the difference is 0 or
//! the whole event. The last [`MAX_BUFFER`] bytes are kept. A snapshot serves the range `offset - buffer.len() ..
//! offset`; the renderer always replays the snapshot's buffer into a cleared terminal, so it never needs more.
//!
//! The host builds one [`Terminals`] with a sink that forwards [`Event::Data`] to `terminal:data` and [`Event::Exit`]
//! to `terminal:exit`. The sink runs on the pump thread while the session's output lock is held (that is what keeps
//! events and snapshots cut at the same points), so it must only queue the event and never call back into
//! [`Terminals`]. Every method blocks briefly; closing a shell can take a quarter of a second. Run them off the async
//! runtime.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;

use crate::platform;

/// How much output a snapshot replays: the TypeScript's limit.
pub const MAX_BUFFER: usize = 200_000;
/// Output is delivered at most this often once it is flowing (the first bytes after a quiet moment go out at once, so
/// typing feels immediate) ...
const FLUSH_DELAY: Duration = Duration::from_millis(16);
/// ... or sooner when this much has piled up.
const FLUSH_BYTES: usize = 64 * 1024;
const READ_CHUNK: usize = 16 * 1024;
/// How often a quiet session is asked whether its shell has exited.
const EXIT_POLL: Duration = Duration::from_millis(50);
/// After the shell exits, how long to wait for the terminal's last output. Unix ends the stream when the shell goes;
/// ConPTY keeps it open until the pseudo-console is closed, so there the wait is what ends it.
const EXIT_GRACE: Duration = Duration::from_millis(300);

/// What the renderer's `terminal:data` event carries.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataEvent {
    pub agent_id: String,
    pub data: String,
    pub start_offset: u64,
    pub end_offset: u64,
}

/// What the renderer's `terminal:exit` event carries.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitEvent {
    pub agent_id: String,
    pub exit_code: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Data(DataEvent),
    Exit(ExitEvent),
}

pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

/// What `terminalStart` and `terminalRestart` return.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub running: bool,
    pub pid: u32,
    /// The most recent output, at most [`MAX_BUFFER`] bytes, starting on a character boundary.
    pub buffer: String,
    pub cwd: String,
    /// The offset just after `buffer`; the next [`DataEvent`] starts here.
    pub offset: u64,
}

pub struct Terminals {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    sink: Sink,
    /// Replaces the platform's shell. Only tests set it, so they do not depend on the machine's `$SHELL`.
    shell: Option<(String, Vec<String>)>,
}

struct Session {
    pid: u32,
    cwd: String,
    output: Mutex<Output>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
}

#[derive(Default)]
struct Output {
    ring: Vec<u8>,
    offset: u64,
    /// Set when the session is closed or over: nothing more is delivered or recorded.
    closed: bool,
}

enum Msg {
    Text(String),
    Eof,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn clamp(value: Option<f64>, default: f64, min: f64, max: f64) -> u16 {
    value
        .filter(|value| value.is_finite())
        .unwrap_or(default)
        .floor()
        .clamp(min, max) as u16
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl Terminals {
    pub fn new(sink: Sink) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions: Mutex::default(),
                sink,
                shell: None,
            }),
        }
    }

    /// The session for `agent_id`, started in `cwd` (the home folder if that is not a folder) if there is none yet.
    /// A running session is returned as it is, buffer included, which is how a reloaded page re-attaches; `cols` and
    /// `rows` then do not apply. Sizes are clamped to 20..=500 columns and 5..=200 rows.
    pub fn start(&self, agent_id: &str, cwd: &Path, cols: Option<f64>, rows: Option<f64>) -> Result<Snapshot> {
        // Held through the spawn: two pages starting the same agent at once must get one shell, not two.
        let mut sessions = lock(&self.inner.sessions);
        if let Some(existing) = sessions.get(agent_id) {
            return Ok(existing.snapshot());
        }
        let session = self.inner.spawn(
            agent_id,
            cwd,
            clamp(cols, 100.0, 20.0, 500.0),
            clamp(rows, 30.0, 5.0, 200.0),
        )?;
        sessions.insert(agent_id.to_string(), session.clone());
        Ok(session.snapshot())
    }

    /// Input for the shell. Silent when there is no session or the shell is already gone: there is nowhere to send it
    /// and nothing for the page to do about it.
    pub fn write(&self, agent_id: &str, data: &str) {
        let Some(session) = self.get(agent_id) else { return };
        if let Some(writer) = lock(&session.writer).as_mut() {
            let _ = writer.write_all(data.as_bytes()).and_then(|()| writer.flush());
        }
    }

    pub fn resize(&self, agent_id: &str, cols: f64, rows: f64) {
        let Some(session) = self.get(agent_id) else { return };
        if let Some(master) = lock(&session.master).as_ref() {
            let _ = master.resize(size(
                clamp(Some(cols), 100.0, 20.0, 500.0),
                clamp(Some(rows), 30.0, 5.0, 200.0),
            ));
        }
    }

    pub fn restart(&self, agent_id: &str, cwd: &Path, cols: Option<f64>, rows: Option<f64>) -> Result<Snapshot> {
        self.close(agent_id);
        self.start(agent_id, cwd, cols, rows)
    }

    /// Forgets the replay buffer. The offset keeps counting, so a page that already has the old output stays in step.
    pub fn clear(&self, agent_id: &str) {
        if let Some(session) = self.get(agent_id) {
            lock(&session.output).ring.clear();
        }
    }

    /// Ends the shell and releases the pseudo-terminal. No event is sent for it, and none arrives after this returns.
    pub fn close(&self, agent_id: &str) {
        let session = lock(&self.inner.sessions).remove(agent_id);
        if let Some(session) = session {
            session.shutdown();
        }
    }

    pub fn shutdown_all(&self) {
        let ids: Vec<String> = lock(&self.inner.sessions).keys().cloned().collect();
        for id in ids {
            self.close(&id);
        }
    }

    fn get(&self, agent_id: &str) -> Option<Arc<Session>> {
        lock(&self.inner.sessions).get(agent_id).cloned()
    }

    #[cfg(test)]
    fn with_shell(sink: Sink, shell: (String, Vec<String>)) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions: Mutex::default(),
                sink,
                shell: Some(shell),
            }),
        }
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

impl Inner {
    fn spawn(self: &Arc<Self>, agent_id: &str, cwd: &Path, cols: u16, rows: u16) -> Result<Arc<Session>> {
        let (shell, args) = self.shell.clone().unwrap_or_else(platform::terminal_shell);
        let cwd = if cwd.is_dir() {
            cwd.to_path_buf()
        } else {
            platform::home_dir()
        };
        let fail = |error: anyhow::Error| anyhow!("Could not start the terminal ({shell}): {error:#}");

        // `portable-pty` starts the shell itself, through the system's pseudo-terminal API; no console window can open,
        // so this does not go through `platform::quiet`.
        let pair = native_pty_system().openpty(size(cols, rows)).map_err(fail)?;
        let mut command = CommandBuilder::new(&shell);
        command.args(&args);
        command.cwd(&cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let mut child = pair.slave.spawn_command(command).map_err(fail)?;
        // The shell has its own end. Keeping ours would keep the terminal open after the shell exits.
        drop(pair.slave);
        let (reader, writer) = match pair
            .master
            .try_clone_reader()
            .and_then(|r| Ok((r, pair.master.take_writer()?)))
        {
            Ok(io) => io,
            Err(error) => {
                let _ = child.kill();
                return Err(fail(error));
            }
        };

        let session = Arc::new(Session {
            pid: child.process_id().unwrap_or(0),
            cwd: cwd.to_string_lossy().into_owned(),
            output: Mutex::default(),
            child: Mutex::new(child),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
        });
        let (tx, rx) = mpsc::channel();
        let keep_open = tx.clone();
        std::thread::spawn(move || read_output(reader, tx));
        let (inner, pumped, id) = (self.clone(), session.clone(), agent_id.to_string());
        std::thread::spawn(move || pump(rx, keep_open, inner, pumped, id));
        Ok(session)
    }
}

impl Session {
    fn snapshot(&self) -> Snapshot {
        let output = lock(&self.output);
        // The ring is cut at a byte count, which can land inside a character: start at the first whole one.
        let start = output
            .ring
            .iter()
            .position(|byte| byte & 0xC0 != 0x80)
            .unwrap_or(output.ring.len());
        Snapshot {
            running: true,
            pid: self.pid,
            buffer: String::from_utf8_lossy(&output.ring[start..]).into_owned(),
            cwd: self.cwd.clone(),
            offset: output.offset,
        }
    }

    fn shutdown(&self) {
        // Waits for a delivery in progress, so nothing is delivered after this returns.
        lock(&self.output).closed = true;
        // The shell first. Dropping the master afterwards closes the pseudo-console, which ends anything still attached.
        let _ = lock(&self.child).kill();
        // A write stuck on a full terminal holds this lock. Linux hangs the terminal up when the shell dies, which fails
        // that write, but ConPTY's behavior is not known: not waiting costs nothing: the writer is dropped with the session once that write returns.
        if let Ok(mut writer) = self.writer.try_lock() {
            writer.take();
        }
        lock(&self.master).take();
    }
}

/// Reads the terminal until it ends, turning bytes into text. A character split across two reads is held back until
/// the rest arrives, so it never reaches the page as two replacement characters.
fn read_output(mut reader: Box<dyn Read + Send>, tx: Sender<Msg>) {
    let mut chunk = vec![0u8; READ_CHUNK];
    let mut carry = Vec::new();
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                carry.extend_from_slice(&chunk[..count]);
                let text = take_text(&mut carry);
                if !text.is_empty() && tx.send(Msg::Text(text)).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            // Linux reports the shell's exit as an I/O error on the master.
            Err(_) => break,
        }
    }
    // The stream ended inside a character; nothing will complete it now.
    if !carry.is_empty() {
        let _ = tx.send(Msg::Text(String::from_utf8_lossy(&carry).into_owned()));
    }
    let _ = tx.send(Msg::Eof);
}

/// Removes the text from the front of `bytes` and returns it. Invalid bytes become U+FFFD; an incomplete character at
/// the very end stays in `bytes` for the next read.
fn take_text(bytes: &mut Vec<u8>) -> String {
    let mut text = String::new();
    let mut keep = 0;
    let mut chunks = bytes.utf8_chunks().peekable();
    while let Some(chunk) = chunks.next() {
        text.push_str(chunk.valid());
        let invalid = chunk.invalid();
        if invalid.is_empty() {
            continue;
        }
        let cut_short = std::str::from_utf8(invalid).is_err_and(|error| error.error_len().is_none());
        if cut_short && chunks.peek().is_none() {
            keep = invalid.len();
        } else {
            text.push(char::REPLACEMENT_CHARACTER);
        }
    }
    bytes.drain(..bytes.len() - keep);
    text
}

/// Gathers a session's output into events and reports its end. Runs until the shell is gone and its output is read.
fn pump(rx: Receiver<Msg>, _keep_open: Sender<Msg>, inner: Arc<Inner>, session: Arc<Session>, agent_id: String) {
    // `_keep_open` stops a finished reader from disconnecting the channel, which would turn the waits below into a spin.
    let mut pending = String::new();
    let mut last_flush = Instant::now();
    let mut eof = false;
    let mut exited: Option<(u32, Instant)> = None;
    loop {
        let now = Instant::now();
        let mut wake = now + EXIT_POLL;
        if !pending.is_empty() {
            wake = wake.min(last_flush + FLUSH_DELAY);
        }
        let idle = match rx.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(Msg::Text(text)) => {
                pending.push_str(&text);
                false
            }
            Ok(Msg::Eof) => {
                eof = true;
                true
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => true,
        };

        let now = Instant::now();
        if idle && exited.is_none() {
            let status = lock(&session.child).try_wait();
            match status {
                Ok(None) => {}
                Ok(Some(status)) => exited = Some((status.exit_code(), now + EXIT_GRACE)),
                Err(_) => exited = Some((1, now + EXIT_GRACE)),
            }
        }
        let finished = exited.is_some_and(|(_, grace)| eof || now >= grace);
        let due = pending.len() >= FLUSH_BYTES || now >= last_flush + FLUSH_DELAY || finished;
        if due && !pending.is_empty() {
            flush(&inner, &session, &agent_id, std::mem::take(&mut pending));
            last_flush = now;
        }
        if let Some((code, _)) = exited.filter(|_| finished) {
            end(&inner, &session, &agent_id, code);
            return;
        }
    }
}

/// Records `data` and hands it to the sink, in one step under the output lock: a snapshot taken at any moment holds
/// exactly the output of the events delivered before it.
fn flush(inner: &Inner, session: &Session, agent_id: &str, data: String) {
    let mut output = lock(&session.output);
    if output.closed {
        return;
    }
    let start_offset = output.offset;
    output.offset += data.len() as u64;
    output.ring.extend_from_slice(data.as_bytes());
    if output.ring.len() > MAX_BUFFER {
        let excess = output.ring.len() - MAX_BUFFER;
        output.ring.drain(..excess);
    }
    let end_offset = output.offset;
    (inner.sink)(Event::Data(DataEvent {
        agent_id: agent_id.to_string(),
        data,
        start_offset,
        end_offset,
    }));
}

/// The shell ended on its own. A session that was closed or replaced meanwhile says nothing.
fn end(inner: &Inner, session: &Arc<Session>, agent_id: &str, exit_code: u32) {
    let mut sessions = lock(&inner.sessions);
    if !sessions
        .get(agent_id)
        .is_some_and(|current| Arc::ptr_eq(current, session))
    {
        return;
    }
    sessions.remove(agent_id);
    drop(sessions);
    lock(&session.output).closed = true;
    (inner.sink)(Event::Exit(ExitEvent {
        agent_id: agent_id.to_string(),
        exit_code,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the decoder, which needs no shell ----

    fn feed(parts: &[&[u8]]) -> (String, Vec<u8>) {
        let mut carry = Vec::new();
        let mut text = String::new();
        for part in parts {
            carry.extend_from_slice(part);
            text.push_str(&take_text(&mut carry));
        }
        (text, carry)
    }

    #[test]
    fn a_character_split_across_reads_arrives_whole() {
        for (character, bytes) in [("é", "é".as_bytes()), ("€", "€".as_bytes()), ("🧬", "🧬".as_bytes())] {
            for cut in 1..bytes.len() {
                let (head, tail) = bytes.split_at(cut);
                let mut carry = head.to_vec();
                assert_eq!(
                    take_text(&mut carry),
                    "",
                    "{character} cut at {cut}: nothing complete yet"
                );
                assert_eq!(carry, head, "{character} cut at {cut}: the start is kept");
                carry.extend_from_slice(tail);
                assert_eq!(take_text(&mut carry), character);
                assert!(carry.is_empty());
            }
        }
    }

    #[test]
    fn every_way_of_splitting_a_stream_decodes_to_the_same_text() {
        let original = "plain ascii, héllo €uro 🧬 dna, 日本語\r\n";
        let bytes = original.as_bytes();
        for first in 0..=bytes.len() {
            for second in first..=bytes.len() {
                let (text, carry) = feed(&[&bytes[..first], &bytes[first..second], &bytes[second..]]);
                assert_eq!(text, original, "cut at {first} and {second}");
                assert!(carry.is_empty());
            }
        }
    }

    #[test]
    fn bytes_that_are_not_utf8_become_one_replacement_each_run() {
        assert_eq!(feed(&[b"a\xFFb"]), ("a\u{FFFD}b".into(), vec![]));
        assert_eq!(feed(&[b"\x80"]), ("\u{FFFD}".into(), vec![]));
        // A lead byte followed by something that cannot continue it is invalid now, not "incomplete".
        assert_eq!(feed(&[b"\xE2a"]), ("\u{FFFD}a".into(), vec![]));
        assert_eq!(feed(&[b"\xE2\x82a"]), ("\u{FFFD}a".into(), vec![]));
        // The same bytes split before the `a` give the same text.
        assert_eq!(feed(&[b"\xE2\x82", b"a"]), ("\u{FFFD}a".into(), vec![]));
        // Only a tail that could still become a character is held back.
        assert_eq!(feed(&[b"ab\xE2\x82"]), ("ab".into(), vec![0xE2, 0x82]));
        assert_eq!(feed(&[b"ab\xFF"]), ("ab\u{FFFD}".into(), vec![]));
        assert_eq!(feed(&[b""]), (String::new(), vec![]));
    }

    #[test]
    fn sizes_are_floored_and_clamped_and_unusable_values_fall_back() {
        assert_eq!(clamp(None, 100.0, 20.0, 500.0), 100);
        assert_eq!(clamp(Some(f64::NAN), 100.0, 20.0, 500.0), 100);
        assert_eq!(clamp(Some(f64::INFINITY), 100.0, 20.0, 500.0), 100);
        assert_eq!(clamp(Some(99.9), 100.0, 20.0, 500.0), 99);
        assert_eq!(clamp(Some(1.0), 100.0, 20.0, 500.0), 20);
        assert_eq!(clamp(Some(-4.0), 30.0, 5.0, 200.0), 5);
        assert_eq!(clamp(Some(9999.0), 30.0, 5.0, 200.0), 200);
    }

    #[test]
    fn events_serialize_as_the_renderer_expects() {
        let data = DataEvent {
            agent_id: "agent-1".into(),
            data: "hi".into(),
            start_offset: 3,
            end_offset: 5,
        };
        assert_eq!(
            serde_json::to_value(&data).unwrap(),
            serde_json::json!({"agentId": "agent-1", "data": "hi", "startOffset": 3, "endOffset": 5})
        );
        let exit = ExitEvent {
            agent_id: "agent-1".into(),
            exit_code: 2,
        };
        assert_eq!(
            serde_json::to_value(&exit).unwrap(),
            serde_json::json!({"agentId": "agent-1", "exitCode": 2})
        );
        let snapshot = Snapshot {
            running: true,
            pid: 7,
            buffer: "b".into(),
            cwd: "/x".into(),
            offset: 1,
        };
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap(),
            serde_json::json!({"running": true, "pid": 7, "buffer": "b", "cwd": "/x", "offset": 1})
        );
    }

    // ---- real shells: `/bin/sh` on Unix, PowerShell on Windows ----

    fn test_shell() -> (String, Vec<String>) {
        if cfg!(windows) {
            ("powershell.exe".into(), vec!["-NoLogo".into(), "-NoProfile".into()])
        } else {
            ("/bin/sh".into(), vec![])
        }
    }

    /// `unix` for the Unix shell, `windows` for PowerShell, then Enter the way a terminal sends it.
    fn command(unix: &str, windows: &str) -> String {
        format!("{}\r", if cfg!(windows) { windows } else { unix })
    }

    fn marker(name: &str) -> (String, String) {
        // The shell echoes what is typed, so the typed form must not contain the printed one.
        (
            command(&format!("echo {name}$((1+1))"), &format!("'{name}' + (1+1)")),
            format!("{name}2"),
        )
    }

    /// One line that runs a command and then prints a marker, so the marker follows all of the command's output and no
    /// echo of a second typed line lands in the middle of it.
    fn then_marker(unix: &str, windows: &str, name: &str) -> (String, String) {
        (
            command(
                &format!("{unix}; echo {name}$((1+1))"),
                &format!("{windows}; '{name}' + (1+1)"),
            ),
            format!("{name}2"),
        )
    }

    struct Rig {
        terminals: Terminals,
        events: Arc<Mutex<Vec<Event>>>,
        /// Terminals whose cursor-position query has been answered (see `wait_until`).
        answered: Mutex<std::collections::HashSet<String>>,
    }

    impl Rig {
        fn new() -> Self {
            Self::with_delay(Duration::ZERO)
        }

        /// A sink that takes `delay` per data event, so the pump falls behind the shell and output queues up.
        fn with_delay(delay: Duration) -> Self {
            let events = Arc::new(Mutex::new(Vec::new()));
            let sink: Sink = {
                let events = events.clone();
                Arc::new(move |event| {
                    std::thread::sleep(delay);
                    lock(&events).push(event)
                })
            };
            Self {
                terminals: Terminals::with_shell(sink, test_shell()),
                events,
                answered: Mutex::default(),
            }
        }

        fn start(&self, id: &str) -> Snapshot {
            self.terminals.start(id, &std::env::temp_dir(), None, None).unwrap()
        }

        fn events(&self) -> Vec<Event> {
            lock(&self.events).clone()
        }

        fn data(&self, id: &str) -> Vec<DataEvent> {
            let all = self.events();
            all.into_iter()
                .filter_map(|event| match event {
                    Event::Data(data) if data.agent_id == id => Some(data),
                    _ => None,
                })
                .collect()
        }

        fn exits(&self) -> Vec<ExitEvent> {
            let all = self.events();
            all.into_iter()
                .filter_map(|event| match event {
                    Event::Exit(exit) => Some(exit),
                    _ => None,
                })
                .collect()
        }

        /// Everything delivered for `id`, checked to be one unbroken run starting at offset zero.
        fn stream(&self, id: &str) -> String {
            let mut text = String::new();
            for event in self.data(id) {
                assert_eq!(
                    event.start_offset,
                    text.len() as u64,
                    "events are contiguous and in order"
                );
                assert_eq!(event.end_offset - event.start_offset, event.data.len() as u64);
                assert!(!event.data.is_empty());
                text.push_str(&event.data);
            }
            text
        }

        /// ConPTY prints nothing until the terminal answers its cursor-position query. In the app the page's terminal does
        /// (xterm.js); here there is no page, so the test does, once per shell (`forget_answer` after a restart).
        fn answer(&self, id: &str) {
            let asked = self.data(id).iter().any(|event| event.data.contains("\u{1b}[6n"));
            if asked && lock(&self.answered).insert(id.to_string()) {
                self.terminals.write(id, "\u{1b}[1;1R");
            }
        }

        fn forget_answer(&self, id: &str) {
            lock(&self.answered).remove(id);
        }

        fn wait_until(&self, what: &str, id: &str, seconds: u64, mut done: impl FnMut(&str) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(seconds);
            loop {
                let text = self.stream(id);
                self.answer(id);
                if done(&text) {
                    return;
                }
                if Instant::now() > deadline {
                    let tail: String = text
                        .chars()
                        .rev()
                        .take(400)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    panic!("timed out waiting for {what}; the output ends: {tail:?}");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn wait_for(&self, id: &str, needle: &str) {
            // Several shells start at once in a test run, and PowerShell prints slowly while they do.
            let seconds = if cfg!(windows) { 120 } else { 30 };
            self.wait_until(&format!("{needle:?}"), id, seconds, |text| text.contains(needle));
        }

        fn say(&self, id: &str, line: &str, expect: &str) {
            self.terminals.write(id, line);
            self.wait_for(id, expect);
        }
    }

    /// Whether a process still exists. A zombie that nobody has reaped yet does not count: it is already dead.
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        use std::process::Command;
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&out.stdout);
        !state.trim().is_empty() && !state.trim().starts_with('Z')
    }

    #[cfg(unix)]
    fn wait_gone(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) {
            assert!(Instant::now() < deadline, "process {pid} is still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_shell_runs_and_its_output_reaches_the_sink() {
        let rig = Rig::new();
        let snapshot = rig.start("a");
        assert!(snapshot.running && snapshot.pid > 0);
        assert_eq!((snapshot.offset, snapshot.buffer.as_str()), (0, ""));
        let (line, expect) = marker("HELLO");
        rig.say("a", &line, &expect);
        let text = rig.stream("a");
        assert!(text.contains(&expect));
        assert_eq!(rig.data("a")[0].start_offset, 0);
        assert!(rig.exits().is_empty());
    }

    #[test]
    fn a_two_hundred_thousand_line_burst_arrives_complete_in_order_and_coalesced() {
        let rig = Rig::new();
        rig.start("a");
        let (line, done) = then_marker(
            "seq -f line%g 1 200000",
            "1..200000 | ForEach-Object { \"line$_\" }",
            "DONE",
        );
        rig.terminals.write("a", &line);
        // PowerShell prints 200,000 lines slowly when the other shells of a test run compete for the machine.
        let seconds = if cfg!(windows) { 400 } else { 120 };
        rig.wait_until("the end of the burst", "a", seconds, |text| text.contains(&done));

        let text = rig.stream("a");
        // The first line can follow the shell's own prompt on the same row, so take what comes after the last "line".
        let numbers: Vec<u32> = text
            .lines()
            .filter_map(|row| row.trim_end().rsplit("line").next()?.parse().ok())
            .collect();
        assert_eq!(numbers.len(), 200_000, "no line lost or doubled");
        assert!(numbers.iter().copied().eq(1..=200_000), "in order");

        let events = rig.data("a");
        // ConPTY hands output over in smaller reads than a Unix pty does, so the bound is looser there; it is still far
        // below one event per line.
        let most = if cfg!(windows) { 20_000 } else { 400 };
        assert!(
            events.len() < most,
            "{} events: output is coalesced, not one event per read",
            events.len()
        );
        let biggest = events.iter().map(|event| event.data.len()).max().unwrap();
        assert!(
            biggest <= FLUSH_BYTES + READ_CHUNK + 4,
            "an event of {biggest} bytes is past the flush threshold"
        );

        // The replay buffer holds exactly the end of that stream, at the limit.
        let snapshot = rig.start("a");
        let delivered: String = events
            .iter()
            .filter(|event| event.end_offset <= snapshot.offset)
            .map(|e| &*e.data)
            .collect();
        assert_eq!(snapshot.offset as usize, delivered.len());
        assert!(delivered.ends_with(&snapshot.buffer));
        assert!(snapshot.buffer.len() <= MAX_BUFFER && snapshot.buffer.len() >= MAX_BUFFER - 3);
    }

    #[cfg(unix)]
    #[test]
    fn a_character_split_across_two_reads_is_not_corrupted() {
        let rig = Rig::new();
        rig.start("a");
        // The euro sign is E2 82 AC; the shell prints the first two bytes, waits, then prints the last.
        let (line, done) = then_marker(
            "printf '\\342\\202'; sleep 0.3; printf '\\254 \\303\\251\\n'",
            "",
            "DONE",
        );
        rig.terminals.write("a", &line);
        rig.wait_for("a", &done);
        let text = rig.stream("a");
        assert!(
            text.contains("\u{20AC} \u{E9}"),
            "the split character is whole: {text:?}"
        );
        assert!(!text.contains('\u{FFFD}'), "no replacement characters: {text:?}");
        let events = rig.data("a");
        let euro = events.iter().position(|event| event.data.contains('\u{20AC}')).unwrap();
        assert!(!events[euro].data.starts_with('\u{FFFD}'));
    }

    #[test]
    fn what_is_typed_is_sent_to_the_shell_as_utf8() {
        let rig = Rig::new();
        rig.start("a");
        let line = command("echo \"héllo wörld $((1+1))\"", "'héllo wörld ' + (1+1)");
        rig.say("a", &line, "héllo wörld 2");
    }

    #[test]
    fn resize_reaches_the_shell_and_is_clamped() {
        let rig = Rig::new();
        let size = |cols: u32, rows: u32| {
            if cfg!(windows) {
                format!("SIZE{cols}x{rows}")
            } else {
                format!("SIZE{rows} {cols}")
            }
        };
        let ask = |name: &str| {
            command(
                &format!("echo {name}$(stty size)"),
                &format!("'{name}' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight"),
            )
        };
        let snapshot = rig
            .terminals
            .start("a", &std::env::temp_dir(), Some(80.7), Some(24.2))
            .unwrap();
        assert!(snapshot.running);
        rig.say("a", &ask("SIZE"), &size(80, 24));
        for (n, (cols, rows, want)) in [
            (120.0, 40.0, (120, 40)),
            (5.0, 1.0, (20, 5)),
            (900.0, 900.0, (500, 200)),
        ]
        .into_iter()
        .enumerate()
        {
            rig.terminals.resize("a", cols, rows);
            // A fresh name each time, so the previous answer cannot satisfy the wait.
            let name = format!("S{n}Z");
            let expect = size(want.0, want.1).replace("SIZE", &name);
            rig.say("a", &ask(&name), &expect);
        }
    }

    #[test]
    fn a_second_start_re_attaches_to_the_same_shell_with_its_buffer() {
        let rig = Rig::new();
        let first = rig.start("a");
        let (line, expect) = marker("BEFORE");
        rig.say("a", &line, &expect);

        let again = rig
            .terminals
            .start("a", &std::env::temp_dir(), Some(30.0), Some(10.0))
            .unwrap();
        assert_eq!(again.pid, first.pid, "same shell");
        assert!(again.buffer.contains(&expect));
        assert_eq!(again.cwd, first.cwd);
        assert!(again.offset > 0);

        // A page that attaches here and then applies the events it is sent sees the stream with nothing lost or twice.
        let (line, later) = marker("AFTER");
        rig.say("a", &line, &later);
        let full = rig.stream("a");
        let served_from = again.offset as usize - again.buffer.len();
        assert_eq!(
            &full[served_from..again.offset as usize],
            again.buffer,
            "the buffer is the stream at its offsets"
        );
        let mut rebuilt = again.buffer.clone();
        for event in rig.data("a") {
            if event.end_offset <= again.offset {
                continue;
            }
            assert!(event.start_offset >= again.offset, "no event straddles the snapshot");
            rebuilt.push_str(&event.data);
        }
        assert_eq!(rebuilt, full[served_from..]);
    }

    #[test]
    fn the_replay_buffer_is_bounded_but_the_offset_keeps_counting() {
        let rig = Rig::new();
        rig.start("a");
        let (line, done) = then_marker(
            "seq -f line%g 1 60000",
            "1..60000 | ForEach-Object { \"line$_\" }",
            "DONE",
        );
        rig.terminals.write("a", &line);
        rig.wait_for("a", &done);
        let snapshot = rig.start("a");
        assert!(snapshot.buffer.len() <= MAX_BUFFER);
        assert!(
            snapshot.offset as usize > MAX_BUFFER,
            "more than the ring holds went through"
        );
        assert!(snapshot.buffer.contains("line60000"));
        assert!(!snapshot.buffer.contains("line1\r"), "the start has been dropped");
        let full = rig.stream("a");
        assert!(full.starts_with(&rig.data("a")[0].data));
    }

    #[test]
    fn the_ring_is_cut_on_a_character_boundary() {
        let rig = Rig::new();
        rig.start("a");
        // Multi-byte text so that the byte limit lands inside a character.
        let (line, done) = then_marker(
            "seq 1 40000 | sed 's/$/ \u{20AC}\u{20AC}/'",
            "1..40000 | ForEach-Object { \"$_ \u{20AC}\u{20AC}\" }",
            "DONE",
        );
        rig.terminals.write("a", &line);
        rig.wait_for("a", &done);
        let snapshot = rig.start("a");
        assert!(snapshot.buffer.len() <= MAX_BUFFER && snapshot.buffer.len() > MAX_BUFFER - 4);
        assert!(!snapshot.buffer.starts_with('\u{FFFD}'));
        assert!(!snapshot.buffer.contains('\u{FFFD}'));
        let full = rig.stream("a");
        assert!(full[..snapshot.offset as usize].ends_with(&snapshot.buffer));
    }

    #[test]
    fn clearing_drops_the_buffer_and_keeps_the_offset() {
        let rig = Rig::new();
        rig.start("a");
        let (line, expect) = marker("FIRST");
        rig.say("a", &line, &expect);
        let before = rig.start("a");
        assert!(!before.buffer.is_empty());
        rig.terminals.clear("a");
        let cleared = rig.start("a");
        assert_eq!(cleared.buffer, "");
        assert!(cleared.offset >= before.offset);
        let (line, expect) = marker("SECOND");
        rig.say("a", &line, &expect);
        let after = rig.start("a");
        assert!(after.buffer.contains(&expect) && !after.buffer.contains("FIRST2"));
        assert!(rig.stream("a").len() as u64 >= after.offset);
    }

    #[test]
    fn restart_replaces_the_shell_and_starts_the_output_over() {
        let rig = Rig::new();
        let old = rig.start("a");
        let (line, expect) = marker("OLD");
        rig.say("a", &line, &expect);
        let old_events = rig.data("a").len();

        let fresh = rig.terminals.restart("a", &std::env::temp_dir(), None, None).unwrap();
        rig.forget_answer("a");
        assert_ne!(fresh.pid, old.pid);
        assert!(!fresh.buffer.contains(&expect));
        #[cfg(unix)]
        wait_gone(old.pid);
        // The new session's output counts from zero.
        let (line, expect) = marker("NEW");
        rig.terminals.write("a", &line);
        let deadline = Instant::now() + Duration::from_secs(30);
        let new_text = loop {
            rig.answer("a");
            let events = rig.data("a");
            let new: Vec<_> = events[old_events..].to_vec();
            let text: String = new.iter().map(|event| &*event.data).collect();
            if text.contains(&expect) {
                assert_eq!(
                    new[0].start_offset, fresh.offset,
                    "the first event of the new shell follows its snapshot"
                );
                break text;
            }
            assert!(Instant::now() < deadline, "no output from the restarted shell");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(!new_text.contains("OLD2"));
        assert!(
            rig.exits().is_empty(),
            "closing the old shell is not an exit the page should hear"
        );
    }

    #[cfg(unix)]
    #[test]
    fn closing_leaves_no_process_behind_and_no_event_after() {
        let rig = Rig::new();
        let snapshot = rig.start("a");
        // A foreground program the shell is waiting on: `exec` keeps the process id the shell reports.
        rig.terminals.write("a", "sh -c 'echo CHILD$$; exec sleep 300'\r");
        // The typed line has `CHILD$$`; only the printed one has digits after the name.
        let child_id = |text: &str| -> Option<u32> {
            let digits = text
                .split("CHILD")
                .skip(1)
                .find(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))?;
            digits
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        };
        rig.wait_until("the child id", "a", 10, |text| child_id(text).is_some());
        let child = child_id(&rig.stream("a")).unwrap();
        assert!(alive(snapshot.pid) && alive(child));

        rig.terminals.close("a");
        assert!(!alive(snapshot.pid), "the shell is dead and reaped when close returns");
        wait_gone(child);
        assert!(lock(&rig.terminals.inner.sessions).is_empty());

        let seen = rig.events().len();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(rig.events().len(), seen, "no output and no exit event after close");
        assert!(rig.exits().is_empty());

        // Everything on the closed id is a no-op.
        rig.terminals.write("a", "echo hi\r");
        rig.terminals.resize("a", 80.0, 24.0);
        rig.terminals.clear("a");
        rig.terminals.close("a");
    }

    #[cfg(unix)]
    #[test]
    fn closing_in_the_middle_of_a_burst_delivers_nothing_afterwards() {
        // A slow sink leaves output queued behind the pump when the shell is closed.
        let rig = Rig::with_delay(Duration::from_millis(30));
        let snapshot = rig.start("a");
        rig.terminals.write("a", &command("seq -f line%g 1 5000000", ""));
        rig.wait_for("a", "line1000");
        std::thread::sleep(Duration::from_millis(300));
        rig.terminals.close("a");
        let seen = rig.events().len();
        std::thread::sleep(Duration::from_millis(1000));
        assert_eq!(
            rig.events().len(),
            seen,
            "output still queued when the shell was closed is dropped"
        );
        assert!(rig.exits().is_empty());
        assert!(!alive(snapshot.pid));
    }

    #[cfg(unix)]
    #[test]
    fn closing_does_not_wait_for_a_write_the_shell_is_not_reading() {
        let rig = Arc::new(Rig::new());
        let snapshot = rig.start("a");
        // `sleep` never reads its input, so a large enough paste fills the terminal and the write blocks.
        rig.say("a", "echo SLEEPING$((1+1)); exec sleep 300\r", "SLEEPING2");
        let writer = {
            let rig = rig.clone();
            std::thread::spawn(move || rig.terminals.write("a", &"x".repeat(8 * 1024 * 1024)))
        };
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            !writer.is_finished(),
            "the paste should be stuck for this test to mean anything"
        );
        let (done_tx, done_rx) = mpsc::channel();
        let closer = {
            let rig = rig.clone();
            std::thread::spawn(move || {
                rig.terminals.close("a");
                let _ = done_tx.send(());
            })
        };
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "close hung on the stuck write"
        );
        closer.join().unwrap();
        writer.join().unwrap();
        assert!(!alive(snapshot.pid));
    }

    #[cfg(unix)]
    #[test]
    fn closing_a_shell_that_ignores_hangup_still_ends_it() {
        let rig = Rig::new();
        let snapshot = rig.start("a");
        rig.say("a", "trap '' HUP; echo TRAPPED$((1+1))\r", "TRAPPED2");
        rig.terminals.close("a");
        assert!(
            !alive(snapshot.pid),
            "the grace period for HUP ran out and the shell was killed"
        );
    }

    #[test]
    fn a_shell_that_exits_by_itself_delivers_its_output_then_its_exit_code() {
        let rig = Rig::new();
        let old = rig.start("a");
        rig.terminals
            .write("a", &command("echo BYE$((1+1)); exit 3", "'BYE' + (1+1); exit 3"));
        let deadline = Instant::now() + Duration::from_secs(30);
        while rig.exits().is_empty() {
            rig.answer("a");
            assert!(Instant::now() < deadline, "no exit event");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            rig.exits(),
            vec![ExitEvent {
                agent_id: "a".into(),
                exit_code: 3
            }],
            "exactly one"
        );
        let events = rig.events();
        assert!(matches!(events.last(), Some(Event::Exit(_))), "the exit comes last");
        assert!(rig.stream("a").contains("BYE2"), "output before the exit was not lost");
        assert!(lock(&rig.terminals.inner.sessions).is_empty());
        #[cfg(unix)]
        assert!(!alive(old.pid));

        // The id is free again: a new start gives a new shell whose output counts from zero.
        let fresh = rig.start("a");
        assert_ne!(fresh.pid, old.pid);
        assert_eq!(fresh.offset, 0);
        rig.terminals.close("a");
    }

    #[test]
    fn terminals_are_independent_per_agent() {
        let rig = Rig::new();
        let (a, b) = (rig.start("a"), rig.start("b"));
        assert_ne!(a.pid, b.pid);
        let (line, expect) = marker("ONLYA");
        rig.say("a", &line, &expect);
        assert!(!rig.stream("b").contains(&expect));
        rig.terminals.close("a");
        let (line, expect) = marker("STILLB");
        rig.say("b", &line, &expect);
        assert_eq!(rig.start("b").pid, b.pid, "closing one leaves the other running");
    }

    #[test]
    fn starting_the_same_agent_from_many_threads_makes_one_shell() {
        let rig = Arc::new(Rig::new());
        let pids: Vec<u32> = (0..8)
            .map(|_| {
                let rig = rig.clone();
                std::thread::spawn(move || rig.start("same").pid)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(pids.iter().all(|pid| *pid == pids[0]), "{pids:?}");
        assert_eq!(lock(&rig.terminals.inner.sessions).len(), 1);
    }

    #[test]
    fn the_shell_starts_in_the_given_folder_or_the_home_folder() {
        let rig = Rig::new();
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("desktop-cwd-check");
        std::fs::create_dir(&named).unwrap();
        let snapshot = rig.terminals.start("here", &named, None, None).unwrap();
        assert_eq!(Path::new(&snapshot.cwd), named);
        rig.say("here", &command("pwd", "(Get-Location).Path"), "desktop-cwd-check");

        let missing = dir.path().join("does-not-exist");
        let fallback = rig.terminals.start("lost", &missing, None, None).unwrap();
        assert_eq!(Path::new(&fallback.cwd), platform::home_dir());
        let file = dir.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let on_a_file = rig.terminals.start("file", &file, None, None).unwrap();
        assert_eq!(
            Path::new(&on_a_file.cwd),
            platform::home_dir(),
            "a file is not a working folder"
        );
    }

    #[test]
    fn a_shell_that_cannot_start_is_an_error_and_leaves_nothing_behind() {
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let sink: Sink = {
            let events = events.clone();
            Arc::new(move |event| lock(&events).push(event))
        };
        let terminals = Terminals::with_shell(sink, ("desktop-no-such-shell".into(), vec![]));
        let error = terminals
            .start("a", &std::env::temp_dir(), None, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("Could not start the terminal (desktop-no-such-shell)"),
            "{error}"
        );
        assert!(lock(&terminals.inner.sessions).is_empty());
        terminals.write("a", "x");
        assert!(lock(&events).is_empty());
    }

    #[test]
    fn unknown_agents_are_ignored() {
        let rig = Rig::new();
        rig.terminals.write("nobody", "x");
        rig.terminals.resize("nobody", 80.0, 24.0);
        rig.terminals.clear("nobody");
        rig.terminals.close("nobody");
        rig.terminals.shutdown_all();
        assert!(rig.events().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn shutting_down_ends_every_shell_and_so_does_dropping_the_manager() {
        let rig = Rig::new();
        let (a, b) = (rig.start("a"), rig.start("b"));
        rig.terminals.shutdown_all();
        assert!(!alive(a.pid) && !alive(b.pid));
        assert!(lock(&rig.terminals.inner.sessions).is_empty());

        let rig = Rig::new();
        let pid = rig.start("c").pid;
        let Rig { terminals, .. } = rig;
        drop(terminals);
        assert!(!alive(pid), "dropping the manager closes its shells");
    }
}

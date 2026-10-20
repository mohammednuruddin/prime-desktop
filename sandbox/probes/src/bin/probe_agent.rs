//! The program the sandbox runs during a probe. It attempts exactly one operation and reports what
//! happened as one JSON line, so the runner never has to guess from an exit code.
//!
//! It is deliberately tiny and uses only the standard library, so what it can do is what any program
//! under the sandbox can do.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

fn report(op: &str, outcome: Result<String, String>) -> ExitCode {
    let line = match outcome {
        Ok(detail) => format!("{{\"op\":{op:?},\"ok\":true,\"detail\":{detail:?}}}"),
        Err(error) => format!("{{\"op\":{op:?},\"ok\":false,\"error\":{error:?}}}"),
    };
    println!("{line}");
    ExitCode::SUCCESS
}

fn io<T>(result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(op) = args.first().map(String::as_str) else {
        eprintln!("usage: probe-agent <op> [args]");
        return ExitCode::from(64);
    };
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let outcome = match op {
        "read" => io(fs::File::open(arg(1))).and_then(|mut f| {
            let mut buf = [0u8; 16];
            io(f.read(&mut buf)).map(|n| format!("{n} bytes"))
        }),
        // Append, never truncate, so a probe that wrongly succeeds cannot destroy a fixture file.
        "write" => io(OpenOptions::new().create(true).append(true).open(arg(1)))
            .and_then(|mut f| io(f.write_all(b"x")).map(|_| "wrote".into())),
        "rename" => io(fs::rename(arg(1), arg(2))).map(|_| "renamed".into()),
        "hardlink" => io(fs::hard_link(arg(1), arg(2))).map(|_| "linked".into()),
        // Creates `<dir>/.git` and writes into it: a protected name that did not exist when the command started.
        "mkgit" => io(fs::create_dir_all(PathBuf::from(arg(1)).join(".git")))
            .and_then(|_| io(fs::write(PathBuf::from(arg(1)).join(".git").join("config"), "x")))
            .map(|_| "created a .git and wrote into it".into()),
        // Echoes an argument back, so a runner can tell whether anything between it and this program rewrote it.
        "arg" => Ok(arg(1)),
        "env" => match env::var(arg(1)) {
            Ok(_) => Ok("present".into()),
            Err(_) => Err("not set".into()),
        },
        "resolve" => io((arg(1), 80u16).to_socket_addrs())
            .and_then(|mut a| a.next().map(|x| x.to_string()).ok_or("no address".into())),
        "connect" => {
            let addr = arg(1).parse().map_err(|e: std::net::AddrParseError| e.to_string());
            addr.and_then(|a| io(TcpStream::connect_timeout(&a, Duration::from_secs(2))))
                .map(|_| "connected".into())
        }
        // Run a program and succeed only if it started and exited cleanly.
        "exec" => io(Command::new(arg(1)).args(args.iter().skip(2)).output()).and_then(|out| {
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                Err(format!("exited with {}", out.status))
            }
        }),
        // A grandchild does the work, to show the policy follows the whole process tree.
        "via-child" => {
            let me = env::current_exe().map_err(|e| e.to_string());
            me.and_then(|exe| io(Command::new(exe).args(args.iter().skip(1)).output()))
                .and_then(|out| {
                    let text = String::from_utf8_lossy(&out.stdout).to_string();
                    if text.contains("\"ok\":true") {
                        Ok("child succeeded".into())
                    } else {
                        Err(format!("child failed: {}", text.trim()))
                    }
                })
        }
        // Starts N copies of itself that sleep, to see how many the policy lets exist at once.
        "fanout" => {
            let n: usize = arg(1).parse().unwrap_or(0);
            let (mut started, mut children) = (0usize, Vec::new());
            for _ in 0..n {
                if let Ok(exe) = env::current_exe()
                    && let Ok(child) = Command::new(exe).arg("nap").spawn()
                {
                    started += 1;
                    children.push(child);
                }
            }
            let outcome = if started == n {
                Ok(format!("started {started} of {n}"))
            } else {
                Err(format!("started only {started} of {n}"))
            };
            for mut child in children {
                let _ = child.kill();
            }
            outcome
        }
        "nap" => {
            std::thread::sleep(Duration::from_secs(5));
            Ok("napped".into())
        }
        "spin" => loop {
            std::thread::sleep(Duration::from_millis(50));
        },
        "flood" => {
            let n: usize = arg(1).parse().unwrap_or(0);
            let chunk = vec![b'x'; 8192];
            let mut out = std::io::stdout().lock();
            let mut left = n;
            while left > 0 {
                let take = left.min(chunk.len());
                if out.write_all(&chunk[..take]).is_err() {
                    break;
                }
                left -= take;
            }
            let _ = out.flush();
            Ok("flooded".into())
        }
        other => Err(format!("unknown op {other}")),
    };
    report(op, outcome)
}

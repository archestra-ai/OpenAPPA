//! The sandboxed half of a Codex command job. Child bytes stay private until
//! the runtime admits a complete result.

use std::io::{Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::loopback_http::{Deadline, Endpoint, request_for_sandboxed_wrapper};

use super::jobs::MAX_OUTPUT;

#[cfg(unix)]
static CANCELLED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn cancel(_signal: libc::c_int) {
    CANCELLED.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
fn install_cancellation() {
    // Only touch an atomic in the handler. The main loop kills the child's
    // process group, including ordinary descendants in that group.
    unsafe {
        libc::signal(libc::SIGINT, cancel as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, cancel as *const () as libc::sighandler_t);
    }
}

#[cfg(unix)]
fn was_cancelled() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

#[cfg(not(unix))]
fn was_cancelled() -> bool {
    false
}

#[cfg(unix)]
fn kill_child_group(child: &mut std::process::Child) {
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_child_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

#[derive(Deserialize)]
struct Specification {
    command: String,
    shell: String,
    cwd: String,
}

#[derive(Serialize)]
struct Report {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}

#[derive(Deserialize)]
struct Admission {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn capture<R: Read>(mut reader: R, total: Arc<AtomicUsize>, overflow: Arc<AtomicBool>) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok(output);
        }
        let previous = total.fetch_add(count, Ordering::Relaxed);
        if previous.saturating_add(count) > MAX_OUTPUT {
            overflow.store(true, Ordering::Relaxed);
            return Ok(Vec::new());
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

fn execute(specification: Specification, mut still_authorized: impl FnMut() -> bool) -> Result<Report, String> {
    if specification.command.is_empty() || specification.cwd.is_empty() {
        return Err("the approved command specification is incomplete".into());
    }
    let shell = std::path::Path::new(&specification.shell);
    if !shell.is_absolute()
        || !matches!(
            shell.file_name().and_then(|name| name.to_str()),
            Some("sh" | "bash" | "zsh")
        )
        || !shell.is_file()
    {
        return Err("the approved command shell is unavailable".into());
    }
    // Native terminal input is deliberately closed until the stdin policy path
    // is available; an interactive command cannot receive unchecked bytes.
    #[cfg(unix)]
    let mut command = Command::new(shell);
    #[cfg(not(unix))]
    let mut command = Command::new("cmd.exe");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg("-lc");
        // A fresh session removes the inherited controlling terminal. The
        // child cannot bypass our pipes by opening /dev/tty.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        install_cancellation();
    }
    #[cfg(not(unix))]
    command.arg("/C");
    // A Stop/Interrupt can arrive after /consume and before process creation.
    // Check immediately before spawn so a cancelled job never uses the first
    // periodic poll as a launch grace period.
    if !still_authorized() {
        return Ok(Report {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        });
    }
    let mut child = command
        .arg(&specification.command)
        .current_dir(&specification.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the approved command could not start: {error}"))?;
    let total = Arc::new(AtomicUsize::new(0));
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().ok_or("the child has no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("the child has no stderr pipe")?;
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    let _out_thread = {
        let total = total.clone();
        let overflow = overflow.clone();
        std::thread::spawn(move || {
            let _ = out_tx.send(capture(stdout, total, overflow));
        })
    };
    let _err_thread = {
        let total = total.clone();
        let overflow = overflow.clone();
        std::thread::spawn(move || {
            let _ = err_tx.send(capture(stderr, total, overflow));
        })
    };
    let mut next_check = Instant::now() + Duration::from_millis(250);
    let status = loop {
        if Instant::now() >= next_check {
            if !still_authorized() {
                kill_child_group(&mut child);
                let _ = child.wait();
                return Ok(Report {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                });
            }
            next_check = Instant::now() + Duration::from_millis(250);
        }
        if overflow.load(Ordering::Relaxed) || was_cancelled() {
            kill_child_group(&mut child);
            break child.wait().map_err(|error| error.to_string())?;
        }
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => break status,
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    // The shell may have exited while a background descendant still holds a
    // pipe open. Terminate the original group before settling the result.
    kill_child_group(&mut child);
    // A daemonized descendant can escape the group while retaining a pipe.
    // Bound the drain and settle indeterminately. Such descendants are outside
    // this finite, non-daemonizing command path; a Unix process group cannot
    // contain a child that creates its own session.
    let stdout = match out_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(bytes)) => bytes,
        _ => {
            return Ok(Report {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            });
        }
    };
    let stderr = match err_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(bytes)) => bytes,
        _ => {
            return Ok(Report {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            });
        }
    };
    if overflow.load(Ordering::Relaxed) || was_cancelled() {
        return Ok(Report {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        });
    }
    let base64 = base64::engine::general_purpose::STANDARD;
    Ok(Report {
        stdout: base64.encode(stdout),
        stderr: base64.encode(stderr),
        exit_code: status.code(),
    })
}

fn run_inner(url: &str, handle: &str) -> Result<i32, String> {
    if !url.starts_with("http://127.0.0.1:") || uuid::Uuid::parse_str(handle).is_err() {
        return Err("invalid Codex job endpoint or handle".into());
    }
    let endpoint = Endpoint::parse(url)?;
    let proxy = std::env::var("HTTP_PROXY")
        .or_else(|_| std::env::var("http_proxy"))
        .ok();
    let request = |method: &str, path: &str, body: &[u8], seconds| {
        request_for_sandboxed_wrapper(
            &endpoint,
            method,
            path,
            body,
            &Deadline::spanning(Duration::from_secs(seconds)),
            proxy.as_deref(),
        )
    };
    let path = format!("/codex/job/{handle}/consume");
    let answer = request("GET", &path, b"", 5)?;
    if !answer.is_success() {
        return Err(format!("the Codex job was not available (status {})", answer.status));
    }
    let specification: Specification =
        serde_json::from_slice(&answer.body).map_err(|error| format!("invalid Codex job specification: {error}"))?;
    let running_path = format!("/codex/job/{handle}/running");
    let report = match execute(specification, || {
        request("GET", &running_path, b"", 3).is_ok_and(|answer| answer.status == 204)
    }) {
        Ok(report) => report,
        Err(_) => Report {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        },
    };
    let body = serde_json::to_vec(&report).map_err(|error| error.to_string())?;
    let path = format!("/codex/job/{handle}/report");
    let answer = request("POST", &path, &body, 120)?;
    if !answer.is_success() {
        return Err(format!(
            "the runtime withheld the command result (status {})",
            answer.status
        ));
    }
    let admission: Admission =
        serde_json::from_slice(&answer.body).map_err(|error| format!("invalid Codex result admission: {error}"))?;
    let base64 = base64::engine::general_purpose::STANDARD;
    let stdout = base64.decode(admission.stdout).map_err(|_| "invalid admitted stdout")?;
    let stderr = base64.decode(admission.stderr).map_err(|_| "invalid admitted stderr")?;
    if stdout.len().saturating_add(stderr.len()) > MAX_OUTPUT {
        return Err("the admitted result is too large".into());
    }
    std::io::stdout()
        .write_all(&stdout)
        .map_err(|error| error.to_string())?;
    std::io::stderr()
        .write_all(&stderr)
        .map_err(|error| error.to_string())?;
    Ok(admission.exit_code)
}

pub fn run(url: &str, handle: &str) -> ExitCode {
    match run_inner(url, handle) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("OpenAPPA withheld the Codex command: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn output_overflow_stops_the_child_and_withholds_all_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("continued-after-overflow");
        let report = execute(
            Specification {
                command: format!(
                    "dd if=/dev/zero bs=1024 count=1025 2>/dev/null; sleep 2; touch '{}'",
                    marker.display()
                ),
                shell: "/bin/sh".into(),
                cwd: dir.path().to_string_lossy().into_owned(),
            },
            || true,
        )
        .unwrap();
        assert_eq!(report.exit_code, None);
        assert!(report.stdout.is_empty());
        assert!(report.stderr.is_empty());
        assert!(!marker.exists());
    }

    #[test]
    fn escaped_descendant_cannot_hold_capture_open_indefinitely() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("escaped.pid");
        let started = Instant::now();
        let report = execute(
            Specification {
                command: format!(
                    "python3 -c 'import os,time; os.setsid(); open(\"{}\",\"w\").write(str(os.getpid())); time.sleep(5)' & sleep 0.3",
                    pid_file.display()
                ),
                shell: "/bin/sh".into(),
                cwd: dir.path().to_string_lossy().into_owned(),
            },
            || true,
        )
        .unwrap();
        assert_eq!(report.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(3));
        // The scoped proxy does not claim to contain a child that calls
        // setsid(). Clean up this deliberate escape so the test leaves none.
        if let Ok(pid) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid.parse::<i32>()
        {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }

    #[test]
    fn cancellation_stops_an_ordinary_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("too-late");
        let command = format!("(sleep 1; touch '{}') & wait", marker.display());
        let started = Instant::now();
        let report = execute(
            Specification {
                command,
                shell: "/bin/sh".into(),
                cwd: dir.path().to_string_lossy().into_owned(),
            },
            || started.elapsed() < Duration::from_millis(300),
        )
        .unwrap();
        assert_eq!(report.exit_code, None);
        std::thread::sleep(Duration::from_secs(1));
        assert!(!marker.exists(), "cancelled grandchild kept running");
    }

    #[test]
    fn cancellation_between_consume_and_spawn_never_launches_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("launched");
        let report = execute(
            Specification {
                command: format!("touch '{}'", marker.display()),
                shell: "/bin/sh".into(),
                cwd: dir.path().to_string_lossy().into_owned(),
            },
            || false,
        )
        .unwrap();
        assert_eq!(report.exit_code, None);
        assert!(!marker.exists());
    }
}

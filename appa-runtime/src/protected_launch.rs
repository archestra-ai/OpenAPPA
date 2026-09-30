//! The process boundary behind `clappa`: launch one gated Claude process,
//! remember the last resumable session it ended, and preserve its exit status.

use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};
use uuid::Uuid;

use crate::hook_client::session_is_gated;

const GATE: &str = "APPA_GATE";
const LAUNCH: &str = "APPA_LAUNCH";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Event {
    Start,
    End,
}

/// Run Claude in the foreground. This function terminates the host process so
/// it can preserve a child's signal death as well as an ordinary exit code.
pub fn launch(settings: &Path, data_dir: &Path, arguments: &[OsString]) -> ! {
    let token = Uuid::new_v4();
    let launches = data_dir.join("launches");
    let session = launches.join(token.to_string());
    if let Err(error) = fs::create_dir_all(&launches) {
        eprintln!("clappa: cannot prepare {}: {error}", launches.display());
        std::process::exit(1);
    }
    cleanup(&session);

    #[cfg(unix)]
    let signals = Signals::install();
    let child = Command::new("claude")
        .arg("--settings")
        .arg(settings)
        .args(arguments)
        .env(GATE, "1")
        .env(LAUNCH, token.to_string())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            cleanup(&session);
            eprintln!("clappa: cannot start Claude Code: {error}");
            std::process::exit(1);
        }
    };
    #[cfg(unix)]
    signals.child(child.id());
    let status = child.wait();
    #[cfg(unix)]
    drop(signals);

    let status = match status {
        Ok(status) => status,
        Err(error) => {
            cleanup(&session);
            eprintln!("clappa: cannot wait for Claude Code: {error}");
            std::process::exit(1);
        }
    };
    let mut stdout = std::io::stdout();
    print_resume(&session, stdout.is_terminal(), &mut stdout);
    cleanup(&session);
    terminate_as(status)
}

/// Record one lifecycle edge without contacting or starting the runtime.
pub fn record(data_dir: &Path, event: Event) -> ExitCode {
    if !session_is_gated() {
        return ExitCode::SUCCESS;
    }
    let Some(token) = std::env::var_os(LAUNCH).and_then(|token| token.into_string().ok()) else {
        return ExitCode::SUCCESS;
    };
    let Ok(token) = Uuid::parse_str(&token) else {
        return ExitCode::SUCCESS;
    };
    let path = data_dir.join("launches").join(token.to_string());
    let mut input = Vec::new();
    if let Err(error) = std::io::stdin().take(1024 * 1024).read_to_end(&mut input) {
        cleanup(&path);
        eprintln!("appa record-launch: cannot read the hook event: {error}");
        return ExitCode::FAILURE;
    }
    match record_event(&path, event, &input) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            cleanup(&path);
            eprintln!("appa record-launch: cannot record the session: {error}");
            ExitCode::FAILURE
        }
    }
}

fn record_event(path: &Path, event: Event, input: &[u8]) -> std::io::Result<()> {
    if event == Event::Start {
        return remove_file(path);
    }
    let hook: Value = match serde_json::from_slice(input) {
        Ok(hook) => hook,
        Err(_) => return remove_file(path),
    };
    let session = hook
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok());
    let transcript = hook.get("transcript_path").and_then(Value::as_str).map(PathBuf::from);
    match (session, transcript) {
        (Some(session), Some(transcript)) if transcript.is_file() => fs::write(path, session.to_string()),
        _ => remove_file(path),
    }
}

fn print_resume(path: &Path, terminal: bool, output: &mut impl Write) {
    let session = fs::read_to_string(path)
        .ok()
        .and_then(|session| Uuid::parse_str(session.trim()).ok());
    if terminal && let Some(session) = session {
        let _ = write!(
            output,
            "\nResume with OpenAPPA protection:\nclappa --resume {session}\n"
        );
        let _ = output.flush();
    }
}

fn remove_file(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup(path: &Path) {
    if let Err(error) = remove_file(path) {
        eprintln!("clappa: cannot remove {}: {error}", path.display());
    }
}

#[cfg(unix)]
fn terminate_as(status: ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signal) = status.signal() {
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        std::process::exit(128 + signal);
    }
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(not(unix))]
fn terminate_as(status: ExitStatus) -> ! {
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(unix)]
struct Signals {
    term: libc::sighandler_t,
    hup: libc::sighandler_t,
}

#[cfg(unix)]
static CHILD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn forward(signal: libc::c_int) {
    let child = CHILD.load(std::sync::atomic::Ordering::Relaxed);
    if child > 0 {
        unsafe { libc::kill(child, signal) };
    }
}

#[cfg(unix)]
impl Signals {
    fn install() -> Self {
        unsafe {
            Self {
                term: libc::signal(libc::SIGTERM, forward as *const () as libc::sighandler_t),
                hup: libc::signal(libc::SIGHUP, forward as *const () as libc::sighandler_t),
            }
        }
    }

    fn child(&self, pid: u32) {
        CHILD.store(pid as i32, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        CHILD.store(0, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            libc::signal(libc::SIGTERM, self.term);
            libc::signal(libc::SIGHUP, self.hup);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_records_only_a_session_with_an_existing_transcript() {
        let root = tempfile::tempdir().unwrap();
        let transcript = root.path().join("transcript.jsonl");
        let launch = root.path().join("launch");
        let session = Uuid::new_v4();
        fs::write(&transcript, "").unwrap();
        let event = serde_json::json!({"session_id": session, "transcript_path": transcript});
        record_event(&launch, Event::End, event.to_string().as_bytes()).unwrap();
        assert_eq!(fs::read_to_string(&launch).unwrap(), session.to_string());

        fs::remove_file(&transcript).unwrap();
        record_event(&launch, Event::End, event.to_string().as_bytes()).unwrap();
        assert!(!launch.exists(), "an empty session removes an earlier resumable id");
    }

    #[test]
    fn start_removes_the_previous_session() {
        let root = tempfile::tempdir().unwrap();
        let launch = root.path().join("launch");
        fs::write(&launch, Uuid::new_v4().to_string()).unwrap();
        record_event(&launch, Event::Start, br#"{"ignored":true}"#).unwrap();
        assert!(!launch.exists());
    }

    #[test]
    fn resume_block_requires_both_a_session_file_and_a_terminal() {
        let root = tempfile::tempdir().unwrap();
        let launch = root.path().join("launch");
        let session = Uuid::new_v4();
        let mut output = Vec::new();
        print_resume(&launch, true, &mut output);
        assert!(output.is_empty());

        fs::write(&launch, session.to_string()).unwrap();
        print_resume(&launch, false, &mut output);
        assert!(output.is_empty());
        print_resume(&launch, true, &mut output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("\nResume with OpenAPPA protection:\nclappa --resume {session}\n")
        );
    }
}

//! Every child process the runtime starts, started one at a time.
//!
//! Where the platform has no `pipe2` (macOS), std creates a child's pipes and marks them
//! close-on-exec in two steps. A child that another thread starts in between inherits
//! both ends and keeps them for its whole life: a reader then waits for an end of file
//! that only comes when that unrelated child exits, and a writer whose reader is gone
//! writes on instead of failing. Two consults running at once could each hang on the
//! other. Holding one process-wide lock across each spawn closes the window. The lock
//! covers the spawn call only, never the wait for the child.

use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

/// [`Command::spawn`], alone.
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    alone(|| command.spawn())
}

/// [`tokio::process::Command::spawn`], alone. Tokio spawns through std's
/// [`Command::spawn`], synchronously inside this call, so the same lock covers it.
pub(crate) fn spawn_async(command: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    alone(|| command.spawn())
}

/// [`Command::output`], alone: stdin empty, stdout and stderr captured.
pub fn output(command: &mut Command) -> io::Result<Output> {
    spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?
    .wait_with_output()
}

/// [`Command::status`], alone: stdio inherited unless `command` sets it.
pub(crate) fn status(command: &mut Command) -> io::Result<ExitStatus> {
    spawn(command)?.wait()
}

/// Platforms with `pipe2` create the pipes close-on-exec in one step and need no lock;
/// Windows std already serializes process creation itself.
fn alone<T>(spawn: impl FnOnce() -> T) -> T {
    #[cfg(target_vendor = "apple")]
    let _alone = {
        static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SPAWNING.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    spawn()
}

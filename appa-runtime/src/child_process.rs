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
pub fn spawn_async(command: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
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
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Eight threads start children at once. Each child writes to its stdout only after
    /// its parent has dropped the reader and told it to go, so the write must fail —
    /// unless another child, started meanwhile, inherited that reader.
    #[test]
    fn a_child_whose_reader_is_gone_cannot_write_while_others_start() {
        let written: usize = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        (0..200)
                            .filter(|_| {
                                let mut child = spawn(
                                    Command::new("/bin/sh")
                                        .args(["-c", "read go && echo written"])
                                        .stdin(Stdio::piped())
                                        .stdout(Stdio::piped())
                                        .stderr(Stdio::null()),
                                )
                                .expect("sh starts");
                                drop(child.stdout.take());
                                let mut go = child.stdin.take().expect("stdin is piped");
                                go.write_all(b"go\n").expect("the child reads its go");
                                drop(go);
                                child.wait().expect("sh exits").success()
                            })
                            .count()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().expect("a spawning thread finishes"))
                .sum()
        });
        assert_eq!(
            written, 0,
            "{written} of 1600 children wrote to a reader their parent had dropped"
        );
    }
}

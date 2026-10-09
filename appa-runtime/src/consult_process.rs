//! The local process a consult runs: a `command` binding's child and the `claude-code`
//! builtin's CLI. The child's whole process tree ends with the consult, its input and
//! answer cross bounded pipes, and the tail of its stderr is kept for the record.

use std::sync::Arc;
use std::time::Duration;

use crate::config::ResolverCommand;
use crate::external::{Diagnostics, MAX_DIAGNOSTIC_BYTES, NoAnswerReason, RECORD_READ_GRACE, Transcript};
use crate::process_tree::ProcessTree;

/// A command consult's stdout on a successful exit, and the transcript a record asked for.
pub(crate) struct CommandRun {
    pub(crate) output: Result<Vec<u8>, NoAnswerReason>,
    pub(crate) transcript: Option<Transcript>,
}

pub(crate) async fn run_command(
    command: &ResolverCommand,
    input: Vec<u8>,
    deadline: tokio::time::Instant,
    max_body_bytes: usize,
    mut transcript: Option<Transcript>,
    credential: Option<(std::ffi::OsString, std::ffi::OsString)>,
) -> CommandRun {
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let command = command.clone();
    let task = tokio::spawn(async move {
        let output = run_command_process(
            command,
            input,
            max_body_bytes,
            deadline,
            cancelled,
            transcript.as_mut(),
            credential,
        )
        .await;
        CommandRun { output, transcript }
    });
    CommandTask {
        cancel: Some(cancel),
        task,
    }
    .wait()
    .await
}

struct CommandTask {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<CommandRun>,
}

impl CommandTask {
    async fn wait(mut self) -> CommandRun {
        let run = (&mut self.task).await.unwrap_or(CommandRun {
            output: Err(NoAnswerReason::Transport),
            transcript: None,
        });
        self.cancel.take();
        run
    }
}

impl Drop for CommandTask {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

/// A consult's subprocess, adopted as a [`ProcessTree`], and the promise that the tree
/// ends with the consult: every outcome, and a dropped future, terminate it.
pub(crate) struct CommandProcess {
    child: Option<tokio::process::Child>,
    tree: Option<ProcessTree>,
}

impl CommandProcess {
    pub(crate) fn spawn(command: &mut tokio::process::Command) -> Result<CommandProcess, NoAnswerReason> {
        let (child, tree) = crate::process_tree::spawn(command)?;
        Ok(CommandProcess {
            child: Some(child),
            tree: Some(tree),
        })
    }

    /// The child to exchange with, and the tree that ends it.
    pub(crate) fn parts(&mut self) -> (&mut tokio::process::Child, &ProcessTree) {
        (
            self.child.as_mut().expect("a live command process owns its child"),
            self.tree.as_ref().expect("a live command process owns its tree"),
        )
    }

    pub(crate) fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("a live command process owns its child")
    }

    fn terminate_tree(&mut self) {
        if let Some(tree) = self.tree.take() {
            tree.kill();
        }
    }

    pub(crate) async fn terminate_and_reap(&mut self) -> Result<std::process::ExitStatus, NoAnswerReason> {
        self.terminate_tree();
        self.child_mut().wait().await.map_err(|_| NoAnswerReason::Transport)
    }

    /// Do not let a child stuck in uninterruptible I/O extend the caller's deadline: the
    /// tree is ended now, and a detached task keeps the reaping responsibility.
    pub(crate) fn terminate_and_reap_later(mut self) {
        self.terminate_tree();
        let Some(mut child) = self.child.take() else {
            return;
        };
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
    }
}

impl Drop for CommandProcess {
    fn drop(&mut self) {
        // Covers runtime shutdown or task abortion. `kill_on_drop` also targets the direct
        // child; Tokio's orphan queue reaps it when an async wait cannot run.
        self.terminate_tree();
    }
}

/// One subprocess exchange, shared by every transport that runs a local process: the
/// input on stdin, the answer read off stdout under `max_body_bytes`, and the child seen
/// out — unreaped — before returning. A helper the child left behind may hold the pipe
/// open after the child itself exited: seeing the exit first ends the tree, so the
/// answer already written is read out instead of lost to the timeout.
pub(crate) async fn exchange_with_child(
    child: &mut tokio::process::Child,
    tree: &ProcessTree,
    input: &[u8],
    max_body_bytes: usize,
) -> Result<Vec<u8>, NoAnswerReason> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut stdin = child.stdin.take().ok_or(NoAnswerReason::Transport)?;
    let mut stdout = child.stdout.take().ok_or(NoAnswerReason::Transport)?;
    // A child may answer without reading its input and close stdin first. A broken pipe
    // here is that early close, not a transport fault: the exit and the answer still decide.
    let write = async {
        let written = match stdin.write_all(input).await {
            Ok(()) => stdin.shutdown().await,
            Err(error) => Err(error),
        };
        drop(stdin);
        match written {
            Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(NoAnswerReason::Transport),
            _ => Ok(()),
        }
    };
    // Read under the cap before anything waits: a child writing past it is reported
    // oversized at once, so a full pipe can never wedge the exchange into the timeout.
    let read = async {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let read = stdout.read(&mut chunk).await.map_err(|_| NoAnswerReason::Transport)?;
            if read == 0 {
                return Ok(bytes);
            }
            if bytes.len().saturating_add(read) > max_body_bytes {
                return Err(NoAnswerReason::Oversized);
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
    };
    // The write and the read run together: a child that answers past the pipe's capacity
    // before draining its input would otherwise block the parent's write, and the two
    // would wait on each other until the deadline. The read ending — answer, EOF, or
    // oversized — settles the exchange whatever the write is doing.
    let output = async {
        tokio::pin!(write);
        tokio::pin!(read);
        tokio::select! {
            bytes = &mut read => bytes,
            written = &mut write => {
                written?;
                read.await
            }
        }
    };
    tokio::pin!(output);
    tokio::select! {
        biased;
        bytes = &mut output => {
            let bytes = bytes?;
            // The answer is already complete here, so an unobservable exit must not
            // discard it: a child something else reaped says nothing about the answer.
            // Whether the child exited well is still decided by the status
            // `terminate_and_reap` returns to the caller.
            let _ = tree.root_exited().await;
            Ok(bytes)
        }
        exited = tree.root_exited() => {
            exited?;
            tree.kill();
            output.await
        }
    }
}

/// The tail of what a child wrote to stderr, read to its end so the pipe never fills: the
/// command's own error, whose last line goes to the log and the no-answer diagnostic.
pub(crate) struct StderrTail {
    read: Arc<std::sync::Mutex<Diagnostics>>,
    task: tokio::task::JoinHandle<()>,
}

pub(crate) fn stderr_tail(stderr: tokio::process::ChildStderr) -> StderrTail {
    let read = Arc::new(std::sync::Mutex::new(Diagnostics::default()));
    let task = tokio::spawn({
        let read = Arc::clone(&read);
        async move {
            use tokio::io::AsyncReadExt as _;
            let mut stderr = stderr;
            let mut chunk = [0u8; 1024];
            while let Ok(count) = stderr.read(&mut chunk).await {
                if count == 0 {
                    break;
                }
                let Ok(mut tail) = read.lock() else { break };
                tail.bytes.extend_from_slice(&chunk[..count]);
                if tail.bytes.len() > MAX_DIAGNOSTIC_BYTES {
                    let excess = tail.bytes.len() - MAX_DIAGNOSTIC_BYTES;
                    tail.bytes.drain(..excess);
                    tail.truncated = true;
                }
            }
        }
    });
    StderrTail { read, task }
}

impl StderrTail {
    /// What the child wrote once it closed the pipe, or once `wait` passed — a helper that
    /// kept the pipe open leaves the tail read so far. Nothing where it wrote nothing.
    /// A reader still waiting is aborted, so a helper holding the pipe keeps no task here.
    async fn within(&mut self, wait: Duration) -> Option<Diagnostics> {
        if tokio::time::timeout(wait, &mut self.task).await.is_err() {
            self.task.abort();
        }
        let tail = std::mem::take(&mut *self.read.lock().ok()?);
        Some(tail).filter(|tail| !tail.bytes.is_empty())
    }
}

/// The stderr tail of a child that failed, given a second to close the pipe.
pub(crate) async fn finished_tail(mut tail: StderrTail) -> Option<Diagnostics> {
    tail.within(Duration::from_secs(1)).await
}

/// `parent` without the runtime's own namespace. A consult child starts from exactly this,
/// its environment cleared first: filtering one read of the environment, rather than
/// removing names from the live one, leaves no gap for a variable set in between.
pub(crate) fn without_runtime_variables(
    parent: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)> {
    parent.into_iter().filter(|(key, _)| {
        !key.to_string_lossy()
            .starts_with(crate::config::RUNTIME_VARIABLE_PREFIX)
    })
}

async fn run_command_process(
    command: ResolverCommand,
    input: Vec<u8>,
    max_body_bytes: usize,
    deadline: tokio::time::Instant,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
    seen: Option<&mut Transcript>,
    credential: Option<(std::ffi::OsString, std::ffi::OsString)>,
) -> Result<Vec<u8>, NoAnswerReason> {
    use std::process::Stdio;

    let Some((executable, arguments)) = command.argv.split_first() else {
        return Err(NoAnswerReason::Unregistered);
    };
    let mut configured = tokio::process::Command::new(executable);
    configured
        .args(arguments)
        .current_dir(&command.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The runtime's own namespace stops here: no bearer token it sends, and no wiring
    // variable, reaches the child. The binding's own provider credential is put back
    // afterwards, so a command inherits the one variable it reads and no other's.
    let parent: Vec<_> = std::env::vars_os().collect();
    configured
        .env_clear()
        .envs(without_runtime_variables(parent))
        .envs(credential);

    let mut process = CommandProcess::spawn(&mut configured)?;
    let tail = process.child_mut().stderr.take().map(stderr_tail);
    let outcome = {
        let (child, tree) = process.parts();
        let exchange = exchange_with_child(child, tree, &input, max_body_bytes);
        tokio::select! {
            biased;
            _ = &mut cancelled => Err(NoAnswerReason::Transport),
            _ = tokio::time::sleep_until(deadline) => Err(NoAnswerReason::Timeout),
            outcome = exchange => outcome,
        }
    };
    let exited = match outcome {
        Ok(output) => process.terminate_and_reap().await.map(|status| (status, output)),
        Err(reason) => {
            process.terminate_and_reap_later();
            Err(reason)
        }
    };
    // A failed exit's tail is read for the log too; an answer's only for the record, and
    // only briefly, so the outcome's time is taken before that wait.
    let (stderr, settled) = match (&exited, tail) {
        (Ok((status, _)), Some(tail)) if !status.success() => (finished_tail(tail).await, std::time::Instant::now()),
        (Ok(_), Some(mut tail)) if seen.is_some() => {
            let settled = std::time::Instant::now();
            (tail.within(RECORD_READ_GRACE).await, settled)
        }
        _ => (None, std::time::Instant::now()),
    };
    if let Some(seen) = seen {
        seen.settled = Some(settled);
        seen.raw_response = exited.as_ref().ok().map(|(_, output)| output.clone());
        seen.diagnostics = stderr.clone();
    }
    let (status, output) = exited?;
    if status.success() {
        return Ok(output);
    }
    let stderr = stderr.and_then(|stderr| stderr.error_line()).unwrap_or_default();
    tracing::warn!(code = ?status.code(), stderr = %stderr, "the command exited without an answer");
    match status.code().and_then(|code| u16::try_from(code).ok()) {
        Some(status) => Err(NoAnswerReason::NonSuccess { status, detail: None }),
        // A signal or another platform-specific status establishes no useful failure
        // classification. The stderr tail remains confined to logs and the recorder.
        None => Err(NoAnswerReason::Transport),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader still pending when the wait runs out is aborted rather than left holding
    /// the pipe for as long as the writer lives.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tail_reader_past_its_wait_is_aborted() {
        let mut child = crate::child_process::spawn_async(
            tokio::process::Command::new("sleep")
                .arg("10")
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true),
        )
        .expect("sleep starts");
        let mut tail = stderr_tail(child.stderr.take().expect("stderr is piped"));

        assert_eq!(tail.within(Duration::from_millis(20)).await, None);
        let joined = tokio::time::timeout(Duration::from_secs(5), &mut tail.task)
            .await
            .expect("an aborted reader ends at once");
        assert!(joined.expect_err("the reader was aborted").is_cancelled());
        child.kill().await.expect("sleep is killed");
    }
}

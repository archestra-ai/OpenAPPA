//! A consult's subprocess and every process it starts, ended as one.
//!
//! On Unix the child leads a fresh process group. On Windows it starts suspended, joins a
//! Job Object, and only then runs, so no descendant can start outside the job. Either
//! way a helper the child leaves behind is killed with it.

use crate::external::NoAnswerReason;

/// Spawn `command` as a process tree: confined before it starts, adopted once it has.
/// A command that does not start is unreachable; a child that cannot be adopted is
/// refused, and its `kill_on_drop` ends it.
pub(crate) fn spawn(
    command: &mut tokio::process::Command,
) -> Result<(tokio::process::Child, ProcessTree), NoAnswerReason> {
    confine(command);
    let child = crate::child_process::spawn_async(command).map_err(|_| NoAnswerReason::Unreachable)?;
    let tree = ProcessTree::adopt(&child)?;
    Ok((child, tree))
}

fn confine(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.as_std_mut().process_group(0);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    }
}

pub(crate) struct ProcessTree {
    #[cfg(unix)]
    group: i32,
    /// Kill-on-close: dropping the last handle ends every process in it.
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
    #[cfg(windows)]
    root: std::os::windows::io::OwnedHandle,
}

impl ProcessTree {
    fn adopt(child: &tokio::process::Child) -> Result<ProcessTree, NoAnswerReason> {
        #[cfg(unix)]
        {
            let group = child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .ok_or(NoAnswerReason::Transport)?;
            Ok(ProcessTree { group })
        }
        #[cfg(windows)]
        {
            windows::adopt(child).map_err(|error| {
                tracing::warn!(%error, "the consult process could not be confined to a job");
                NoAnswerReason::Transport
            })
        }
    }

    /// End the child and every descendant. Cleanup runs after every outcome, so a
    /// resolver cannot keep a helper alive after answering.
    pub(crate) fn kill(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
        #[cfg(windows)]
        windows::kill(&self.job);
    }

    /// Wait for the direct child to exit, leaving its exit status to be reaped later.
    pub(crate) async fn root_exited(&self) -> Result<(), NoAnswerReason> {
        loop {
            if self.root_has_exited()? {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Unix observes the exit without reaping: the zombie keeps its pid and group id
    /// reserved, so a group kill that follows cannot hit a recycled id. `ECHILD` here
    /// means something else reaped the child. On Windows the open process handle
    /// reserves the pid.
    #[cfg(unix)]
    fn root_has_exited(&self) -> Result<bool, NoAnswerReason> {
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.group as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            if result == -1 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(NoAnswerReason::Transport);
            }
            let info = unsafe { info.assume_init() };
            return Ok((unsafe { info.si_pid() }) == self.group);
        }
    }

    #[cfg(windows)]
    fn root_has_exited(&self) -> Result<bool, NoAnswerReason> {
        windows::has_exited(&self.root).map_err(|_| NoAnswerReason::Transport)
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::mem::{size_of, size_of_val};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME, WaitForSingleObject,
    };

    use super::ProcessTree;

    pub(super) fn adopt(child: &tokio::process::Child) -> io::Result<ProcessTree> {
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("the child has already been reaped"))?;
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("the child has already been reaped"))?;
        let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        kill_on_close(&job)?;
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let tree = ProcessTree {
            root: duplicate(process)?,
            job,
        };
        // Once in the job, the child may run: everything it starts joins the job too.
        resume_only_thread(pid).inspect_err(|_| kill(&tree.job))?;
        Ok(tree)
    }

    pub(super) fn kill(job: &OwnedHandle) {
        unsafe {
            TerminateJobObject(job.as_raw_handle(), 1);
        }
    }

    pub(super) fn has_exited(process: &OwnedHandle) -> io::Result<bool> {
        match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn kill_on_close(job: &OwnedHandle) -> io::Result<()> {
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The tree's own handle to the child: Tokio's may be closed by a reap the tree does
    /// not control.
    fn duplicate(process: HANDLE) -> io::Result<OwnedHandle> {
        let mut copy: HANDLE = null_mut();
        let current = unsafe { GetCurrentProcess() };
        if unsafe { DuplicateHandle(current, process, current, &mut copy, 0, 0, DUPLICATE_SAME_ACCESS) } == 0 {
            return Err(io::Error::last_os_error());
        }
        owned(copy)
    }

    /// A process created suspended has exactly one thread, which has not run yet.
    /// Anything else means the pid is not the child this tree adopted.
    fn resume_only_thread(pid: u32) -> io::Result<()> {
        let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut threads = Vec::new();
        let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                threads.push(entry.th32ThreadID);
            }
            more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
        }
        let [thread] = threads[..] else {
            return Err(io::Error::other(format!(
                "a suspended child has one thread, pid {pid} has {}",
                threads.len()
            )));
        };
        let thread = owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread) })?;
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::io::AsyncReadExt as _;

    use crate::external::{CommandProcess, exchange_with_child};

    /// Starts a helper that holds the shell's stdout open for a minute.
    #[cfg(unix)]
    const HELPER: &str = "sleep 60 &";
    /// The redirect is the inner `cmd`'s, so that `cmd` itself keeps the pipe.
    #[cfg(windows)]
    const HELPER: &str = r#"start /b cmd /c "ping -n 60 127.0.0.1 >nul" &"#;
    #[cfg(unix)]
    const BLOCK: &str = "sleep 60";
    #[cfg(windows)]
    const BLOCK: &str = "ping -n 60 127.0.0.1 >nul";
    /// Far below the helper's minute: finishing inside it proves the helper is gone.
    const WITHIN: Duration = Duration::from_secs(10);

    fn shell(script: &str) -> tokio::process::Command {
        #[cfg(unix)]
        let mut command = {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.arg("-c").arg(script);
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = tokio::process::Command::new("cmd");
            command.arg("/c").raw_arg(script);
            command
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }

    #[tokio::test]
    async fn an_answer_is_read_although_a_helper_holds_the_pipe() {
        let mut process =
            CommandProcess::spawn(&mut shell(&format!("{HELPER} echo answer"))).expect("the shell starts as a tree");
        let (child, tree) = process.parts();
        let answer = tokio::time::timeout(WITHIN, exchange_with_child(child, tree, b"input", 1024))
            .await
            .expect("the exchange ends without waiting for the helper")
            .expect("the exchange answers");
        assert_eq!(String::from_utf8_lossy(&answer).trim(), "answer");
    }

    #[tokio::test]
    async fn killing_the_tree_ends_every_process_holding_the_pipe() {
        let (mut child, tree) = super::spawn(&mut shell(&format!("{HELPER} {BLOCK}"))).expect("the shell starts");
        let mut stdout = child.stdout.take().expect("stdout is piped");
        tree.kill();
        let mut rest = Vec::new();
        tokio::time::timeout(WITHIN, stdout.read_to_end(&mut rest))
            .await
            .expect("no process of the tree keeps the pipe open")
            .expect("the pipe reads to its end");
        tree.root_exited().await.expect("the root's exit is observed");
    }
}

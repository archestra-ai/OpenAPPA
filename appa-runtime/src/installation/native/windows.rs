//! Windows 10+ native activation containment. Association happens inside
//! CreateProcessW, before the selected executable can create descendants.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

pub(super) struct NativeChild {
    process: OwnedHandle,
    job: OwnedHandle,
    completed: bool,
    cleanup_attempted: bool,
}

impl NativeChild {
    pub(super) fn spawn(binary: &Path, args: &[&OsStr], stdout: &File, stderr: &File) -> io::Result<Self> {
        if !binary.is_absolute()
            || !binary
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native executable must be an absolute .exe path",
            ));
        }
        let application = wide(binary.as_os_str())?;
        let mut command_line = command_line(binary.as_os_str(), args)?;
        // A null security descriptor makes the job handle non-inheritable.
        let raw_job = unsafe { CreateJobObjectW(null(), null()) };
        if raw_job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned, valid handle.
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        set_kill_on_close(&job, true)?;

        let input = File::open("NUL")?;
        let input = inheritable(&input)?;
        let output = inheritable(stdout)?;
        let diagnostics = inheritable(stderr)?;
        let handles = [
            input.as_raw_handle(),
            output.as_raw_handle(),
            diagnostics.as_raw_handle(),
        ];
        let jobs = [job.as_raw_handle()];
        // Keep borrowed arrays and their handles alive until after the list is
        // deleted. Only these three standard handles are inherited.
        let attributes = Attributes::new(&handles, &jobs)?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attributes.pointer();
        let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: all buffers/handles outlive the call, the command line is
        // mutable and NUL-terminated, and the initialized attribute list binds
        // the process atomically to our job. Environment and cwd are inherited.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup.StartupInfo,
                &mut information,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful CreateProcessW transfers these two owned handles.
        let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
        drop(thread);
        Ok(Self {
            process,
            job,
            completed: false,
            cleanup_attempted: false,
        })
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        // Test the handle, rather than interpreting STILL_ACTIVE as an exit
        // code: an exited process may itself have returned code 259.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(code)))
            }
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("unexpected native process wait result")),
        }
    }

    pub(super) fn terminate(&mut self) -> io::Result<()> {
        if self.completed {
            return Ok(());
        }
        if self.cleanup_attempted {
            return Err(io::Error::other("native process cleanup was already attempted"));
        }
        self.cleanup_attempted = true;
        let deadline = Instant::now() + Duration::from_secs(5);
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        loop {
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe {
                QueryInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of_val(&accounting) as u32,
                    null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if accounting.ActiveProcesses == 0 {
                self.completed = true;
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native process job did not empty within five seconds",
                ));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(25)));
        }
    }

    pub(super) fn complete(&mut self) -> io::Result<()> {
        if self.cleanup_attempted {
            return Err(io::Error::other("cannot preserve a terminated native process job"));
        }
        // The caller has verified successful exit and bounded output. Runtime
        // descendants intentionally remain alive after the installer returns.
        set_kill_on_close(&self.job, false)?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        if !self.completed
            && !self.cleanup_attempted
            && let Err(error) = self.terminate()
        {
            tracing::error!(%error, "native activation job cleanup failed");
        }
        // Closing our non-inherited job handle still requests termination if
        // explicit cleanup failed; Drop never repeats the five-second wait.
    }
}

fn set_kill_on_close(job: &OwnedHandle, enabled: bool) -> io::Result<()> {
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    if enabled {
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    }
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

fn inheritable(file: &File) -> io::Result<OwnedHandle> {
    let current = unsafe { GetCurrentProcess() };
    let mut duplicated = null_mut();
    if unsafe {
        DuplicateHandle(
            current,
            file.as_raw_handle(),
            current,
            &mut duplicated,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle returned a distinct owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicated) })
}

struct Attributes {
    // Pointer-width storage satisfies the native attribute list's alignment.
    storage: Vec<usize>,
}

impl Attributes {
    fn new(handles: &[HANDLE], jobs: &[HANDLE]) -> io::Result<Self> {
        let mut bytes = 0;
        let sized = unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes) };
        if sized != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || bytes == 0
        {
            return Err(io::Error::other("could not size native process attributes"));
        }
        let words = bytes
            .checked_add(size_of::<usize>() - 1)
            .ok_or_else(|| io::Error::other("native process attribute size overflow"))?
            / size_of::<usize>();
        let mut storage = vec![0usize; words];
        if unsafe { InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let attributes = Self { storage };
        for (kind, values) in [
            (PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles),
            (PROC_THREAD_ATTRIBUTE_JOB_LIST, jobs),
        ] {
            if unsafe {
                UpdateProcThreadAttribute(
                    attributes.pointer(),
                    0,
                    kind as usize,
                    values.as_ptr().cast(),
                    size_of_val(values),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(attributes)
    }

    fn pointer(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_ptr().cast_mut().cast()
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native process argument contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

fn command_line(binary: &OsStr, arguments: &[&OsStr]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    for argument in std::iter::once(binary).chain(arguments.iter().copied()) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        let argument = wide(argument)?;
        line.push(b'"' as u16);
        let mut slashes = 0;
        for &character in &argument[..argument.len() - 1] {
            if character == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            let escaped = character == b'"' as u16;
            line.extend(std::iter::repeat_n(
                b'\\' as u16,
                if escaped { slashes * 2 + 1 } else { slashes },
            ));
            line.push(character);
            slashes = 0;
        }
        line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        line.push(b'"' as u16);
    }
    line.push(0);
    if line.len() > 32767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native process command line exceeds Windows limit",
        ));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_preserves_empty_space_quote_and_trailing_backslash() {
        let quoted = command_line(
            OsStr::new("C:\\app a\\appa.exe"),
            &[
                OsStr::new(""),
                OsStr::new("a b"),
                OsStr::new("a\"b"),
                OsStr::new("tail\\"),
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf16(&quoted[..quoted.len() - 1]).unwrap(),
            "\"C:\\app a\\appa.exe\" \"\" \"a b\" \"a\\\"b\" \"tail\\\\\""
        );
        assert!(command_line(OsStr::new("appa.exe"), &[OsStr::new("bad\0argument")]).is_err());
    }
}

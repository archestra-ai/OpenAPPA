//! Real Windows processes: the test executable supplies the child fixtures.

use super::*;
use std::ffi::{OsStr, OsString};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Command;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

const FIXTURE: &str = "installation::native::windows_tests::child_fixture";
const MARKER: &str = "appa-native-fixture";

fn arguments(mode: &str, directory: &Path) -> Vec<OsString> {
    ["--ignored", "--exact", FIXTURE, "--nocapture", "--", MARKER, mode]
        .into_iter()
        .map(OsString::from)
        .chain([directory.as_os_str().to_owned()])
        .collect()
}

fn await_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(Instant::now() < deadline, "fixture did not produce {}", path.display());
        std::thread::sleep(Duration::from_millis(25));
    }
}

struct Descendant(OwnedHandle);

impl Descendant {
    fn read(directory: &Path) -> Self {
        let path = directory.join("pid");
        await_file(&path);
        let pid: u32 = fs::read_to_string(path).unwrap().parse().unwrap();
        // The fixture waits for a release file, keeping this process alive
        // while the test obtains its handle. Assertions never rely on PID reuse.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        assert!(!handle.is_null(), "{}", std::io::Error::last_os_error());
        Self(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn assert_stopped(&self) {
        assert_eq!(
            unsafe { WaitForSingleObject(self.0.as_raw_handle(), 5000) },
            WAIT_OBJECT_0
        );
    }

    fn assert_alive(&self) {
        assert_eq!(unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) }, WAIT_TIMEOUT);
    }
}

impl Drop for Descendant {
    fn drop(&mut self) {
        unsafe {
            TerminateProcess(self.0.as_raw_handle(), 1);
        }
        unsafe {
            WaitForSingleObject(self.0.as_raw_handle(), 5000);
        }
    }
}

fn primary_exited(child: &mut NativeChild) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "primary fixture did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn job_owns_descendants_after_primary_exit_and_releases_success() {
    for mode in ["fail", "success", "powershell-fail", "powershell-success"] {
        let directory = tempfile::tempdir().unwrap();
        let stdout = tempfile::tempfile().unwrap();
        let stderr = tempfile::tempfile().unwrap();
        let args = arguments(mode, directory.path());
        let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
        let mut primary = NativeChild::spawn(&std::env::current_exe().unwrap(), &refs, &stdout, &stderr).unwrap();
        let descendant = Descendant::read(directory.path());
        descendant.assert_alive();
        fs::write(directory.path().join("release"), b"go").unwrap();
        let status = primary_exited(&mut primary);
        if mode.ends_with("success") {
            assert!(status.success());
            primary.complete().unwrap();
            drop(primary);
            descendant.assert_alive();
        } else {
            assert!(!status.success());
            primary.terminate().unwrap();
            descendant.assert_stopped();
        }
    }
}

#[test]
fn dropping_job_stops_live_primary_and_descendant() {
    let directory = tempfile::tempdir().unwrap();
    let stdout = tempfile::tempfile().unwrap();
    let stderr = tempfile::tempfile().unwrap();
    let args = arguments("timeout", directory.path());
    let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
    let primary = NativeChild::spawn(&std::env::current_exe().unwrap(), &refs, &stdout, &stderr).unwrap();
    let descendant = Descendant::read(directory.path());
    drop(primary);
    descendant.assert_stopped();
}

#[test]
fn supervisor_exit_closes_nested_job_without_inherited_job_handles() {
    let directory = tempfile::tempdir().unwrap();
    let stdout = tempfile::tempfile().unwrap();
    let stderr = tempfile::tempfile().unwrap();
    let args = arguments("supervisor", directory.path());
    let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
    let mut supervisor = NativeChild::spawn(&std::env::current_exe().unwrap(), &refs, &stdout, &stderr).unwrap();
    let descendant = Descendant::read(directory.path());
    fs::write(directory.path().join("supervisor-exit"), b"go").unwrap();
    assert!(primary_exited(&mut supervisor).success());
    // The outer job is still open. Only the nested supervisor's OS handle
    // cleanup can have terminated this descendant: Rust Drop was bypassed.
    descendant.assert_stopped();
    supervisor.complete().unwrap();
}

#[test]
fn invoke_bounds_output_and_time_and_cleans_descendants() {
    for mode in ["noisy", "stderr", "timeout"] {
        let directory = tempfile::tempdir().unwrap();
        let args = arguments(mode, directory.path());
        let task = std::thread::spawn(move || {
            let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
            invoke(&std::env::current_exe().unwrap(), &refs, Duration::from_secs(10))
        });
        let descendant = Descendant::read(directory.path());
        fs::write(directory.path().join("release"), b"go").unwrap();
        assert!(task.join().unwrap().is_err());
        descendant.assert_stopped();
    }
}

#[test]
fn spawn_preserves_rust_arguments_and_refuses_invalid_executables() {
    let directory = tempfile::tempdir().unwrap();
    let expected = [
        "",
        "plain",
        "with spaces",
        "λ-日本語",
        "quote\"inside",
        "trailing\\",
        "space and trailing\\",
        "\\\\\"quoted",
    ];
    let mut args = arguments("echo", directory.path());
    args.extend(expected.iter().map(OsString::from));
    let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
    invoke(&std::env::current_exe().unwrap(), &refs, Duration::from_secs(10)).unwrap();
    let observed: Vec<String> = serde_json::from_slice(&fs::read(directory.path().join("args.json")).unwrap()).unwrap();
    assert_eq!(observed, expected);
    let stdout = tempfile::tempfile().unwrap();
    let stderr = tempfile::tempfile().unwrap();
    assert!(NativeChild::spawn(&directory.path().join("absent.exe"), &[], &stdout, &stderr).is_err());
    let invalid = directory.path().join("invalid.exe");
    fs::write(&invalid, b"not an executable").unwrap();
    assert!(NativeChild::spawn(&invalid, &[], &stdout, &stderr).is_err());
}

#[test]
#[ignore = "subprocess fixture, called by Windows containment tests"]
fn child_fixture() {
    let args: Vec<_> = std::env::args_os().collect();
    let Some(marker) = args.iter().position(|arg| arg == OsStr::new(MARKER)) else {
        return;
    };
    let mode = args[marker + 1].to_str().unwrap();
    let directory = Path::new(&args[marker + 2]);
    if mode == "echo" {
        let values: Vec<_> = args[marker + 3..].iter().map(|arg| arg.to_str().unwrap()).collect();
        fs::write(directory.join("args.json"), serde_json::to_vec(&values).unwrap()).unwrap();
        std::process::exit(0);
    }
    if mode == "sleep" {
        // Finite even if a broken test loses its cleanup handle.
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(0);
    }
    let executable = std::env::current_exe().unwrap();
    if mode == "supervisor" {
        let stdout = tempfile::tempfile().unwrap();
        let stderr = tempfile::tempfile().unwrap();
        let args = arguments("timeout", directory);
        let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
        let _child = NativeChild::spawn(&executable, &refs, &stdout, &stderr).unwrap();
        await_file(&directory.join("supervisor-exit"));
        std::process::exit(0);
    }
    let child_args = arguments("sleep", directory);
    let pid = if mode.starts_with("powershell-") {
        // Same Start-Process options as hook.ps1; the executable is a Rust
        // fixture instead of a listening runtime. Inputs are child-only env.
        let argument_line = child_args
            .iter()
            .map(|arg| format!("\"{}\"", arg.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$p = Start-Process -FilePath $env:APPA_TEST_EXECUTABLE -WindowStyle Hidden -ArgumentList $env:APPA_TEST_ARGUMENTS -PassThru; [Console]::Write($p.Id)"])
            .env("APPA_TEST_EXECUTABLE", executable)
            .env("APPA_TEST_ARGUMENTS", argument_line)
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().parse::<u32>().unwrap()
    } else {
        Command::new(executable)
            .args(child_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
            .id()
    };
    fs::write(directory.join("pid.tmp"), pid.to_string()).unwrap();
    fs::rename(directory.join("pid.tmp"), directory.join("pid")).unwrap();
    await_file(&directory.join("release"));
    match mode {
        "noisy" => {
            std::io::stdout().write_all(&vec![b'x'; 70000]).unwrap();
        }
        "stderr" => {
            std::io::stderr().write_all(&vec![b'x'; 70000]).unwrap();
        }
        "timeout" => {
            std::thread::sleep(Duration::from_secs(60));
        }
        _ => {}
    }
    std::process::exit(if mode.ends_with("success") { 0 } else { 7 });
}

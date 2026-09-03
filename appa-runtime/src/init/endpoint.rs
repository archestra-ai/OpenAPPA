//! Who owns the runtime endpoint, and what init may do about it.

use crate::plugin_bundle::Endpoint;
#[cfg(unix)]
use std::ffi::OsStr;
use std::io::IsTerminal;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::{Command, Output};

use super::config::ComposedPolicy;
#[cfg(windows)]
use super::powershell;
use super::receipt::friendly_path;
use super::{Answer, Confirmation, InitError};

/// What this init did about the policy the running runtime serves.
///
/// The starter leaves an already-healthy runtime alone, and that process keeps serving the
/// policy it loaded at startup. A restart loads this file itself, so the keys agree and
/// there is nothing to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeOutcome {
    /// Serving the configuration this init validated, with nothing to reconcile.
    Healthy,
    /// Serving an older policy until it was reloaded, at the user's word.
    Reloaded,
    /// Still serving an older policy, because the user declined the reload.
    OlderPolicy,
}

impl RuntimeOutcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            RuntimeOutcome::Healthy => "healthy",
            RuntimeOutcome::Reloaded => "healthy (policy reloaded)",
            RuntimeOutcome::OlderPolicy => "healthy (serving an older policy)",
        }
    }
}

/// Why a running runtime may not be answering under the file this init validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Divergence {
    /// It serves a different policy than this file composes to.
    Serving,
    /// Whether it serves this file cannot be established here.
    Unestablished,
}

/// Who is answering the endpoint, as far as one probe can establish.
///
/// A healthy runtime left by an init under a different `APPA_INSTALL_DIR` or
/// `APPA_DATA_DIR` is foreign: it is named, and stopped only after the user
/// confirms it and only when it identifies a same-user `appa` pid. Stale
/// runtimes are cleared before this classification through their separate
/// health protocol.
#[derive(Debug, PartialEq, Eq)]
enum EndpointOwner {
    /// Nothing answered, or what answered serves no fingerprint. Before the
    /// start this is the ordinary case; after it, it is a failure.
    Unidentified,
    /// The binary whose bytes were offered for comparison, serving this
    /// configuration, in the process it names.
    Deployment { pid: i32 },
    /// A different build or a different configuration, naming the pid that
    /// serves it.
    Foreign { pid: i32 },
}

/// How long init waits for a runtime it asked to stop, and how often it looks.
///
/// Every wait for a stopping runtime uses the same budget: a runtime that outlives
/// one of them has outlived all of them, and `RuntimeSurvived` means the same thing
/// wherever it is raised.
const STOP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Every question init asks a runtime goes out through here, so the flags that
/// decide how long init waits and whether curl reports its own failure have one
/// definition rather than one per question.
fn ask_endpoint(endpoint: &Endpoint, path: &str, arguments: &[&str]) -> std::io::Result<Output> {
    Command::new("curl").args(arguments).arg(endpoint.join(path)).output()
}

pub(super) fn endpoint_health(endpoint: &Endpoint) -> Result<Option<String>, InitError> {
    let output = ask_endpoint(endpoint, "/health", &["--fail", "--silent", "--max-time", "2"]).map_err(|error| {
        InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: error.to_string(),
        }
    })?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn positive_pid(pid: &str) -> Option<i32> {
    if pid.is_empty() || pid.starts_with('0') || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

fn stale_pid(answer: &str) -> Option<i32> {
    positive_pid(answer.strip_prefix("stale ")?)
}

/// Stop the exact stale APPA runtime named by the endpoint before classifying
/// any remaining responder as foreign.
///
/// The runtime's own health protocol names its pid. The pid is not trusted by
/// itself; init applies the same same-user/process-name check as the shipped
/// starter before sending a signal. An `ok`, malformed, or absent health answer
/// never grants shutdown authority.
pub(super) fn clear_stale_endpoint(endpoint: &Endpoint) -> Result<(), InitError> {
    let Some(answer) = endpoint_health(endpoint)? else {
        return Ok(());
    };
    let Some(pid) = stale_pid(&answer) else {
        return Ok(());
    };
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!(
                "the endpoint names stale pid {pid}, but it is not this user's appa runtime; not stopping it"
            ),
        });
    }
    // Close the validation-to-signal race: the process must still be the one
    // answering with the same stale pid immediately before it is terminated.
    match endpoint_health(endpoint)? {
        None => return Ok(()),
        Some(ref current) if current == "ok" => return Ok(()),
        Some(ref current) if stale_pid(current) == Some(pid) => {}
        Some(_) => {
            return Err(InitError::RuntimeIdentity {
                endpoint: endpoint.url().to_owned(),
                message: format!("the endpoint changed ownership before stale pid {pid} could be stopped"),
            });
        }
    }
    terminate_owned_appa_runtime(pid, endpoint)?;

    let deadline = std::time::Instant::now() + STOP_DEADLINE;
    while std::time::Instant::now() < deadline {
        match endpoint_health(endpoint)? {
            None => return Ok(()),
            Some(ref current) if current == "ok" => return Ok(()),
            Some(ref current) if stale_pid(current) == Some(pid) => {
                std::thread::sleep(STOP_POLL);
            }
            Some(_) => {
                return Err(InitError::RuntimeIdentity {
                    endpoint: endpoint.url().to_owned(),
                    message: format!("the endpoint changed ownership while stale pid {pid} was stopping"),
                });
            }
        }
    }
    Err(InitError::RuntimeSurvived {
        pid,
        endpoint: endpoint.url().to_owned(),
    })
}

/// Signal `pid`, confirming immediately before that it is still this user's
/// appa runtime: the check-to-signal window is what a forged answer would use.
fn terminate_owned_appa_runtime(pid: i32, endpoint: &Endpoint) -> Result<(), InitError> {
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("pid {pid} is not this user's appa runtime; not stopping it"),
        });
    }
    terminate_appa_pid(pid)
}

/// Stop a runtime this init started, and wait for its process to go.
pub(super) fn stop_owned_appa_runtime(pid: i32, endpoint: &Endpoint) -> Result<(), InitError> {
    terminate_owned_appa_runtime(pid, endpoint)?;
    let deadline = std::time::Instant::now() + STOP_DEADLINE;
    while std::time::Instant::now() < deadline {
        if !process_exists(pid) {
            return Ok(());
        }
        std::thread::sleep(STOP_POLL);
    }
    Err(InitError::RuntimeSurvived {
        pid,
        endpoint: endpoint.url().to_owned(),
    })
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(windows)]
fn process_exists(pid: i32) -> bool {
    powershell(
        "if (Get-Process -Id $env:APPA_STALE_PID -ErrorAction SilentlyContinue) { 'alive' }",
        [("APPA_STALE_PID", pid.to_string())],
    )
    .is_ok_and(|answer| answer.trim() == "alive")
}

#[cfg(unix)]
fn is_owned_appa_runtime(pid: i32) -> Result<bool, InitError> {
    if pid == std::process::id() as i32 {
        return Ok(false);
    }
    let query = |field: &str| -> Option<String> {
        let output = Command::new("ps")
            .args(["-o", field, "-p", &pid.to_string()])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(uid) = query("uid=").and_then(|uid| uid.parse::<u32>().ok()) else {
        return Ok(false);
    };
    if uid != unsafe { libc::geteuid() } {
        return Ok(false);
    }
    let Some(command_name) = query("comm=") else {
        return Ok(false);
    };
    if Path::new(&command_name).file_name().and_then(OsStr::to_str) != Some("appa") {
        return Ok(false);
    }
    Ok(true)
}

#[cfg(unix)]
fn terminate_appa_pid(pid: i32) -> Result<(), InitError> {
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let source = std::io::Error::last_os_error();
    if source.kind() == std::io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(InitError::InstallRuntime {
        path: PathBuf::from(format!("pid {pid}")),
        source,
    })
}

#[cfg(windows)]
const WINDOWS_APPA_IDENTITY_SCRIPT: &str = r#"
$appaPid = [int]$env:APPA_STALE_PID
$appaProcess = Get-Process -Id $appaPid -ErrorAction SilentlyContinue
$appaState = 'missing'
if ($null -ne $appaProcess) {
    $appaState = 'foreign'
    $appaCim = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $appaPid" -ErrorAction SilentlyContinue
    $appaOwner = if ($null -ne $appaCim) {
        Invoke-CimMethod -InputObject $appaCim -MethodName GetOwnerSid -ErrorAction SilentlyContinue
    } else {
        $null
    }
    $appaCallerSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($appaProcess.ProcessName -ieq 'appa' -and
        $null -ne $appaCim -and $appaCim.Name -ieq 'appa.exe' -and
        $null -ne $appaOwner -and $appaOwner.ReturnValue -eq 0 -and
        $appaOwner.Sid -eq $appaCallerSid) {
        $appaState = 'owned'
    }
}
"#;

#[cfg(windows)]
fn is_owned_appa_runtime(pid: i32) -> Result<bool, InitError> {
    let command = format!("{WINDOWS_APPA_IDENTITY_SCRIPT}\n$appaState");
    let answer = powershell(&command, [("APPA_STALE_PID", pid.to_string())])?;
    Ok(answer.trim() == "owned")
}

#[cfg(windows)]
fn terminate_appa_pid(pid: i32) -> Result<(), InitError> {
    // Resolve the process object first and stop that object, not a freshly
    // looked-up PID. The SID/name checks are repeated in this same PowerShell
    // invocation so an elevated init never turns a forged health answer into
    // authority to terminate another user's process.
    let command = format!(
        "{WINDOWS_APPA_IDENTITY_SCRIPT}\n\
         if ($appaState -eq 'missing') {{ exit 0 }}\n\
         if ($appaState -ne 'owned') {{ throw 'pid is not this user''s appa runtime' }}\n\
         if (-not $appaProcess.HasExited) {{ \
             Stop-Process -InputObject $appaProcess -Force -ErrorAction Stop \
         }}"
    );
    powershell(&command, [("APPA_STALE_PID", pid.to_string())]).map(drop)
}

/// Who owns the endpoint: this deployment, another one, or a process that will not say.
///
/// A deployment is a build *and* the configuration it serves. Comparing builds alone makes
/// every install of one build look like the same deployment, which is how an install ends
/// up reloading, and reporting on, a runtime that is not its own.
fn endpoint_owner(binary: &Path, config: &Path, endpoint: &Endpoint) -> Result<EndpointOwner, InitError> {
    let expected = crate::runtime_cli::binary_digest(binary).map_err(|source| InitError::InstallRuntime {
        path: binary.to_path_buf(),
        source,
    })?;
    let output = ask_endpoint(
        endpoint,
        "/binary-fingerprint",
        &["--fail", "--silent", "--max-time", "2"],
    )
    .map_err(|error| InitError::RuntimeIdentity {
        endpoint: endpoint.url().to_owned(),
        message: error.to_string(),
    })?;
    if !output.status.success() {
        return Ok(EndpointOwner::Unidentified);
    }
    let answer = String::from_utf8_lossy(&output.stdout);
    classify_endpoint_owner(&expected, config, endpoint, &answer)
}

/// A process is this deployment only when it names both this build and this configuration.
/// Anything else — a different build, a different config, or an answer that names no
/// config at all — is another deployment, to be stopped before this install proceeds.
/// Either way the answer must name the pid that serves it: a runtime that cannot be
/// stopped by pid can be neither cleared nor rolled back.
fn classify_endpoint_owner(
    expected: &str,
    config: &Path,
    endpoint: &Endpoint,
    answer: &str,
) -> Result<EndpointOwner, InitError> {
    let (identity, rest) = answer.split_once('\n').unwrap_or((answer, ""));
    let mut fields = identity.split_whitespace();
    let actual = fields.next().unwrap_or_default();
    let pid = fields
        .next()
        .and_then(positive_pid)
        .ok_or_else(|| InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "the answering runtime does not identify its pid; stop it and rerun init".to_owned(),
        })?;
    // Everything after the first newline is the path, less the one the transport appends:
    // a config path may itself hold a newline, and splitting again would truncate it.
    let serves = rest.strip_suffix('\n').unwrap_or(rest);
    if actual == expected && !serves.is_empty() && Path::new(serves) == config {
        Ok(EndpointOwner::Deployment { pid })
    } else {
        Ok(EndpointOwner::Foreign { pid })
    }
}

fn confirm_stop(pid: i32, endpoint: &Endpoint) -> Result<Answer, InitError> {
    let stop = Confirmation {
        question: format!(
            "appa: another appa deployment (pid {pid}) owns {}. Stop it and continue?",
            endpoint.url()
        ),
        default: Answer::Yes,
    };
    let stdin = std::io::stdin();
    let stderr = std::io::stderr();
    stop.ask(&mut stdin.lock(), &mut stderr.lock())
        .map_err(|source| InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("cannot ask permission to stop pid {pid}: {source}"),
        })
}

/// Clear a foreign owner while Claude and the launcher are still untouched.
///
/// Silence is accepted because that is a first install. A foreign process is
/// eligible only when the APPA identity response names a same-user `appa`
/// process and the user confirms the stop. The identity is checked again
/// immediately before signalling to close the prompt-to-kill race.
pub(super) fn clear_foreign_endpoint(binary: &Path, config: &Path, endpoint: &Endpoint) -> Result<(), InitError> {
    match endpoint_owner(binary, config, endpoint)? {
        EndpointOwner::Deployment { .. } | EndpointOwner::Unidentified => Ok(()),
        EndpointOwner::Foreign { pid } => clear_confirmed_foreign_with(binary, config, endpoint, pid, confirm_stop),
    }
}

fn clear_confirmed_foreign_with(
    binary: &Path,
    config: &Path,
    endpoint: &Endpoint,
    pid: i32,
    confirm: impl FnOnce(i32, &Endpoint) -> Result<Answer, InitError>,
) -> Result<(), InitError> {
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!(
                "a different build names pid {pid}, but it is not this user's appa runtime; not stopping it"
            ),
        });
    }
    if confirm(pid, endpoint)? == Answer::No {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("another appa deployment (pid {pid}) still owns this endpoint; init cancelled"),
        });
    }
    match endpoint_owner(binary, config, endpoint)? {
        EndpointOwner::Unidentified => return Ok(()),
        EndpointOwner::Deployment { .. } => return Ok(()),
        EndpointOwner::Foreign { pid: current } if current == pid => {}
        EndpointOwner::Foreign { .. } => {
            return Err(InitError::RuntimeIdentity {
                endpoint: endpoint.url().to_owned(),
                message: "the endpoint changed ownership after approval; not stopping either process".to_owned(),
            });
        }
    }
    terminate_owned_appa_runtime(pid, endpoint)?;

    let deadline = std::time::Instant::now() + STOP_DEADLINE;
    while std::time::Instant::now() < deadline {
        match endpoint_health(endpoint)? {
            None => return Ok(()),
            Some(_) => std::thread::sleep(STOP_POLL),
        }
    }
    Err(InitError::RuntimeSurvived {
        pid,
        endpoint: endpoint.url().to_owned(),
    })
}

/// Reconcile the policy a surviving runtime serves with the file this init validated.
///
/// The starter replaces a runtime whose executable changed, and that fresh process loads
/// this file itself. A runtime it left running does not: it still serves what it loaded at
/// startup, and a config written since is on disk only. Comparing the two keys keeps the
/// question to the case that has one — an install that changed nothing asks nothing.
pub(super) fn reconcile_policy(
    endpoint: &Endpoint,
    config: &Path,
    composed: &ComposedPolicy,
) -> Result<RuntimeOutcome, InitError> {
    let Some(divergence) = policy_divergence(composed, &serving_policy_key(endpoint)?) else {
        return Ok(RuntimeOutcome::Healthy);
    };
    if confirm_reload(config, divergence)? == Answer::No {
        return Ok(RuntimeOutcome::OlderPolicy);
    }
    reload_policy(endpoint, config)?;
    Ok(RuntimeOutcome::Reloaded)
}

/// Why a serving runtime may not be answering under the file this init validated, or
/// `None` when it demonstrably is.
///
/// A config init cannot compose is not settled by assumption: the runtime can be asked,
/// and only a person can decide, so the question is put. The reload itself resolves the
/// secret where the runtime runs, which is the environment that has it.
fn policy_divergence(composed: &ComposedPolicy, serving: &str) -> Option<Divergence> {
    match composed {
        ComposedPolicy::Key(key) if key == serving => None,
        ComposedPolicy::Key(_) => Some(Divergence::Serving),
        ComposedPolicy::Unknowable => Some(Divergence::Unestablished),
    }
}

/// The policy key the endpoint answers under. A runtime that does not answer for one
/// cannot be reconciled, and a plugin bound to it is the skew init exists to prevent.
fn serving_policy_key(endpoint: &Endpoint) -> Result<String, InitError> {
    let refused = |message: String| InitError::PolicyKey {
        endpoint: endpoint.url().to_owned(),
        message,
    };
    let output = ask_endpoint(
        endpoint,
        "/policy-key",
        &["--fail", "--silent", "--show-error", "--max-time", "2"],
    )
    .map_err(|error| refused(error.to_string()))?;
    if !output.status.success() {
        return Err(refused(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        return Err(refused("the answer names no policy key".to_owned()));
    }
    Ok(key)
}

/// Ask the running runtime to serve the configuration on disk.
///
/// The endpoint's owner is this deployment by the time this runs, so the reload reads this
/// deployment's file. The runtime validates it again before it swaps: a refusal here is a
/// fault worth naming, not a receipt footnote, because the older policy keeps serving.
fn reload_policy(endpoint: &Endpoint, config: &Path) -> Result<(), InitError> {
    let refused = |message: String| InitError::ReloadRefused {
        endpoint: endpoint.url().to_owned(),
        path: config.to_path_buf(),
        message,
    };
    let output = ask_endpoint(
        endpoint,
        "/reload",
        &["--fail", "--silent", "--show-error", "--max-time", "10", "-X", "POST"],
    )
    .map_err(|error| refused(error.to_string()))?;
    if !output.status.success() {
        return Err(refused(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
    }
    Ok(())
}

/// A terminal is asked; anything else reloads. A script that just wrote a config wants it
/// serving, and there is no one there to answer.
fn confirm_reload(config: &Path, divergence: Divergence) -> Result<Answer, InitError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(Answer::Yes);
    }
    let prompt = |source| InitError::WriteFile {
        path: config.to_path_buf(),
        source,
    };
    let config = friendly_path(config);
    // Each case states exactly what init established, and no more: one knows the running
    // runtime serves something else, the other knows only that it cannot tell.
    let established = match divergence {
        Divergence::Serving => {
            format!("appa: the running runtime still serves the policy it started with, not {config}.")
        }
        Divergence::Unestablished => format!(
            "appa: {config} resolves a secret only where the runtime runs, so this cannot tell\n\
             whether the running runtime already serves it."
        ),
    };
    let reload = Confirmation {
        question: format!(
            "{established}\nReload it now? Sessions open right now keep the deployment they started with."
        ),
        default: Answer::Yes,
    };
    let stderr = std::io::stderr();
    reload.ask(&mut stdin.lock(), &mut stderr.lock()).map_err(prompt)
}

/// The endpoint answers for this deployment: this build, serving this configuration.
/// Answers with the pid of the process serving it.
pub(super) fn verify_runtime_deployment(runtime: &Path, config: &Path, endpoint: &Endpoint) -> Result<i32, InitError> {
    match endpoint_owner(runtime, config, endpoint)? {
        EndpointOwner::Deployment { pid } => Ok(pid),
        EndpointOwner::Unidentified => Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "stop it, then run `appa init` again.".to_owned(),
        }),
        EndpointOwner::Foreign { .. } => Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "another appa deployment is answering; stop that process and rerun init".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    /// A stand-in the ownership check can find and the signal can reach.
    ///
    /// macOS kills a copied platform binary outright -- a copy of `/bin/sh` or
    /// `/bin/sleep` dies with SIGKILL before it runs a single instruction -- so
    /// a test built on one would pass without ever exercising the stop,
    /// because a killed process is also a stopped one. `perl` copies and runs.
    #[cfg(unix)]
    const STAND_IN: &str = "/usr/bin/perl";

    /// What every starter runs: the subcommand plus the endpoint it binds.
    #[cfg(unix)]
    const RUNTIME_ARGUMENTS: &[&str] = &["runtime", "--listen", "127.0.0.1:8787"];

    /// A process whose executable really *is* `at`, started with `arguments`, so
    /// the ownership check sees the process name it decides on.
    ///
    /// The stand-in is reaped on its own thread. A dead child that nobody has
    /// waited for is a zombie, and `kill(pid, 0)` still succeeds on one, so
    /// without the reaper these tests could not tell a stopped process from a
    /// running one. In production a stopped runtime is never init's child.
    #[cfg(unix)]
    fn process_executing(at: &Path, arguments: &[&str]) -> Option<i32> {
        use std::os::unix::fs::PermissionsExt;

        if !Path::new(STAND_IN).is_file() {
            return None;
        }
        fs::create_dir_all(at.parent().expect("a parent")).expect("the directory exists");
        fs::copy(STAND_IN, at).expect("the stand-in executable is copied");
        fs::set_permissions(at, fs::Permissions::from_mode(0o755)).expect("the stand-in is executable");

        // The stand-in announces itself by creating a file. Waiting on a signal
        // it sends is the only reliable liveness proof here: a copied binary the
        // platform refuses to run leaves a zombie that `kill(pid, 0)` still
        // reports as alive, so checking the pid would accept a process that
        // never ran.
        let ready = at.with_extension("ready");
        let script = "open(my $f, '>', $ARGV[0]) or die; close $f; sleep 30";
        let mut child = Command::new(at)
            .args(["-e", script])
            .arg(&ready)
            .args(arguments)
            .spawn()
            .expect("the stand-in process starts");
        let pid = child.id() as i32;
        std::thread::spawn(move || child.wait());

        let deadline = std::time::Instant::now() + STOP_DEADLINE;
        while std::time::Instant::now() < deadline {
            if ready.is_file() {
                return Some(pid);
            }
            std::thread::sleep(STOP_POLL);
        }
        // This platform will not run the stand-in at all. Skip rather than
        // report a pass that exercised nothing.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        None
    }

    /// Whether a pid is still running, once its reaper has had a moment.
    #[cfg(unix)]
    fn still_running(pid: i32) -> bool {
        std::thread::sleep(std::time::Duration::from_millis(300));
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// A loopback fixture serving `answers` in turn, with the request lines it served. A
    /// probe's path is part of the contract it has with the runtime, so a test that cares
    /// which endpoint init asks reads them.
    #[cfg(unix)]
    fn recorded_answers(answers: Vec<String>) -> (Endpoint, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback health fixture binds");
        let endpoint = Endpoint::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&asked);
        std::thread::spawn(move || {
            for answer in answers {
                let (mut connection, _) = listener.accept().expect("the health probe connects");
                let mut request = [0u8; 2048];
                let read = connection.read(&mut request).unwrap_or(0);
                let requested = String::from_utf8_lossy(&request[..read])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                recorder
                    .lock()
                    .expect("the request recorder is never poisoned")
                    .push(requested);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                connection
                    .write_all(response.as_bytes())
                    .expect("the health answer writes");
            }
        });
        (endpoint, asked)
    }

    #[test]
    fn only_canonical_stale_health_answers_grant_a_pid() {
        assert_eq!(stale_pid("stale 42"), Some(42));
        for answer in [
            "",
            "ok",
            "stale",
            "stale ",
            "stale 0",
            "stale 01",
            "stale -1",
            "stale 42 extra",
        ] {
            assert_eq!(stale_pid(answer), None, "accepted {answer:?}");
        }
    }

    #[test]
    fn only_this_build_serving_this_config_is_this_deployment() {
        let endpoint = Endpoint::parse("http://127.0.0.1:8787").expect("the endpoint parses");
        let classify = |expected: &str, config: &Path, answer: &str| {
            classify_endpoint_owner(expected, config, &endpoint, answer).expect("the answer classifies")
        };
        let mine = Path::new("/home/user/config/appa.toml");
        assert_eq!(
            classify("same", mine, "same 42\n/home/user/config/appa.toml"),
            EndpointOwner::Deployment { pid: 42 }
        );
        // The build alone never settles it: one build serves as many deployments as there
        // are configurations, and each is a stranger to the others.
        assert_eq!(
            classify("same", mine, "same 42\n/home/other/config/appa.toml"),
            EndpointOwner::Foreign { pid: 42 }
        );
        // A path with spaces is one path, not two fields.
        let spaced = Path::new("/home/user/Application Support/appa.toml");
        assert_eq!(
            classify("same", spaced, "same 42\n/home/user/Application Support/appa.toml"),
            EndpointOwner::Deployment { pid: 42 }
        );
        // On Unix a directory name may hold a newline, so the path is read as the whole
        // remainder of the answer the runtime composes — and a transport that appends a
        // newline of its own does not turn one deployment into a stranger.
        let newlined = Path::new("/home/user/two\nlines/appa.toml");
        assert_eq!(
            classify("same", newlined, "same 42\n/home/user/two\nlines/appa.toml"),
            EndpointOwner::Deployment { pid: 42 }
        );
        assert_eq!(
            classify("same", mine, "same 42\n/home/user/config/appa.toml\n"),
            EndpointOwner::Deployment { pid: 42 }
        );
        assert_eq!(
            classify("same", mine, "different 42\n/home/user/config/appa.toml"),
            EndpointOwner::Foreign { pid: 42 }
        );
        // An answer that names no configuration cannot claim to be this deployment, and
        // one that names no pid cannot be stopped, so it is refused outright.
        assert_eq!(classify("same", mine, "same 42"), EndpointOwner::Foreign { pid: 42 });
        assert!(matches!(
            classify_endpoint_owner("same", mine, &endpoint, "different"),
            Err(InitError::RuntimeIdentity { .. })
        ));
    }

    #[test]
    fn a_serving_runtime_is_reconciled_only_when_agreement_is_not_established() {
        let key = |key: &str| ComposedPolicy::Key(key.to_string());
        assert_eq!(
            policy_divergence(&key("composed"), "serving"),
            Some(Divergence::Serving)
        );
        // An install that changed nothing must ask nothing.
        assert_eq!(policy_divergence(&key("same"), "same"), None);
        // A config this process cannot compose is unsettled, never settled: assuming
        // agreement here is what would leave an older policy serving unremarked.
        assert_eq!(
            policy_divergence(&ComposedPolicy::Unknowable, "serving"),
            Some(Divergence::Unestablished)
        );
    }

    #[test]
    fn a_serving_policy_key_is_read_from_the_policy_route() {
        let (endpoint, asked) = recorded_answers(vec!["c54f1509".to_string()]);
        assert_eq!(serving_policy_key(&endpoint).expect("the key reads"), "c54f1509");
        assert_eq!(
            asked.lock().expect("the request recorder is never poisoned").as_slice(),
            ["GET /policy-key HTTP/1.1".to_string()],
            "the probe reads the policy route, and reads it without mutating"
        );
    }

    /// A runtime that answers nothing usable, and a port nothing answers on, both
    /// refuse init: a plugin bound to a runtime whose policy cannot be established is
    /// the skew init exists to prevent.
    #[test]
    fn a_runtime_that_does_not_answer_for_its_policy_refuses_init() {
        let blank = recorded_answers(vec![String::new()]).0;
        assert!(matches!(serving_policy_key(&blank), Err(InitError::PolicyKey { .. })));
        let unbound = Endpoint::parse("http://127.0.0.1:1").expect("the endpoint parses");
        assert!(matches!(serving_policy_key(&unbound), Err(InitError::PolicyKey { .. })));
    }

    #[test]
    fn a_matching_policy_key_reconciles_without_asking_or_reloading() {
        // One answer is served: the key probe. A reload would need a second connection,
        // so reaching one at all would hang rather than pass.
        let endpoint = recorded_answers(vec!["agreed".to_string()]).0;
        let config = PathBuf::from("/home/user/config/appa.toml");
        assert_eq!(
            reconcile_policy(&endpoint, &config, &ComposedPolicy::Key("agreed".to_string()))
                .expect("the reconcile completes"),
            RuntimeOutcome::Healthy
        );
    }

    #[test]
    fn a_reload_that_installed_this_deployments_policy_is_reported_as_reloaded() {
        let (endpoint, _asked) = recorded_answers(vec![
            "older".to_string(),
            r#"{"policy_key":"this-deployment","policy_identity":"x","changed":true}"#.to_string(),
        ]);
        let config = PathBuf::from("/home/user/config/appa.toml");
        assert_eq!(
            reconcile_policy(&endpoint, &config, &ComposedPolicy::Key("this-deployment".to_string()))
                .expect("the reconcile completes"),
            RuntimeOutcome::Reloaded
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_approved_foreign_appa_runtime_is_stopped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let foreign = directory.path().join("foreign/appa");
        let Some(pid) = process_executing(&foreign, RUNTIME_ARGUMENTS) else {
            return;
        };
        let candidate = directory.path().join("candidate-appa");
        fs::write(&candidate, "a different candidate build").expect("the candidate binary exists");
        let endpoint = recorded_answers(vec![format!("different-fingerprint {pid}")]).0;

        let config = directory.path().join("appa.toml");
        clear_confirmed_foreign_with(&candidate, &config, &endpoint, pid, |approved_pid, _| {
            assert_eq!(approved_pid, pid);
            Ok(Answer::Yes)
        })
        .expect("the approved foreign runtime stops");

        assert!(!still_running(pid), "the approved runtime still owns its process");
    }

    #[cfg(unix)]
    #[test]
    fn init_reclaims_an_unlinked_runtime_named_by_its_stale_health_answer() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let replaced = directory.path().join("bin/appa");
        let Some(pid) = process_executing(&replaced, RUNTIME_ARGUMENTS) else {
            return;
        };
        fs::remove_file(&replaced).expect("the installed binary is unlinked while its runtime remains");
        let endpoint = recorded_answers(vec![format!("stale {pid}"), format!("stale {pid}")]).0;

        clear_stale_endpoint(&endpoint).expect("init stops its stale unlinked runtime");

        assert!(!still_running(pid), "the stale runtime still owns its process");
    }

    #[cfg(unix)]
    #[test]
    fn a_spoofed_stale_pid_does_not_grant_process_shutdown() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let other = directory.path().join("bin/not-appa");
        let Some(pid) = process_executing(&other, RUNTIME_ARGUMENTS) else {
            return;
        };
        let endpoint = recorded_answers(vec![format!("stale {pid}")]).0;

        let refused = clear_stale_endpoint(&endpoint);

        assert!(matches!(refused, Err(InitError::RuntimeIdentity { .. })));
        assert!(still_running(pid), "a non-appa process was terminated");
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

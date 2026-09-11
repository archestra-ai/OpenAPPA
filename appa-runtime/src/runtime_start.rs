//! Bring up the deployed runtime when nothing healthy answers its endpoint,
//! and stop the one that does.
//!
//! Two callers share the start: `appa plugin install claude-code`, as its last
//! step, and every protected SessionStart, before the hook posts its first
//! event. A protected session therefore needs no login service, and an install
//! that ends here leaves the runtime up, so the first protected session pays
//! nothing for the start. The process started is this same executable: a hook
//! entry names the binary its deployment installed, so the runtime it brings up
//! is that deployment's own.
//!
//! Concurrent starts need no lock. The runtime binds the loopback port, so the
//! first process to bind serves and every later one exits at once, and both
//! callers then see the same healthy runtime. The port is a mutex that cannot
//! go stale, where a lock file would outlive a hook the harness kills at its
//! timeout and block every start after it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::init::endpoint::{is_owned_appa_runtime, process_exists, terminate_appa_pid};
use crate::loopback_http::{Deadline, Endpoint, get};
use crate::runtime_url::RuntimeTarget;

/// The health probe is one cheap round trip. A dead loopback port refuses at
/// once on most systems but hangs under some network stacks (WSL2 mirrored
/// networking), where this deadline is what the probe actually costs.
const PROBE_BUDGET: Duration = Duration::from_secs(1);
/// How long a stale runtime gets to leave the port after being signalled.
pub(crate) const STOP_BUDGET: Duration = Duration::from_secs(10);
/// How long a started runtime gets to answer healthy. The whole start must
/// finish inside the timeout the SessionStart hook entry declares.
pub(crate) const START_BUDGET: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Error)]
pub enum StartError {
    #[error("{url} is not a runtime endpoint: {reason}")]
    Endpoint { url: String, reason: String },
    #[error("the installed deployment's paths cannot be resolved: {0}")]
    Paths(String),
    #[error("nothing answers {url}, and a runtime at a URL the session named is the user's own to start")]
    UserOwnedUnreachable { url: String },
    #[error(transparent)]
    Stop(#[from] StopError),
    #[error("cannot create {path}: {source}")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot start the runtime from {executable}: {source}")]
    Spawn {
        executable: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the runtime did not become healthy at {url}. Its own error is the last line of {log}")]
    NotHealthy { url: String, log: PathBuf },
}

/// Why the runtime answering an endpoint was not stopped.
#[derive(Debug, Error)]
pub enum StopError {
    #[error("{url} is not a runtime endpoint: {reason}")]
    Endpoint { url: String, reason: String },
    #[error("the runtime at {url} is the user's own to stop: the session named that URL itself")]
    UserOwned { url: String },
    #[error("{url} answers healthy but does not identify its pid; stop that process yourself")]
    Unidentified { url: String },
    #[error("pid {pid} is not this user's appa runtime; not stopping it")]
    NotOwned { pid: i32 },
    #[error("cannot tell whether pid {pid} is this user's appa runtime: {detail}")]
    Ownership { pid: i32, detail: String },
    #[error("the runtime at {url} (pid {pid}) did not stop")]
    DidNotStop { url: String, pid: i32 },
    #[error("{url} answers neither ok nor stale: {answer:?}")]
    Unexpected { url: String, answer: String },
}

/// What a stop found at the endpoint, and did.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Nothing answered.
    Nothing,
    /// This user's appa runtime answered from the process named, and is gone.
    Runtime { pid: i32 },
}

/// What one health probe found at the endpoint.
enum Health {
    Ok,
    /// The runtime serves a build that an install has since replaced on disk.
    Stale(i32),
    Other(String),
    Unreachable,
}

fn probe(endpoint: &Endpoint) -> Health {
    let Ok(answer) = get(endpoint, "/health", &Deadline::spanning(PROBE_BUDGET)) else {
        return Health::Unreachable;
    };
    if !answer.is_success() {
        return Health::Other(String::from_utf8_lossy(&answer.body).into_owned());
    }
    let body = String::from_utf8_lossy(&answer.body).trim().to_owned();
    match body.as_str() {
        "ok" => Health::Ok,
        _ => match body
            .strip_prefix("stale ")
            .and_then(crate::init::endpoint::positive_pid)
        {
            Some(pid) => Health::Stale(pid),
            None => Health::Other(body),
        },
    }
}

/// Where the runtime this start brings up reads and writes.
pub struct Deployment {
    pub config: PathBuf,
    pub data_dir: PathBuf,
}

impl Deployment {
    /// The given paths, or the installed deployment's own for whichever is absent.
    pub fn installed(config: Option<PathBuf>, data_dir: Option<PathBuf>) -> Result<Self, StartError> {
        let data_dir = match data_dir {
            Some(data_dir) => data_dir,
            None => crate::init::paths::installed_data_dir()
                .map_err(|error| StartError::Paths(error.to_string()))?
                .ok_or_else(|| StartError::Paths("no home directory names the data directory".to_owned()))?,
        };
        Ok(Self {
            config: config.unwrap_or_else(crate::init::installed_config_path),
            data_dir,
        })
    }
}

/// Make a healthy runtime answer `target`, starting `executable` when none does.
///
/// A runtime the user runs at a URL of their own is theirs to start and restart:
/// it is healthy while it answers, stale or not, and nothing is started there
/// when nothing answers. Only the deployment's own endpoint has its runtime
/// started or its stale one replaced, and only when the process answering is
/// this user's own appa process: the pid arrives in an HTTP body from whoever
/// holds the port, so it is checked before it is signalled.
pub fn ensure(target: &RuntimeTarget, deployment: &Deployment, executable: &Path) -> Result<(), StartError> {
    let endpoint = Endpoint::parse(&target.url).map_err(|reason| StartError::Endpoint {
        url: target.url.clone(),
        reason,
    })?;
    match probe(&endpoint) {
        Health::Ok => return Ok(()),
        Health::Stale(_) if target.user_owned => return Ok(()),
        Health::Stale(pid) => {
            if stop_stale(&endpoint, &target.url, pid)? {
                return Ok(());
            }
        }
        Health::Other(answer) => {
            return Err(StopError::Unexpected {
                url: target.url.clone(),
                answer,
            }
            .into());
        }
        Health::Unreachable if target.user_owned => {
            return Err(StartError::UserOwnedUnreachable {
                url: target.url.clone(),
            });
        }
        Health::Unreachable => {}
    }
    start(&endpoint, &target.url, deployment, executable)
}

/// Stop the runtime answering `target`, whichever deployment started it, when
/// it is this user's own appa process.
///
/// An install claims the deployment's endpoint, so a runtime an earlier
/// deployment left there is stopped rather than reported. A process that is
/// not appa, or not this user's, is refused with its pid named: the pid
/// arrives in an HTTP body from whoever holds the port. A runtime at a URL the
/// session named is the user's own, and is left as it is.
pub fn stop(target: &RuntimeTarget) -> Result<Stopped, StopError> {
    let endpoint = Endpoint::parse(&target.url).map_err(|reason| StopError::Endpoint {
        url: target.url.clone(),
        reason,
    })?;
    if target.user_owned {
        return Err(StopError::UserOwned {
            url: target.url.clone(),
        });
    }
    let pid = match probe(&endpoint) {
        Health::Unreachable => return Ok(Stopped::Nothing),
        Health::Stale(pid) => pid,
        Health::Ok => serving_pid(&endpoint).ok_or_else(|| StopError::Unidentified {
            url: target.url.clone(),
        })?,
        Health::Other(answer) => {
            return Err(StopError::Unexpected {
                url: target.url.clone(),
                answer,
            });
        }
    };
    stop_pid(&endpoint, &target.url, pid)?;
    Ok(Stopped::Runtime { pid })
}

/// Stop the process answering `endpoint` as `pid`, once it proves to be this
/// user's appa runtime, and wait until the endpoint no longer answers from
/// it. Gone means that: a process its parent has not reaped yet still
/// exists, and holds nothing.
pub(crate) fn stop_pid(endpoint: &Endpoint, url: &str, pid: i32) -> Result<(), StopError> {
    terminate_owned(pid)?;
    let deadline = Instant::now() + STOP_BUDGET;
    while process_exists(pid) && answering_pid(endpoint) == Some(pid) {
        if Instant::now() >= deadline {
            return Err(StopError::DidNotStop {
                url: url.to_owned(),
                pid,
            });
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// The pid the endpoint answers from, healthy or stale.
fn answering_pid(endpoint: &Endpoint) -> Option<i32> {
    match probe(endpoint) {
        Health::Ok => serving_pid(endpoint),
        Health::Stale(pid) => Some(pid),
        Health::Other(_) | Health::Unreachable => None,
    }
}

/// The pid a healthy runtime names at `/binary-fingerprint`: the second field
/// of its first line.
fn serving_pid(endpoint: &Endpoint) -> Option<i32> {
    let answer = get(endpoint, "/binary-fingerprint", &Deadline::spanning(PROBE_BUDGET)).ok()?;
    if !answer.is_success() {
        return None;
    }
    String::from_utf8_lossy(&answer.body)
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .and_then(crate::init::endpoint::positive_pid)
}

/// Signal `pid` once it proves to be this user's appa runtime. No process at
/// that pid is not a failure: a concurrent stop got there first.
fn terminate_owned(pid: i32) -> Result<(), StopError> {
    if !process_exists(pid) {
        return Ok(());
    }
    match is_owned_appa_runtime(pid) {
        Ok(true) => {}
        Ok(false) => return Err(StopError::NotOwned { pid }),
        Err(error) => {
            return Err(StopError::Ownership {
                pid,
                detail: error.to_string(),
            });
        }
    }
    terminate_appa_pid(pid).map_err(|error| StopError::Ownership {
        pid,
        detail: error.to_string(),
    })
}

/// Stop the stale runtime and wait for the port to refuse. `Ok(true)` means a
/// concurrent starter has already put a healthy runtime there.
fn stop_stale(endpoint: &Endpoint, url: &str, pid: i32) -> Result<bool, StartError> {
    terminate_owned(pid)?;
    let deadline = Instant::now() + STOP_BUDGET;
    loop {
        match probe(endpoint) {
            Health::Unreachable => return Ok(false),
            Health::Ok => return Ok(true),
            Health::Stale(still) if still == pid => {}
            Health::Stale(other) => {
                return Err(StopError::Unexpected {
                    url: url.to_owned(),
                    answer: format!("stale {other}"),
                }
                .into());
            }
            Health::Other(answer) => {
                return Err(StopError::Unexpected {
                    url: url.to_owned(),
                    answer,
                }
                .into());
            }
        }
        if Instant::now() >= deadline {
            return Err(StopError::DidNotStop {
                url: url.to_owned(),
                pid,
            }
            .into());
        }
        std::thread::sleep(POLL);
    }
}

fn start(endpoint: &Endpoint, url: &str, deployment: &Deployment, executable: &Path) -> Result<(), StartError> {
    // The runtime writes the default policy on its first start and refuses to
    // start when it cannot.
    let directory = |path: &Path| {
        std::fs::create_dir_all(path).map_err(|source| StartError::Directory {
            path: path.to_path_buf(),
            source,
        })
    };
    if let Some(parent) = deployment.config.parent() {
        directory(parent)?;
    }
    directory(&deployment.data_dir)?;
    let stderr_log = deployment.data_dir.join("runtime.stderr.log");
    let log = |name: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(deployment.data_dir.join(name))
            .map_err(|source| StartError::Directory {
                path: deployment.data_dir.join(name),
                source,
            })
    };
    let mut command = Command::new(executable);
    command
        .arg("runtime")
        .arg("--listen")
        .arg(endpoint.authority())
        .arg("--config")
        .arg(&deployment.config)
        .arg("--db")
        .arg(deployment.data_dir.join("appa.db"))
        .stdin(Stdio::null())
        .stdout(log("runtime.stdout.log")?)
        .stderr(log("runtime.stderr.log")?);
    detach(&mut command);
    command.spawn().map_err(|source| StartError::Spawn {
        executable: executable.to_path_buf(),
        source,
    })?;

    // A wall-clock budget, not a count of probes: one probe costs microseconds
    // where the port refuses and the full deadline where it hangs. A start that
    // lost the port to a concurrent one is waited on the same way, for the
    // runtime that won it.
    let deadline = Instant::now() + START_BUDGET;
    while Instant::now() < deadline {
        if let Health::Ok = probe(endpoint) {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    Err(StartError::NotHealthy {
        url: url.to_owned(),
        log: stderr_log,
    })
}

/// The runtime outlives the hook that starts it: it leaves the hook's process
/// group so the harness reaping the hook does not take the runtime with it.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime the session named is healthy while it answers at all: a stale
    /// answer there is the user's to act on, and the start returns at once
    /// without signalling anything.
    #[test]
    fn a_stale_runtime_the_session_owns_is_left_alone() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port binds");
        let url = format!("http://{}", listener.local_addr().expect("the bound address"));
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut socket, _) = listener.accept().expect("the probe connects");
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request);
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nstale 99");
        });
        let target = RuntimeTarget { url, user_owned: true };
        let deployment = Deployment {
            config: PathBuf::from("unused.toml"),
            data_dir: PathBuf::from("unused"),
        };
        ensure(&target, &deployment, Path::new("unused")).expect("the session's own runtime is left as it is");
    }

    /// Nothing answering at a URL the session named is the user's to start:
    /// the deployment's binary is never bound to their port, and nothing of
    /// the deployment is written.
    #[test]
    fn nothing_is_started_at_a_url_the_session_owns() {
        let vacated = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port binds");
        let url = format!("http://{}", vacated.local_addr().expect("the bound address"));
        drop(vacated);
        let root = tempfile::tempdir().expect("temporary directory");
        let deployment = Deployment {
            config: root.path().join("config/appa.toml"),
            data_dir: root.path().join("data"),
        };
        let target = RuntimeTarget { url, user_owned: true };
        let error = ensure(&target, &deployment, &root.path().join("absent/appa")).expect_err("nothing is started");
        assert!(matches!(error, StartError::UserOwnedUnreachable { .. }), "{error}");
        assert!(!root.path().join("config").exists());
        assert!(!root.path().join("data").exists());
    }

    /// Nothing listening and no executable to start: the start fails with the
    /// spawn error, after creating the directories the runtime would write.
    #[test]
    fn a_missing_executable_fails_the_start_after_the_directories_exist() {
        let vacated = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port binds");
        let url = format!("http://{}", vacated.local_addr().expect("the bound address"));
        drop(vacated);
        let root = tempfile::tempdir().expect("temporary directory");
        let deployment = Deployment {
            config: root.path().join("config/appa.toml"),
            data_dir: root.path().join("data"),
        };
        let target = RuntimeTarget { url, user_owned: false };
        let error = ensure(&target, &deployment, &root.path().join("absent/appa")).expect_err("nothing to start");
        assert!(matches!(error, StartError::Spawn { .. }), "{error}");
        assert!(root.path().join("config").is_dir());
        assert!(root.path().join("data").is_dir());
    }
}

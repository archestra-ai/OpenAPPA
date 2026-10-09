//! One bounded read-only subprocess per battery. stdout is a strict enum-only protocol.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::credentials::{CredentialStore, resolve};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Status {
    Ready,
    NeedsConfiguration,
    Unavailable,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Authentication {
    Token,
    Cli,
    None,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Reason {
    Verified,
    /// Every declared requirement is present; the battery declares no check to verify them.
    Configured,
    MissingConfiguration,
    MissingCredential,
    CliNotAuthenticated,
    MissingExecutable,
    InvalidCredential,
    InsufficientAccess,
    ProviderUnavailable,
    CheckFailed,
    CheckTimedOut,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckResult {
    pub status: Status,
    pub authentication: Authentication,
    pub reason: Reason,
    #[serde(default)]
    pub checked_at: u64,
}
impl CheckResult {
    pub(super) fn new(status: Status, reason: Reason) -> Self {
        Self {
            status,
            authentication: Authentication::None,
            reason,
            checked_at: now(),
        }
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn executable_available(executable: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(executable);
            let Ok(metadata) = candidate.metadata() else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
    })
}

/// Ready means nothing the battery declares is missing: its executables, the values of the
/// variables its policy binds as `token_env`, and, when it declares a check, the check's verdict.
pub(super) async fn check(dir: &Path, battery: &appa_package::Battery, store: &CredentialStore) -> CheckResult {
    let readiness = battery.readiness.as_ref();
    if readiness
        .into_iter()
        .flat_map(|r| &r.required_executables)
        .any(|name| !executable_available(name))
    {
        return CheckResult::new(Status::NeedsConfiguration, Reason::MissingExecutable);
    }
    let mut environment = BTreeMap::new();
    for variable in &battery.credentials {
        match resolve(Some(store), variable) {
            Ok(Some(value)) if !value.is_empty() => {
                environment.insert(variable.clone(), value);
            }
            Ok(_) => (),
            Err(_) => return CheckResult::new(Status::Unavailable, Reason::CheckFailed),
        }
    }
    let Some(readiness) = readiness.filter(|r| !r.command.is_empty()) else {
        // Without a check, only presence is knowable: a set variable is not a verified one.
        return if environment.len() == battery.credentials.len() {
            CheckResult::new(Status::Ready, Reason::Configured)
        } else {
            CheckResult::new(Status::NeedsConfiguration, Reason::MissingCredential)
        };
    };
    let mut command = tokio::process::Command::new(&readiness.command[0]);
    command
        .args(&readiness.command[1..])
        .current_dir(dir)
        .env_clear()
        .envs(crate::consult_process::without_runtime_variables(
            std::env::vars_os().collect(),
        ))
        .envs(environment)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let Ok(mut child) = crate::child_process::spawn_async(&mut command) else {
        return CheckResult::new(Status::Unavailable, Reason::CheckFailed);
    };
    // Kill the whole group on timeout/cancellation, including CLI subprocesses holding pipes.
    #[cfg(unix)]
    let _group = ProcessGroup(child.id());
    let Some(stdout) = child.stdout.take() else {
        return CheckResult::new(Status::Unavailable, Reason::CheckFailed);
    };
    let exchange = async {
        let mut bytes = Vec::new();
        stdout.take(4097).read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > 4096 {
            return None;
        }
        if !child.wait().await.ok()?.success() {
            return None;
        }
        serde_json::from_slice::<CheckResult>(&bytes).ok()
    };
    match tokio::time::timeout(Duration::from_secs(15), exchange).await {
        Ok(Some(mut result)) => {
            // No freeform stdout/stderr text crosses this boundary: a helper cannot echo a token.
            result.checked_at = now();
            if (result.status == Status::Ready) != matches!(result.reason, Reason::Verified) {
                return CheckResult::new(Status::Unavailable, Reason::CheckFailed);
            }
            result
        }
        Ok(None) => CheckResult::new(Status::Unavailable, Reason::CheckFailed),
        Err(_) => CheckResult::new(Status::Unavailable, Reason::CheckTimedOut),
    }
}
#[cfg(unix)]
struct ProcessGroup(Option<u32>);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

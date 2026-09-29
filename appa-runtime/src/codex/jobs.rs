//! Runtime-owned, one-use command jobs. A handle conveys only one local job.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome};
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::api::Runtime;

const LIFETIME: Duration = Duration::from_secs(600);
const MAX_JOBS: usize = 128;
const MAX_RECENT_JOBS: usize = 512;
const MAX_TOMBSTONES: usize = 10_000;
pub(crate) const MAX_OUTPUT: usize = 1024 * 1024;

pub(crate) struct Jobs(Mutex<HashMap<String, Job>>, Option<Mutex<Connection>>);

impl Default for Jobs {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()), None)
    }
}

struct Job {
    actor: Actor,
    call: ProposedCall,
    call_id: String,
    command: String,
    shell: String,
    cwd: String,
    wrapper: String,
    created: Instant,
    state: JobState,
}

enum JobState {
    Pending,
    Running,
    Reporting,
    Settled(Admission),
    Cancelled,
}

#[derive(Clone, Serialize)]
pub(crate) struct Specification {
    pub command: String,
    pub shell: String,
    pub cwd: String,
}

#[derive(Deserialize)]
pub(crate) struct Report {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Admission {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl Jobs {
    pub(crate) fn persistent(db: &Path) -> Result<Self, String> {
        let path = db.with_extension("codex-jobs.sqlite");
        let connection = Connection::open(&path)
            .map_err(|error| format!("cannot open Codex job ownership at {}: {error}", path.display()))?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS settled_jobs (
            wrapper TEXT PRIMARY KEY, root TEXT NOT NULL, child TEXT NOT NULL,
            call_id TEXT NOT NULL, settled_at INTEGER NOT NULL
        ); CREATE INDEX IF NOT EXISTS settled_jobs_call ON settled_jobs(root, child, call_id);",
            )
            .map_err(|error| format!("cannot initialize Codex job ownership: {error}"))?;
        Ok(Self(Mutex::new(HashMap::new()), Some(Mutex::new(connection))))
    }

    pub(crate) fn create(
        &self,
        event: &HookEvent,
        binary: &std::path::Path,
        url: &str,
        shell: &str,
    ) -> Result<String, String> {
        if !cfg!(unix) {
            return Err("the buffered Codex command wrapper is currently supported on Unix only".into());
        }
        let HookEvent::ToolCall {
            actor,
            call,
            call_id: Some(call_id),
            ..
        } = event
        else {
            return Err("the Codex command has no call identity".into());
        };
        if call.tool != "host/codex/appa_exec" {
            return Err("the call is not a Codex command".into());
        }
        let arguments: serde_json::Value =
            serde_json::from_str(call.arguments.get()).map_err(|_| "the Codex command arguments are invalid")?;
        let command = arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|command| !command.is_empty() && !command.contains('\0'))
            .ok_or("the Codex command is missing")?;
        if arguments.get("tty").and_then(serde_json::Value::as_bool) == Some(true) {
            return Err("terminal commands are outside the buffered Codex command path".into());
        }
        let cwd = call
            .cwd
            .as_deref()
            .ok_or("the Codex hook supplied no working directory")?;
        if arguments
            .get("shell")
            .is_some_and(|value| value.as_str() != Some(shell))
            || arguments
                .get("login")
                .is_some_and(|value| value.as_bool() != Some(true))
            || arguments
                .get("workdir")
                .is_some_and(|value| value.as_str() != Some(cwd))
            || arguments.get("env").is_some()
        {
            return Err(
                "the Codex command requests shell or environment semantics the wrapper cannot reproduce".into(),
            );
        }
        let shell_path = Path::new(shell);
        if !shell_path.is_absolute()
            || !matches!(
                shell_path.file_name().and_then(|name| name.to_str()),
                Some("sh" | "bash" | "zsh")
            )
            || !shell_path.is_file()
        {
            return Err("the Codex command shell is missing or unsupported".into());
        }
        let binary = binary.to_str().ok_or("the APPA binary path is not UTF-8")?;
        if !url.starts_with("http://127.0.0.1:") {
            return Err("the Codex job runtime must use a 127.0.0.1 endpoint".into());
        }
        let mut jobs = self.0.lock().map_err(|_| "the Codex job registry is unavailable")?;
        jobs.retain(|_, job| job.created.elapsed() < LIFETIME);
        if jobs
            .values()
            .filter(|job| !matches!(job.state, JobState::Settled(_) | JobState::Cancelled))
            .count()
            >= MAX_JOBS
        {
            return Err("too many outstanding Codex commands".into());
        }
        if jobs.values().any(|job| job.actor == *actor && job.call_id == *call_id) {
            return Err("the Codex call already owns a command job".into());
        }
        if let Some(store) = &self.1 {
            let store = store.lock().map_err(|_| "the Codex ownership store is unavailable")?;
            let child = actor.child.as_ref().map_or("", |child| child.0.as_str());
            let exists: Option<i64> = store
                .query_row(
                    "SELECT 1 FROM settled_jobs WHERE root = ?1 AND child = ?2 AND call_id = ?3 LIMIT 1",
                    params![actor.root.0, child, call_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            if exists.is_some() {
                return Err("the Codex call already settled a command job".into());
            }
        }
        let handle = uuid::Uuid::new_v4().to_string();
        let wrapper = format!(
            "{} codex-exec --url {} {}",
            shell_literal(binary),
            shell_literal(url),
            shell_literal(&handle)
        );
        if jobs.len() >= MAX_RECENT_JOBS
            && let Some(oldest) = jobs
                .iter()
                .filter(|(_, job)| matches!(job.state, JobState::Settled(_) | JobState::Cancelled))
                .min_by_key(|(_, job)| job.created)
                .map(|(handle, _)| handle.clone())
        {
            jobs.remove(&oldest);
        }
        jobs.insert(
            handle,
            Job {
                actor: actor.clone(),
                call: call.clone(),
                call_id: call_id.clone(),
                command: command.to_owned(),
                shell: shell.to_owned(),
                cwd: cwd.to_owned(),
                wrapper: wrapper.clone(),
                created: Instant::now(),
                state: JobState::Pending,
            },
        );
        Ok(wrapper)
    }

    pub(crate) fn consume(&self, handle: &str) -> Option<Specification> {
        let mut jobs = self.0.lock().ok()?;
        let job = jobs.get_mut(handle)?;
        if job.created.elapsed() >= LIFETIME || !matches!(job.state, JobState::Pending) {
            return None;
        }
        job.state = JobState::Running;
        Some(Specification {
            command: job.command.clone(),
            shell: job.shell.clone(),
            cwd: job.cwd.clone(),
        })
    }

    pub(crate) fn settled(&self, actor: &Actor, call_id: &str, command: &str) -> bool {
        // A persisted tombstone prevents replay after restart, but cannot
        // prove the turn is still open. Only this live runtime may ACK the
        // outer result; a restarted runtime withholds it conservatively.
        self.0.lock().is_ok_and(|jobs| {
            jobs.values().any(|job| {
                job.actor == *actor
                    && job.call_id == call_id
                    && job.wrapper == command
                    && matches!(job.state, JobState::Settled(_))
            })
        })
    }

    pub(crate) fn running(&self, handle: &str) -> bool {
        self.0.lock().is_ok_and(|jobs| {
            jobs.get(handle)
                .is_some_and(|job| matches!(job.state, JobState::Running) && job.created.elapsed() < LIFETIME)
        })
    }

    pub(crate) fn cancel_actor(&self, actor: &Actor) -> Vec<HookEvent> {
        let Ok(mut jobs) = self.0.lock() else { return Vec::new() };
        jobs.values_mut()
            .filter_map(|job| {
                if job.actor.root != actor.root
                    || (actor.child.is_some() && job.actor != *actor)
                    || matches!(job.state, JobState::Cancelled)
                {
                    return None;
                }
                let settled = matches!(job.state, JobState::Settled(_));
                job.state = JobState::Cancelled;
                if settled {
                    return None;
                }
                Some(HookEvent::ToolResult {
                    actor: job.actor.clone(),
                    call: job.call.clone(),
                    call_id: Some(job.call_id.clone()),
                    outcome: ToolOutcome::Indeterminate,
                })
            })
            .collect()
    }

    pub(crate) async fn report(&self, runtime: &Runtime, handle: &str, report: Report) -> Result<Admission, String> {
        let stdout = base64::engine::general_purpose::STANDARD
            .decode(&report.stdout)
            .map_err(|_| "stdout is not base64")?;
        let stderr = base64::engine::general_purpose::STANDARD
            .decode(&report.stderr)
            .map_err(|_| "stderr is not base64")?;
        if stdout.len().saturating_add(stderr.len()) > MAX_OUTPUT {
            return Err("the captured output exceeds the Codex command limit".into());
        }
        let (actor, call, call_id) = {
            let mut jobs = self.0.lock().map_err(|_| "the Codex job registry is unavailable")?;
            let job = jobs.get_mut(handle).ok_or("the Codex job is unknown")?;
            if job.created.elapsed() >= LIFETIME {
                return Err("the Codex job expired".into());
            }
            match &job.state {
                JobState::Settled(admission) => return Ok(admission.clone()),
                JobState::Running => job.state = JobState::Reporting,
                _ => return Err("the Codex job is not ready for a result".into()),
            }
            (job.actor.clone(), job.call.clone(), job.call_id.clone())
        };
        let complete =
            report.exit_code.is_some() && std::str::from_utf8(&stdout).is_ok() && std::str::from_utf8(&stderr).is_ok();
        let body = if complete {
            format!(
                "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                report.exit_code.unwrap(),
                std::str::from_utf8(&stdout).unwrap(),
                std::str::from_utf8(&stderr).unwrap()
            )
        } else {
            String::new()
        };
        let effect_free = runtime
            .session(&actor.root, actor.child.as_ref().unwrap_or(&actor.root))
            .is_ok_and(|session| session.released_call_has_no_effects(&call));
        let outcome = if complete && (report.exit_code == Some(0) || effect_free) {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            }
        } else {
            ToolOutcome::Indeterminate
        };
        let decision = crate::hooks::handle(
            runtime,
            HookEvent::ToolResult {
                actor,
                call,
                call_id: Some(call_id),
                outcome: outcome.clone(),
            },
        )
        .await;
        let admission = match (outcome, decision) {
            (ToolOutcome::Success { .. }, HookDecision::Ack) => Admission {
                stdout: report.stdout,
                stderr: report.stderr,
                exit_code: report.exit_code.unwrap(),
            },
            (ToolOutcome::Success { .. }, HookDecision::DeliverValue { value }) => Admission {
                stdout: base64::engine::general_purpose::STANDARD.encode(value),
                stderr: String::new(),
                exit_code: report.exit_code.unwrap(),
            },
            (_, HookDecision::ReplaceOutput { output } | HookDecision::Block { reason: output }) => Admission {
                stdout: base64::engine::general_purpose::STANDARD.encode(output),
                stderr: String::new(),
                exit_code: 1,
            },
            _ => Admission {
                stdout: base64::engine::general_purpose::STANDARD.encode("OpenAPPA withheld the command result"),
                stderr: String::new(),
                exit_code: 1,
            },
        };
        let mut jobs = self.0.lock().map_err(|_| "the Codex job registry is unavailable")?;
        let job = jobs.get_mut(handle).ok_or("the Codex job vanished")?;
        if matches!(job.state, JobState::Cancelled) {
            return Err("the Codex job was cancelled before result admission".into());
        }
        let store = self.1.as_ref().ok_or("Codex job ownership is not persistent")?;
        let store = store.lock().map_err(|_| "the Codex ownership store is unavailable")?;
        let child = job.actor.child.as_ref().map_or("", |child| child.0.as_str());
        let settled_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs() as i64;
        store
            .execute(
                "INSERT INTO settled_jobs(wrapper, root, child, call_id, settled_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![job.wrapper, job.actor.root.0, child, job.call_id, settled_at],
            )
            .map_err(|error| format!("cannot persist Codex result ownership: {error}"))?;
        store.execute(
            "DELETE FROM settled_jobs WHERE rowid <= (SELECT rowid FROM settled_jobs ORDER BY rowid DESC LIMIT 1 OFFSET ?1)",
            params![MAX_TOMBSTONES as i64],
        ).map_err(|error| format!("cannot bound Codex result ownership: {error}"))?;
        job.state = JobState::Settled(admission.clone());
        Ok(admission)
    }
}

fn shell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime_api::TrajectoryId;

    #[test]
    fn root_turn_end_cancels_a_running_child_job() {
        let jobs = Jobs::default();
        let root = TrajectoryId("codex:session".into());
        let actor = Actor {
            root: root.clone(),
            child: Some(TrajectoryId("codex:session:child".into())),
        };
        let call = ProposedCall {
            tool: "host/codex/appa_exec".into(),
            arguments: serde_json::value::to_raw_value(&serde_json::json!({"command":"sleep 5"})).unwrap(),
            cwd: Some("/tmp".into()),
        };
        jobs.0.lock().unwrap().insert(
            "handle".into(),
            Job {
                actor: actor.clone(),
                call,
                call_id: "call".into(),
                command: "sleep 5".into(),
                shell: "/bin/sh".into(),
                cwd: "/tmp".into(),
                wrapper: "wrapper".into(),
                created: Instant::now(),
                state: JobState::Running,
            },
        );
        assert_eq!(jobs.cancel_actor(&Actor { root, child: None }).len(), 1);
        assert!(!jobs.running("handle"));
    }
}

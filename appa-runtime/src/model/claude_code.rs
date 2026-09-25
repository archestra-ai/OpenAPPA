//! The `claude-code` builtin: one isolated, tool-less `claude` process per consult.

use std::sync::Arc;

use crate::config::ClaudeCode;
use crate::consult::ModelPrompt;
use crate::external::{NoAnswerReason, acquire_within};

/// The CLI's `--output-format json` result. On a failure the CLI still exits through
/// this envelope: `is_error` set and its own message — "Not logged in · Please run
/// /login" — in `result`, on stdout rather than stderr.
#[cfg(unix)]
#[derive(Debug, serde::Deserialize)]
struct ClaudeResultEnvelope {
    structured_output: Option<serde_json::Value>,
    #[serde(default)]
    is_error: bool,
    result: Option<String>,
}

#[cfg(unix)]
impl ClaudeResultEnvelope {
    /// The message the CLI reported a failure with, where the output is that envelope.
    fn reported_error(output: &[u8]) -> Option<String> {
        let envelope: ClaudeResultEnvelope = serde_json::from_slice(output).ok()?;
        envelope
            .is_error
            .then_some(envelope.result)
            .flatten()
            .map(|result| crate::external::error_line(&result))
            .filter(|line| !line.is_empty())
    }
}

/// The stock `claude-code` model transport: one isolated, tool-less `claude` process per
/// consult, answering under the consult's own output schema. The deployment may override
/// the executable (a service environment often has no usable `PATH`), the model, and the
/// consult limits. Its permit pool is the deployment's own, bounded by `max_concurrent`,
/// so two deployments never share or resize one.
#[derive(Debug, Clone)]
pub(crate) struct ClaudeCodeBackend {
    #[cfg(unix)]
    command: std::path::PathBuf,
    #[cfg(unix)]
    model: String,
    timeout: std::time::Duration,
    #[cfg(unix)]
    max_body_bytes: usize,
    gate: Arc<tokio::sync::Semaphore>,
}

impl ClaudeCodeBackend {
    pub(crate) fn new(config: &ClaudeCode, max_body_bytes: usize) -> ClaudeCodeBackend {
        #[cfg(not(unix))]
        let _ = max_body_bytes;
        ClaudeCodeBackend {
            #[cfg(unix)]
            command: config.command.clone(),
            #[cfg(unix)]
            model: config.model.clone(),
            timeout: config.limits.timeout,
            #[cfg(unix)]
            max_body_bytes,
            gate: Arc::new(tokio::sync::Semaphore::new(config.limits.max_concurrent)),
        }
    }

    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        self.gate.available_permits()
    }

    /// One consult. The deadline covers the permit wait and the subprocess: queueing behind
    /// the pool spends the same budget the consult itself would, so a saturated pool cannot
    /// stack timeout waves.
    pub(crate) async fn consult(&self, prompt: &ModelPrompt, name: &str) -> Result<serde_json::Value, NoAnswerReason> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let permit = acquire_within(&self.gate, deadline, "claude", name).await?;
        let answered = run_claude_code(self, prompt, deadline).await;
        drop(permit);
        answered
    }
}

#[cfg(unix)]
pub(crate) async fn run_claude_code(
    backend: &ClaudeCodeBackend,
    prompt: &ModelPrompt,
    deadline: tokio::time::Instant,
) -> Result<serde_json::Value, NoAnswerReason> {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;

    use crate::external::{CommandProcess, exchange_with_child, finished_tail, stderr_tail};

    let schema = serde_json::to_string(&prompt.schema).map_err(|_| NoAnswerReason::Malformed)?;
    let work = tempfile::tempdir().map_err(|_| NoAnswerReason::Transport)?;
    let mut command = tokio::process::Command::new(&backend.command);
    command
        .arg("-p")
        .arg("--model")
        .arg(&backend.model)
        .arg("--safe-mode")
        .arg("--setting-sources")
        .arg("")
        .arg("--disable-slash-commands")
        .arg("--tools")
        .arg("")
        .arg("--permission-mode")
        .arg("dontAsk")
        .arg("--no-session-persistence")
        .arg("--output-format")
        .arg("json")
        .arg("--json-schema")
        .arg(schema)
        .arg("--system-prompt")
        .arg(&prompt.system)
        .current_dir(work.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    isolate_claude_environment(&mut command);
    tracing::debug!("claude consult starts");
    let mut child = command.spawn().map_err(|_| {
        tracing::warn!(command = %backend.command.display(), "the claude executable did not start");
        NoAnswerReason::Unreachable
    })?;
    // The CLI's own error — a bad model name — is the one line an operator needs when
    // every consult fails; it is read to the end so the pipe never blocks the answer.
    let tail = child.stderr.take().map(stderr_tail);
    // The guard ends the consult's whole process group on every outcome, a dropped future
    // included: no helper the CLI spawned outlives the answer.
    let mut process = CommandProcess::spawned(child)?;
    let process_group = process.process_group();
    let exchanged = tokio::time::timeout_at(
        deadline,
        exchange_with_child(
            process.child_mut(),
            process_group,
            prompt.input.as_bytes(),
            backend.max_body_bytes,
        ),
    )
    .await;
    let output = match exchanged {
        Ok(Ok(output)) => output,
        Ok(Err(reason)) => {
            process.terminate_and_reap_later();
            return Err(reason);
        }
        Err(_) => {
            process.terminate_and_reap_later();
            tracing::warn!("claude consult timed out and was terminated");
            return Err(NoAnswerReason::Timeout);
        }
    };
    let status = process.terminate_and_reap().await?;
    if !status.success() {
        let stderr = match tail {
            Some(tail) => finished_tail(tail).await.and_then(|stderr| stderr.error_line()),
            None => None,
        };
        // The envelope's own message first: the CLI reports a login failure there and
        // writes nothing to stderr.
        let detail = ClaudeResultEnvelope::reported_error(&output).or(stderr);
        tracing::warn!(
            code = ?status.code(),
            error = detail.as_deref().unwrap_or(""),
            "claude exited without an answer"
        );
        return Err(NoAnswerReason::NonSuccess {
            status: status.code().and_then(|code| u16::try_from(code).ok()).unwrap_or(0),
            detail,
        });
    }
    let envelope: ClaudeResultEnvelope = serde_json::from_slice(&output).map_err(|_| NoAnswerReason::Malformed)?;
    envelope.structured_output.ok_or(NoAnswerReason::Malformed)
}

#[cfg(unix)]
fn isolate_claude_environment(command: &mut tokio::process::Command) {
    // Claude Code marks its own process tree and refuses to start a nested CLI
    // while that marker is present. This consult is deliberately isolated,
    // tool-less, and non-persistent, so it is safe and necessary to clear the
    // harness marker before launching it.
    command.env_remove(appa_adapter_claude_code::environment::SESSION_MARKER);
    // A consult is one answer, not a session: the CLI's background traffic (telemetry,
    // bootstrap fetches, the session-title call) is one more connection per consult
    // on a host that may run many consults at once, and none of it reaches the answer.
    command.env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    // No APPA variable of any kind reaches the model: the child needs its own credentials
    // and HOME, never this runtime's bearer tokens — and not the provider credential a
    // `command` external inherits either, which this consult never reads.
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .starts_with(crate::config::RUNTIME_VARIABLE_PREFIX)
        {
            command.env_remove(key);
        }
    }
}

/// The builtin is a local process under a process group this platform lacks; the
/// configuration refuses it before a deployment opens, so this is never reached.
#[cfg(not(unix))]
pub(crate) async fn run_claude_code(
    _backend: &ClaudeCodeBackend,
    _prompt: &ModelPrompt,
    _deadline: tokio::time::Instant,
) -> Result<serde_json::Value, NoAnswerReason> {
    Err(NoAnswerReason::Unregistered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_claude_consult_clears_the_parent_session_marker() {
        let mut command = tokio::process::Command::new("claude");
        command.env("CLAUDECODE", "1");
        isolate_claude_environment(&mut command);
        assert!(
            command
                .as_std()
                .get_envs()
                .any(|(name, value)| name == "CLAUDECODE" && value.is_none()),
            "the nested-session marker is explicitly removed"
        );
        assert!(
            command.as_std().get_envs().any(|(name, value)| {
                name == "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC" && value.is_some_and(|value| value == "1")
            }),
            "the consult runs without the CLI's background traffic"
        );
    }

    /// A fake `claude` that reads its input and exits 1 after `script`.
    #[cfg(unix)]
    async fn failed_consult(script: &str) -> Result<serde_json::Value, NoAnswerReason> {
        let dir = tempfile::tempdir().expect("a temp dir");
        let fake = crate::test_support::fake_claude(dir.path(), &format!("cat > /dev/null\n{script}\nexit 1"));
        let backend = ClaudeCodeBackend::new(
            &ClaudeCode {
                command: fake,
                model: "m".to_string(),
                limits: crate::config::ModelLimits {
                    timeout: std::time::Duration::from_secs(5),
                    max_concurrent: 1,
                },
            },
            65_536,
        );
        let prompt = ModelPrompt {
            system: "rule".to_string(),
            input: "{}".to_string(),
            schema: serde_json::json!({"type": "object"}),
        };
        run_claude_code(
            &backend,
            &prompt,
            tokio::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .await
    }

    /// A CLI that exits without an answer names its own error, so a failed consult is
    /// not just `status=1`: the message of an error envelope on stdout — how the CLI
    /// reports a logged-out session — or else the last stderr line.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_claude_consult_carries_the_clis_own_error() {
        let logged_out = r#"echo '{"type":"result","is_error":true,"result":"Not logged in · Please run /login"}'"#;
        assert_eq!(
            failed_consult(logged_out).await,
            Err(NoAnswerReason::NonSuccess {
                status: 1,
                detail: Some("Not logged in · Please run /login".to_string()),
            })
        );

        let bad_model =
            "echo 'warning: something else' >&2\necho '[claude-code:unrecognized_model] {\"model\":\"m\"}' >&2";
        assert_eq!(
            failed_consult(bad_model).await,
            Err(NoAnswerReason::NonSuccess {
                status: 1,
                detail: Some("[claude-code:unrecognized_model] {\"model\":\"m\"}".to_string()),
            })
        );

        assert_eq!(
            failed_consult("").await,
            Err(NoAnswerReason::NonSuccess {
                status: 1,
                detail: None
            })
        );
    }

    /// A helper the CLI leaves running — here a backgrounded `sleep` that keeps the
    /// CLI's stdout open, whose pid the fake records — neither stalls the answer nor
    /// survives the consult that started it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_claude_consult_takes_its_helpers_down_with_it() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let pid_file = dir.path().join("helper.pid");
        let fake = crate::test_support::fake_claude(
            dir.path(),
            &format!(
                "cat > /dev/null\nprintf '%s' '{{\"structured_output\":{{\"ruling\":\"approve\",\"reason\":\"ok\"}}}}'\nsleep 30 &\necho $! > {}",
                pid_file.display()
            ),
        );
        let backend = ClaudeCodeBackend::new(
            &ClaudeCode {
                command: fake,
                model: "m".to_string(),
                limits: crate::config::ModelLimits {
                    timeout: std::time::Duration::from_secs(5),
                    max_concurrent: 1,
                },
            },
            65_536,
        );
        let prompt = ModelPrompt {
            system: "rule".to_string(),
            input: "{}".to_string(),
            schema: serde_json::json!({"type": "object"}),
        };

        let started = std::time::Instant::now();
        let answer = run_claude_code(
            &backend,
            &prompt,
            tokio::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .await;
        assert_eq!(answer, Ok(serde_json::json!({"ruling": "approve", "reason": "ok"})));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "the answer is read as soon as the CLI exits"
        );

        let helper: i32 = std::fs::read_to_string(&pid_file)
            .expect("the fake recorded its helper")
            .trim()
            .parse()
            .expect("a pid");
        let gone_by = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let alive = unsafe { libc::kill(helper, 0) } == 0;
            if !alive {
                break;
            }
            assert!(std::time::Instant::now() < gone_by, "the helper outlived the consult");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

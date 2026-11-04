//! The `claude-code` builtin: one isolated, tool-less `claude` process per consult.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::config::ClaudeCode;
use crate::consult::{AnnotationAnswer, AnnotationDeclaration, ModelPrompt};
use crate::external::{ConsultGates, NoAnswerReason, Transcript, acquire_within};
use appa_policy::AnnotatorBuiltin;

/// The CLI's `--output-format json` result. On a failure the CLI still exits through
/// this envelope: `is_error` set and its own message — "Not logged in · Please run
/// /login" — in `result`, on stdout rather than stderr.
#[derive(Debug, serde::Deserialize)]
struct ClaudeResultEnvelope {
    structured_output: Option<serde_json::Value>,
    #[serde(default)]
    is_error: bool,
    result: Option<String>,
}

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
/// consult limits. Its permit pool is the runtime's `claude-code` gate.
#[derive(Clone)]
pub(crate) struct ClaudeCodeBackend {
    config: ClaudeCode,
    max_body_bytes: usize,
    gates: ConsultGates,
    verdicts: Arc<Mutex<VerdictCache>>,
}

/// The most annotation answers one backend keeps; the oldest is dropped first.
const VERDICT_CACHE_ENTRIES: usize = 512;
/// The longest answer it keeps, serialized; an ordinary annotation is well under 1 KiB.
const VERDICT_CACHE_ANSWER_BYTES: usize = 16 * 1024;

/// Annotation answers this backend already gave, keyed by [`ClaudeCodeBackend::verdict_key`].
#[derive(Default)]
struct VerdictCache {
    answers: HashMap<[u8; 32], serde_json::Value>,
    order: VecDeque<[u8; 32]>,
}

impl VerdictCache {
    fn insert(&mut self, key: [u8; 32], answer: serde_json::Value) {
        if self.answers.insert(key, answer).is_some() {
            return;
        }
        self.order.push_back(key);
        if self.order.len() > VERDICT_CACHE_ENTRIES
            && let Some(oldest) = self.order.pop_front()
        {
            self.answers.remove(&oldest);
        }
    }
}

impl ClaudeCodeBackend {
    pub(crate) fn new(config: &ClaudeCode, max_body_bytes: usize, gates: &ConsultGates) -> ClaudeCodeBackend {
        ClaudeCodeBackend {
            config: config.clone(),
            max_body_bytes,
            gates: gates.clone(),
            verdicts: Arc::default(),
        }
    }

    /// Everything that reaches the model: the executable, the model, and the rendered prompt.
    fn verdict_key(&self, prompt: &ModelPrompt) -> [u8; 32] {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        let schema = prompt.schema.to_string();
        for part in [
            self.config.command.as_os_str().as_encoded_bytes(),
            self.config.model.as_bytes(),
            prompt.system.as_bytes(),
            prompt.input.as_bytes(),
            schema.as_bytes(),
        ] {
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hasher.finalize().into()
    }

    fn verdicts(&self) -> std::sync::MutexGuard<'_, VerdictCache> {
        self.verdicts.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// An annotation consult answered from an identical earlier prompt where one gave an answer
    /// inside `declaration`'s mandate, and otherwise asked, keeping only such an answer.
    pub(crate) async fn annotate(
        &self,
        prompt: &ModelPrompt,
        declaration: &AnnotationDeclaration,
        name: &str,
        seen: Option<&mut Transcript>,
    ) -> Result<serde_json::Value, NoAnswerReason> {
        let admitted = |answer: &serde_json::Value| AnnotationAnswer::from_wire(answer, declaration).is_ok();
        let key = self.verdict_key(prompt);
        let recalled = self.verdicts().answers.get(&key).cloned();
        if let Some(answer) = recalled.filter(admitted) {
            tracing::debug!(name, "claude consult answered from an identical earlier prompt");
            return Ok(answer);
        }
        let answer = self.consult(prompt, name, seen).await?;
        if admitted(&answer) && answer.to_string().len() <= VERDICT_CACHE_ANSWER_BYTES {
            self.verdicts().insert(key, answer.clone());
        }
        Ok(answer)
    }

    /// One consult. The deadline covers the permit wait and the subprocess: queueing behind
    /// the pool spends the same budget the consult itself would, so a saturated pool cannot
    /// stack timeout waves.
    pub(crate) async fn consult(
        &self,
        prompt: &ModelPrompt,
        name: &str,
        seen: Option<&mut Transcript>,
    ) -> Result<serde_json::Value, NoAnswerReason> {
        let deadline = tokio::time::Instant::now() + self.config.limits.timeout;
        let gate = self.gates.model(AnnotatorBuiltin::ClaudeCode);
        let permit = acquire_within(&gate, deadline, "claude", name).await?;
        let answered = run_claude_code(self, prompt, deadline, seen).await;
        drop(permit);
        answered
    }
}

pub(crate) async fn run_claude_code(
    backend: &ClaudeCodeBackend,
    prompt: &ModelPrompt,
    deadline: tokio::time::Instant,
    seen: Option<&mut Transcript>,
) -> Result<serde_json::Value, NoAnswerReason> {
    use std::process::Stdio;

    use crate::consult_process::{CommandProcess, exchange_with_child, finished_tail, stderr_tail};

    let schema = serde_json::to_string(&prompt.schema).map_err(|_| NoAnswerReason::Malformed)?;
    let work = tempfile::tempdir().map_err(|_| NoAnswerReason::Transport)?;
    let mut command = tokio::process::Command::new(&backend.config.command);
    command
        .arg("-p")
        .arg("--model")
        .arg(&backend.config.model)
        .arg("--safe-mode")
        .arg("--setting-sources")
        .arg("")
        // `--setting-sources ""` leaves out the settings files, not the user's global
        // `~/.claude.json`. Its `verbose = true` turns `--output-format json` into the
        // whole event list instead of the one result envelope parsed below, and every
        // consult then ends malformed. The answer's shape is fixed here, not by the host.
        .arg("--settings")
        .arg(r#"{"verbose":false}"#)
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
    isolate_claude_environment(&mut command, std::env::vars_os().collect());
    tracing::debug!("claude consult starts");
    // The guard ends the consult's whole process tree on every outcome, a dropped future
    // included: no helper the CLI spawned outlives the answer.
    let mut process = CommandProcess::spawn(&mut command).inspect_err(|reason| {
        if matches!(reason, NoAnswerReason::Unreachable) {
            tracing::warn!(command = %backend.config.command.display(), "the claude executable did not start");
        }
    })?;
    // The CLI's own error — a bad model name — is the one line an operator needs when
    // every consult fails; it is read to the end so the pipe never blocks the answer.
    let tail = process.child_mut().stderr.take().map(stderr_tail);
    let exchanged = {
        let (child, tree) = process.parts();
        tokio::time::timeout_at(
            deadline,
            exchange_with_child(child, tree, prompt.input.as_bytes(), backend.max_body_bytes),
        )
        .await
    };
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
    if let Some(seen) = seen {
        seen.raw_response = Some(output.clone());
    }
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

fn isolate_claude_environment(
    command: &mut tokio::process::Command,
    parent: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) {
    // No APPA variable of any kind reaches the model: the child needs its own credentials
    // and HOME, never this runtime's bearer tokens — and not the provider credential a
    // `command` external inherits either, which this consult never reads.
    command
        .env_clear()
        .envs(crate::consult_process::without_runtime_variables(parent));
    // Claude Code marks its own process tree and refuses to start a nested CLI
    // while that marker is present. This consult is deliberately isolated,
    // tool-less, and non-persistent, so it is safe and necessary to clear the
    // harness marker before launching it.
    command.env_remove(appa_adapter_claude_code::environment::SESSION_MARKER);
    // A consult is one answer, not a session: the CLI's background traffic (telemetry,
    // bootstrap fetches, the session-title call) is one more connection per consult
    // on a host that may run many consults at once, and none of it reaches the answer.
    command.env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::{PROCESS_BUDGET, assert_process_gone, recorded_pid};

    #[test]
    fn a_claude_consult_inherits_neither_the_session_marker_nor_the_runtimes_variables() {
        use std::ffi::OsStr;
        let parent = [
            ("CLAUDECODE", "1"),
            ("APPA_TEST_SECRET_TOKEN", "leaky"),
            ("HOME", "/home/user"),
        ]
        .map(|(name, value)| (name.into(), value.into()));
        let mut command = tokio::process::Command::new("claude");
        isolate_claude_environment(&mut command, parent.into());
        let child: Vec<_> = command.as_std().get_envs().collect();
        assert_eq!(
            child,
            [
                (
                    OsStr::new("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
                    Some(OsStr::new("1"))
                ),
                (OsStr::new("HOME"), Some(OsStr::new("/home/user"))),
            ],
            "the child starts from the parent's environment without the nested-session \
             marker or any APPA variable, and without the CLI's background traffic"
        );
    }

    /// One consult of a fake `claude` in `dir` that reads its input, then runs `script`.
    async fn fake_consult(dir: &std::path::Path, script: &str) -> Result<serde_json::Value, NoAnswerReason> {
        let command = crate::test_support::fake_claude(dir, &format!("cat > /dev/null\n{script}"));
        let config = ClaudeCode {
            command,
            ..ClaudeCode::default()
        };
        let backend = ClaudeCodeBackend::new(&config, 65_536, &crate::external::ConsultGates::per_runtime());
        let prompt = ModelPrompt {
            system: "rule".to_string(),
            input: "{}".to_string(),
            schema: serde_json::json!({"type": "object"}),
        };
        let deadline = tokio::time::Instant::now() + PROCESS_BUDGET;
        run_claude_code(&backend, &prompt, deadline, None).await
    }

    async fn failed_consult(script: &str) -> Result<serde_json::Value, NoAnswerReason> {
        let dir = tempfile::tempdir().expect("a temp dir");
        fake_consult(dir.path(), &format!("{script}\nexit 1")).await
    }

    /// A CLI that exits without an answer names its own error, so a failed consult is
    /// not just `status=1`: the message of an error envelope on stdout — how the CLI
    /// reports a logged-out session — or else the last stderr line.
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

    /// The CLI answers with its whole event list, not the one result envelope, when the
    /// user's global config turns `verbose` on; the consult overrides it on every call,
    /// so a host's personal setting never turns every answer malformed.
    #[tokio::test]
    async fn a_claude_consult_turns_verbose_output_off() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let script = r#"envelope='{"type":"result","structured_output":{"ruling":"approve"}}'
verbose=true
while [ $# -gt 0 ]; do
  if [ "$1" = --settings ] && [ "$2" = '{"verbose":false}' ]; then verbose=false; fi
  shift
done
if [ "$verbose" = true ]; then printf '[{"type":"system"},%s]' "$envelope"; else printf '%s' "$envelope"; fi"#;
        assert_eq!(
            fake_consult(dir.path(), script).await,
            Ok(serde_json::json!({"ruling": "approve"}))
        );
    }

    /// A helper the CLI leaves running — here a backgrounded `sleep` that keeps the
    /// CLI's stdout open, whose pid the fake records — neither stalls the answer nor
    /// survives the consult that started it.
    #[tokio::test]
    async fn a_claude_consult_takes_its_helpers_down_with_it() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let pid_file = dir.path().join("helper.pid");
        let script = format!(
            "printf '%s' '{{\"structured_output\":{{\"ruling\":\"approve\",\"reason\":\"ok\"}}}}'\nsleep 30 &\necho $! > {}",
            pid_file.display()
        );
        // The deadline is under the helper's `sleep 30`: a consult that waited on the
        // helper's end of the pipe times out instead of answering.
        let answer = fake_consult(dir.path(), &script).await;
        assert_eq!(answer, Ok(serde_json::json!({"ruling": "approve", "reason": "ok"})));
        assert_process_gone(recorded_pid(&pid_file).await).await;
    }

    /// The mandate admits `suspicious` and `trusted`; the out-of-mandate answer is complete
    /// and well formed, and differs only in a trust rank the declaration does not list.
    #[tokio::test]
    async fn only_an_answer_inside_the_mandate_is_stored() {
        let declaration = AnnotationDeclaration {
            hint: None,
            deployment_hint: None,
            context_hints: Default::default(),
            inputs: vec![],
            trust_ranks: vec!["suspicious".to_string(), "trusted".to_string()],
            audiences: appa_engine::registry::AudienceVocabulary::parse_entries(&["internal".to_string()])
                .expect("a fixture vocabulary parses"),
            attention_marks: vec![],
            effects: vec![],
        };
        let prompt = ModelPrompt {
            system: "rule".to_string(),
            input: "{}".to_string(),
            schema: serde_json::json!({"type": "object"}),
        };
        let stored = async |trust: &str| {
            let answer = serde_json::json!({
                "delta": {"trust": trust},
                "forwards": {},
                "requires": {"history": [], "attention": []},
                "emits": [],
            });
            let dir = tempfile::tempdir().expect("a temp dir");
            let envelope = serde_json::json!({"type": "result", "structured_output": answer});
            let command =
                crate::test_support::fake_claude(dir.path(), &format!("cat > /dev/null\nprintf '%s' '{envelope}'"));
            let config = ClaudeCode {
                command,
                ..ClaudeCode::default()
            };
            let backend = ClaudeCodeBackend::new(&config, 65_536, &crate::external::ConsultGates::per_runtime());
            assert_eq!(backend.annotate(&prompt, &declaration, "judge", None).await, Ok(answer));
            backend.verdicts().answers.len()
        };
        assert_eq!(stored("suspicious").await, 1, "an admitted answer is stored");
        assert_eq!(stored("root").await, 0, "an answer outside the mandate is not stored");
    }
}

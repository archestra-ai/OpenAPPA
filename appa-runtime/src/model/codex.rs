//! Saved-login Codex CLI transport for APPA's existing model consults.

use appa_policy::AnnotatorBuiltin;

use crate::config::Codex;
use crate::consult::ModelPrompt;
use crate::external::{ConsultGates, NoAnswerReason, Transcript, acquire_within};

#[derive(Clone)]
pub(crate) struct CodexBackend {
    config: Codex,
    max_body_bytes: usize,
    gates: ConsultGates,
}

impl CodexBackend {
    pub(crate) fn new(config: &Codex, max_body_bytes: usize, gates: &ConsultGates) -> Self {
        Self {
            config: config.clone(),
            max_body_bytes,
            gates: gates.clone(),
        }
    }

    pub(crate) async fn consult(
        &self,
        prompt: &ModelPrompt,
        name: &str,
        seen: Option<&mut Transcript>,
    ) -> Result<serde_json::Value, NoAnswerReason> {
        let deadline = tokio::time::Instant::now() + self.config.limits.timeout;
        let gate = self.gates.model(AnnotatorBuiltin::Codex);
        let permit = acquire_within(&gate, deadline, "codex", name).await?;
        let answer = run(self, prompt, deadline, seen).await;
        drop(permit);
        answer
    }
}

#[cfg(unix)]
async fn run(
    backend: &CodexBackend,
    prompt: &ModelPrompt,
    deadline: tokio::time::Instant,
    seen: Option<&mut Transcript>,
) -> Result<serde_json::Value, NoAnswerReason> {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;

    use crate::external::{CommandProcess, exchange_with_child, finished_tail, stderr_tail};

    let work = tempfile::tempdir().map_err(|_| NoAnswerReason::Transport)?;
    let schema = work.path().join("answer.schema.json");
    let instructions = work.path().join("instructions.md");
    let result = work.path().join("answer.json");
    let catalog = work.path().join("models.json");
    std::fs::write(
        &schema,
        serde_json::to_vec(&codex_schema(prompt.schema.clone())?).map_err(|_| NoAnswerReason::Malformed)?,
    )
    .map_err(|_| NoAnswerReason::Transport)?;
    std::fs::write(&instructions, &prompt.system).map_err(|_| NoAnswerReason::Transport)?;
    let mut models = bundled_models(&backend.config.command, work.path(), deadline).await?;
    disable_catalog_tools(&mut models, backend.config.model.as_deref())?;
    std::fs::write(
        &catalog,
        serde_json::to_vec(&models).map_err(|_| NoAnswerReason::Malformed)?,
    )
    .map_err(|_| NoAnswerReason::Transport)?;

    let mut command = tokio::process::Command::new(&backend.config.command);
    command
        .arg("exec")
        .args([
            "--ephemeral",
            "--ignore-user-config",
            "--ignore-rules",
            "--skip-git-repo-check",
            "--json",
        ])
        .args(["--sandbox", "read-only", "--output-schema"])
        .arg(&schema)
        .arg("--output-last-message")
        .arg(&result)
        .args([
            "--disable",
            "hooks",
            "--disable",
            "shell_tool",
            "--disable",
            "multi_agent",
        ])
        .args([
            "--disable",
            "apps",
            "--disable",
            "browser_use",
            "--disable",
            "skill_search",
        ])
        .args([
            "--disable",
            "computer_use",
            "--disable",
            "browser_use_external",
            "--disable",
            "browser_use_full_cdp_access",
            "--disable",
            "plugins",
            "--disable",
            "unified_exec",
            "--disable",
            "unified_exec_tty",
            "--disable",
            "image_generation",
            "--disable",
            "view_image",
        ])
        .args([
            "--disable",
            "code_mode_host",
            "--disable",
            "in_app_browser",
            "--disable",
            "in_app_local_automation",
            "--disable",
            "remote_plugin",
            "--disable",
            "sleep_tool",
        ])
        .args([
            "-c",
            "web_search=\"disabled\"",
            "-c",
            "tools.web_search=false",
            "-c",
            "tools.experimental_request_user_input.enabled=false",
        ])
        .arg("-c")
        .arg(format!(
            "model_instructions_file={}",
            toml::Value::String(instructions.to_string_lossy().into_owned())
        ))
        .arg("-c")
        .arg(format!(
            "model_catalog_json={}",
            toml::Value::String(catalog.to_string_lossy().into_owned())
        ))
        .current_dir(work.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(model) = &backend.config.model {
        command.arg("--model").arg(model);
    }
    command.arg("-");
    command.as_std_mut().process_group(0);
    isolate_environment(&mut command, std::env::vars_os().collect());

    let mut child = crate::child_process::spawn_async(&mut command).map_err(|_| NoAnswerReason::Unreachable)?;
    let tail = child.stderr.take().map(stderr_tail);
    let mut process = CommandProcess::spawned(child)?;
    let group = process.process_group();
    let exchanged = tokio::time::timeout_at(
        deadline,
        exchange_with_child(
            process.child_mut(),
            group,
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
            return Err(NoAnswerReason::Timeout);
        }
    };
    if let Some(seen) = seen {
        seen.raw_response = Some(output.clone());
    }
    let status = process.terminate_and_reap().await?;
    if !status.success() {
        let detail = match tail {
            Some(tail) => finished_tail(tail).await.and_then(|stderr| stderr.error_line()),
            None => None,
        };
        return Err(NoAnswerReason::NonSuccess {
            status: status.code().and_then(|code| u16::try_from(code).ok()).unwrap_or(0),
            detail,
        });
    }
    validate_events(&output)?;
    let bytes = std::fs::read(&result).map_err(|_| NoAnswerReason::Malformed)?;
    if bytes.len() > backend.max_body_bytes {
        return Err(NoAnswerReason::Oversized);
    }
    serde_json::from_slice(&bytes).map_err(|_| NoAnswerReason::Malformed)
}

#[cfg(unix)]
async fn bundled_models(
    executable: &std::path::Path,
    work: &std::path::Path,
    deadline: tokio::time::Instant,
) -> Result<serde_json::Value, NoAnswerReason> {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;

    use crate::external::{CommandProcess, exchange_with_child};

    let catalog_home = work.join("catalog-home");
    std::fs::create_dir(&catalog_home).map_err(|_| NoAnswerReason::Transport)?;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(["debug", "models", "--bundled"])
        .current_dir(work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    isolate_environment(&mut command, std::env::vars_os().collect());
    command.env("CODEX_HOME", catalog_home);
    let child = crate::child_process::spawn_async(&mut command).map_err(|_| NoAnswerReason::Unreachable)?;
    let mut process = CommandProcess::spawned(child)?;
    let group = process.process_group();
    let exchanged = tokio::time::timeout_at(
        deadline,
        exchange_with_child(process.child_mut(), group, b"", 2_097_152),
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
            return Err(NoAnswerReason::Timeout);
        }
    };
    let status = process.terminate_and_reap().await?;
    if !status.success() {
        return Err(NoAnswerReason::NonSuccess {
            status: status.code().and_then(|code| u16::try_from(code).ok()).unwrap_or(0),
            detail: None,
        });
    }
    serde_json::from_slice(&output).map_err(|_| NoAnswerReason::Malformed)
}

/// A per-consult copy of this CLI release's own catalog prevents built-in model
/// metadata from registering tools independently of the disabled feature flags.
#[cfg(unix)]
fn disable_catalog_tools(catalog: &mut serde_json::Value, selected: Option<&str>) -> Result<(), NoAnswerReason> {
    let models = catalog
        .get_mut("models")
        .and_then(serde_json::Value::as_array_mut)
        .filter(|models| !models.is_empty())
        .ok_or(NoAnswerReason::Malformed)?;
    let mut selected_found = selected.is_none();
    for model in models {
        let fields = model.as_object_mut().ok_or(NoAnswerReason::Malformed)?;
        let slug = fields
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .ok_or(NoAnswerReason::Malformed)?;
        selected_found |= selected == Some(slug);
        fields.insert("apply_patch_tool_type".into(), serde_json::Value::Null);
        fields.insert("shell_type".into(), serde_json::Value::String("disabled".into()));
        fields.insert("tool_mode".into(), serde_json::Value::Null);
        fields.insert("experimental_supported_tools".into(), serde_json::json!([]));
        fields.insert("supports_search_tool".into(), serde_json::Value::Bool(false));
    }
    if !selected_found {
        return Err(NoAnswerReason::Unregistered);
    }
    Ok(())
}

/// Codex's structured-output endpoint supports nested `anyOf` but not `oneOf`.
/// APPA validates the returned answer against its original mandate after the CLI
/// completes, so this transport rewrite cannot widen an accepted policy answer.
#[cfg(unix)]
pub(crate) fn codex_schema(mut schema: serde_json::Value) -> Result<serde_json::Value, NoAnswerReason> {
    fn rewrite(value: &mut serde_json::Value) -> Result<(), NoAnswerReason> {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(variants) = fields.remove("oneOf") {
                    if fields.contains_key("anyOf") {
                        return Err(NoAnswerReason::Malformed);
                    }
                    fields.insert("anyOf".to_string(), variants);
                }
                for child in fields.values_mut() {
                    rewrite(child)?;
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    rewrite(child)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    rewrite(&mut schema)?;
    Ok(schema)
}

/// A completed answer is usable only if no tool could have run during the consult.
fn validate_events(output: &[u8]) -> Result<(), NoAnswerReason> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        AwaitThread,
        AwaitTurn,
        InTurn,
        Complete,
    }
    let mut state = State::AwaitThread;
    let mut saw_answer = false;
    for line in output.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let event: serde_json::Value = serde_json::from_slice(line).map_err(|_| NoAnswerReason::Malformed)?;
        match (state, event.get("type").and_then(serde_json::Value::as_str)) {
            (State::AwaitThread, Some("thread.started")) => state = State::AwaitTurn,
            (State::AwaitTurn, Some("turn.started")) => state = State::InTurn,
            (State::InTurn, Some("turn.completed")) if saw_answer => state = State::Complete,
            (State::InTurn, Some("item.started" | "item.updated" | "item.completed")) => match event
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(serde_json::Value::as_str)
            {
                Some("agent_message") => saw_answer = true,
                Some("reasoning") => {}
                _ => return Err(NoAnswerReason::Malformed),
            },
            _ => return Err(NoAnswerReason::Malformed),
        }
    }
    (state == State::Complete)
        .then_some(())
        .ok_or(NoAnswerReason::Malformed)
}

#[cfg(unix)]
fn isolate_environment(command: &mut tokio::process::Command, parent: Vec<(std::ffi::OsString, std::ffi::OsString)>) {
    command
        .env_clear()
        .envs(parent.into_iter().filter(|(key, _)| environment_key_allowed(key)));
}

#[cfg(unix)]
fn environment_key_allowed(key: &std::ffi::OsStr) -> bool {
    matches!(
        key.to_str(),
        Some(
            "PATH"
                | "HOME"
                | "USER"
                | "LOGNAME"
                | "SHELL"
                | "TMPDIR"
                | "LANG"
                | "LC_ALL"
                | "LC_CTYPE"
                | "CODEX_HOME"
                | "XDG_CONFIG_HOME"
                | "XDG_DATA_HOME"
                | "XDG_CACHE_HOME"
                | "XDG_STATE_HOME"
                | "XDG_RUNTIME_DIR"
                | "DBUS_SESSION_BUS_ADDRESS"
                | "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "ALL_PROXY"
                | "NO_PROXY"
                | "http_proxy"
                | "https_proxy"
                | "all_proxy"
                | "no_proxy"
                | "SSL_CERT_FILE"
                | "SSL_CERT_DIR"
                | "REQUESTS_CA_BUNDLE"
                | "CURL_CA_BUNDLE"
        )
    )
}

#[cfg(not(unix))]
async fn run(
    _backend: &CodexBackend,
    _prompt: &ModelPrompt,
    _deadline: tokio::time::Instant,
    _seen: Option<&mut Transcript>,
) -> Result<serde_json::Value, NoAnswerReason> {
    Err(NoAnswerReason::Unregistered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const CATALOG_RESPONSE: &str = "if [ \"$1\" = debug ]; then printf '%s\\n' '{\"models\":[{\"slug\":\"test-model\",\"apply_patch_tool_type\":\"freeform\",\"shell_type\":\"unified_exec\"}]}'; exit 0; fi\n";
    #[cfg(unix)]
    const RESULT_ARG: &str = "while [ \"$#\" -gt 0 ]; do if [ \"$1\" = '--output-last-message' ]; then shift; result=$1; break; fi; shift; done";
    #[cfg(unix)]
    const COMPLETE_EVENTS: &str = "printf '%s\\n' '{\"type\":\"thread.started\"}' '{\"type\":\"turn.started\"}' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\"}}' '{\"type\":\"turn.completed\"}'";

    #[cfg(unix)]
    fn fake_backend(
        fixture: &tempfile::TempDir,
        script: &str,
        limits: crate::config::ModelLimits,
        gates: &ConsultGates,
    ) -> CodexBackend {
        CodexBackend::new(
            &Codex {
                command: crate::test_support::fake_claude(fixture.path(), &format!("{CATALOG_RESPONSE}{script}")),
                model: None,
                limits,
            },
            65_536,
            gates,
        )
    }

    #[cfg(unix)]
    fn test_prompt() -> ModelPrompt {
        ModelPrompt {
            system: "system instructions".into(),
            input: "input artifact".into(),
            schema: serde_json::json!({"type":"object"}),
        }
    }

    #[test]
    fn rejects_tool_events_and_incomplete_turns() {
        assert_eq!(
            validate_events(b"{\"type\":\"turn.completed\"}\n"),
            Err(NoAnswerReason::Malformed)
        );
        let prefix: &[u8] = b"{\"type\":\"thread.started\"}\n{\"type\":\"turn.started\"}\n";
        let answer: &[u8] = b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\"}}\n";
        let complete: &[u8] = b"{\"type\":\"turn.completed\"}\n";
        assert_eq!(validate_events(&[prefix, answer, complete].concat()), Ok(()));
        assert_eq!(
            validate_events(&[prefix, complete].concat()),
            Err(NoAnswerReason::Malformed)
        );
        assert_eq!(
            validate_events(&[prefix, answer, complete, answer].concat()),
            Err(NoAnswerReason::Malformed)
        );
        assert_eq!(
            validate_events(
                &[
                    prefix,
                    b"{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\"}}\n",
                    complete
                ]
                .concat()
            ),
            Err(NoAnswerReason::Malformed)
        );
        assert_eq!(
            validate_events(b"{\"type\":\"turn.failed\"}\n"),
            Err(NoAnswerReason::Malformed)
        );
    }

    #[cfg(unix)]
    #[test]
    fn child_environment_allows_login_and_transport_but_excludes_provider_secrets() {
        for name in [
            "PATH",
            "HOME",
            "CODEX_HOME",
            "XDG_CONFIG_HOME",
            "HTTPS_PROXY",
            "SSL_CERT_FILE",
        ] {
            assert!(environment_key_allowed(std::ffi::OsStr::new(name)), "{name}");
        }
        for name in [
            "APPA_TEST_SECRET_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "DATABASE_URL",
            "OPENAI_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "GITHUB_TOKEN",
        ] {
            assert!(!environment_key_allowed(std::ffi::OsStr::new(name)), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_catalog_removes_model_declared_tools() {
        let mut catalog = serde_json::json!({"models": [
            {"slug": "one", "apply_patch_tool_type": "freeform", "shell_type": "unified_exec", "tool_mode": "code_mode_only", "experimental_supported_tools": ["clock"], "supports_search_tool": true},
            {"slug": "two", "apply_patch_tool_type": "freeform", "shell_type": "unified_exec"}
        ]});
        assert_eq!(disable_catalog_tools(&mut catalog, Some("one")), Ok(()));
        for model in catalog["models"].as_array().unwrap() {
            assert!(model["apply_patch_tool_type"].is_null());
            assert_eq!(model["shell_type"], "disabled");
            assert!(model["tool_mode"].is_null());
            assert_eq!(model["experimental_supported_tools"], serde_json::json!([]));
            assert_eq!(model["supports_search_tool"], false);
        }
        assert_eq!(
            disable_catalog_tools(&mut catalog, Some("unknown")),
            Err(NoAnswerReason::Unregistered)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_cli_receives_isolated_prompt_and_returns_structured_answer() {
        let fixture = tempfile::tempdir().unwrap();
        let args = fixture.path().join("args");
        let input = fixture.path().join("input");
        let environment = fixture.path().join("environment");
        let script = format!(
            "printf '%s\\n' \"$@\" > '{}'\ncat > '{}'\n\
             env > '{}'\n\
             while [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = '--output-last-message' ]; then\n    shift\n    printf '{{\"ruling\":\"approve\"}}' > \"$1\"\n    break\n  fi\n  shift\ndone\n\
             printf '{{\"type\":\"thread.started\"}}\\n{{\"type\":\"turn.started\"}}\\n{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"done\"}}}}\\n{{\"type\":\"turn.completed\"}}\\n'\n",
            args.display(),
            input.display(),
            environment.display()
        );
        let executable = crate::test_support::fake_claude(fixture.path(), &format!("{CATALOG_RESPONSE}{script}"));
        let backend = CodexBackend::new(
            &Codex {
                command: executable,
                model: Some("test-model".into()),
                limits: crate::config::ModelLimits::MODEL_CALL,
            },
            65_536,
            &ConsultGates::per_runtime(),
        );
        let prompt = ModelPrompt {
            system: "system instructions".into(),
            input: "input artifact".into(),
            schema: serde_json::json!({"type":"object"}),
        };
        assert_eq!(
            backend.consult(&prompt, "test", None).await,
            Ok(serde_json::json!({"ruling":"approve"}))
        );
        assert_eq!(std::fs::read_to_string(input).unwrap(), "input artifact");
        let arguments = std::fs::read_to_string(args).unwrap();
        assert!(arguments.contains("--ignore-user-config\n"));
        assert!(arguments.contains("--disable\nhooks\n"));
        assert!(arguments.contains("--disable\ncomputer_use\n"));
        assert!(arguments.contains("--disable\nplugins\n"));
        assert!(arguments.contains("--disable\nunified_exec\n"));
        assert!(arguments.contains("--sandbox\nread-only\n"));
        assert!(arguments.contains("model_instructions_file="));
        assert!(arguments.contains("model_catalog_json="));
        assert!(arguments.contains("--model\ntest-model\n"));
        assert!(arguments.contains("tools.experimental_request_user_input.enabled=false\n"));
        assert!(
            !std::fs::read_to_string(environment)
                .unwrap()
                .contains("APPA_TEST_SECRET_TOKEN=")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_cli_rejects_missing_malformed_and_tool_using_results() {
        let cases = [
            (format!("{RESULT_ARG}\n{COMPLETE_EVENTS}"), NoAnswerReason::Malformed),
            (
                format!("{RESULT_ARG}\nprintf 'not-json' > \"$result\"\n{COMPLETE_EVENTS}"),
                NoAnswerReason::Malformed,
            ),
            (
                format!(
                    "{RESULT_ARG}\nprintf '{{\"ruling\":\"approve\"}}' > \"$result\"\nprintf '%s\\n' '{{\"type\":\"thread.started\"}}' '{{\"type\":\"turn.started\"}}' '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\"}}}}' '{{\"type\":\"turn.completed\"}}'"
                ),
                NoAnswerReason::Malformed,
            ),
        ];
        for (script, expected) in cases {
            let fixture = tempfile::tempdir().unwrap();
            let backend = fake_backend(
                &fixture,
                &script,
                crate::config::ModelLimits::MODEL_CALL,
                &ConsultGates::per_runtime(),
            );
            assert_eq!(backend.consult(&test_prompt(), "failure", None).await, Err(expected));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_cli_nonzero_exit_cannot_supply_an_answer() {
        let fixture = tempfile::tempdir().unwrap();
        let script =
            format!("{RESULT_ARG}\nprintf '{{\"ruling\":\"approve\"}}' > \"$result\"\n{COMPLETE_EVENTS}\nexit 7");
        let backend = fake_backend(
            &fixture,
            &script,
            crate::config::ModelLimits::MODEL_CALL,
            &ConsultGates::per_runtime(),
        );
        assert!(matches!(
            backend.consult(&test_prompt(), "failure", None).await,
            Err(NoAnswerReason::NonSuccess { status: 7, .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_and_cancellation_end_descendants() {
        let fixture = tempfile::tempdir().unwrap();
        let pid_file = fixture.path().join("descendant.pid");
        let script = format!("sleep 30 &\necho $! > '{}'\nwait", pid_file.display());
        let limits = crate::config::ModelLimits {
            timeout: crate::test_support::PROCESS_BUDGET,
            max_concurrent: 4,
        };
        let backend = fake_backend(&fixture, &script, limits, &ConsultGates::per_runtime());
        let pending = tokio::spawn({
            let backend = backend.clone();
            async move { backend.consult(&test_prompt(), "cancel", None).await }
        });
        let descendant = crate::test_support::recorded_pid(&pid_file).await;
        pending.abort();
        let _ = pending.await;
        crate::test_support::assert_process_gone(descendant).await;

        std::fs::remove_file(&pid_file).unwrap();
        let timeout_backend = fake_backend(
            &fixture,
            &script,
            crate::config::ModelLimits {
                timeout: std::time::Duration::from_millis(500),
                max_concurrent: 4,
            },
            &ConsultGates::per_runtime(),
        );
        assert_eq!(
            timeout_backend.consult(&test_prompt(), "timeout", None).await,
            Err(NoAnswerReason::Timeout)
        );
        let descendant = crate::test_support::recorded_pid(&pid_file).await;
        crate::test_support::assert_process_gone(descendant).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn saturated_gate_times_out_without_spawning_codex() {
        let fixture = tempfile::tempdir().unwrap();
        let marker = fixture.path().join("spawned");
        let gates = ConsultGates::per_runtime();
        let backend = fake_backend(
            &fixture,
            &format!("touch '{}'", marker.display()),
            crate::config::ModelLimits {
                timeout: std::time::Duration::from_millis(50),
                max_concurrent: 4,
            },
            &gates,
        );
        let gate = gates.model(AnnotatorBuiltin::Codex);
        let _held = gate.acquire_many_owned(4).await.unwrap();
        assert_eq!(
            backend.consult(&test_prompt(), "busy", None).await,
            Err(NoAnswerReason::Timeout)
        );
        assert!(!marker.exists());
    }
}

mod common;
use common::repo_root;
#[cfg(unix)]
use common::{actor, last_offer, offer_of, propose, ran, raw, root};

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use appa_runtime::api::{OfferId, RemedyOutcome, Runtime};
#[cfg(unix)]
use appa_runtime::config::AnnotatorImplementation;
use appa_runtime::config::Config;
#[cfg(unix)]
use appa_runtime::hooks;
#[cfg(unix)]
use appa_runtime_api::{HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome};

/// The policies a package directory ships, by the suffix every one of them
/// carries. The package's own `appa-package.toml` manifest is not a policy.
#[cfg(unix)]
fn policy_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("the directory entry is readable").path();
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".appa.toml"))
        {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn opens(path: &Path) {
    let battery_dirs = [repo_root().join("marketplace/batteries")];
    let config = Config::load_from(path, &battery_dirs)
        .unwrap_or_else(|error| panic!("{} does not load: {error}", path.display()));
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    Runtime::open(config, dir.path().join("appa.db"), None)
        .unwrap_or_else(|error| panic!("{} does not open: {error}", path.display()));
}

#[cfg(unix)]
#[test]
fn every_shipped_example_opens() {
    let examples = policy_files(&repo_root().join("marketplace/plugins/claude-code"));
    assert!(
        !examples.is_empty(),
        "the shipped examples were found and checked, not {examples:?}"
    );
    for path in &examples {
        opens(path);
    }
}

#[test]
fn the_kagent_policies_open() {
    opens(&repo_root().join("marketplace/plugins/kagent/default.appa.toml"));
    opens(&repo_root().join("integrations/kagent/demo/chart/files/demo.appa.toml"));
}

#[cfg(unix)]
#[test]
fn the_live_replay_roots_open() {
    opens(&repo_root().join("examples/live-replays/github/appa.toml"));
    opens(&repo_root().join("examples/live-replays/linear/appa.toml"));
}

/// The initialized default with the Claude Code battery included, as `appa plugin install claude-code` composes
/// them: the battery's rules run before the default's.
#[cfg(unix)]
fn composed_with_the_battery(dir: &tempfile::TempDir) -> Config {
    let battery_dir = dir.path().join("batteries/claude-code");
    std::fs::create_dir_all(&battery_dir).expect("the battery directory is created");

    let repository = repo_root();
    let default = std::fs::read_to_string(repository.join("marketplace/plugins/claude-code/default.appa.toml"))
        .expect("the initialized default is readable");
    std::fs::copy(
        repository.join("marketplace/batteries/claude-code/appa.toml"),
        battery_dir.join("appa.toml"),
    )
    .expect("the battery file is copied");
    std::fs::copy(
        repository.join("marketplace/batteries/claude-code/repository.py"),
        battery_dir.join("repository.py"),
    )
    .expect("the repository input is copied");

    let root = dir.path().join("appa.toml");
    std::fs::write(
        &root,
        format!("include = [\"batteries/claude-code/appa.toml\"]\n\n{default}"),
    )
    .expect("the initialized config includes the battery");

    Config::load(&root).expect("the initialized config and battery compose")
}

#[cfg(unix)]
#[test]
fn the_initialized_default_composes_with_the_claude_code_battery() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = composed_with_the_battery(&dir);
    let Some(AnnotatorImplementation::Command(repository)) = config.externals.inputs.get("claude-code.repository")
    else {
        panic!("the root registers the battery's repository input");
    };
    assert!(repository.cwd.join(&repository.argv[1]).is_file());
    let annotators = config.policy_file().value()["annotator"]
        .as_array()
        .expect("the composed Annotators are an array");
    let bash_annotators = annotators
        .iter()
        .filter(|annotator| annotator["name"].as_str() == Some("claude-code.bash-requirements"))
        .collect::<Vec<_>>();
    assert_eq!(bash_annotators.len(), 1, "the root supplies the Bash Annotator");
    let tools = config.policy_file().value()["tool"]
        .as_array()
        .expect("the composed tools are an array");
    for (name, annotator) in [
        ("host/claude-code/Bash", "claude-code.bash-requirements"),
        (
            "host/claude-code/Bash(command:*git push*)",
            "claude-code.bash-repository-requirements",
        ),
        ("*", "claude-code.undeclared-tool"),
    ] {
        let matches = tools
            .iter()
            .filter(|tool| tool["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{name} has exactly one composed rule");
        assert_eq!(matches[0]["annotator"].as_str(), Some(annotator));
    }
    let read = tools
        .iter()
        .filter(|tool| tool["name"].as_str() == Some("host/claude-code/Read"))
        .collect::<Vec<_>>();
    assert_eq!(
        read.len(),
        1,
        "the plain Read rule composes once, after the battery's selectors"
    );
    assert!(
        read[0].get("annotator").is_none(),
        "Read is static: no annotator names a reader"
    );

    let database = dir.path().join("appa.db");
    Runtime::open(config, database, None).expect("the composed deployment opens");
}

/// The shipped default and battery leave `audiences` omitted, so each Annotator's mandate
/// is the policy's whole audience vocabulary: the chain words and every reader a
/// declaration names — a reader the deployer adds included.
#[cfg(unix)]
#[test]
fn the_shipped_annotators_admit_every_audience_the_policy_writes() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    composed_with_the_battery(&dir);
    let root = dir.path().join("appa.toml");
    let mut composed = std::fs::read_to_string(&root).expect("the composed root is readable");
    composed.push_str("\n[[policy.tool]]\nname = \"ReadPayroll\"\ndelta = { audience = [\"payroll@corp.example\"] }\n");
    std::fs::write(&root, composed).expect("the root gains a reader-bearing declaration");
    let config = Config::load(&root).expect("the extended config composes");
    let policy = appa_policy::Config::from_toml_str(
        &toml::to_string(config.policy_file().value()).expect("the composed policy renders"),
    )
    .expect("the composed policy loads");

    for annotator in ["claude-code.bash-requirements", "claude-code.undeclared-tool"] {
        let entries: Vec<String> = policy
            .engine()
            .registry()
            .annotator_mandate(&appa_engine::names::AnnotatorName::new(annotator))
            .unwrap_or_else(|| panic!("{annotator} registers"))
            .audiences()
            .entries()
            .collect();
        for expected in ["self", "internal", "payroll@corp.example"] {
            assert!(
                entries.iter().any(|entry| entry == expected),
                "{annotator} admits {expected}: {entries:?}"
            );
        }
        assert!(
            !entries.iter().any(|entry| entry == "public"),
            "{annotator}: `public` is always admissible and never listed: {entries:?}"
        );
    }
}

/// The offer the feedback attributes to the `redact-secrets` sanitizer. An offer's
/// `returns` describes a child's return only, so the feedback is where an output
/// sanitizer's offer is named.
#[cfg(unix)]
fn masker_offer(feedback: &str) -> OfferId {
    let line = feedback
        .lines()
        .skip_while(|line| !line.contains("sanitizer redact-secrets"))
        .nth(1)
        .unwrap_or_else(|| panic!("no redact-secrets offer in feedback: {feedback}"));
    last_offer(line)
}

#[cfg(unix)]
fn call(tool: &str, argument: &str, value: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ argument: value })),
        cwd: None,
    }
}

/// A credential named relatively — `.env`, `cat .netrc` — is judged like its absolute
/// spelling. A Bash call naming one narrows the trajectory to `self` and offers the stock
/// masker, which returns the command's masked output to `public`. A Read narrows the
/// trajectory to `self`, after which a public sink requires an exact-call human review.
#[cfg(unix)]
#[tokio::test]
async fn the_battery_judges_relative_credentials_and_offers_review_for_public_release() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = composed_with_the_battery(&dir);
    let runtime =
        Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the composed deployment opens"));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None
            }
        )
        .await,
        HookDecision::Ack
    );

    // A credential read is `self` data: the block offers the masker beside the plain
    // narrowing and consults no person; the masked output is what reaches the model.
    for command in [
        "cat .env",
        "cat .netrc",
        "cat ~/.ssh/id_ed25519",
        "cat /home/me/.aws/credentials",
    ] {
        let read = call("host/claude-code/Bash", "command", command);
        let blocked = propose(&runtime, read.clone()).await;
        let HookDecision::DenyCall {
            feedback,
            offers,
            review,
        } = blocked
        else {
            panic!("`{command}` is blocked with the masker offered, got {blocked:?}");
        };
        assert_eq!(offers.len(), 2, "`{command}` offers the narrowing and the masker");
        assert!(review.is_empty(), "`{command}` consults no person");
        assert!(matches!(
            runtime.execute_remedy(&actor(), masker_offer(&feedback)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        let delivered = hooks::handle(
            &runtime,
            HookEvent::ToolResult {
                actor: actor(),
                call: read,
                call_id: None,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(
                        "GITHUB_TOKEN=ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789\nAWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\nREGION=eu-west-1\n"
                            .to_string(),
                    ),
                },
            },
        )
        .await;
        let HookDecision::DeliverValue { value } = delivered else {
            panic!("`{command}`'s output crosses through the masker, got {delivered:?}");
        };
        assert_eq!(
            value,
            "GITHUB_TOKEN=[redacted-secret]\nAWS_SECRET_ACCESS_KEY=[redacted-secret]\nREGION=eu-west-1\n"
        );
    }

    for path in ["./README.md", "../src/main.rs", "src/.gitignore/../main.rs"] {
        let ordinary = call("host/claude-code/Read", "file_path", path);
        assert_eq!(
            propose(&runtime, ordinary.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "`{path}` is an ordinary read: a dot in a relative path is not a hidden name"
        );
        ran(&runtime, ordinary).await;
    }

    let read = call("host/claude-code/Read", "file_path", ".env");
    let narrowing = propose(&runtime, read.clone()).await;
    let HookDecision::DenyCall { feedback, .. } = narrowing else {
        panic!("reading `.env` is offered as a narrowing to `self`, got {narrowing:?}");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;

    // With the trajectory at `self`, a credential command needs no masker: it runs, and
    // its confined result is kept as the tool returned it.
    let settled = call("host/claude-code/Bash", "command", "cat ~/.npmrc");
    assert_eq!(
        propose(&runtime, settled.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, settled).await;

    let artifact = call("host/claude-code/Artifact", "file_path", "page.html");
    assert_eq!(
        propose(&runtime, artifact).await,
        HookDecision::AllowCall { spawn: None },
        "an artifact is private to the requester until they share it"
    );

    let publication = propose(&runtime, call("host/claude-code/WebSearch", "query", "page.html")).await;
    let HookDecision::DenyCall {
        feedback,
        offers,
        review,
    } = publication
    else {
        panic!("a trajectory narrowed to `self` requires review before publishing: {publication:?}");
    };
    assert_eq!(
        offers.len(),
        1,
        "the default authority can review the audience expansion"
    );
    assert_eq!(review.len(), 1, "the offer is backed by the default human authority");
    assert!(feedback.contains("Submit for approval"));
    assert!(feedback.contains("The confirmation card is not open yet"));
    assert!(review[0].text.contains("page.html"), "the review shows the exact call");
    assert!(
        review[0].text.contains("public"),
        "the review shows the audience expansion it covers"
    );
}

/// The Slack battery requires `contains = ["internal"]` on writes: a public session can
/// post autonomously without human approval, while a session holding `self` secrets cannot
/// leak them into Slack channels.
#[cfg(unix)]
#[tokio::test]
async fn the_slack_battery_allows_public_writes_and_blocks_leaking_self_secrets() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let slack_battery_dir = dir.path().join("batteries/slack");
    let claude_battery_dir = dir.path().join("batteries/claude-code");
    std::fs::create_dir_all(&slack_battery_dir).expect("slack battery directory is created");
    std::fs::create_dir_all(&claude_battery_dir).expect("claude battery directory is created");

    let repository = repo_root();
    let default = std::fs::read_to_string(repository.join("marketplace/plugins/claude-code/default.appa.toml"))
        .expect("the initialized default is readable");
    std::fs::copy(
        repository.join("marketplace/batteries/slack/appa.toml"),
        slack_battery_dir.join("appa.toml"),
    )
    .expect("slack battery file is copied");
    std::fs::copy(
        repository.join("marketplace/batteries/claude-code/appa.toml"),
        claude_battery_dir.join("appa.toml"),
    )
    .expect("claude battery file is copied");

    let root_path = dir.path().join("appa.toml");
    std::fs::write(
        &root_path,
        format!("include = [\"batteries/claude-code/appa.toml\", \"batteries/slack/appa.toml\"]\n\n{default}"),
    )
    .expect("the config includes both batteries");

    let config = Config::load(&root_path).expect("the config loads");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("opens"));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None
            }
        )
        .await,
        HookDecision::Ack
    );

    let slack_send = ProposedCall {
        tool: "mcp/claude_ai_Slack/slack_send_message".to_string(),
        arguments: raw(serde_json::json!({ "channel_id": "C123", "text": "hello" })),
        cwd: None,
    };

    // 1. Fresh public session: slack write is allowed autonomously
    assert_eq!(
        propose(&runtime, slack_send.clone()).await,
        HookDecision::AllowCall { spawn: None },
        "a public session can post to slack without hitl"
    );
    ran(&runtime, slack_send.clone()).await;

    // 2. Read .env and accept narrowing to self
    let read_env = call("host/claude-code/Read", "file_path", ".env");
    let narrowing = propose(&runtime, read_env.clone()).await;
    let HookDecision::DenyCall { feedback, .. } = narrowing else {
        panic!("reading .env narrows to self");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_env.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_env).await;

    // 3. Narrowed to self: slack write is BLOCKED from leaking secrets
    let blocked_slack = propose(&runtime, slack_send).await;
    let HookDecision::DenyCall { offers, .. } = blocked_slack else {
        panic!("slack write must be blocked when session holds self secrets, got {blocked_slack:?}");
    };
    assert!(
        offers.is_empty(),
        "slack write cannot leak self secrets: no remedy plan"
    );
}

/// The battery's masker is not scoped to the static credential rules: a command the
/// Annotator narrows to `self` is blocked with the same masker offered, and its masked
/// output is what reaches the model. An Annotator's answer cannot tag a call, so a
/// tagged sanitizer would never reach a command the static rules do not name.
#[cfg(unix)]
#[tokio::test]
async fn the_masker_is_offered_for_a_command_the_annotator_narrows_to_self() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let battery_dir = dir.path().join("batteries/claude-code");
    std::fs::create_dir_all(&battery_dir).expect("the battery directory is created");
    std::fs::copy(
        repo_root().join("marketplace/batteries/claude-code/appa.toml"),
        battery_dir.join("appa.toml"),
    )
    .expect("the battery file is copied");
    std::fs::write(
        dir.path().join("annotator.sh"),
        "cat > /dev/null\nprintf '%s' '{\"version\":1,\"answer\":{\"delta\":{\"audience\":[\"self\"]},\"requires\":{\"history\":[],\"attention\":[]},\"emits\":[]}}'",
    )
    .expect("the annotator script writes");
    let root_path = dir.path().join("appa.toml");
    std::fs::write(
        &root_path,
        r#"include = ["batteries/claude-code/appa.toml"]

[policy]
version = 2

[[policy.annotator]]
name = "claude-code.bash-requirements"

[[policy.tool]]
name = "host/claude-code/Bash"
annotator = "claude-code.bash-requirements"

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.annotators."claude-code.bash-requirements"]
command = ["/bin/sh", "annotator.sh"]
"#,
    )
    .expect("the root replaces the battery Annotator with a script");
    let config = Config::load(&root_path).expect("the root and battery compose");
    let runtime =
        Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the composed deployment opens"));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None
            }
        )
        .await,
        HookDecision::Ack
    );

    let read = call("host/claude-code/Bash", "command", "cat ~/.bashrc");
    let blocked = propose(&runtime, read.clone()).await;
    let HookDecision::DenyCall { feedback, offers, .. } = blocked else {
        panic!("an annotated `self` command is blocked with the masker offered, got {blocked:?}");
    };
    assert_eq!(offers.len(), 2, "the narrowing and the masker are offered");
    assert!(matches!(
        runtime.execute_remedy(&actor(), masker_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    let delivered = hooks::handle(
        &runtime,
        HookEvent::ToolResult {
            actor: actor(),
            call: read,
            call_id: None,
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(
                    "export OPENAI_API_KEY=sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789\nalias ll='ls -l'\n"
                        .to_string(),
                ),
            },
        },
    )
    .await;
    let HookDecision::DeliverValue { value } = delivered else {
        panic!("the output crosses through the masker, got {delivered:?}");
    };
    assert_eq!(value, "export OPENAI_API_KEY=[redacted-secret]\nalias ll='ls -l'\n");
}

/// A search inside a credential path is a read of it and narrows the session like the
/// Read rules do. Writing into one needs a `trusted` session, so it runs before untrusted
/// content arrives and is refused after, and so is writing a file a later process reads
/// as instructions or runs as code. The files that can turn the protection off ask the
/// person for each exact call.
#[cfg(unix)]
#[tokio::test]
async fn the_battery_covers_grep_write_and_edit_of_the_requesters_secrets() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = composed_with_the_battery(&dir);
    let runtime =
        Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the composed deployment opens"));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None
            }
        )
        .await,
        HookDecision::Ack
    );

    let ordinary = call("host/claude-code/Grep", "path", "src");
    assert_eq!(
        propose(&runtime, ordinary.clone()).await,
        HookDecision::AllowCall { spawn: None },
        "a search over ordinary paths keeps the session's label"
    );
    ran(&runtime, ordinary).await;
    let inside = call("host/claude-code/Grep", "path", "/home/me/.aws/credentials");
    let narrowing = propose(&runtime, inside).await;
    let HookDecision::DenyCall { offers, review, .. } = narrowing else {
        panic!("a search inside a credential path is offered as a narrowing to `self`, got {narrowing:?}");
    };
    assert_eq!(offers.len(), 1, "the narrowing is the one offer");
    assert!(review.is_empty(), "no person is consulted");

    let credential = call("host/claude-code/Write", "file_path", ".env");
    assert_eq!(
        propose(&runtime, credential.clone()).await,
        HookDecision::AllowCall { spawn: None },
        "a trusted session writes a credential path"
    );
    ran(&runtime, credential.clone()).await;

    let settings = call("host/claude-code/Edit", "file_path", "/home/me/.claude/settings.json");
    let asked = propose(&runtime, settings).await;
    let HookDecision::DenyCall { review, .. } = asked else {
        panic!("editing the harness's settings asks the person, got {asked:?}");
    };
    assert_eq!(review.len(), 1, "the exact call is shown to the person");
    assert!(review[0].text.contains("settings.json"));
    for (tool, path) in [
        ("host/claude-code/Write", ".mcp.json"),
        ("host/claude-code/Edit", "/home/me/.claude/hooks/guard.sh"),
    ] {
        let asked = propose(&runtime, call(tool, "file_path", path)).await;
        let HookDecision::DenyCall { review, .. } = asked else {
            panic!("{path}: a file that can turn the protection off asks the person, got {asked:?}");
        };
        assert_eq!(review.len(), 1, "{path}");
    }
    let instructions = call("host/claude-code/Edit", "file_path", "/repo/CLAUDE.md");
    assert_eq!(
        propose(&runtime, instructions.clone()).await,
        HookDecision::AllowCall { spawn: None },
        "a trusted session edits the agent's instructions"
    );
    ran(&runtime, instructions).await;

    // Reading a web page is offered as the drop to `suspicious`; the session accepts it.
    let page = call("host/claude-code/WebFetch", "url", "https://docs.example/page");
    let drop = propose(&runtime, page.clone()).await;
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&drop)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, page.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, page).await;
    let refused = propose(&runtime, credential).await;
    let HookDecision::DenyCall {
        feedback,
        offers,
        review,
    } = refused
    else {
        panic!("a session that read untrusted content does not write a credential path, got {refused:?}");
    };
    assert_eq!(offers.len(), 1, "the person can approve the exact call: {feedback}");
    assert_eq!(review.len(), 1);
    assert!(review[0].text.contains(".env"), "the review shows the exact call");
    for (tool, path) in [
        ("host/claude-code/Edit", "/repo/CLAUDE.md"),
        ("host/claude-code/Write", ".claude/skills/review/SKILL.md"),
        ("host/claude-code/Write", "/repo/.git/hooks/pre-commit"),
        ("host/claude-code/Edit", "/home/me/.zshrc"),
        ("host/claude-code/Write", "/home/me/Library/LaunchAgents/agent.plist"),
    ] {
        let refused = propose(&runtime, call(tool, "file_path", path)).await;
        let HookDecision::DenyCall { offers, .. } = refused else {
            panic!(
                "{path}: a session that read untrusted content does not write what a later process runs, got {refused:?}"
            );
        };
        assert_eq!(offers.len(), 1, "{path}: the person can approve the exact call");
    }
    let note = call("host/claude-code/Write", "file_path", "notes.md");
    assert_eq!(
        propose(&runtime, note).await,
        HookDecision::AllowCall { spawn: None },
        "an ordinary path takes the session's label as it is"
    );
}

//! The Dogwood plugin as Annotator and recorder behind the real runtime and the real hook
//! path. A Dogwood policy with two temporal rules gates two tools: a deploy needs a successful
//! backup of the same environment within the last hour, and at most two deploys run per hour.

use std::sync::Arc;

use appa_runtime::api::Runtime;
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};

const PLUGIN: &str = env!("CARGO_BIN_EXE_appa-dogwood");

const SCHEMA: &str = r#"namespace Ops {
  entity Agent;
  entity Gateway;
  type EnvInput = { env: String };
  action "Backup" appliesTo { principal: [Agent], resource: [Gateway], context: { input: EnvInput } };
  action "Deploy" appliesTo { principal: [Agent], resource: [Gateway], context: { input: EnvInput } };
}
"#;

const POLICY: &str = r#"
permit (principal, action == Ops::Action::"Backup", resource);

permit (principal, action == Ops::Action::"Deploy", resource)
when temporal {
    formerly within 1h Ops::Action::"Backup"::response{ input.env: context.input.env }
};

forbid (principal, action == Ops::Action::"Deploy", resource)
when temporal { count_within(1h, Ops::Action::"Deploy"::request{}) > 2 };
"#;

const PLUGIN_CONFIG: &str = r#"
store = "dogwood.json"
policy = "policy.dw"
schema = "ops.cedarschema"
principal = 'Ops::Agent::"appa"'
resource = 'Ops::Gateway::"ops"'
settle_wait_ms = 10000

[tools]
"mcp/ops/backup" = "Ops::Action::Backup"
"mcp/ops/deploy" = "Ops::Action::Deploy"
"#;

fn appa_policy(plugin_config: &std::path::Path) -> String {
    let command = |verb: &str| {
        format!(
            "[{PLUGIN:?}, {verb:?}, \"--config\", {:?}]",
            plugin_config.display().to_string()
        )
    };
    format!(
        r#"
[policy]
version = 2

[[policy.annotator]]
name = "dogwood"
marks = ["blocked"]

[[policy.tool]]
name = "mcp/ops/backup"
annotator = "dogwood"
recorder = "dogwood"

[[policy.tool]]
name = "mcp/ops/deploy"
annotator = "dogwood"
recorder = "dogwood"

[externals]
timeout_ms = 20000
max_body_bytes = 65536

[externals.annotators.dogwood]
command = {annotate}

[externals.recorders.dogwood]
command = {record}
"#,
        annotate = command("annotate"),
        record = command("record"),
    )
}

fn actor() -> Actor {
    Actor {
        root: TrajectoryId("session".to_string()),
        child: None,
    }
}

fn call(tool: &str, env: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: serde_json::value::to_raw_value(&serde_json::json!({ "env": env })).unwrap(),
        cwd: None,
    }
}

async fn propose(runtime: &Arc<Runtime>, tool: &str, env: &str) -> HookDecision {
    let event = HookEvent::ToolCall {
        actor: actor(),
        call: call(tool, env),
        call_id: None,
        spawn: None,
        prompt: None,
        ruling: None,
    };
    hooks::handle(runtime, event).await
}

/// Propose a call, and when it is released, report it run successfully.
async fn attempt(runtime: &Arc<Runtime>, tool: &str, env: &str) -> bool {
    let decision = propose(runtime, tool, env).await;
    match decision {
        HookDecision::AllowCall { spawn: None } => {}
        HookDecision::DenyCall { feedback, .. } => {
            assert!(
                !feedback.contains("offer_id"),
                "a Dogwood denial is `blocked`, which no remedy unblocks: {feedback}"
            );
            return false;
        }
        other => panic!("{tool}: {other:?}"),
    }
    let event = HookEvent::ToolResult {
        actor: actor(),
        call: call(tool, env),
        call_id: None,
        outcome: ToolOutcome::Success {
            body: OutcomeBody::Available("ok".to_string()),
        },
    };
    assert_eq!(hooks::handle(runtime, event).await, HookDecision::Ack);
    true
}

#[tokio::test(flavor = "multi_thread")]
async fn temporal_rules_gate_calls_over_the_recorded_history() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ops.cedarschema"), SCHEMA).unwrap();
    std::fs::write(dir.path().join("policy.dw"), POLICY).unwrap();
    let plugin_config = dir.path().join("dogwood.toml");
    std::fs::write(&plugin_config, PLUGIN_CONFIG).unwrap();
    let config_path = dir.path().join("appa.toml");
    std::fs::write(&config_path, appa_policy(&plugin_config)).unwrap();
    let runtime =
        Arc::new(Runtime::open(Config::load(&config_path).unwrap(), dir.path().join("appa.db"), None).unwrap());
    let start = HookEvent::SessionStart {
        root: actor().root,
        principal: None,
        address: None,
        title: None,
        launch: None,
        start: None,
    };
    assert_eq!(hooks::handle(&runtime, start).await, HookDecision::Ack);

    const BACKUP: &str = "mcp/ops/backup";
    const DEPLOY: &str = "mcp/ops/deploy";
    assert!(!attempt(&runtime, DEPLOY, "prod").await, "no backup yet");
    assert!(attempt(&runtime, BACKUP, "prod").await);
    assert!(
        !attempt(&runtime, DEPLOY, "staging").await,
        "the backup was of prod, and the rule correlates the environment"
    );
    assert!(
        attempt(&runtime, DEPLOY, "prod").await,
        "the first deploy after the backup"
    );
    assert!(
        attempt(&runtime, DEPLOY, "prod").await,
        "the second deploy within the hour"
    );
    assert!(
        !attempt(&runtime, DEPLOY, "prod").await,
        "a third deploy within the hour is over the cap"
    );

    let store: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("dogwood.json")).unwrap()).unwrap();
    let kinds: Vec<(&str, &str)> = store["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["tool"].as_str().unwrap(), entry["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        [
            (BACKUP, "request"),
            (BACKUP, "response"),
            (DEPLOY, "request"),
            (DEPLOY, "response"),
            (DEPLOY, "request"),
            (DEPLOY, "response"),
        ],
        "only released calls and their outcomes enter the history"
    );
}

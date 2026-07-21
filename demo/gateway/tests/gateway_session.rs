
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use appa_demo::gateway::{ConfigError, GatewayConfig, Outcome, Session};

const SCENARIO_TOOLS: &str = r#"
[response]
readers = ["alice@archestra.ai", "bob@archestra.ai"]

[[tool]]
name = "invoices_list"
description = "List invoices."
result = "47 invoices totaling $1,248,000."

[[tool]]
name = "send_email"
description = "Send an email."
result = "Email sent to {to}."

[[tool.arg]]
name = "to"
required = true

[[tool.arg]]
name = "subject"

[[tool.arg]]
name = "body"

[[tool]]
name = "db_drop"
description = "Drop the database."
result = "Database dropped."
"#;

const SCENARIO_POLICY: &str = r#"
[[authority]]
name = "human-in-the-loop"
rule = "escalate"
audience = ["alice@archestra.ai", "bob@archestra.ai", "alex@finance-audit.com"]
may_release_control = true
confirms = true
acknowledge_unknown = true

[[tool]]
name = "invoices_list"
requires = {}
output = { audience = ["alice@archestra.ai", "bob@archestra.ai"], trust = "trusted" }

[[tool]]
name = "send_email"
requires = { audience = "$.args.to" }
output = { audience = "public", trust = "trusted", effects = ["egress"] }

[[tool]]
name = "db_drop"
requires = { attention = "explicit_confirmation" }
output = { audience = "public", trust = "trusted", effects = ["mutation"] }
"#;

fn session() -> Session {
    let config = GatewayConfig::from_toml(SCENARIO_TOOLS, SCENARIO_POLICY).expect("scenario parses");
    Session::new(Arc::new(config))
}

fn args(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), (*v).into())).collect()
}

fn email_args() -> serde_json::Map<String, serde_json::Value> {
    args(&[
        ("to", "alex@finance-audit.com"),
        ("subject", "Q2 invoices"),
        ("body", "totals attached"),
    ])
}

fn read_invoices(session: &mut Session) {
    match session.call_tool("invoices_list", &args(&[])) {
        Outcome::Executed { result, .. } => assert_eq!(result, "47 invoices totaling $1,248,000."),
        other => panic!("expected the read to execute, got {other:?}"),
    }
}

#[test]
fn clean_read_executes_with_template_result() {
    let mut session = session();
    read_invoices(&mut session);
}

#[test]
fn out_of_audience_send_soft_blocks() {
    let mut session = session();
    read_invoices(&mut session);
    match session.call_tool("send_email", &email_args()) {
        Outcome::SoftBlocked {
            violations, recipients, ..
        } => {
            assert!(!violations.is_empty());
            assert_eq!(
                recipients,
                std::collections::BTreeSet::from([appa_core::UserId::new("alex@finance-audit.com")])
            );
        }
        other => panic!("expected a soft block, got {other:?}"),
    }
}

#[tokio::test]
async fn approved_escalation_dispatches_the_canonical_request_once() {
    let mut session = session();
    read_invoices(&mut session);
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &email_args()) else {
        panic!("expected a soft block");
    };

    let asks = AtomicUsize::new(0);
    let outcome = session
        .escalate("auditor needs the summary", |message| {
            asks.fetch_add(1, Ordering::SeqCst);
            assert!(message.contains("alex@finance-audit.com"));
            async { Some(true) }
        })
        .await;
    match outcome {
        Outcome::Granted { result, .. } => assert_eq!(result, "Email sent to alex@finance-audit.com."),
        other => panic!("expected the escalation to be granted, got {other:?}"),
    }
    assert_eq!(asks.load(Ordering::SeqCst), 1, "the human is asked exactly once");

    match session.escalate("again", |_| async { Some(true) }).await {
        Outcome::NothingPending => {}
        other => panic!("expected nothing pending after the grant, got {other:?}"),
    }
}

#[tokio::test]
async fn declined_escalation_denies_and_clears_the_action() {
    let mut session = session();
    read_invoices(&mut session);
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &email_args()) else {
        panic!("expected a soft block");
    };

    match session.escalate("auditor needs it", |_| async { Some(false) }).await {
        Outcome::Denied { .. } => {}
        other => panic!("expected the escalation to be denied, got {other:?}"),
    }

    match session.call_tool("send_email", &email_args()) {
        Outcome::SoftBlocked { .. } => {}
        other => panic!("expected a fresh soft block after denial, got {other:?}"),
    }
}

#[tokio::test]
async fn escalation_without_a_pending_action_reports_nothing_pending() {
    let mut session = session();
    match session.escalate("nothing was blocked", |_| async { Some(true) }).await {
        Outcome::NothingPending => {}
        other => panic!("expected nothing pending, got {other:?}"),
    }
}

#[tokio::test]
async fn a_different_call_abandons_the_pending_action() {
    let mut session = session();
    read_invoices(&mut session);
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &email_args()) else {
        panic!("expected a soft block");
    };

    read_invoices(&mut session);
    match session.escalate("stale", |_| async { Some(true) }).await {
        Outcome::NothingPending => {}
        other => panic!("expected the abandoned action to be gone, got {other:?}"),
    }
}

#[test]
fn a_reissued_identical_call_soft_blocks_again() {
    let mut session = session();
    read_invoices(&mut session);
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &email_args()) else {
        panic!("expected a soft block");
    };
    match session.call_tool("send_email", &email_args()) {
        Outcome::SoftBlocked { .. } => {}
        other => panic!("expected idempotent re-entry to soft-block again, got {other:?}"),
    }
}

#[tokio::test]
async fn a_reissued_call_after_completion_is_a_new_action() {
    let mut session = session();
    read_invoices(&mut session);
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &email_args()) else {
        panic!("expected a soft block");
    };
    let Outcome::Granted { .. } = session.escalate("auditor needs it", |_| async { Some(true) }).await else {
        panic!("expected the escalation to be granted");
    };

    match session.call_tool("send_email", &email_args()) {
        Outcome::SoftBlocked { .. } => {}
        other => panic!("expected the replay to re-enter policy as a new action, got {other:?}"),
    }
}

#[tokio::test]
async fn split_mandates_ask_each_authority_once() {
    const SPLIT_TOOLS: &str = r#"
[[tool]]
name = "invoices_list"
description = "List invoices."
result = "47 invoices totaling $1,248,000."

[[tool]]
name = "send_email"
description = "Send an email."
result = "Email sent to {to}."

[[tool.arg]]
name = "to"
required = true
"#;
    const SPLIT_POLICY: &str = r#"
[[authority]]
name = "audience-endorser"
rule = "escalate"
audience = ["alice@archestra.ai", "bob@archestra.ai", "alex@finance-audit.com"]

[[authority]]
name = "effects-officer"
rule = "escalate"
may_release_control = true

[[tool]]
name = "invoices_list"
requires = {}
output = { audience = ["alice@archestra.ai", "bob@archestra.ai"], trust = "trusted" }

[[tool]]
name = "send_email"
requires = { audience = "$.args.to" }
output = { audience = "public", trust = "trusted", effects = ["egress"] }
"#;
    let config = GatewayConfig::from_toml(SPLIT_TOOLS, SPLIT_POLICY).expect("scenario parses");
    let mut session = Session::new(Arc::new(config));
    read_invoices(&mut session);
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &args(&[("to", "alex@finance-audit.com")]))
    else {
        panic!("expected a soft block");
    };

    let asked = std::sync::Mutex::new(Vec::new());
    let outcome = session
        .escalate("auditor needs it", |message| {
            asked.lock().unwrap().push(message);
            async { Some(true) }
        })
        .await;
    let Outcome::Granted { .. } = outcome else {
        panic!("expected the escalation to be granted, got {outcome:?}");
    };
    let asked = asked.into_inner().unwrap();
    assert_eq!(asked.len(), 2, "one prompt per authority, not per grant");
    assert!(asked[0].contains("audience-endorser") ^ asked[1].contains("audience-endorser"));
    assert!(asked[0].contains("effects-officer") ^ asked[1].contains("effects-officer"));
}

#[tokio::test]
async fn no_ruling_fails_closed_and_keeps_the_action_pending() {
    let mut session = session();
    read_invoices(&mut session);
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &email_args()) else {
        panic!("expected a soft block");
    };

    match session.escalate("auditor needs it", |_| async { None }).await {
        Outcome::EscalationUnavailable { .. } => {}
        other => panic!("expected no-ruling to fail closed, got {other:?}"),
    }
    match session.escalate("auditor needs it", |_| async { Some(true) }).await {
        Outcome::Granted { result, .. } => assert_eq!(result, "Email sent to alex@finance-audit.com."),
        other => panic!("expected the retried escalation to succeed, got {other:?}"),
    }
}

#[test]
fn executor_failure_closes_the_receipt() {
    const OPTIONAL_TOOLS: &str = r#"
[[tool]]
name = "greet"
description = "Greet someone."
result = "Hello {name}."

[[tool.arg]]
name = "name"
"#;
    const OPTIONAL_POLICY: &str = r#"
[[tool]]
name = "greet"
requires = {}
output = { audience = "public", trust = "trusted" }
"#;
    let config = GatewayConfig::from_toml(OPTIONAL_TOOLS, OPTIONAL_POLICY).expect("scenario parses");
    let mut session = Session::new(Arc::new(config));
    match session.call_tool("greet", &args(&[])) {
        Outcome::ExecutorFailed { .. } => {}
        other => panic!("expected the render to fail after release, got {other:?}"),
    }
    match session.call_tool("greet", &args(&[("name", "alice")])) {
        Outcome::Executed { result, .. } => assert_eq!(result, "Hello alice."),
        other => panic!("expected the next call to execute, got {other:?}"),
    }
}

#[test]
fn wire_validation_fails_closed() {
    let mut session = session();
    match session.call_tool("send_email", &args(&[("subject", "no recipient")])) {
        Outcome::BadArguments { .. } => {}
        other => panic!("expected missing required arg to be rejected, got {other:?}"),
    }
    match session.call_tool("send_email", &args(&[("to", "bob@archestra.ai"), ("cc", "x")])) {
        Outcome::BadArguments { .. } => {}
        other => panic!("expected undeclared arg to be rejected, got {other:?}"),
    }
    let mut non_string = args(&[]);
    non_string.insert("to".into(), serde_json::json!(42));
    match session.call_tool("send_email", &non_string) {
        Outcome::BadArguments { .. } => {}
        other => panic!("expected non-string arg to be rejected, got {other:?}"),
    }
    match session.call_tool("rm_rf", &args(&[])) {
        Outcome::UnknownTool { .. } => {}
        other => panic!("expected an unknown tool to be rejected, got {other:?}"),
    }
}

#[test]
fn config_rejects_bad_policies() {
    const SEND_TOOL: &str = r#"
[[tool]]
name = "send"
description = "d"
result = "r"

[[tool.arg]]
name = "to"
"#;
    assert!(matches!(
        GatewayConfig::from_toml(
            "[[tool]]\nname = \"x\"\ndescription = \"d\"\nresult = \"r\"\ntypo = 1",
            ""
        ),
        Err(ConfigError::Parse(_))
    ));
    assert!(matches!(
        GatewayConfig::from_toml(SEND_TOOL, "[[tool]]\nname = \"send\"\ntypo = 1"),
        Err(ConfigError::Contracts(_))
    ));
    let bad_recipients_policy = r#"
[[tool]]
name = "send"
requires = { audience = "$.args.too" }
"#;
    assert!(matches!(
        GatewayConfig::from_toml(SEND_TOOL, bad_recipients_policy),
        Err(ConfigError::UnknownRecipientsArg { tool, arg }) if tool == "send" && arg == "too"
    ));
    assert!(matches!(
        GatewayConfig::from_toml(SEND_TOOL, "[[tool]]\nname = \"ghost\"\nrequires = {}"),
        Err(ConfigError::ContractWithoutTool(tool)) if tool == "ghost"
    ));
    let webhook_policy = "[[authority]]\nname = \"remote\"\nrule = \"escalate\"\nmay_release_control = true\n\
                          webhook = { url = \"https://approvals.example/rule\" }";
    assert!(matches!(
        GatewayConfig::from_toml("", webhook_policy),
        Err(ConfigError::WebhookAuthority(name)) if name == "remote"
    ));
    let transformer_policy = "[[transformer]]\nname = \"pii-redactor\"\nbuiltin = \"redact-email\"\n\
                              output = { trust = \"trusted\", audience = \"public\" }";
    assert!(matches!(
        GatewayConfig::from_toml("", transformer_policy),
        Err(ConfigError::Transformer(name)) if name == "pii-redactor"
    ));
    let reserved = r#"
[[tool]]
name = "appa__escalate"
description = "d"
result = "r"
"#;
    assert!(matches!(
        GatewayConfig::from_toml(reserved, ""),
        Err(ConfigError::ReservedToolName)
    ));
    assert!(matches!(
        GatewayConfig::from_toml("", "[[tool]]\nname = \"appa__escalate\"\nrequires = {}"),
        Err(ConfigError::ReservedContractName)
    ));
    let duplicated = r#"
[[tool]]
name = "x"
description = "d"
result = "r"

[[tool]]
name = "x"
description = "d"
result = "r"
"#;
    assert!(matches!(
        GatewayConfig::from_toml(duplicated, ""),
        Err(ConfigError::DuplicateTool(tool)) if tool == "x"
    ));
    let unknown_placeholder = r#"
[[tool]]
name = "greet"
description = "d"
result = "Hello {nope}."
"#;
    assert!(matches!(
        GatewayConfig::from_toml(unknown_placeholder, ""),
        Err(ConfigError::BadResultTemplate { .. })
    ));
    let unclosed = r#"
[[tool]]
name = "greet"
description = "d"
result = "Hello {name."

[[tool.arg]]
name = "name"
"#;
    assert!(matches!(
        GatewayConfig::from_toml(unclosed, ""),
        Err(ConfigError::BadResultTemplate { .. })
    ));
}

#[tokio::test]
async fn checked_in_config_preserves_the_demo_scenario() {
    let policy =
        appa_contracts::Contracts::from_toml(include_str!("../gateway-policy.toml")).expect("checked-in policy parses");
    let send = policy
        .contracts
        .iter()
        .find(|c| c.name.as_str() == "send_email")
        .expect("send_email is contracted");
    assert_eq!(send.output_label.trust, appa_core::Trust::TRUSTED);
    assert_eq!(send.output_label.audience, appa_core::Audience::PUBLIC);
    assert_eq!(
        send.effects,
        appa_core::Effects::declared([appa_core::Effect::Egress]),
        "send_email's egress moved from contract level into output.effects in the migration"
    );

    let config = GatewayConfig::from_toml(include_str!("../gateway.toml"), include_str!("../gateway-policy.toml"))
        .expect("checked-in config parses");
    let mut session = Session::new(Arc::new(config));

    match session.call_tool("invoices_list", &args(&[])) {
        Outcome::Executed { .. } => {}
        other => panic!("expected the read to execute, got {other:?}"),
    }
    let Outcome::SoftBlocked { .. } = session.call_tool("send_email", &email_args()) else {
        panic!("expected the out-of-audience send to soft-block");
    };
    match session
        .escalate("auditor needs the summary", |_| async { Some(true) })
        .await
    {
        Outcome::Granted { result, .. } => assert_eq!(result, "Email sent to alex@finance-audit.com."),
        other => panic!("expected the escalation to be granted, got {other:?}"),
    }

    match session.call_tool("send_email", &args(&[("to", "bob@archestra.ai"), ("subject", "fyi")])) {
        Outcome::Executed { result, .. } => assert_eq!(result, "Email sent to bob@archestra.ai."),
        other => panic!("expected the in-audience follow-up send to execute, got {other:?}"),
    }
}

#[test]
fn respond_emits_exactly_the_permitted_bytes() {
    let mut session = session();
    let Outcome::Executed { .. } = session.call_tool("invoices_list", &args(&[])) else {
        panic!("the read should execute");
    };
    let answer = "47 invoices this quarter.";
    let Outcome::Responded { rendered } = session.respond(answer) else {
        panic!("a readable answer should emit");
    };
    assert_eq!(rendered, format!("{answer:?}"));
}

#[test]
fn respond_blocks_a_leak_and_delivers_nothing() {
    let narrow_readers = SCENARIO_TOOLS.replace(
        r#"readers = ["alice@archestra.ai", "bob@archestra.ai"]"#,
        r#"readers = ["mallory@example.com"]"#,
    );
    let config = GatewayConfig::from_toml(&narrow_readers, SCENARIO_POLICY).expect("scenario parses");
    let mut session = Session::new(Arc::new(config));
    let Outcome::Executed { .. } = session.call_tool("invoices_list", &args(&[])) else {
        panic!("the read should execute");
    };
    let outcome = session.respond("the totals are $1,248,000");
    assert!(
        matches!(outcome, Outcome::ResponseBlocked { .. }),
        "expected a blocked response, got {outcome:?}"
    );
}

#[tokio::test]
async fn attention_demand_is_confirmed_through_elicitation_once() {
    let mut session = session();
    let Outcome::SoftBlocked { .. } = session.call_tool("db_drop", &args(&[])) else {
        panic!("expected the confirmation demand to soft-block");
    };
    let Outcome::Granted { result, .. } = session.escalate("operator confirms", |_| async { Some(true) }).await else {
        panic!("expected the confirmation to grant");
    };
    assert_eq!(result, "Database dropped.");
    match session.call_tool("db_drop", &args(&[])) {
        Outcome::SoftBlocked { .. } => {}
        other => panic!("expected a fresh soft block, got {other:?}"),
    }
}

#[tokio::test]
async fn attention_demand_denied_stays_blocked() {
    let mut session = session();
    let Outcome::SoftBlocked { .. } = session.call_tool("db_drop", &args(&[])) else {
        panic!("expected the confirmation demand to soft-block");
    };
    match session.escalate("operator declines", |_| async { Some(false) }).await {
        Outcome::Denied { .. } => {}
        other => panic!("expected the denial to report Denied, got {other:?}"),
    }
}

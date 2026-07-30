use std::collections::BTreeMap;
use std::sync::Arc;

use appa_engine::fact::{BoundaryKind, Fact, ReturnPolicy};
use appa_engine::label::{Audience, Dim, Label, ReaderId, Trust};
use appa_engine::names::{AuthorityName, CastName, SanitizerName};
use appa_engine::projection::Projection;
use appa_engine::value::ToolName;
use appa_runtime::config::ConfigError;
use appa_runtime::external::{AuthorityBackend, CastBackend, SanitizerBackend};
use appa_runtime::store::TenantId;
use appa_runtime::tool::{BuiltinTool, EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT, ToolBackend};
use appa_runtime::{BeginTurnError, Config, InitError, Mediator, WireTool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const MIXED_TOOL_CONFIG: &str = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "local"

[[tool]]
name = "remote"
[tool.implementation.http]
url = "http://tools.internal/remote"
timeout_ms = 5000
"#;

fn config(source: &str) -> Config {
    Config::from_toml_str(source).expect("config parses")
}

fn builtin(body: &str) -> ToolBackend {
    ToolBackend::Builtin(BuiltinTool::Echo(body.to_string()))
}

#[test]
fn assembly_requires_exact_tool_backend_coverage() {
    let mut builtins = BTreeMap::new();
    builtins.insert(ToolName::new("local"), BuiltinTool::Echo("ok".to_string()));
    let mediator = Mediator::new(config(MIXED_TOOL_CONFIG), builtins).expect("exact builtins assemble");
    assert!(matches!(
        mediator.tool_backend(&ToolName::new("local")),
        Some(ToolBackend::Builtin(_))
    ));
    assert!(matches!(
        mediator.tool_backend(&ToolName::new("remote")),
        Some(ToolBackend::Http(_))
    ));

    let mut supplied = BTreeMap::new();
    supplied.insert(ToolName::new("local"), builtin("host"));
    let mediator =
        Mediator::with_tool_backends(config(MIXED_TOOL_CONFIG), supplied).expect("exact concrete backends assemble");
    assert!(matches!(
        mediator.tool_backend(&ToolName::new("local")),
        Some(ToolBackend::Builtin(_))
    ));

    assert!(matches!(
        Mediator::with_tool_backends(config(MIXED_TOOL_CONFIG), BTreeMap::new()),
        Err(InitError::UncoveredTool(name)) if name == "local"
    ));

    let mut extra = BTreeMap::new();
    extra.insert(ToolName::new("local"), builtin("ok"));
    extra.insert(ToolName::new("unknown"), builtin("extra"));
    assert!(matches!(
        Mediator::with_tool_backends(config(MIXED_TOOL_CONFIG), extra),
        Err(InitError::UnexpectedSuppliedBackend(name)) if name == "unknown"
    ));

    let mut duplicate_http = BTreeMap::new();
    duplicate_http.insert(ToolName::new("local"), builtin("ok"));
    duplicate_http.insert(ToolName::new("remote"), builtin("override"));
    assert!(matches!(
        Mediator::with_tool_backends(config(MIXED_TOOL_CONFIG), duplicate_http),
        Err(InitError::UnexpectedSuppliedBackend(name)) if name == "remote"
    ));
}

#[test]
fn assembly_builds_every_configured_backend_family() {
    let mediator = Mediator::new(
        config(
            r#"
version = 1

[[tool]]
name = "remote"
[tool.implementation.http]
url = "http://tools.internal/remote"

[[authority]]
name = "approver"
[authority.mandate]
can_waive = ["egress"]
[authority.implementation]
builtin = "approve"

[[sanitizer]]
name = "redact"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"

[[cast]]
name = "classifier"
resolver = { url = "http://classify.internal/resolve", may_cast = { trust = ["suspicious"] } }
"#,
        ),
        BTreeMap::new(),
    )
    .expect("configured backends assemble");

    assert!(matches!(
        mediator.tool_backend(&ToolName::new("remote")),
        Some(ToolBackend::Http(_))
    ));
    assert!(matches!(
        mediator.authority_backend(&AuthorityName::new("approver")),
        Some(AuthorityBackend::Builtin(_))
    ));
    assert!(matches!(
        mediator.sanitizer_backend(&SanitizerName::new("redact")),
        Some(SanitizerBackend::Builtin(_))
    ));
    assert!(matches!(
        mediator.cast_backend(&CastName::new("classifier")),
        Some(CastBackend::Http { .. })
    ));
}

#[test]
fn every_reserved_name_is_rejected_as_a_policy_tool() {
    for reserved in [EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT] {
        let source = format!(
            r#"
version = 1
[[tool]]
name = "{reserved}"
[tool.implementation.http]
url = "http://tools.internal/reserved"
"#
        );
        assert!(matches!(
            Mediator::new(config(&source), BTreeMap::new()),
            Err(InitError::ReservedToolConflict(name)) if name == reserved
        ));
    }
}

#[test]
fn reserved_tool_schemas_are_strict_json_schema_objects() {
    let mediator = Mediator::new(config("version = 1\n"), BTreeMap::new()).expect("empty policy assembles");
    let tools = mediator.advertised_tools(true, true);

    assert_eq!(
        parameters(&tools, EXECUTE_REMEDY_PLAN),
        &json!({
            "type": "object",
            "properties": { "plan_id": { "type": "string" } },
            "required": ["plan_id"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        parameters(&tools, FORK),
        &json!({
            "type": "object",
            "properties": { "task": { "type": "string", "minLength": 1 } },
            "required": ["task"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        parameters(&tools, SUBMIT_RESULT),
        &json!({
            "type": "object",
            "properties": { "value": { "type": ["string", "null"] } },
            "required": ["value"],
            "additionalProperties": false
        })
    );
}

#[test]
fn advertised_surfaces_are_deterministic_and_role_specific() {
    let mediator = Mediator::new(
        config(
            r#"
version = 1
[[tool]]
name = "zeta"
[tool.implementation.http]
url = "http://tools.internal/zeta"
[[tool]]
name = "alpha"
[tool.implementation.http]
url = "http://tools.internal/alpha"
"#,
        ),
        BTreeMap::new(),
    )
    .expect("HTTP tools assemble");

    assert_eq!(
        names(mediator.advertised_tools(false, false)),
        ["alpha", "zeta", EXECUTE_REMEDY_PLAN]
    );
    assert_eq!(
        names(mediator.advertised_tools(false, true)),
        ["alpha", "zeta", EXECUTE_REMEDY_PLAN, FORK]
    );
    assert_eq!(
        names(mediator.advertised_tools(true, false)),
        ["alpha", "zeta", EXECUTE_REMEDY_PLAN, SUBMIT_RESULT]
    );
    assert_eq!(
        names(mediator.advertised_tools(true, true)),
        ["alpha", "zeta", EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT]
    );
    assert!(mediator.advertised_tools(false, false)[0].function.parameters.is_none());
}

#[test]
fn declared_tool_parameters_are_advertised_verbatim() {
    let mediator = Mediator::new(
        config(
            r#"
version = 1
[[tool]]
name = "read_hr"
parameters = { type = "object", properties = { file = { type = "string" } }, required = ["file"], additionalProperties = false }
[tool.implementation.http]
url = "http://tools.internal/read_hr"
[[tool]]
name = "bare"
[tool.implementation.http]
url = "http://tools.internal/bare"
"#,
        ),
        BTreeMap::new(),
    )
    .expect("HTTP tools assemble");

    let tools = mediator.advertised_tools(false, false);
    assert_eq!(
        parameters(&tools, "read_hr"),
        &json!({
            "type": "object",
            "properties": { "file": { "type": "string" } },
            "required": ["file"],
            "additionalProperties": false
        })
    );
    let bare = tools
        .iter()
        .find(|tool| tool.function.name == "bare")
        .expect("bare is advertised");
    assert!(bare.function.parameters.is_none());
}

#[test]
fn non_object_tool_parameters_are_refused_at_load() {
    let source = r#"
version = 1
[[tool]]
name = "broken"
parameters = "not a schema"
[tool.implementation.http]
url = "http://tools.internal/broken"
"#;
    assert!(matches!(
        Config::from_toml_str(source),
        Err(ConfigError::ToolParametersNotAnObject { tool }) if tool == "broken"
    ));
}

#[tokio::test]
async fn fork_records_metadata_and_the_exact_parent_label_seed() {
    let mediator = Arc::new(
        Mediator::new(
            config(
                r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[boundary]
trust = "suspicious"
audience = { exactly = ["internal"] }

[[sanitizer]]
name = "redact"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"

[child]
return_sanitizer = "redact"
"#,
            ),
            BTreeMap::new(),
        )
        .expect("child policy assembles"),
    );
    let tenant = TenantId::new("host-a");
    let parent = mediator.create_session(tenant.clone());
    let parent_label = Label::new(
        Dim::Known(Trust::new(0)),
        Dim::Known(Audience::restricted([ReaderId::new("internal")])),
    );
    let turn = mediator
        .begin_turn(tenant.clone(), parent.clone(), "parent value", CancellationToken::new())
        .await
        .expect("parent turn begins");
    drop(turn);

    let child = mediator.fork_session(&tenant, &parent).expect("fork succeeds");
    assert!(!mediator.is_child(&tenant, &parent).expect("root exists"));
    assert!(mediator.is_child(&tenant, &child).expect("child exists"));
    assert_eq!(
        mediator.parent_of(&tenant, &child).expect("metadata exists"),
        Some(parent.clone())
    );

    let (facts, revision) = mediator.snapshot(&tenant, &parent).expect("family snapshot");
    let projection = Projection::build(&facts, revision);
    assert_eq!(projection.view(&child).current_label(), parent_label);
    let fork = facts
        .iter()
        .find_map(|fact| match fact {
            Fact::Boundary {
                trajectory,
                kind:
                    BoundaryKind::Fork {
                        parent,
                        seed,
                        return_policy,
                    },
            } if trajectory == &child => Some((parent, seed, return_policy)),
            _ => None,
        })
        .expect("child has a fork boundary");
    assert_eq!(fork.0, &parent);
    assert_eq!(fork.1, &parent_label);
    assert_eq!(fork.2, &ReturnPolicy::Sanitized(SanitizerName::new("redact")));
}

#[tokio::test]
async fn a_reserved_child_cannot_cross_between_mediators_with_colliding_ids() {
    let first = Arc::new(Mediator::new(config("version = 1\n"), BTreeMap::new()).unwrap());
    let second = Arc::new(Mediator::new(config("version = 1\n"), BTreeMap::new()).unwrap());
    let tenant = TenantId::new("tenant");
    let first_parent = first.create_session(tenant.clone());
    let second_parent = second.create_session(tenant.clone());
    let first_child = first.fork_session_reserved(&tenant, &first_parent).unwrap();
    let second_child = second.fork_session_reserved(&tenant, &second_parent).unwrap();

    assert_eq!(first_child.session(), second_child.session());
    assert!(matches!(
        second.begin_forked_turn(tenant.clone(), first_child, "wrong mediator", CancellationToken::new()),
        Err(BeginTurnError::ForeignFork)
    ));

    let turn = second
        .begin_forked_turn(tenant, second_child, "right mediator", CancellationToken::new())
        .expect("the originating mediator accepts its reservation");
    drop(turn);
}

fn parameters<'a>(tools: &'a [WireTool], name: &str) -> &'a serde_json::Value {
    tools
        .iter()
        .find(|tool| tool.function.name == name)
        .and_then(|tool| tool.function.parameters.as_ref())
        .expect("reserved tool has parameters")
}

fn names(tools: Vec<WireTool>) -> Vec<String> {
    tools.into_iter().map(|tool| tool.function.name).collect()
}

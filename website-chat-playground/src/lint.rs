//! The demo box's own gate on submitted policies.

use std::collections::BTreeSet;

use appa_policy::{Config, ConfigError};

use crate::params::InjectError;
use crate::systems::System;
use crate::world::merge_policy;

/// Why a submitted policy was refused. `Load` carries the real loader's own
/// message — it goes to the editor verbatim.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    Inject(#[from] InjectError),
    #[error("{0}")]
    Load(#[from] ConfigError),
    #[error(
        "sanitizer {name:?} declares no `hint`: in this playground every derivation is produced by \
         running your model, and the hint is the instruction it runs under — without one this \
         sanitizer could never derive anything"
    )]
    SanitizerWithoutHint { name: String },
    #[error(
        "dynamic resolver {name:?} declares a builtin: this playground answers every resolver \
         through its own hosted directory, and a builtin would run on the demo host itself"
    )]
    BuiltinResolver { name: String },
}

#[derive(Debug)]
pub struct CheckedPolicy {
    pub config: Config,
    pub tool_count: usize,
    pub defaulted: Vec<String>,
    pub dropped: Vec<String>,
    /// The composed TOML the config was loaded from, kept so session
    /// creation can hand the runtime the same policy it validated.
    pub merged_toml: String,
}

/// Compose the editor's policy with the enabled systems, then run the result
/// through the real loader and this host's own rule.
pub fn check_policy(policy: &str, enabled: &BTreeSet<System>) -> Result<CheckedPolicy, PolicyError> {
    let merged = merge_policy(policy, enabled)?;
    let config = Config::from_toml_str(&merged.toml)?;

    for sanitizer in &config.registry_config().sanitizers {
        if sanitizer.hint.is_none() {
            return Err(PolicyError::SanitizerWithoutHint {
                name: sanitizer.name.as_str().to_string(),
            });
        }
    }

    // A visitor's policy never selects an implementation on this host: a builtin resolver
    // would start a claude process under the demo service's own account.
    if let Some((name, _)) = config.dynamic_resolver_builtins().next() {
        return Err(PolicyError::BuiltinResolver {
            name: name.as_str().to_string(),
        });
    }

    let tool_count = config.registry_config().tools.len();
    Ok(CheckedPolicy {
        config,
        tool_count,
        defaulted: merged.defaulted,
        dropped: merged.dropped,
        merged_toml: merged.toml,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use appa_engine::authority::DeclaredTransition;
    use appa_engine::contract::AudienceDelta;
    use appa_engine::groups::DeclaredAudience;
    use appa_engine::label::ReaderId;
    use appa_engine::value::ToolName;

    fn systems(list: &str) -> BTreeSet<System> {
        list.split(',')
            .map(|name| System::parse(name.trim()).unwrap())
            .collect()
    }

    #[test]
    fn shipped_preset_loads_clean() {
        let all: BTreeSet<System> = System::ALL.into_iter().collect();
        let checked = check_policy(include_str!("../policies/default.toml"), &all).expect("preset loads");
        assert_eq!(checked.tool_count, 8);
        assert!(checked.dropped.is_empty(), "the preset only names available tools");
        assert!(checked.defaulted.is_empty(), "the preset contracts every tool");
        let invoices = checked
            .config
            .registry()
            .tool(&ToolName::new("list_invoices"))
            .expect("the finance system provides list_invoices");
        assert!(matches!(
            invoices.delta.as_ref().and_then(|delta| delta.audience.as_ref()),
            Some(AudienceDelta::Static(audience))
                if *audience == DeclaredAudience::restricted([
                    ReaderId::new("cfo@corp.example"),
                    ReaderId::new("ap-lead@corp.example"),
                ])
        ));
        let email = checked
            .config
            .registry()
            .tool(&ToolName::new("send_email"))
            .expect("the email system provides send_email");
        assert!(matches!(
            email.uses.as_slice(),
            [uses]
                if uses.resolver.as_str() == "email-recipient-readers"
                    && uses.inputs.get("to")
                        == Some(&appa_engine::contract::ToolCallSource::argument("to").expect("a plain name is a source"))
        ));
        let sanitizers = &checked.config.registry_config().sanitizers;
        assert_eq!(sanitizers.len(), 2);
        assert!(sanitizers.iter().all(|sanitizer| sanitizer.hint.is_some()));
        assert!(
            sanitizers
                .iter()
                .any(|sanitizer| matches!(sanitizer.transition, DeclaredTransition::Audience { .. }))
        );
        assert!(
            sanitizers
                .iter()
                .any(|sanitizer| matches!(sanitizer.transition, DeclaredTransition::Trust { .. }))
        );
    }

    #[test]
    fn a_builtin_resolver_declaration_is_refused() {
        let policy = r#"
version = 1
[[dynamic_resolver]]
name = "classify"
builtin = "claude-code"
returns = ["delta.trust"]
[[tool]]
name = "list_customers"
description = "Lists the customer records."
uses = [{ resolver = "classify" }]
delta = { trust = "resolver.classify.trust" }
"#;
        assert!(matches!(
            check_policy(policy, &systems("crm")),
            Err(PolicyError::BuiltinResolver { name }) if name == "classify"
        ));
    }

    #[test]
    fn a_sanitizer_without_a_hint_is_refused() {
        let policy = r#"
version = 1

[[sanitizer]]
name = "digest"
on = ["tool_output"]
mandate.trust = { from = "suspicious", to = "trusted" }
"#;
        assert!(matches!(
            check_policy(policy, &systems("crm")),
            Err(PolicyError::SanitizerWithoutHint { .. })
        ));
    }

    #[test]
    fn shipped_preset_survives_disabled_systems() {
        let checked =
            check_policy(include_str!("../policies/default.toml"), &systems("crm,github")).expect("preset loads");
        assert_eq!(checked.tool_count, 4);
        assert_eq!(
            checked.dropped,
            vec!["send_email", "list_invoices", "make_transfer", "list_recordings"]
        );
    }

    #[test]
    fn a_visitor_can_bind_nothing_to_an_endpoint_of_their_own() {
        let bindings = [
            (
                "tool",
                r#"
version = 1

[[tool]]
name = "list_customers"
delta = {}
implementation = { http = { url = "http://169.254.169.254/latest/meta-data" } }
"#,
                systems("crm"),
            ),
            (
                "authority",
                r#"
version = 1

[[tool]]
name = "create_issue"
requires = { trust = "trusted" }
delta = {}

[[authority]]
name = "release-desk"
mandate = { can_cover_trust_to = "trusted" }
implementation = { resolver = { url = "http://10.0.0.1/exfil" } }
"#,
                systems("github"),
            ),
            (
                "dynamic resolver",
                r#"
version = 1

[[dynamic_resolver]]
name = "directory"
resolver = { url = "http://169.254.169.254/readers" }
inputs = ["to"]
returns = ["requires.audience"]

[[tool]]
name = "send_email"
uses = [{ resolver = "directory", inputs = { to = "$tool_call.arguments.to" } }]
requires = { audience = "resolver.directory.audience" }
delta = {}
"#,
                systems("email"),
            ),
            (
                "sanitizer",
                r#"
version = 1

[[tool]]
name = "list_customers"
delta = { audience = { exactly = ["crm"] } }

[[sanitizer]]
name = "leaky"
on = ["tool_output"]
hint = "clean it"
implementation = { resolver = { url = "http://internal.cluster.local/" } }
mandate = { audience = { from = { includes = ["crm"] }, to = { exactly = ["public"] } } }
"#,
                systems("crm"),
            ),
        ];
        for (kind, policy, enabled) in bindings {
            assert!(
                matches!(check_policy(policy, &enabled), Err(PolicyError::Load(_))),
                "a visitor-named {kind} endpoint must be refused by the loader",
            );
        }
    }

    #[test]
    fn contracts_for_a_foreign_world_are_all_dropped() {
        let checked = check_policy(
            r#"
version = 1

[[tool]]
name = "read_hr"
delta = {}

[[tool]]
name = "send_email"
delta = {}
"#,
            &systems("crm,github"),
        )
        .expect("loads");
        assert_eq!(checked.dropped, vec!["read_hr", "send_email"]);
        assert_eq!(checked.tool_count, 4, "only the enabled systems' tools remain");
    }

    #[test]
    fn loader_errors_pass_through_verbatim() {
        let error = check_policy("version = 1\ntrust_chain = [\"a\", \"a\"]\n", &systems("crm")).unwrap_err();
        assert!(matches!(error, PolicyError::Load(_)));
    }
}

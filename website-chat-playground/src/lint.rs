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
    Load(Box<ConfigError>),
    #[error(
        "sanitizer {name:?} declares no `hint`: in this playground every derivation is produced by \
         running your model, and the hint is the instruction it runs under — without one this \
         sanitizer could never derive anything"
    )]
    SanitizerWithoutHint { name: String },
    #[error(
        "annotator {name:?} declares a builtin: this playground answers every annotator \
         through its own hosted directory, and a builtin would run on the demo host itself"
    )]
    BuiltinAnnotator { name: String },
}

impl From<ConfigError> for PolicyError {
    fn from(error: ConfigError) -> Self {
        PolicyError::Load(Box::new(error))
    }
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

    // A visitor's policy never selects an implementation on this host: a builtin annotator
    // would start a claude process under the demo service's own account.
    if let Some((name, _)) = config.annotators().find(|(_, binding)| binding.builtin.is_some()) {
        return Err(PolicyError::BuiltinAnnotator {
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
    use appa_engine::label::DeclaredAudience;
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
            .variants(&ToolName::new("list_invoices"))
            .next()
            .expect("the finance system provides list_invoices");
        let appa_engine::contract::ToolDeclaration::Declared(invoices) = invoices else {
            panic!("list_invoices is a declared contract");
        };
        assert_eq!(
            invoices.delta.audience.as_ref(),
            Some(&DeclaredAudience::restricted([
                ReaderId::new("cfo@corp.example"),
                ReaderId::new("ap-lead@corp.example"),
            ]))
        );
        let email = checked
            .config
            .registry()
            .variants(&ToolName::new("send_email"))
            .next()
            .expect("the email system provides send_email");
        assert!(matches!(
            email,
            appa_engine::contract::ToolDeclaration::Annotated { annotator, .. }
                if annotator.as_str() == "email-recipient-readers"
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
    fn a_builtin_annotator_declaration_is_refused() {
        for builtin in appa_policy::AnnotatorBuiltin::ALL {
            let policy = format!(
                r#"
version = 2
[[annotator]]
name = "classify"
builtin = "{}"
[[tool]]
name = "list_customers"
description = "Lists the customer records."
annotator = "classify"
"#,
                builtin.wire_name()
            );
            assert!(matches!(
                check_policy(&policy, &systems("crm")),
                Err(PolicyError::BuiltinAnnotator { name }) if name == "classify"
            ));
        }
    }

    #[test]
    fn a_sanitizer_without_a_hint_is_refused() {
        let policy = r#"
version = 2

[[sanitizer]]
name = "digest"
on = ["tool_output"]
permits.trust = { from = "suspicious", to = "trusted" }
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
version = 2

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
version = 2

[[tool]]
name = "create_issue"
requires = { trust = "trusted" }
delta = {}

[[authority]]
name = "release-desk"
permits = { trust_below = "trusted" }
implementation = { resolver = { url = "http://10.0.0.1/exfil" } }
"#,
                systems("github"),
            ),
            (
                "annotator",
                r#"
version = 2

[[annotator]]
name = "directory"
implementation = { resolver = { url = "http://169.254.169.254/readers" } }
inputs = { to = "$tool_call.arguments.to" }

[[tool]]
name = "send_email"
annotator = "directory"
"#,
                systems("email"),
            ),
            (
                "sanitizer",
                r#"
version = 2

[[tool]]
name = "list_customers"
delta = { audience = ["crm"] }

[[sanitizer]]
name = "leaky"
on = ["tool_output"]
hint = "clean it"
implementation = { resolver = { url = "http://internal.cluster.local/" } }
permits = { audience = { from = ["crm"], to = ["public"] } }
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
version = 2

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
        let error = check_policy("version = 2\ntrust_chain = [\"a\", \"a\"]\n", &systems("crm")).unwrap_err();
        assert!(matches!(error, PolicyError::Load(_)));
    }
}

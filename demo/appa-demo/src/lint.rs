//! The demo box's own gate on submitted policies: service-hosted implementations only.

use std::collections::BTreeSet;

use crate::systems::System;
use appa_runtime::Config;
use appa_runtime::config::{AuthorityImpl, CastImpl, ConfigError, SanitizerImpl};

use appa_runtime::external::BuiltinSanitizer;

use crate::params::InjectError;
use crate::world::{DYNAMIC_RESOLVER_PLACEHOLDER, merge_policy};

/// Why a submitted policy was refused. `Load` carries the real loader's own
/// message — it goes to the editor verbatim.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    Inject(#[from] InjectError),
    #[error("{0}")]
    Load(#[from] ConfigError),
    #[error(
        "the demo runs service-hosted implementations only: {kind} {name:?} names an HTTP endpoint, \
         which this box will not call on a visitor's behalf"
    )]
    NonBuiltinImplementation { kind: &'static str, name: String },
    #[error(
        "sanitizer {name:?} is `builtin = \"hosted\"` but declares no `hint`: in this playground the \
         hint is the instruction the derivation runs under, so a hosted sanitizer without one could \
         never derive anything"
    )]
    HostedWithoutHint { name: String },
}

#[derive(Debug)]
pub struct CheckedPolicy {
    pub config: Config,
    pub tool_count: usize,
    pub defaulted: Vec<String>,
    pub dropped: Vec<String>,
    /// The composed TOML the config was loaded from, kept so session creation
    /// can rebind `hitl` authorities to the session's approval desk.
    pub merged_toml: String,
}

/// Compose the editor's policy with the enabled systems, then run the result
/// through the real loader and the builtin-only lint.
pub fn check_policy(policy: &str, enabled: &BTreeSet<System>) -> Result<CheckedPolicy, PolicyError> {
    let merged = merge_policy(policy, enabled)?;
    let config = Config::from_toml_str(&merged.toml)?;

    for tool in &config.registry_config().tools {
        if config.tool_impl(&tool.name).is_some() {
            return Err(PolicyError::NonBuiltinImplementation {
                kind: "tool",
                name: tool.name.as_str().to_string(),
            });
        }
    }
    for (name, implementation) in config.dynamic_resolvers() {
        if implementation.url != DYNAMIC_RESOLVER_PLACEHOLDER {
            return Err(PolicyError::NonBuiltinImplementation {
                kind: "dynamic resolver",
                name: name.as_str().to_string(),
            });
        }
    }
    for authority in &config.registry_config().authorities {
        if let Some(AuthorityImpl::HttpResolver { .. }) = config.authority_impl(&authority.name) {
            return Err(PolicyError::NonBuiltinImplementation {
                kind: "authority",
                name: authority.name.as_str().to_string(),
            });
        }
    }
    for sanitizer in &config.registry_config().sanitizers {
        match config.sanitizer_impl(&sanitizer.name) {
            Some(SanitizerImpl::HttpResolver { .. }) => {
                return Err(PolicyError::NonBuiltinImplementation {
                    kind: "sanitizer",
                    name: sanitizer.name.as_str().to_string(),
                });
            }
            // A hosted derivation runs under its hint. Without one there is no
            // instruction to run, so every plan the sanitizer appears in would be
            // offered and then fail — refuse it here instead, where the message
            // reaches the editor.
            Some(SanitizerImpl::Builtin(BuiltinSanitizer::Hosted)) if sanitizer.hint.is_none() => {
                return Err(PolicyError::HostedWithoutHint {
                    name: sanitizer.name.as_str().to_string(),
                });
            }
            _ => {}
        }
    }
    for cast in &config.registry_config().casts {
        if let Some(CastImpl::HttpResolver { .. }) = config.cast_impl(&cast.name) {
            return Err(PolicyError::NonBuiltinImplementation {
                kind: "cast",
                name: cast.name.as_str().to_string(),
            });
        }
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

    use appa_engine::authority::Transition;
    use appa_engine::contract::{AudienceDelta, AudienceRequirement, RecipientSpec};
    use appa_engine::label::{Audience, ReaderId};
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
        let dynamic_resolvers = checked.config.dynamic_resolvers();
        assert_eq!(dynamic_resolvers.len(), 1);
        assert!(
            dynamic_resolvers
                .values()
                .all(|implementation| implementation.url == DYNAMIC_RESOLVER_PLACEHOLDER)
        );
        let invoices = checked
            .config
            .registry()
            .tool(&ToolName::new("list_invoices"))
            .expect("the finance system provides list_invoices");
        assert!(matches!(
            invoices.delta.as_ref().and_then(|delta| delta.audience.as_ref()),
            Some(AudienceDelta::Static(audience))
                if *audience == Audience::restricted([
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
            email.requires.label.audience.as_slice(),
            [AudienceRequirement::Includes(RecipientSpec::Dynamic(binding))]
                if binding.resolver.as_str() == "email-recipient-readers" && binding.argument == "to"
        ));
        let sanitizers = &checked.config.registry_config().sanitizers;
        assert_eq!(sanitizers.len(), 1);
        assert!(sanitizers.iter().all(|sanitizer| sanitizer.hint.is_some()));
        assert!(
            sanitizers
                .iter()
                .all(|sanitizer| matches!(sanitizer.transition, Transition::Audience { .. }))
        );
    }

    #[test]
    fn a_hosted_sanitizer_without_a_hint_is_refused() {
        let policy = r#"
version = 1

[[sanitizer]]
name = "digest"
on = ["tool_output"]
mandate.trust = { from = "suspicious", to = "trusted" }
implementation.builtin = "hosted"
"#;
        assert!(matches!(
            check_policy(policy, &systems("crm")),
            Err(PolicyError::HostedWithoutHint { .. })
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
    fn tool_http_implementation_is_refused() {
        let error = check_policy(
            r#"
version = 1

[[tool]]
name = "list_customers"
delta = {}
implementation = { http = { url = "http://169.254.169.254/latest/meta-data" } }
"#,
            &systems("crm"),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PolicyError::NonBuiltinImplementation { kind: "tool", .. }
        ));
    }

    #[test]
    fn http_authority_resolver_is_refused() {
        let error = check_policy(
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
            &systems("github"),
        )
        .unwrap_err();
        assert!(
            matches!(error, PolicyError::NonBuiltinImplementation { kind: "authority", .. }),
            "got: {error}"
        );
    }

    #[test]
    fn visitor_dynamic_resolver_is_refused() {
        let error = check_policy(
            r#"
version = 1

[[dynamic_resolver]]
name = "directory"
resolver = { url = "http://169.254.169.254/readers" }

[[tool]]
name = "send_email"
requires = { audience = { includes = { resolver = "directory", argument = "to" } } }
delta = {}
"#,
            &systems("email"),
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                PolicyError::NonBuiltinImplementation {
                    kind: "dynamic resolver",
                    ..
                }
            ),
            "got: {error}"
        );
    }

    #[test]
    fn http_sanitizer_resolver_is_refused() {
        let error = check_policy(
            r#"
version = 1

[[tool]]
name = "list_customers"
delta = { audience = { exactly = ["crm"] } }

[[sanitizer]]
name = "leaky"
on = ["tool_output"]
implementation = { resolver = { url = "http://internal.cluster.local/" } }
mandate = { audience = { from = { includes = ["crm"] }, to = { exactly = ["public"] } } }
"#,
            &systems("crm"),
        )
        .unwrap_err();
        assert!(
            matches!(error, PolicyError::NonBuiltinImplementation { kind: "sanitizer", .. }),
            "got: {error}"
        );
    }

    #[test]
    fn builtin_sanitizer_passes() {
        check_policy(
            r#"
version = 1

[[tool]]
name = "list_customers"
delta = { audience = { exactly = ["crm"] } }

[[sanitizer]]
name = "pii-redactor"
on = ["tool_output"]
implementation = { builtin = "redact-numbers" }
mandate = { audience = { from = { includes = ["crm"] }, to = { exactly = ["public"] } } }
"#,
            &systems("crm"),
        )
        .expect("builtin implementations are allowed");
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

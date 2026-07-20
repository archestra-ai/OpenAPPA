
use appa_contracts::{Contracts, ContractsError};
use appa_core::AuthorityMode;
use appa_edge::{ResolveError, WebhookResolver};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error(transparent)]
    Contracts(#[from] ContractsError),
    #[error(
        "authority `{0}` has rule = \"escalate\" but no webhook, which the proxy cannot serve; declare webhook = {{ url = \"…\" }} or remove it"
    )]
    EscalateWithoutWebhook(String),
    #[error("webhook resolver could not be built: {0}")]
    Resolver(#[from] ResolveError),
}

/// The runtime policy the proxy evaluates against. Built once at startup and
/// shared read-only across requests.
#[derive(Debug, Clone)]
pub struct Policy {
    pub upstream_base_url: String,
    pub contracts: Contracts,
    /// Routes each pending approval to the declared endpoint of the
    /// authority it names; built once from `contracts.endpoints`.
    pub resolver: WebhookResolver,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProxyConfig {
    upstream_base_url: String,
    #[serde(default)]
    contracts: toml::Table,
}

impl Policy {
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let raw: RawProxyConfig = toml::from_str(text)?;
        let contracts = Contracts::from_toml(&raw.contracts.to_string())?;
        if let Some(external) = contracts
            .authorities
            .iter()
            .find(|a| matches!(a.mode, AuthorityMode::External) && !contracts.endpoints.contains_key(&a.name))
        {
            return Err(ConfigError::EscalateWithoutWebhook(external.name.as_str().to_string()));
        }
        let resolver = WebhookResolver::new(contracts.endpoints.clone())?;
        Ok(Self {
            upstream_base_url: raw.upstream_base_url,
            contracts,
            resolver,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nests_contracts_under_their_own_table() {
        let p = Policy::from_toml(
            r#"
            upstream_base_url = "http://upstream.invalid"

            [contracts.trajectory]
            audience = ["operator"]

            [[contracts.tool]]
            name = "get_logs"
            output = { trust = "suspicious" }
            "#,
        )
        .unwrap();
        assert_eq!(p.upstream_base_url, "http://upstream.invalid");
        assert_eq!(
            p.contracts.trajectory_label.audience,
            appa_core::Audience::readers([appa_core::UserId::new("operator")])
        );
        assert_eq!(p.contracts.contracts.len(), 1);
    }

    #[test]
    fn prototype_policy_knobs_are_rejected() {
        assert!(Policy::from_toml(r#"upstream_base_url = "x""#).is_ok());
        assert!(Policy::from_toml("upstream_base_url = \"x\"\nunknown_policy = \"deny\"").is_err());
    }

    #[test]
    fn escalate_authority_without_webhook_is_rejected_at_load() {
        let text = r#"
            upstream_base_url = "http://upstream.invalid"

            [[contracts.authority]]
            name = "human-in-the-loop"
            rule = "escalate"
            acquire_effects = true
        "#;
        assert!(matches!(
            Policy::from_toml(text),
            Err(ConfigError::EscalateWithoutWebhook(name)) if name == "human-in-the-loop"
        ));
    }

    #[test]
    fn escalate_authority_with_webhook_loads() {
        let p = Policy::from_toml(
            r#"
            upstream_base_url = "http://upstream.invalid"

            [[contracts.authority]]
            name = "ops-approver"
            rule = "escalate"
            acquire_effects = true
            webhook = { url = "http://ops-approver.kagent.svc/rule" }
            "#,
        )
        .unwrap();
        assert_eq!(p.contracts.authorities.len(), 1);
        assert_eq!(p.contracts.endpoints.len(), 1);
    }

    #[test]
    fn allow_authority_still_loads() {
        let p = Policy::from_toml(
            r#"
            upstream_base_url = "http://upstream.invalid"

            [[contracts.authority]]
            name = "default-allow"
            rule = "allow"
            acknowledge_unknown = true
            "#,
        )
        .unwrap();
        assert_eq!(p.contracts.authorities.len(), 1);
    }

    #[test]
    fn kagent_demo_policy_loads() {
        let p = Policy::from_toml(include_str!("../../demo/kagent/policy.toml")).expect("demo policy parses");
        assert_eq!(p.contracts.transformers.len(), 1);
        assert!(!p.contracts.endpoints.is_empty());
    }
}

//! The plugin's configuration: the Dogwood policy and its schemas, the entities every request
//! names, where the history lives, and which Dogwood action each APPA tool is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dogwood_language::{LoweredPolicySet, PolicySchema, ServiceSchema, Validator};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    /// The history file. Relative paths here are relative to the configuration file.
    store: PathBuf,
    /// The Dogwood (or Cedar) policy set.
    policy: PathBuf,
    /// The Cedar action schema the policy is written against.
    schema: PathBuf,
    /// A Dogwood event schema; omitted takes Dogwood's request/response default.
    event_schema: Option<PathBuf>,
    /// The principal and resource every request names, as Cedar entity literals.
    principal: String,
    resource: String,
    /// The Dogwood action of each APPA tool routed to this plugin, as `Ns::Action::Id`.
    tools: BTreeMap<String, String>,
    /// How long an annotation waits for an earlier annotated call's release record.
    #[serde(default = "default_settle_wait_ms")]
    settle_wait_ms: u64,
    /// How long an annotated call no record ever mentions stays in the history.
    #[serde(default = "default_pending_timeout_s")]
    pending_timeout_s: u64,
}

fn default_settle_wait_ms() -> u64 {
    2_000
}

fn default_pending_timeout_s() -> u64 {
    300
}

pub struct Config {
    pub store: PathBuf,
    policy: String,
    schema: String,
    event_schema: Option<String>,
    pub principal: String,
    pub resource: String,
    tools: BTreeMap<String, String>,
    pub settle_wait: Duration,
    pub pending_timeout: Duration,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let raw: Raw = toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        let read = |file: &Path| {
            let file = base.join(file);
            std::fs::read_to_string(&file).map_err(|error| format!("{}: {error}", file.display()))
        };
        Ok(Config {
            store: base.join(&raw.store),
            policy: read(&raw.policy)?,
            schema: read(&raw.schema)?,
            event_schema: raw.event_schema.as_deref().map(read).transpose()?,
            principal: raw.principal,
            resource: raw.resource,
            tools: raw.tools,
            settle_wait: Duration::from_millis(raw.settle_wait_ms),
            pending_timeout: Duration::from_secs(raw.pending_timeout_s),
        })
    }

    /// The Dogwood action an APPA tool is, or a refusal for a tool the plugin does not serve.
    pub fn action(&self, tool: &str) -> Result<&str, String> {
        self.tools
            .get(tool)
            .map(String::as_str)
            .ok_or_else(|| format!("no Dogwood action is configured for the tool {tool}"))
    }

    /// The policy set, lowered and type-checked. A policy that does not validate decides
    /// nothing: every consult is refused rather than answered from a mistyped rule, which
    /// Dogwood would read as permanently false.
    pub fn policies(&self) -> Result<LoweredPolicySet, String> {
        let service = match &self.event_schema {
            None => ServiceSchema::defaults(),
            Some(events) => ServiceSchema::builder()
                .event_schema_str(events)
                .build()
                .map_err(|error| format!("event schema: {error:?}"))?,
        };
        let schema = PolicySchema::from_cedarschema_str(&self.schema).map_err(|error| format!("schema: {error:?}"))?;
        let policies =
            LoweredPolicySet::from_str(&self.policy, &service, &schema).map_err(|error| format!("policy: {error}"))?;
        let validation = Validator::new().validate(&policies);
        if !validation.validation_passed() {
            let errors: Vec<String> = validation.validation_errors().map(|error| error.to_string()).collect();
            return Err(format!("the policy does not validate: {}", errors.join("; ")));
        }
        Ok(policies)
    }
}

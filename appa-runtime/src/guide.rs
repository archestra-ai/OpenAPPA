//! The host-neutral policy-writing rules every host of the appa-guide skill
//! consumes.

pub const CORE: &str = include_str!("../../integrations/appa-guide/references/core.md");

/// The contract reference every host's setup hands the agent beside a battery.
pub const CONTRACTS: &str = include_str!("../../website/content/docs/contracts.md");

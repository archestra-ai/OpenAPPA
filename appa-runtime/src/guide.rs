//! The host-neutral policy-writing rules every host of the appa-guide skill
//! consumes.

/// The router every host's guide opens with: host detection and the modes.
pub const ROUTER: &str = include_str!("../../integrations/appa-guide/SKILL.md");

pub const CORE: &str = include_str!("../../integrations/appa-guide/references/core.md");

/// The OpenCode host reference, which `appa opencode setup` appends.
pub const OPENCODE: &str = include_str!("../../integrations/appa-guide/references/opencode.md");

/// The contract reference every host's setup hands the agent beside a battery.
pub const CONTRACTS: &str = include_str!("../../website/content/docs/contracts.md");

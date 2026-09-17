//! Host observations, not policy declarations. An inventory can be partial: failure to
//! enumerate a connection says nothing about whether its tools are covered by policy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Adapter, CanonicalTool, ParseRefusal};

pub const MAX_INVENTORY_TOOLS: usize = 10_000;
pub const MAX_INVENTORY_BYTES: usize = 10 * 1024 * 1024;

// Count the exact JSON encoding without allocating another inventory-sized
// buffer. Stop once the same wire-size limit has been exceeded.
struct InventorySize(usize);

impl std::io::Write for InventorySize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        if self.0 > MAX_INVENTORY_BYTES {
            return Err(std::io::Error::other("tool inventory exceeds its size limit"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A read-only preflight. Omit root_id before opening a family to check the
/// serving policy; supply it afterwards to check that family's pinned policy.
/// This neither reserves names nor authorizes calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationRequest {
    pub protocol: u32,
    pub adapter: crate::AdapterName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_id: Option<String>,
    pub inventory: ToolInventory,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolInventory {
    #[serde(default)]
    pub tools: Vec<ObservedTool>,
    #[serde(default)]
    pub sources: Vec<InventorySource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedTool {
    /// The name the host actually dispatches, not a policy alias.
    pub name: String,
    /// The adapter's wire spelling. Only the server-side adapter derives identity.
    pub tool: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventorySource {
    /// Configured connection identity, scoped to the plugin instance/session.
    pub server: String,
    pub status: InventoryStatus,
    /// Whether this host can expose additional tools after this observation.
    #[serde(default)]
    pub dynamic: bool,
    /// A credential-free diagnostic, never a URL containing credentials or headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryStatus {
    Complete,
    Partial,
    Unavailable,
}

impl ToolInventory {
    /// Add observations without changing an identity already seen in this scope.
    /// A failed candidate leaves the caller's accepted snapshot unchanged. Missing
    /// tools in a later listing do not free their names for reuse in this session.
    pub fn extending(&self, candidate: &Self, adapter: Adapter) -> Result<Self, ParseRefusal> {
        self.validate(adapter)?;
        candidate.validate(adapter)?;
        let mut combined = self.clone();
        let previous: BTreeMap<_, _> = self.tools.iter().map(|tool| (&tool.name, &tool.tool)).collect();
        for observed in &candidate.tools {
            match previous.get(&observed.name) {
                Some(tool) if **tool == observed.tool => {}
                Some(_) => {
                    return Err(ParseRefusal::Malformed {
                        detail: format!("tool {:?} changed identity within this session", observed.name),
                    });
                }
                None => combined.tools.push(observed.clone()),
            }
        }
        let mut sources: BTreeMap<_, _> = self.sources.iter().map(|source| (&source.server, source)).collect();
        for source in &candidate.sources {
            sources.insert(&source.server, source);
        }
        combined.sources = sources.into_values().cloned().collect();
        combined
            .tools
            .sort_by(|a, b| a.name.cmp(&b.name).then(a.tool.cmp(&b.tool)));
        combined.tools.dedup();
        combined.validate(adapter)?;
        Ok(combined)
    }

    /// Validate even when a wildcard covers every tool: a wildcard cannot repair an
    /// ambiguous dispatch name. An identical repeated observation is harmless.
    pub fn validate(&self, adapter: Adapter) -> Result<(), ParseRefusal> {
        let refuse = |detail| ParseRefusal::Malformed { detail };
        if self.tools.len() > MAX_INVENTORY_TOOLS || serde_json::to_writer(InventorySize(0), self).is_err() {
            return Err(refuse("tool inventory exceeds its size limit".into()));
        }
        let mut names = BTreeMap::new();
        let mut identities = BTreeMap::new();
        for observed in &self.tools {
            if observed.name.is_empty() || observed.name.chars().any(char::is_control) {
                return Err(refuse("inventory contains an empty or invalid host tool name".into()));
            }
            let identity = (adapter.derive)(&observed.tool)?.canonical;
            if adapter.name == crate::AdapterName::Kagent
                && observed.tool.starts_with("mcp:")
                && identity.as_str().rsplit('/').next() != Some(observed.name.as_str())
            {
                return Err(refuse(
                    "kagent MCP observations must use the actual unprefixed tool name".into(),
                ));
            }
            if names
                .insert(&observed.name, identity.clone())
                .is_some_and(|previous| previous != identity)
            {
                return Err(refuse(format!(
                    "host tool {:?} identifies multiple tools; rename or filter it in the host configuration",
                    observed.name
                )));
            }
            if identities
                .insert(identity, &observed.name)
                .is_some_and(|previous| previous != &observed.name)
            {
                return Err(refuse("multiple host names resolve to the same tool identity".into()));
            }
        }
        let mut sources = BTreeMap::new();
        for source in &self.sources {
            if source.server.is_empty() || sources.insert(&source.server, ()).is_some() {
                return Err(refuse("inventory source names must be nonempty and unique".into()));
            }
        }
        Ok(())
    }

    pub fn identities(&self, adapter: Adapter) -> Result<Vec<(String, CanonicalTool, bool)>, ParseRefusal> {
        self.validate(adapter)?;
        self.tools
            .iter()
            .map(|observed| {
                let derived = (adapter.derive)(&observed.tool)?;
                Ok((observed.name.clone(), derived.canonical, derived.spawn))
            })
            .collect()
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn counts_encoded_bytes_and_stops_at_the_wire_limit() {
        let value = serde_json::json!({"detail": "quotes \" and backslash \\ and Unicode λ"});
        let mut count = InventorySize(0);
        serde_json::to_writer(&mut count, &value).unwrap();
        assert_eq!(count.0, serde_json::to_vec(&value).unwrap().len());
        let mut nearly_full = InventorySize(MAX_INVENTORY_BYTES - 1);
        assert!(serde_json::to_writer(&mut nearly_full, &value).is_err());
    }
}

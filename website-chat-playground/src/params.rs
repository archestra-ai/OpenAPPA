//! Server-side argument schemas for the playground's tools.

use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    #[error("malformed TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("re-serializing the policy: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("converting a parameter schema to TOML: {0}")]
    Schema(String),
}

pub fn tool_parameters(tool: &str) -> Option<serde_json::Value> {
    let schema = match tool {
        "list_customers" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "create_customer_data" => json!({
            "type": "object",
            "properties": {
                "customer": { "type": "string", "description": "The customer record name to create, e.g. `northwind`." },
                "content": { "type": "string", "description": "The full contents of the record." }
            },
            "required": ["customer", "content"],
            "additionalProperties": false
        }),
        "list_issues" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "create_issue" => json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "The issue title." },
                "body": { "type": "string", "description": "The issue body text." }
            },
            "required": ["title", "body"],
            "additionalProperties": false
        }),
        "send_email" => json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Recipient address." },
                "subject": { "type": "string", "description": "Subject line." },
                "body": { "type": "string", "description": "Message body text." }
            },
            "required": ["to", "subject", "body"],
            "additionalProperties": false
        }),
        "list_invoices" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "make_transfer" => json!({
            "type": "object",
            "properties": {
                "amount_usd": { "type": "number", "description": "Amount in US dollars." },
                "to_account": { "type": "string", "description": "Destination account." },
                "memo": { "type": "string", "description": "What the transfer is for." }
            },
            "required": ["amount_usd", "to_account"],
            "additionalProperties": false
        }),
        "list_recordings" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        _ => return None,
    };
    Some(schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::System;

    #[test]
    fn every_tool_has_a_schema_and_nothing_else_does() {
        for system in System::ALL {
            for tool in system.tools() {
                assert!(tool_parameters(tool).is_some(), "{tool} needs a schema");
            }
        }
        assert!(tool_parameters("frobnicate").is_none());
    }
}

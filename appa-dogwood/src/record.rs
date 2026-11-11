//! The recorder: one delivery of dispatch records in, the history updated. A release binds
//! the annotated request to its dispatch; a successful outcome adds a `response` event and a
//! failed one an `error` event; a batch that did not release a call removes its request.

use serde_json::{Value, json};

use crate::config::Config;
use crate::history::{Entry, Kind, Locked};

pub fn record(config: &Config, delivery: &Value) -> Result<Value, String> {
    if delivery["kind"] != "recorder" {
        return Err("the Dogwood plugin records recorder deliveries only".to_string());
    }
    let records = delivery["records"].as_array().ok_or("a delivery carries its records")?;
    let mut locked = Locked::open(&config.store)?;
    let store = &mut locked.store;
    for record in records {
        let tool = record["tool"].as_str();
        let evidence = record["evidence"].as_str().unwrap_or_default();
        let annotated = |entry: &Entry| {
            entry.pending()
                && Some(entry.tool.as_str()) == tool
                && entry.arguments == record["arguments"]
                && entry.evidence == evidence
        };
        match record["record"].as_str() {
            Some("released") => {
                let dispatch = record["dispatch"].as_str().ok_or("a release names its dispatch")?;
                let tool = tool.ok_or("a release names its tool")?;
                if store
                    .events
                    .iter()
                    .any(|entry| entry.dispatch.as_deref() == Some(dispatch))
                {
                    continue;
                }
                match store.events.iter_mut().find(|entry| annotated(entry)) {
                    Some(entry) => entry.dispatch = Some(dispatch.to_string()),
                    // A release whose annotation the history no longer holds: it happened, so
                    // it is recorded now.
                    None => {
                        let at = store.next_at();
                        store.events.push(Entry {
                            kind: Kind::Request,
                            tool: tool.to_string(),
                            arguments: record["arguments"].clone(),
                            at,
                            evidence: evidence.to_string(),
                            dispatch: Some(dispatch.to_string()),
                        });
                    }
                }
            }
            Some("outcome") => {
                let dispatch = record["dispatch"].as_str().ok_or("an outcome names its dispatch")?;
                let kind = match record["outcome"].as_str() {
                    Some("success") => Kind::Response,
                    Some("failure") => Kind::Error,
                    // Neither happened as far as the history can tell.
                    _ => {
                        store.settled.insert(dispatch.to_string());
                        continue;
                    }
                };
                if !store.settled.insert(dispatch.to_string()) {
                    continue;
                }
                let Some(request) = store
                    .events
                    .iter()
                    .find(|entry| entry.kind == Kind::Request && entry.dispatch.as_deref() == Some(dispatch))
                    .cloned()
                else {
                    continue;
                };
                let at = store.next_at();
                store.events.push(Entry { kind, at, ..request });
            }
            Some("unreleased") => {
                let key = format!(
                    "{}\t{}\t{}\t{}",
                    record["trajectory"]
                        .as_str()
                        .ok_or("an unreleased record names its trajectory")?,
                    record["batch"].as_str().ok_or("an unreleased record names its batch")?,
                    record["tool"],
                    record["arguments"],
                );
                if !store.given_back.insert(key) {
                    continue;
                }
                if let Some(at) = store.events.iter().position(annotated) {
                    store.events.remove(at);
                }
            }
            _ => return Err("a record is released, outcome, or unreleased".to_string()),
        }
    }
    locked.save()?;
    Ok(json!({ "version": 1, "answer": {} }))
}

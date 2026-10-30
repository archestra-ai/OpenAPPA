//! The recorder: one delivery of dispatch records in, the store updated. A release binds the
//! annotation's reservation to its dispatch and the label the call published; the outcome
//! settles the files; a batch that did not release the call gives its reservation back.

use serde_json::{Value, json};

use crate::config::{Config, Role};
use crate::label::Label;
use crate::paths::named;
use crate::store::{Locked, Reservation, now};

pub fn record(config: &Config, delivery: &Value) -> Result<Value, String> {
    if delivery["kind"] != "recorder" {
        return Err("the file-tracker records recorder deliveries only".to_string());
    }
    let records = delivery["records"].as_array().ok_or("a delivery carries its records")?;
    let mut locked = Locked::open(&config.store)?;
    let store = &mut locked.store;
    for record in records {
        let dispatch = record["dispatch"].as_str();
        let evidence = record["evidence"].as_str().unwrap_or_default();
        let unbound = |reservation: &Reservation| {
            reservation.dispatch.is_none()
                && reservation.tool == record["tool"]
                && reservation.arguments == record["arguments"]
                && reservation.evidence == evidence
        };
        match record["record"].as_str() {
            Some("released") => {
                let dispatch = dispatch.ok_or("a release names its dispatch")?;
                let tool = record["tool"].as_str().ok_or("a release names its tool")?;
                let Some(role) = config.role(tool).filter(|role| role.role != Role::Read) else {
                    continue;
                };
                if store.settled.contains(dispatch)
                    || store
                        .reservations
                        .iter()
                        .any(|held| held.dispatch.as_deref() == Some(dispatch))
                {
                    continue;
                }
                let published = Label::from_published(&record["label"])?;
                match store.reservations.iter_mut().find(|reservation| unbound(reservation)) {
                    Some(reservation) => {
                        reservation.dispatch = Some(dispatch.to_string());
                        reservation.published = Some(published);
                    }
                    // A release whose annotation the store no longer holds: settle it on
                    // its files all the same, and treat each as changed.
                    None => store.reservations.push(Reservation {
                        tool: tool.to_string(),
                        arguments: record["arguments"].clone(),
                        evidence: evidence.to_string(),
                        writes: named(role, &record["arguments"])?
                            .writes
                            .into_iter()
                            .map(|path| (path, "unknown".to_string()))
                            .collect(),
                        created: now(),
                        dispatch: Some(dispatch.to_string()),
                        published: Some(published),
                    }),
                }
            }
            Some("outcome") => {
                let Some(at) = store
                    .reservations
                    .iter()
                    .position(|reservation| reservation.dispatch.as_deref() == dispatch)
                else {
                    continue;
                };
                let reservation = store.reservations.remove(at);
                store.settle(reservation, record["outcome"] == "success");
            }
            Some("unreleased") => {
                if let Some(at) = store.reservations.iter().position(unbound) {
                    store.reservations.remove(at);
                }
            }
            _ => return Err("a record is released, outcome, or unreleased".to_string()),
        }
    }
    locked.save()?;
    Ok(json!({ "version": 1, "answer": {} }))
}

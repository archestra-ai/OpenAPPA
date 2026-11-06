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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// A refusal the runtime delivers again — as every delivery after a restart is — gives
    /// back nothing a second time: the identical proposal that came after it keeps its
    /// reservation.
    #[test]
    fn a_replayed_refusal_keeps_the_reservation_of_a_later_identical_proposal() {
        let dir = std::env::temp_dir().join(format!("appa-ft-replay-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("file-tracker.toml");
        std::fs::write(
            &config_path,
            format!("store = {:?}\n", dir.join("store.json").display().to_string()),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let arguments = json!({ "file_path": dir.join("notes.txt").display().to_string(), "content": "x" });
        let reserve = || {
            let mut locked = Locked::open(&config.store).unwrap();
            locked.store.reservations.push(Reservation {
                tool: "host/claude-code/Write".to_string(),
                arguments: arguments.clone(),
                evidence: "e".to_string(),
                writes: Default::default(),
                created: now(),
                dispatch: None,
                published: None,
            });
            locked.save().unwrap();
        };
        let refusal = json!({ "kind": "recorder", "records": [{
            "record": "unreleased", "trajectory": "t1", "batch": "b1",
            "tool": "host/claude-code/Write", "arguments": arguments, "evidence": "e",
        }]});
        let held = || -> Store { Locked::open(&config.store).unwrap().store };

        reserve();
        record(&config, &refusal).unwrap();
        assert!(held().reservations.is_empty(), "the refusal gives its reservation back");
        reserve();
        record(&config, &refusal).unwrap();
        let reservations = held().reservations.len();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            reservations, 1,
            "the replayed refusal leaves the later reservation held"
        );
    }
}

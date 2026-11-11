//! The Annotator: one consult in, one annotation out. The proposed call is decided as the
//! next Dogwood request over the recorded history. A denied call requires `blocked`, which no
//! authority can permit; an allowed call requires nothing, enters the history as a request,
//! and changes no label.

use std::time::Instant;

use serde_json::{Value, json};

use crate::config::Config;
use crate::history::{Entry, Kind, Locked};

pub fn annotate(config: &Config, request: &Value) -> Result<Value, String> {
    if request["kind"] != "annotation" {
        return Err("the Dogwood plugin answers annotation consults only".to_string());
    }
    let call = &request["artifact"]["args"];
    let tool = call["name"]
        .as_str()
        .ok_or("the consult carries the complete call: declare the dogwood annotator without inputs")?;
    config.action(tool)?;
    let marks = &request["declaration"]["attention_marks"];
    if !marks.as_array().into_iter().flatten().any(|mark| mark == "blocked") {
        return Err("the dogwood annotator must be declared with marks = [\"blocked\"]".to_string());
    }
    let policies = config.policies()?;

    // An earlier call whose records have not all arrived may still be released, or may have
    // finished without its `response` in the history yet. Wait for its records; past the
    // wait, decide on the history as it stands, with an unreleased call counted as a request.
    let deadline = Instant::now() + config.settle_wait;
    let mut locked = loop {
        let mut locked = Locked::open(&config.store)?;
        locked.store.expire(config.pending_timeout);
        if !locked.store.unsettled(config.pending_timeout) || Instant::now() >= deadline {
            break locked;
        }
        drop(locked);
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    let evidence = locked.store.digest();
    let candidate = Entry {
        kind: Kind::Request,
        tool: tool.to_string(),
        arguments: call["arguments"].clone(),
        at: locked.store.next_at(),
        evidence: evidence.clone(),
        dispatch: None,
    };
    let response = locked.store.decide(config, policies, &candidate)?;
    let attention: Vec<&str> = match response.allowed() {
        true => vec![],
        false => {
            let rules: Vec<usize> = response.diagnostics().reason().map(|rule| rule.rule_index).collect();
            let errors: Vec<&str> = response.diagnostics().errors().collect();
            eprintln!("appa-dogwood: {tool} denied; determining rules {rules:?}, errors {errors:?}");
            vec!["blocked"]
        }
    };
    if response.allowed() {
        locked.store.events.push(candidate);
    }
    locked.save()?;
    Ok(json!({
        "version": 1,
        "answer": {
            "delta": {},
            "requires": { "history": [], "attention": attention },
            "emits": [],
            "evidence": evidence,
        }
    }))
}

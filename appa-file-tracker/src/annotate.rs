//! The Annotator: one consult in, one annotation out. A read's `delta` is the file's label; an
//! edit forwards its predecessor's label, so the label the edit publishes keeps it; a write
//! forwards nothing, so the label it publishes replaces the one it destroys; SafeBash forwards
//! the meet of its inputs' labels. The hosts SafeBash reaches add nothing to its label: the
//! sandbox confines a command to them, and what a host may receive is not judged here. Every
//! file a call may change is reserved until its records settle it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::config::{Config, Role};
use crate::label::{Label, Ranks};
use crate::paths::{Named, named};
use crate::store::{Locked, Reservation, Store, digest_lines, now, version};

/// What the consult declares the answer may use.
struct Mandate {
    ranks: Ranks,
    audiences: Vec<String>,
}

pub fn annotate(config: &Config, request: &Value) -> Result<Value, String> {
    if request["kind"] != "annotation" {
        return Err("the file-tracker answers annotation consults only".to_string());
    }
    let call = &request["artifact"]["args"];
    let tool = call["name"]
        .as_str()
        .ok_or("the consult carries the complete call: declare the file-tracker annotator without inputs")?;
    let arguments = &call["arguments"];
    let role = config
        .role(tool)
        .ok_or_else(|| format!("the file-tracker serves no tool {tool}"))?;
    let named = named(role, arguments)?;
    let strings = |field: &str| -> Vec<String> {
        request["declaration"][field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let mandate = Mandate {
        ranks: Ranks(strings("trust_ranks")),
        audiences: strings("audiences"),
    };

    let deadline = Instant::now() + config.settle_wait;
    let mut locked = loop {
        let mut locked = Locked::open(&config.store)?;
        locked.store.ranks = mandate.ranks.clone();
        locked.store.expire(config.reservation_timeout);
        match named
            .reads
            .iter()
            .chain(&named.writes)
            .find(|path| locked.store.holder(path).is_some())
        {
            None => break locked,
            Some(path) if Instant::now() >= deadline => {
                return Err(format!(
                    "{} is reserved by a call whose records have not settled; propose this call again",
                    path.display()
                ));
            }
            Some(_) => {
                drop(locked);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };

    let store = &locked.store;
    let label_of = |path: &PathBuf| file_label(config, store, path);
    let inputs = named
        .reads
        .iter()
        .map(label_of)
        .fold(Label::default(), |met, label| met.meet(&label, &store.ranks));
    let (delta, forwards) = match role.role {
        Role::Read => (inputs, Label::default()),
        Role::Write => (Label::default(), Label::default()),
        Role::Edit => (inputs.clone(), inputs),
        Role::Run => (inputs.clone(), inputs),
    };
    let rules = || {
        config
            .paths
            .iter()
            .filter(|rule| named.writes.iter().any(|path| rule.matches(path)))
    };
    let mut write_trust = None;
    for rank in rules().filter_map(|rule| rule.write_trust.as_deref()) {
        write_trust = store.ranks.higher(write_trust, Some(required_rank(rank, &mandate)?));
    }
    let mut attention: Vec<&str> = rules()
        .flat_map(|rule| rule.write_attention.iter().map(String::as_str))
        .collect();
    attention.sort();
    attention.dedup();

    let evidence = evidence(role.role, &named);
    let mut requires = json!({ "history": [], "attention": attention });
    if let Some(trust) = write_trust {
        requires["trust"] = json!(trust);
    }
    let answer = json!({
        "version": 1,
        "answer": {
            "delta": answered(&delta, &mandate)?,
            "forwards": answered(&forwards, &mandate)?,
            "requires": requires,
            "emits": [],
            "evidence": evidence,
        }
    });

    if !named.writes.is_empty() {
        locked.store.reservations.push(Reservation {
            tool: tool.to_string(),
            arguments: arguments.clone(),
            evidence,
            writes: named.writes.iter().map(|path| (path.clone(), version(path))).collect(),
            created: now(),
            dispatch: None,
            published: None,
        });
    }
    locked.save()?;
    Ok(answer)
}

/// A file's label: what its recorded writes left, met with every path rule that matches it.
pub fn file_label(config: &Config, store: &Store, path: &Path) -> Label {
    config
        .paths
        .iter()
        .filter(|rule| rule.matches(path))
        .map(|rule| rule.label())
        .fold(store.recorded(path).cloned().unwrap_or_default(), |met, label| {
            met.meet(&label, &store.ranks)
        })
}

/// What the call was judged over: each file's version, so the same call over changed files is
/// a different proposal.
fn evidence(role: Role, named: &Named) -> String {
    match role {
        Role::Read => version(&named.reads[0]),
        Role::Write | Role::Edit => version(&named.writes[0]),
        Role::Run => {
            let mut lines: Vec<String> = named
                .reads
                .iter()
                .map(|path| format!("in\t{}\t{}", path.display(), version(path)))
                .chain(
                    named
                        .writes
                        .iter()
                        .map(|path| format!("out\t{}\t{}", path.display(), version(path))),
                )
                .chain(named.hosts.iter().map(|host| format!("host\t{host}")))
                .collect();
            lines.sort();
            digest_lines(&lines)
        }
    }
}

/// A label as an answer's `delta` or `forwards`. A rank the mandate does not hold answers as
/// its lowest; an audience must be one clause of declared entries, or the annotation fails.
fn answered(label: &Label, mandate: &Mandate) -> Result<Value, String> {
    let mut answer = json!({});
    if let Some(trust) = &label.trust {
        answer["trust"] = json!(answered_rank(trust, mandate)?);
    }
    match label.audience.as_slice() {
        [] => {}
        [clause] => {
            if let Some(entry) = clause.iter().find(|entry| !mandate.audiences.contains(entry)) {
                return Err(format!(
                    "the stored audience entry {entry:?} is outside the policy's audiences"
                ));
            }
            answer["audience"] = json!(clause);
        }
        _ => return Err("the stored audience is an intersection no single answer can carry".to_string()),
    }
    Ok(answer)
}

/// A required trust floor. A rank the mandate does not hold answers as its highest: a floor
/// fails closed where a label fails open.
fn required_rank<'a>(rank: &'a str, mandate: &'a Mandate) -> Result<&'a str, String> {
    match mandate.ranks.0.iter().any(|known| known == rank) {
        true => Ok(rank),
        false => mandate
            .ranks
            .0
            .last()
            .map(String::as_str)
            .ok_or_else(|| "the consult declares no trust rank".to_string()),
    }
}

fn answered_rank<'a>(rank: &'a str, mandate: &'a Mandate) -> Result<&'a str, String> {
    match mandate.ranks.0.iter().any(|known| known == rank) {
        true => Ok(rank),
        false => mandate
            .ranks
            .lowest()
            .ok_or_else(|| "the consult declares no trust rank".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write floor names a rank this policy's chain lacks — the built-in `.env` rule says
    /// `trusted` — beside one it holds. The answer requires the chain's highest rank, whatever
    /// order the rules match in, never the lowest.
    #[test]
    fn a_write_floor_the_chain_does_not_name_requires_its_highest_rank() {
        let dir = std::env::temp_dir().join(format!("appa-ft-floor-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("file-tracker.toml");
        std::fs::write(
            &config_path,
            format!(
                "store = {:?}\n[[path]]\npattern = \"*/.env*\"\nwrite_trust = \"low\"\n",
                dir.join("store.json").display().to_string()
            ),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let request = json!({
            "kind": "annotation",
            "artifact": { "args": {
                "name": "host/claude-code/Write",
                "arguments": { "file_path": dir.join(".env").display().to_string(), "content": "" },
            }},
            "declaration": { "trust_ranks": ["low", "high"], "audiences": ["self"] },
        });
        let answer = annotate(&config, &request).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(answer["answer"]["requires"]["trust"], "high");
    }
}

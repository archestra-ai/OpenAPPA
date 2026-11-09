//! Internal JSON Lines worker for the historical release. Each input produces
//! one serialized Result; diagnostics go to stderr. The parent owns its lifetime.
use std::io::{BufRead, Write};
use std::num::NonZeroUsize;

const POLICY: &str = "version = 2\n[[tool]]\nname = \"note\"\n";

fn url() -> String {
    std::env::var("OPENAPPA_TEST_DATABASE_URL").expect("OPENAPPA_TEST_DATABASE_URL")
}

mod old {
    use super::*;
    pub use old_engine::fact::Fact;
    use old_engine::profile::{DeploymentName, PolicyRevision};
    use old_engine::transition::{EngineEvent, ProposalBatch, ProposalBatchId, ProposedCall};
    use old_engine::value::{OfferNonce, ToolName, TrajectoryId};

    pub fn store() -> old_eventlog::LogStore {
        old_eventlog::LogStore::open(old_eventlog::Backend::Postgres {
            url: url(),
            max_connections: NonZeroUsize::new(2).unwrap(),
        })
        .expect("the old store opens")
    }

    /// The policy compiled as the previous release's snapshot engine: a family it opens names no
    /// deployment.
    pub fn snapshot(policy: &str) -> old_engine::engine::Engine {
        old_policy::Config::from_toml_str(policy)
            .expect("compiles")
            .engine()
            .clone()
    }

    pub fn line(policy: &str, deployment: &str, revision: u64) -> old_engine::engine::Engine {
        snapshot(policy).at(DeploymentName::new(deployment), PolicyRevision::new(revision))
    }

    pub fn publish(store: &old_eventlog::LogStore, deployment: &str, expected: Option<u64>, policy: &str) -> u64 {
        store
            .publish(
                &DeploymentName::new(deployment),
                expected.map(PolicyRevision::new),
                policy.as_bytes(),
            )
            .expect("publishes")
            .revision
            .value()
    }

    pub fn open(store: &old_eventlog::LogStore, engine: &old_engine::engine::Engine, root: &str) {
        let id = TrajectoryId::new(root);
        let opening = engine
            .open_trajectory(&id, old_engine::profile::PolicyFileKey::of(POLICY.as_bytes()), None)
            .expect("seals")
            .into_unsealed();
        store
            .create_root(opening, POLICY.as_bytes())
            .expect("the old version opens the root");
    }

    /// Replay the family (each batch under its own revision), propose one `note` call, append.
    pub fn decide(
        store: &old_eventlog::LogStore,
        engine: &old_engine::engine::Engine,
        root: &str,
        batch: &str,
    ) -> Result<Vec<Fact>, String> {
        let id = TrajectoryId::new(root);
        let log = store.log(&id).map_err(|e| format!("old read: {e}"))?;
        let view = engine
            .replay(&id, &log.stamped_batches(), &[])
            .map_err(|e| format!("old replay: {e}"))?;
        let call = engine.resolve_call(ToolName::new("note"), b"{}").unwrap();
        let decision = engine
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: ProposalBatchId::new(batch),
                    trajectory: id.clone(),
                    provider_results: Vec::new(),
                    proposals: vec![ProposedCall {
                        tool: call.tool().clone(),
                        arguments: call.canonical_arguments().canonical_bytes().to_vec(),
                        annotation: None,
                    }],
                    spawn: None,
                    offer_nonce: OfferNonce::new([1; 32]),
                    evidence: Vec::new(),
                    audience: Default::default(),
                }),
            )
            .map_err(|e| format!("old decide: {e}"))?;
        let facts = decision.append.expect("a release appends").into_unsealed();
        store
            .append(&log, &facts, None)
            .map_err(|e| format!("old append: {e}"))?;
        Ok(facts)
    }

    pub fn reads(store: &old_eventlog::LogStore, root: &str) -> Result<u64, String> {
        store
            .log(&TrajectoryId::new(root))
            .map(|log| log.basis())
            .map_err(|e| format!("old read: {e}"))
    }
}

fn main() {
    let store = old::store();
    for line in std::io::stdin().lock().lines() {
        let request: serde_json::Value = serde_json::from_str(&line.expect("read request")).expect("valid request");
        let engine = || {
            let policy = request["policy"].as_str().expect("policy");
            match request["deployment"].as_str() {
                Some(deployment) => old::line(policy, deployment, request["revision"].as_u64().expect("revision")),
                None => old::snapshot(policy),
            }
        };
        let result: Result<serde_json::Value, String> = match request["method"].as_str().expect("method") {
            "publish" => Ok(serde_json::json!(old::publish(
                &store,
                request["deployment"].as_str().expect("deployment"),
                request["expected"].as_u64(),
                request["policy"].as_str().expect("policy"),
            ))),
            "open" => {
                old::open(&store, &engine(), request["root"].as_str().expect("root"));
                Ok(serde_json::Value::Null)
            }
            "decide" => old::decide(
                &store,
                &engine(),
                request["root"].as_str().expect("root"),
                request["batch"].as_str().expect("batch"),
            )
            .map(|facts| serde_json::json!(facts.len())),
            "reads" => old::reads(&store, request["root"].as_str().expect("root")).map(serde_json::Value::from),
            method => Err(format!("unknown historical worker method: {method}")),
        };
        println!("{}", serde_json::to_string(&result).expect("serialize response"));
        std::io::stdout().flush().expect("flush response");
    }
}

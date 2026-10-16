//! Mixed-version proof: the previous release (the `rev` in Cargo.toml) and this checkout share one
//! PostgreSQL store, as old and new pods do during a rolling release and after a rollback.

use std::num::NonZeroUsize;

const POLICY: &str = "version = 2\n[[tool]]\nname = \"note\"\n";
const POLICY_NEXT: &str = "version = 2\n[[tool]]\nname = \"note\"\ndescription = \"the next revision\"\n";

fn url() -> String {
    std::env::var("OPENAPPA_TEST_DATABASE_URL").expect("OPENAPPA_TEST_DATABASE_URL")
}

fn unique(prefix: &str) -> String {
    format!("{prefix}:{}", std::process::id()) + &format!(":{:?}", std::time::SystemTime::now())
}

mod old {
    use super::*;
    pub use old_engine::fact::Fact;
    use old_engine::transition::{EngineEvent, ProposalBatch, ProposalBatchId, ProposedCall};
    use old_engine::value::{OfferNonce, ToolName, TrajectoryId};

    pub fn store() -> old_eventlog::LogStore {
        old_eventlog::LogStore::open(old_eventlog::Backend::Postgres {
            url: url(),
            max_connections: NonZeroUsize::new(2).unwrap(),
        })
        .expect("the old store opens")
    }

    pub fn engine(policy: &str) -> old_engine::engine::Engine {
        old_policy::Config::from_toml_str(policy).expect("compiles").engine().clone()
    }

    pub fn open(store: &old_eventlog::LogStore, engine: &old_engine::engine::Engine, root: &str) {
        let id = TrajectoryId::new(root);
        let opening = engine
            .open_trajectory(&id, old_engine::profile::PolicyFileKey::of(POLICY.as_bytes()), None)
            .expect("seals")
            .into_unsealed();
        store.create_root(opening, POLICY.as_bytes()).expect("the old version opens the root");
    }

    /// Replay the family under the old engine, propose one `note` call, append the decision.
    pub fn decide(store: &old_eventlog::LogStore, engine: &old_engine::engine::Engine, root: &str, batch: &str) -> Result<Vec<Fact>, String> {
        let id = TrajectoryId::new(root);
        let log = store.log(&id).map_err(|e| format!("old read: {e}"))?;
        let view = engine
            .view(&id, log.facts().to_vec(), log.basis())
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
        store.append(&log, &facts).map_err(|e| format!("old append: {e}"))?;
        Ok(facts)
    }

    pub fn replays(store: &old_eventlog::LogStore, engine: &old_engine::engine::Engine, root: &str) -> Result<u64, String> {
        let id = TrajectoryId::new(root);
        let log = store.log(&id).map_err(|e| format!("old read: {e}"))?;
        engine
            .view(&id, log.facts().to_vec(), log.basis())
            .map(|view| view.revision())
            .map_err(|e| format!("old replay: {e}"))
    }
}

mod new {
    use super::*;
    pub use new_engine::fact::Fact;
    use new_engine::profile::{DeploymentName, PolicyRevision};
    use new_engine::transition::{EngineEvent, ProposalBatch, ProposalBatchId, ProposedCall};
    use new_engine::value::{OfferNonce, ToolName, TrajectoryId};

    pub fn store() -> new_eventlog::LogStore {
        new_eventlog::LogStore::open(new_eventlog::Backend::Postgres {
            url: url(),
            max_connections: NonZeroUsize::new(2).unwrap(),
        })
        .expect("the new store opens")
    }

    pub fn snapshot(policy: &str) -> new_engine::engine::Engine {
        new_policy::Config::from_toml_str(policy).expect("compiles").engine().clone()
    }

    pub fn line(policy: &str, deployment: &str, revision: u64) -> new_engine::engine::Engine {
        snapshot(policy).at(DeploymentName::new(deployment), PolicyRevision::new(revision))
    }

    pub fn publish(store: &new_eventlog::LogStore, deployment: &str, expected: Option<u64>, policy: &str) -> u64 {
        store
            .publish(&DeploymentName::new(deployment), expected.map(PolicyRevision::new), policy.as_bytes())
            .expect("publishes")
            .revision
            .value()
    }

    pub fn open(store: &new_eventlog::LogStore, engine: &new_engine::engine::Engine, root: &str) {
        let id = TrajectoryId::new(root);
        let opening = engine
            .open_trajectory(&id, new_engine::profile::PolicyFileKey::of(POLICY.as_bytes()), None)
            .expect("seals")
            .into_unsealed();
        store.create_root(opening, POLICY.as_bytes()).expect("the new version opens the root");
    }

    /// Replay the family (each batch under its own revision), propose one `note` call, append.
    pub fn decide(
        store: &new_eventlog::LogStore,
        engine: &new_engine::engine::Engine,
        earlier: &[&new_engine::engine::Engine],
        root: &str,
        batch: &str,
    ) -> Result<Vec<Fact>, String> {
        let id = TrajectoryId::new(root);
        let log = store.log(&id).map_err(|e| format!("new read: {e}"))?;
        let view = engine
            .replay(&id, &log.stamped_batches(), earlier)
            .map_err(|e| format!("new replay: {e}"))?;
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
                    offer_nonce: OfferNonce::new([2; 32]),
                    evidence: Vec::new(),
                    audience: Default::default(),
                }),
            )
            .map_err(|e| format!("new decide: {e}"))?;
        let facts = decision.append.expect("a release appends").into_unsealed();
        store.append(&log, &facts, None).map_err(|e| format!("new append: {e}"))?;
        Ok(facts)
    }

    pub fn stamps(store: &new_eventlog::LogStore, root: &str) -> Vec<Option<u64>> {
        store
            .log(&TrajectoryId::new(root))
            .expect("reads")
            .stamped_batches()
            .iter()
            .map(|batch| batch.revision.map(|revision| revision.value()))
            .collect()
    }

    pub fn deployment(store: &new_eventlog::LogStore, root: &str) -> Option<String> {
        store
            .log(&TrajectoryId::new(root))
            .expect("reads")
            .deployment()
            .map(|deployment| deployment.as_str().to_string())
    }
}

fn check(name: &str, outcome: Result<impl std::fmt::Debug, String>) {
    match outcome {
        Ok(value) => println!("PASS  {name}: {value:?}"),
        Err(error) => {
            println!("FAIL  {name}: {error}");
            std::process::exit(1);
        }
    }
}

fn expect_refused(name: &str, outcome: Result<impl std::fmt::Debug, String>) {
    match outcome {
        Ok(value) => {
            println!("FAIL  {name}: expected a refusal, got {value:?}");
            std::process::exit(1);
        }
        Err(error) => println!("PASS  {name} (refused as documented): {error}"),
    }
}

fn main() {
    let (old_store, new_store) = (old::store(), new::store());
    let old_engine = old::engine(POLICY);

    // ---------------------------------------------------------------- A: a live pinned family
    let root_a = unique("mixver-pinned");
    old::open(&old_store, &old_engine, &root_a);
    check("A1 old version opens a family and decides", old::decide(&old_store, &old_engine, &root_a, "a1").map(|f| f.len()));
    let snapshot = new::snapshot(POLICY);
    check("A2 new version reads it as pinned", Ok::<_, String>(new::deployment(&new_store, &root_a)).and_then(|d| if d.is_none() { Ok("no deployment") } else { Err(format!("{d:?}")) }));
    check("A3 new version continues it under the snapshot", new::decide(&new_store, &snapshot, &[], &root_a, "a2").map(|f| f.len()));
    check("A4 new version stamps nothing on a pinned family", Ok::<_, String>(new::stamps(&new_store, &root_a)).and_then(|s| if s.iter().all(Option::is_none) { Ok(s) } else { Err(format!("{s:?}")) }));
    check("A5 old version (rollback) replays it", old::replays(&old_store, &old_engine, &root_a));
    check("A6 old version (rollback) continues it", old::decide(&old_store, &old_engine, &root_a, "a3").map(|f| f.len()));
    check("A7 new version replays it again", new::decide(&new_store, &snapshot, &[], &root_a, "a4").map(|f| f.len()));

    // ---------------------------------------------------------------- B: a line family
    let deployment = unique("mixver-line");
    let head = new::publish(&new_store, &deployment, None, POLICY);
    let at_one = new::line(POLICY, &deployment, head);
    let root_b = unique("mixver-following");
    new::open(&new_store, &at_one, &root_b);
    check("B1 new version opens a following family and decides at the head", new::decide(&new_store, &at_one, &[], &root_b, "b1").map(|f| f.len()));
    check("B2 stamps", Ok::<_, String>(new::stamps(&new_store, &root_b)));
    check("B3 old version (mid-roll or rollback) replays it as pinned to the opening file", old::replays(&old_store, &old_engine, &root_b));
    check("B4 old version continues it", old::decide(&old_store, &old_engine, &root_b, "b2").map(|f| f.len()));
    check("B5 new version replays the old batch under the opening revision and decides", new::decide(&new_store, &at_one, &[], &root_b, "b3").map(|f| f.len()));
    check("B6 stamps", Ok::<_, String>(new::stamps(&new_store, &root_b)));

    // A publish moves the head: the stale engine's next decision is fenced.
    let next = new::publish(&new_store, &deployment, Some(head), POLICY_NEXT);
    expect_refused("B7 a decision made at the previous revision does not commit", new::decide(&new_store, &at_one, &[], &root_b, "b4"));
    let at_two = new::line(POLICY_NEXT, &deployment, next);
    check("B8 the head decides, replaying earlier batches under revision 1", new::decide(&new_store, &at_two, &[&at_one], &root_b, "b5").map(|f| f.len()));
    check("B9 stamps", Ok::<_, String>(new::stamps(&new_store, &root_b)));
    // After a decision under a later revision, the old version may or may not reproduce it: it
    // knows only the opening file. Report what it does.
    match old::replays(&old_store, &old_engine, &root_b) {
        Ok(position) => println!("INFO  B10 old version still replays the family at position {position} (the revision-2 change did not alter any recorded fact)"),
        Err(error) => println!("INFO  B10 old version refuses the family after a later-revision decision: {error}"),
    }
    // An old pod writing after the publish (a publish mid-roll) is unfenced; the new version then
    // refuses the regression, as the runbook documents.
    let _ = old::decide(&old_store, &old_engine, &root_b, "b6");
    expect_refused("B11 an old write after a later-revision batch is a regression", new::decide(&new_store, &at_two, &[&at_one], &root_b, "b7"));
    println!("done");
}

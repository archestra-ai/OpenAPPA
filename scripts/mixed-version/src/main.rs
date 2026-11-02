//! Mixed-version proof: the previous release (the `rev` in Cargo.toml) and this checkout share one
//! PostgreSQL store, as old and new pods do during a rolling release and after a rollback. Both
//! open families that follow a deployment. This checkout reads and continues the families the
//! previous release opened. The previous release refuses every family this checkout opened or
//! wrote to, so a rollback across this release restarts those families. A family the previous
//! release opened pinned to a policy snapshot is refused by this checkout as a legacy family.

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
        old_policy::Config::from_toml_str(policy).expect("compiles").engine().clone()
    }

    pub fn line(policy: &str, deployment: &str, revision: u64) -> old_engine::engine::Engine {
        snapshot(policy).at(DeploymentName::new(deployment), PolicyRevision::new(revision))
    }

    pub fn publish(store: &old_eventlog::LogStore, deployment: &str, expected: Option<u64>, policy: &str) -> u64 {
        store
            .publish(&DeploymentName::new(deployment), expected.map(PolicyRevision::new), policy.as_bytes())
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
        store.create_root(opening, POLICY.as_bytes()).expect("the old version opens the root");
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
        store.append(&log, &facts, None).map_err(|e| format!("old append: {e}"))?;
        Ok(facts)
    }

    pub fn reads(store: &old_eventlog::LogStore, root: &str) -> Result<u64, String> {
        store
            .log(&TrajectoryId::new(root))
            .map(|log| log.basis())
            .map_err(|e| format!("old read: {e}"))
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

    pub fn line(policy: &str, deployment: &str, revision: u64) -> new_engine::engine::Engine {
        new_policy::Config::from_toml_str(policy)
            .expect("compiles")
            .engine()
            .clone()
            .at(DeploymentName::new(deployment), PolicyRevision::new(revision))
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

    pub fn deployment(store: &new_eventlog::LogStore, root: &str) -> String {
        store
            .log(&TrajectoryId::new(root))
            .expect("reads")
            .deployment()
            .as_str()
            .to_string()
    }

    /// Whether the family's read is refused as one opened before live policy revisions.
    pub fn refused_as_legacy(store: &new_eventlog::LogStore, root: &str) -> Result<String, String> {
        match store.log(&TrajectoryId::new(root)) {
            Err(error @ new_eventlog::ReadError::LegacyFamily { .. }) => Ok(error.to_string()),
            Err(error) => Err(format!("refused, but not as a legacy family: {error}")),
            Ok(log) => Err(format!("read at position {}", log.basis())),
        }
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

fn equal<T: PartialEq + std::fmt::Debug>(found: T, expected: T) -> Result<T, String> {
    match found == expected {
        true => Ok(found),
        false => Err(format!("expected {expected:?}, found {found:?}")),
    }
}

fn main() {
    let (old_store, new_store) = (old::store(), new::store());

    // ------------------------------------- A: a family the previous release opened, read here
    let line_a = unique("mixver-old-line");
    let head = old::publish(&old_store, &line_a, None, POLICY);
    let old_at_one = old::line(POLICY, &line_a, head);
    let root_a = unique("mixver-old-family");
    old::open(&old_store, &old_at_one, &root_a);
    check(
        "A1 old version opens a following family and decides",
        old::decide(&old_store, &old_at_one, &root_a, "a1").map(|f| f.len()),
    );
    check(
        "A2 new version reads the deployment it follows",
        equal(new::deployment(&new_store, &root_a), line_a.clone()),
    );
    let at_one = new::line(POLICY, &line_a, head);
    check(
        "A3 new version replays it and decides at the head",
        new::decide(&new_store, &at_one, &[], &root_a, "a2").map(|f| f.len()),
    );
    check(
        "A4 every batch is stamped at the head",
        equal(new::stamps(&new_store, &root_a), vec![Some(head); 3]),
    );
    let next = new::publish(&new_store, &line_a, Some(head), POLICY_NEXT);
    let at_two = new::line(POLICY_NEXT, &line_a, next);
    check(
        "A5 after a publish, the new head decides, replaying the old release's batches under revision 1",
        new::decide(&new_store, &at_two, &[&at_one], &root_a, "a3").map(|f| f.len()),
    );
    check(
        "A6 stamps",
        equal(
            new::stamps(&new_store, &root_a),
            vec![Some(head), Some(head), Some(head), Some(next)],
        ),
    );
    expect_refused(
        "A7 old version (rollback) does not read the family once this version wrote to it",
        old::reads(&old_store, &root_a),
    );

    // ---------------------- B: a family this checkout opened, which the previous release refuses
    let line_b = unique("mixver-new-line");
    let head = new::publish(&new_store, &line_b, None, POLICY);
    let at_one = new::line(POLICY, &line_b, head);
    let root_b = unique("mixver-new-family");
    new::open(&new_store, &at_one, &root_b);
    check(
        "B1 new version opens a following family and decides at the head",
        new::decide(&new_store, &at_one, &[], &root_b, "b1").map(|f| f.len()),
    );
    expect_refused(
        "B2 old version (rollback) does not read a family this version opened",
        old::reads(&old_store, &root_b),
    );
    let old_at_one = old::line(POLICY, &line_b, head);
    expect_refused(
        "B3 old version (rollback) does not continue it",
        old::decide(&old_store, &old_at_one, &root_b, "b2"),
    );
    check(
        "B4 new version continues it: the refused rollback wrote nothing",
        new::decide(&new_store, &at_one, &[], &root_b, "b3").map(|f| f.len()),
    );

    // ------------------------------------------------- C: a publish fences the previous revision
    let next = new::publish(&new_store, &line_b, Some(head), POLICY_NEXT);
    expect_refused(
        "C1 a decision made at the previous revision does not commit",
        new::decide(&new_store, &at_one, &[], &root_b, "c1"),
    );
    let at_two = new::line(POLICY_NEXT, &line_b, next);
    check(
        "C2 the head decides, replaying earlier batches under revision 1",
        new::decide(&new_store, &at_two, &[&at_one], &root_b, "c2").map(|f| f.len()),
    );

    // ------------------------ D: a family the previous release opened pinned to a policy snapshot
    let root_d = unique("mixver-old-snapshot");
    let snapshot = old::snapshot(POLICY);
    old::open(&old_store, &snapshot, &root_d);
    check(
        "D1 old version opens a pinned family and decides",
        old::decide(&old_store, &snapshot, &root_d, "d1").map(|f| f.len()),
    );
    check(
        "D2 new version refuses it as a family opened before live policy revisions",
        new::refused_as_legacy(&new_store, &root_d),
    );
    println!("done");
}

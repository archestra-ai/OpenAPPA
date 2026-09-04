//! One trajectory's decision record, stripped for export.
//!
//! This is what a yell carries when it carries a trajectory: the engine's own facts and the
//! runtime's own events, both walked against [`super::tables`], plus the topology and the
//! trust ranks a reader needs to make sense of them. No prompt, no argument, no tool output,
//! no path, and — under [`Mode::Pseudonymized`] — no name the deployment chose.
//!
//! ## Why the export is bounded by bytes and not by a fact count
//!
//! A count is not a bound: fifty thousand facts carrying large arguments are gigabytes before
//! the first one is stripped. So the builder walks the log twice. The first pass runs
//! newest-first with a throwaway token map, keeping nothing but a running byte total, to find
//! the oldest fact that still fits the budget. The second runs from there oldest-first with
//! the real map, so the export is in log order and token numbers follow first appearance. One
//! stripped fact is live at a time in the first pass.

use serde::Serialize;
use serde_json::Value;

use appa_engine::fact::Fact;
use appa_runtime_api::TrajectoryId;

use super::strip::{Drift, Stripped, strip};
use super::tables;
use super::tokens::{Class, Mode, Tokens};
use crate::engine::ReplayRefusalClass;

/// How much stripped fact JSON one export may hold. The report envelope around it is trimmed
/// separately against the receiver's own limits; this bounds what the *builder* materializes.
const MAX_FACT_BYTES: usize = 24 * 1024 * 1024;
/// What each entry's own JSON framing costs on top of the fact, so the budget is measured
/// against something close to the emitted document rather than the facts alone.
const ENTRY_OVERHEAD: usize = 32;

/// Which trajectory an export is about.
#[derive(Debug, Clone)]
pub(crate) enum Selection {
    /// A caller with a vouched session: the root whose log is read, and the trajectory within
    /// it that is doing the yelling, which is the root itself for a CLI caller.
    Root {
        root: TrajectoryId,
        yelling: Option<TrajectoryId>,
    },
    /// A caller with no session of its own. The runtime picks the one trajectory that was
    /// active recently, and refuses to guess between several.
    Recent,
}

/// How long after its last event a trajectory is still "the current one".
pub(crate) const RECENT_WINDOW: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// The export, or why there is none.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Diagnostic {
    Omitted { omitted_reason: OmittedReason },
    Present(Box<Export>),
}

/// Why an export holds no trajectory. Each is a state a caller can act on: wait for the agent
/// to do something, name a session, or read the store error the runtime already logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OmittedReason {
    NoRecentTrajectory,
    /// More than one trajectory was active in the window. Guessing between them would put one
    /// session's decisions in another session's report.
    Ambiguous,
    LogUnavailable,
}

#[derive(Debug, Serialize)]
pub(crate) struct Export {
    /// Every trajectory the log names, with its parent where the log could be replayed.
    pub(crate) branches: Vec<Branch>,
    /// Set when the log did not replay. The facts are exported anyway — a log the engine
    /// refuses is exactly the thing worth reporting — but nothing derived from a view is.
    pub(crate) replay_refused: Option<ReplayRefusalClass>,
    /// The policy's trust ranks, lowest first, so a reader can read the numeric ranks the
    /// facts carry.
    pub(crate) trust_chain: Vec<String>,
    pub(crate) facts: Vec<FactEntry>,
    pub(crate) runtime_events: Vec<EventEntry>,
    /// Facts older than this were left out to stay inside the byte budget. `None` when the
    /// whole log is here.
    pub(crate) truncated_before_seq: Option<usize>,
    /// Runtime events the in-process log's own bounds dropped, and the sequence the hole runs
    /// through. Named separately for the two lists because their truncation is separate: a
    /// reader must be able to tell "this trajectory's early hooks were evicted" from "a
    /// deployment reload was evicted".
    pub(crate) events_dropped: u64,
    pub(crate) events_dropped_through_seq: Option<u64>,
    pub(crate) deployment_events_dropped: u64,
    pub(crate) deployment_events_dropped_through_seq: Option<u64>,
    /// Where the classification inventory has drifted from the engine. A reader learns that an
    /// unclassified field exists, which aggregate it belongs to, and where it sits — and no
    /// part of what sat there is carried.
    pub(crate) unclassified: Vec<Drift>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Branch {
    pub(crate) id: String,
    pub(crate) parent: Option<String>,
    /// The trajectory this report is about. False for every other branch of the same family.
    pub(crate) yelling: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct FactEntry {
    /// The fact's index in the log, kept even when older facts were trimmed, so a reader can
    /// see the gap rather than a renumbered sequence.
    pub(crate) seq: usize,
    pub(crate) fact: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct EventEntry {
    pub(crate) seq: u64,
    /// Wall time, for a reader to look at. Ordering is `seq`.
    pub(crate) at: String,
    pub(crate) event: Value,
}

/// What the builder needs from the runtime, gathered before anything is stripped so this
/// module holds no lock and reads no store.
pub(crate) struct Source<'a> {
    pub(crate) facts: &'a [Fact],
    pub(crate) events: crate::events::Events,
    pub(crate) trust_chain: Vec<String>,
    pub(crate) parents: Vec<(String, Option<String>)>,
    pub(crate) replay_refused: Option<ReplayRefusalClass>,
    pub(crate) yelling: Option<TrajectoryId>,
}

/// Every trajectory the log names, with its parent.
///
/// The ids come from the facts rather than from the view, so a log the engine refuses still
/// reports its topology as far as the facts show it — the parents are simply unknown, which
/// is what a refused replay honestly leaves.
pub(crate) fn branches(
    facts: &[Fact],
    view: Option<&crate::engine::EngineView>,
    policy: Option<&crate::engine::PolicyEngine<'_>>,
) -> Vec<(String, Option<String>)> {
    let mut ids: Vec<String> = Vec::new();
    for fact in facts {
        let id = fact.trajectory().as_str();
        if !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let parent = view.zip(policy).and_then(|(view, policy)| {
                policy
                    .engine()
                    .parent_of(view, &TrajectoryId(id.clone()))
                    .map(|parent| parent.0)
            });
            (id, parent)
        })
        .collect()
}

/// How long ago the chosen trajectory was last active, or why none was chosen.
pub(crate) fn resolve(recent: crate::events::Recent) -> Result<TrajectoryId, OmittedReason> {
    match recent {
        crate::events::Recent::One { root, age } if age <= RECENT_WINDOW => Ok(TrajectoryId(root)),
        crate::events::Recent::One { .. } | crate::events::Recent::None => Err(OmittedReason::NoRecentTrajectory),
        crate::events::Recent::Ambiguous => Err(OmittedReason::Ambiguous),
    }
}

pub(crate) fn build(source: Source<'_>, mode: Mode) -> Export {
    let mut tokens = Tokens::default();
    let mut unclassified: Vec<Drift> = Vec::new();
    let from = budget_start(source.facts, mode);

    let trust_chain = source
        .trust_chain
        .iter()
        .map(|rank| tokens.token(mode, Class::Trust, rank))
        .collect();
    let branches = source
        .parents
        .iter()
        .map(|(id, parent)| Branch {
            yelling: source.yelling.as_ref().is_some_and(|one| &one.0 == id),
            id: tokens.token(mode, Class::Trajectory, id),
            parent: parent
                .as_ref()
                .map(|parent| tokens.token(mode, Class::Trajectory, parent)),
        })
        .collect();

    let mut facts = Vec::with_capacity(source.facts.len() - from);
    for (seq, fact) in source.facts.iter().enumerate().skip(from) {
        let stripped = strip(&serialized(fact), &tables::FACT, &mut tokens, mode);
        record_drift(&mut unclassified, &stripped, "fact");
        facts.push(FactEntry {
            seq,
            fact: stripped.value,
        });
    }

    let mut runtime_events = Vec::new();
    for entry in source.events.entries.iter().chain(source.events.deployment.iter()) {
        let event = serialized(&entry.event);
        let stripped = match tables::event_table(&event) {
            Some(table) => strip(&event, table, &mut tokens, mode),
            // A variant the inventory does not name carries nothing, exactly as an unnamed key
            // does, and says so.
            None => Stripped {
                value: Value::String(super::strip::UNCLASSIFIED.to_string()),
                unclassified: vec![Drift {
                    path: String::new(),
                    table: "RuntimeEvent",
                }],
            },
        };
        record_drift(&mut unclassified, &stripped, "runtime_event");
        runtime_events.push(EventEntry {
            seq: entry.seq,
            at: rfc3339(entry.at),
            event: stripped.value,
        });
    }
    runtime_events.sort_by_key(|entry| entry.seq);

    Export {
        branches,
        replay_refused: source.replay_refused,
        trust_chain,
        facts,
        runtime_events,
        truncated_before_seq: (from > 0).then_some(from),
        events_dropped: source.events.dropped,
        events_dropped_through_seq: source.events.dropped_through_seq,
        deployment_events_dropped: source.events.deployment_dropped,
        deployment_events_dropped_through_seq: source.events.deployment_dropped_through_seq,
        unclassified,
    }
}

/// The oldest fact index that still fits [`MAX_FACT_BYTES`], found newest-first.
///
/// The token map here is thrown away: it exists only so the measured bytes are the bytes the
/// second pass will produce, and reusing it would number tokens newest-first.
fn budget_start(facts: &[Fact], mode: Mode) -> usize {
    let mut measure = Tokens::default();
    let mut total = 0usize;
    for (index, fact) in facts.iter().enumerate().rev() {
        let stripped = strip(&serialized(fact), &tables::FACT, &mut measure, mode);
        total += serde_json::to_string(&stripped.value).map_or(0, |text| text.len()) + ENTRY_OVERHEAD;
        if total > MAX_FACT_BYTES {
            return index + 1;
        }
    }
    0
}

/// A fact or an event as its own serde produces it. Every engine and runtime type here
/// derives or hand-writes an infallible `Serialize`, so a failure would be a bug in this
/// crate rather than anything the trajectory did.
fn serialized<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("a fact and a runtime event both serialize")
}

/// Drift entries, qualified by which list they came from and deduplicated across the whole
/// export. Both halves are the walk's own vocabulary; nothing read from the input is here.
fn record_drift(into: &mut Vec<Drift>, stripped: &Stripped, section: &str) {
    for drift in &stripped.unclassified {
        let qualified = Drift {
            path: match drift.path.is_empty() {
                true => section.to_string(),
                false => format!("{section}.{}", drift.path),
            },
            table: drift.table,
        };
        if !into.contains(&qualified) {
            into.push(qualified);
        }
    }
}

fn rfc3339(at: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::api::Runtime;
    use crate::config::Config;

    const POLICY: &str = r#"
        [policy]
        version = 2

        [[policy.tool]]
        name = "Bash"

        [[policy.tool]]
        name = "Write"

        [[policy.tool]]
        name = "AskUserQuestion"

        [[policy.tool]]
        name = "Task"

        [[policy.tool]]
        name = "Agent"

        [[policy.tool]]
        name = "Read"

        [policy.deployment]
        context_control = true

        [externals]
        timeout_ms = 1000
        max_body_bytes = 4096
    "#;

    fn config(dir: &tempfile::TempDir) -> Config {
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, POLICY).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    fn fixtures() -> Vec<serde_json::Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks.jsonl");
        std::fs::read_to_string(path)
            .expect("the recorded hook fixtures are readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each fixture line is JSON"))
            .collect()
    }

    /// One recorded Claude Code session, replayed through the real hook path so the log holds
    /// facts the engine actually produced rather than ones this test invented.
    async fn recorded_session(dir: &tempfile::TempDir) -> (Runtime, TrajectoryId) {
        let runtime = Runtime::open(config(dir), dir.path().join("appa.db"), None).expect("the deployment opens");
        let codec = appa_adapter_claude_code::codec();
        let mut root = None;
        for event in fixtures() {
            let id = TrajectoryId(format!(
                "cc:{}",
                event["session_id"].as_str().expect("each fixture names its session")
            ));
            let body = serde_json::to_vec(&event).expect("the fixture re-serializes");
            // A marked spawn blocks until its return is declared; the recorded session then
            // proposes it again. Declaring here is what lets the fork facts reach the log.
            let spawn = event["hook_event_name"] == "PreToolUse"
                && event["tool_name"] == "Agent"
                && event.get("agent_id").is_none();
            if spawn {
                crate::hooks::answer(&runtime, &codec, &body).await;
                let quoted = runtime
                    .minted_offers(&id, &id)
                    .into_iter()
                    .next()
                    .expect("the block surfaced the return declaration");
                let arguments = crate::engine::RemedyArguments {
                    label: Some(crate::engine::LabelSpelling::default()),
                    return_schema: None,
                };
                let actor = crate::api::Actor {
                    root: id.clone(),
                    child: None,
                };
                let outcome = runtime
                    .execute_remedy_with(&actor, crate::api::OfferId(quoted.0), arguments)
                    .await;
                assert!(
                    matches!(outcome, crate::api::RemedyOutcome::Authorized { .. }),
                    "the declaration approves the spawn, got {outcome:?}"
                );
            }
            let (status, answer) = crate::hooks::answer(&runtime, &codec, &body).await;
            assert_eq!(status, 200, "the recorded session replays: {answer}");
            // The recorded file holds several sessions, and the last of them opens no root.
            // The one this test exports is the last that did open one — the session with the
            // subagent, so the export covers fork and return facts too.
            if event["hook_event_name"] == "SessionStart" {
                root = Some(id);
            }
        }
        (runtime, root.expect("a recorded session opened a root"))
    }

    fn export(runtime: &Runtime, root: &TrajectoryId, mode: Mode) -> Export {
        match runtime.diagnostic(
            Selection::Root {
                root: root.clone(),
                yelling: None,
            },
            mode,
        ) {
            Diagnostic::Present(export) => *export,
            Diagnostic::Omitted { omitted_reason } => panic!("the recorded session exports, got {omitted_reason:?}"),
        }
    }

    /// The acceptance criterion the whole inventory exists for: a real session's facts and
    /// events are classified end to end. A field or a variant added to the engine without a
    /// line in [`super::tables`] fails here, naming the aggregate and the path that need one.
    #[tokio::test]
    async fn a_recorded_session_is_classified_end_to_end() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (runtime, root) = recorded_session(&dir).await;
        let export = export(&runtime, &root, Mode::Pseudonymized);
        assert!(
            export.unclassified.is_empty(),
            "the inventory does not cover {:?}",
            export.unclassified
        );
        assert!(!export.facts.is_empty(), "the replay produced facts");
        assert!(!export.runtime_events.is_empty(), "the replay produced runtime events");
    }

    /// The property a person is promised when they answer yes to pseudonymization: nothing a
    /// reader of the report could use to name this machine, this session, or this file.
    #[tokio::test]
    async fn nothing_the_session_carried_survives_pseudonymization() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (runtime, root) = recorded_session(&dir).await;
        let export = export(&runtime, &root, Mode::Pseudonymized);
        let rendered = serde_json::to_string(&export).expect("the export serializes");

        let session = root.0.trim_start_matches("cc:").to_string();
        assert!(!rendered.contains(&session), "the harness session id is gone");
        // The recorded session's own vocabulary: the tools it called and the paths in their
        // arguments. Every one of them is either tokenized or dropped.
        for spelled in ["Bash", "Write", "Read", "Agent", "/home/user", "hookrec"] {
            assert!(!rendered.contains(spelled), "{spelled} survived pseudonymization");
        }
    }

    /// Baseline names what the deployment spells and hides exactly the same values, so the two
    /// modes differ in naming alone and cannot drift into two classifications.
    #[tokio::test]
    async fn baseline_differs_from_pseudonymized_only_in_the_names() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (runtime, root) = recorded_session(&dir).await;
        let baseline = export(&runtime, &root, Mode::Baseline);
        let pseudonymized = export(&runtime, &root, Mode::Pseudonymized);

        assert_eq!(baseline.facts.len(), pseudonymized.facts.len());
        assert_eq!(baseline.runtime_events.len(), pseudonymized.runtime_events.len());
        assert_eq!(baseline.unclassified, pseudonymized.unclassified);
        let rendered = serde_json::to_string(&baseline).expect("the export serializes");
        assert!(rendered.contains("Bash"), "baseline carries the names as spelled");
        assert!(
            !rendered.contains("/home/user"),
            "baseline still carries no argument value"
        );
    }

    /// A caller that names no trajectory must never be handed someone else's.
    #[test]
    fn a_stale_or_ambiguous_recent_trajectory_is_refused_rather_than_guessed() {
        let stale = crate::events::Recent::One {
            root: "cc:whatever".to_string(),
            age: RECENT_WINDOW + std::time::Duration::from_secs(1),
        };
        assert_eq!(resolve(stale), Err(OmittedReason::NoRecentTrajectory));
        assert_eq!(resolve(crate::events::Recent::Ambiguous), Err(OmittedReason::Ambiguous));
        assert_eq!(
            resolve(crate::events::Recent::None),
            Err(OmittedReason::NoRecentTrajectory)
        );
    }
}

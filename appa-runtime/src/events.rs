//! What the runtime did, as opposed to what the engine decided.
//!
//! The engine's facts are the trajectory: they are validated, replayable, and persisted by
//! `appa-eventlog`. They say nothing about the machinery around them — which hook the harness
//! actually sent, how long an authority took to answer, whether a consult timed out, when the
//! deployment reloaded, that a store read failed. When someone reports that APPA is in the way,
//! that machinery is usually the answer, and none of it is in the log.
//!
//! So the runtime keeps its own account of it, and keeps it *here*: in this process, bounded,
//! and gone on restart. Nothing in this module is persisted and nothing in it is authoritative —
//! it is a diagnostic, never an input to a decision. `record` is infallible for the same reason:
//! it cannot fail a decision the engine has already made.
//!
//! ## Why bounded, and bounded four ways
//!
//! A long-lived runtime serves many trajectories and an unbounded account of them is a leak.
//! One byte budget is the real bound; the three counts stop any single trajectory from
//! crowding out the rest, which a byte budget alone does not do. Every drop is counted and the
//! highest dropped sequence is kept, so a reader is told exactly where the hole is rather than
//! being quietly handed a partial history.

use std::collections::BTreeMap;
use std::time::SystemTime;

use appa_engine::value::DispatchId;
use appa_runtime_api::TrajectoryId;
use serde::Serialize;

/// The whole account this runtime keeps of itself. Every list is bounded; see the module
/// documentation for why there are four bounds rather than one.
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// Entries kept for one trajectory, and for the deployment-wide list.
const MAX_ENTRIES_PER_LIST: usize = 5_000;
/// Trajectories kept at once. The coldest is evicted whole.
const MAX_ROOTS: usize = 32;
/// A tool name reaches this from an untrusted hook body, so it is clamped at admission: a
/// count cap bounds records, not memory.
const MAX_NAME_BYTES: usize = 256;
/// Offer ids on one hook decision, likewise clamped.
const MAX_OFFERS: usize = 32;
/// What a clamped name ends with, so a reader can tell a long name from a truncated one.
const ELISION: &str = "…";

/// One thing the runtime did.
///
/// Every variant is a closed shape. Nothing here holds a free-form message: a report built
/// from these events is sent off the machine, and prose is how names, paths and addresses
/// escape a projection that has otherwise classified every field.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RuntimeEvent {
    /// A hook arrived and was answered.
    Hook {
        event: HookKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dispatch: Option<DispatchId>,
        outcome: HookOutcome,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        offers: Vec<String>,
    },
    /// The runtime consulted something outside itself and waited for it.
    External {
        role: ExternalRole,
        name: String,
        outcome: ExternalOutcome,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        offer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dispatch: Option<DispatchId>,
    },
    /// A control tool ran: a remedy, or a report.
    Control {
        call: ControlCall,
        outcome: ControlOutcome,
        duration_ms: u64,
    },
    /// The deployment was reloaded. `changed` is false when the new policy composed to the
    /// same key, which is the common case and worth distinguishing.
    Reload { policy_key: String, changed: bool },
    /// A store operation failed. The class is closed and carries nothing of the failure; see
    /// [`appa_eventlog::StoreErrorClass`].
    StoreError {
        operation: StoreOperation,
        class: appa_eventlog::StoreErrorClass,
    },
}

/// Which hook arrived. One variant per `HookEvent`, plus the two answers that never become
/// an event at all: a body the codec could not read, and a hook this codec does not map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookKind {
    SessionStart,
    Prompt,
    TurnEnd,
    ToolCall,
    ToolResult,
    ChildStart,
    ChildEnd,
    SpawnResult,
    /// The body did not parse.
    Unparsable,
    /// The codec answered "not mine". Neither an event nor a parse failure — and worth
    /// recording precisely because "the harness fired a hook and APPA did nothing" is a
    /// common shape of the confusion a report is about.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookOutcome {
    Allowed,
    Denied,
    PassControl,
    Acked,
    Refused,
    Blocked,
    Ignored,
    Malformed,
    Unreadable,
}

/// One per [`crate::consult::ConsultKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalRole {
    Authority,
    Sanitizer,
    Annotator,
    AudienceSource,
    Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalOutcome {
    Answered,
    NoAnswer(NoAnswerClass),
}

/// Why an external did not answer, as a closed class.
///
/// Deliberately not [`crate::external::NoAnswerReason`] itself: that type carries
/// `MalformedAnswer(String)`, and the string is a fragment of what the external actually
/// said. A report built from these events leaves the machine, so the detail is dropped here
/// rather than filtered later. A status code is a scalar, not prose, and is worth keeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoAnswerClass {
    Unregistered,
    Unreachable,
    Dismissed,
    NonSuccess { status: u16 },
    Timeout,
    Transport,
    Malformed,
    Oversized,
    UnsupportedVersion,
    ModuleError,
    ModulePanicked,
}

impl From<&crate::external::NoAnswerReason> for NoAnswerClass {
    fn from(reason: &crate::external::NoAnswerReason) -> Self {
        use crate::external::NoAnswerReason as Reason;
        match reason {
            Reason::Unregistered => NoAnswerClass::Unregistered,
            Reason::Unreachable => NoAnswerClass::Unreachable,
            Reason::Dismissed => NoAnswerClass::Dismissed,
            Reason::NonSuccess { status } => NoAnswerClass::NonSuccess { status: *status },
            Reason::Timeout => NoAnswerClass::Timeout,
            Reason::Transport => NoAnswerClass::Transport,
            // The detail is what the external said, and it is not carried.
            Reason::Malformed | Reason::MalformedAnswer(_) => NoAnswerClass::Malformed,
            Reason::Oversized => NoAnswerClass::Oversized,
            Reason::UnsupportedVersion => NoAnswerClass::UnsupportedVersion,
            Reason::ModuleError => NoAnswerClass::ModuleError,
            Reason::ModulePanicked => NoAnswerClass::ModulePanicked,
        }
    }
}

impl From<&crate::external::ConsultOutcome> for ExternalOutcome {
    fn from(outcome: &crate::external::ConsultOutcome) -> Self {
        match outcome {
            crate::external::ConsultOutcome::Answer(_) => ExternalOutcome::Answered,
            crate::external::ConsultOutcome::NoAnswer(reason) => ExternalOutcome::NoAnswer(reason.into()),
        }
    }
}

impl From<crate::consult::ConsultKind> for ExternalRole {
    fn from(kind: crate::consult::ConsultKind) -> Self {
        match kind {
            crate::consult::ConsultKind::Authority => ExternalRole::Authority,
            crate::consult::ConsultKind::Sanitizer => ExternalRole::Sanitizer,
            crate::consult::ConsultKind::Annotation => ExternalRole::Annotator,
            crate::consult::ConsultKind::AudienceSource => ExternalRole::AudienceSource,
            crate::consult::ConsultKind::Identity => ExternalRole::Identity,
        }
    }
}

/// A remedy carries the offer it executes; a report carries the dispatch it was released
/// under. One shape with a single optional string could model neither honestly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub(crate) enum ControlCall {
    Remedy {
        offer: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        dispatch: Option<DispatchId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlOutcome {
    /// The remedy ran and the call may proceed.
    Executed,
    /// An authority said no, or none answered. Both leave the agent blocked, and which of
    /// the two it was is the first thing a report about a stuck agent needs.
    Declined,
    NoAnswer,
    /// The offer did not stand: unknown, unpursued, cross-turn, or terminal.
    Refused,
}

impl From<&crate::api::RemedyOutcome> for ControlOutcome {
    fn from(outcome: &crate::api::RemedyOutcome) -> Self {
        use crate::api::RemedyOutcome;
        match outcome {
            // The feedback and the released call are the outcome's own payload and stay out
            // of the entry: a report carries the shape of what happened, never its prose.
            RemedyOutcome::Authorized { .. } | RemedyOutcome::Substituted { .. } | RemedyOutcome::Returned { .. } => {
                ControlOutcome::Executed
            }
            RemedyOutcome::Declined { .. } => ControlOutcome::Declined,
            RemedyOutcome::NoAnswer { .. } => ControlOutcome::NoAnswer,
            RemedyOutcome::Refused { .. } => ControlOutcome::Refused,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StoreOperation {
    Open,
    Read,
    Append,
}

/// One entry as it is kept and reported.
///
/// Ordering is `seq`, the runtime's own counter, never `at`: two entries a millisecond
/// apart must still be comparable, and a clock that steps backwards must not reorder them.
/// `at` is for a reader to look at, and nothing more.
#[derive(Debug, Clone)]
pub(crate) struct RecordedEvent {
    pub(crate) seq: u64,
    pub(crate) at: SystemTime,
    pub(crate) event: RuntimeEvent,
}

impl RecordedEvent {
    /// What this entry costs the budget. An estimate, not a measurement: it exists to bound
    /// memory, and the two fields that an untrusted caller can grow are clamped before they
    /// ever reach here.
    fn retained_bytes(&self) -> usize {
        const FIXED: usize = std::mem::size_of::<RecordedEvent>();
        FIXED
            + match &self.event {
                RuntimeEvent::Hook { tool, offers, .. } => {
                    tool.as_ref().map_or(0, String::len) + offers.iter().map(String::len).sum::<usize>()
                }
                RuntimeEvent::External { name, offer, .. } => name.len() + offer.as_ref().map_or(0, String::len),
                RuntimeEvent::Control { call, .. } => match call {
                    ControlCall::Remedy { offer, .. } => offer.len(),
                },
                RuntimeEvent::Reload { policy_key, .. } => policy_key.len(),
                RuntimeEvent::StoreError { .. } => 0,
            }
    }
}

/// One trajectory's entries, with what the bounds took from them.
#[derive(Debug, Default)]
struct RootEvents {
    entries: std::collections::VecDeque<RecordedEvent>,
    dropped: u64,
    dropped_through_seq: Option<u64>,
    bytes: usize,
}

impl RootEvents {
    fn push(&mut self, entry: RecordedEvent) {
        self.bytes += entry.retained_bytes();
        self.entries.push_back(entry);
        while self.entries.len() > MAX_ENTRIES_PER_LIST {
            self.drop_oldest();
        }
    }

    fn drop_oldest(&mut self) -> bool {
        match self.entries.pop_front() {
            Some(entry) => {
                self.bytes = self.bytes.saturating_sub(entry.retained_bytes());
                self.dropped += 1;
                self.dropped_through_seq = Some(entry.seq);
                true
            }
            None => false,
        }
    }

    /// The newest sequence this list holds, for choosing which trajectory is coldest.
    fn newest_seq(&self) -> Option<u64> {
        self.entries.back().map(|entry| entry.seq)
    }
}

/// The runtime's account of itself: one list per trajectory plus one for everything that
/// belongs to no trajectory — reloads, store failures, hooks refused before an actor exists.
#[derive(Debug, Default)]
pub(crate) struct EventLog {
    /// Keyed by the trajectory's spelling, as the runtime's other in-memory maps are
    /// (`permits`, `executing`, `prompted`): `TrajectoryId` is a public wire type and this
    /// is not a reason to widen its derives.
    roots: BTreeMap<String, RootEvents>,
    deployment: RootEvents,
    /// Allocated under the same lock as the insert. An atomic outside the lock would let a
    /// later sequence land first, and eviction order would then disagree with sequence order.
    next_seq: u64,
}

impl EventLog {
    pub(crate) fn record(&mut self, root: Option<&TrajectoryId>, event: RuntimeEvent) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let entry = RecordedEvent {
            seq,
            at: SystemTime::now(),
            event: clamp(event),
        };
        match root {
            Some(root) => match self.roots.get_mut(&root.0) {
                Some(events) => events.push(entry),
                None => {
                    self.roots.entry(root.0.clone()).or_default().push(entry);
                    self.evict_coldest_roots();
                }
            },
            None => self.deployment.push(entry),
        }
        self.enforce_byte_budget();
    }

    fn evict_coldest_roots(&mut self) {
        while self.roots.len() > MAX_ROOTS {
            let coldest = self
                .roots
                .iter()
                .min_by_key(|(_, events)| events.newest_seq().unwrap_or(0))
                .map(|(root, _)| root.clone());
            match coldest {
                Some(root) => {
                    self.roots.remove(&root);
                }
                None => break,
            }
        }
    }

    fn bytes(&self) -> usize {
        self.deployment.bytes + self.roots.values().map(|events| events.bytes).sum::<usize>()
    }

    /// Drop the oldest entry anywhere until the whole account is under budget. A list that
    /// empties is removed, so this terminates: every pass either drops an entry or runs out
    /// of lists holding one.
    fn enforce_byte_budget(&mut self) {
        while self.bytes() > MAX_TOTAL_BYTES {
            let oldest_root = self
                .roots
                .iter()
                .filter_map(|(root, events)| events.entries.front().map(|entry| (entry.seq, root.clone())))
                .min_by_key(|(seq, _)| *seq);
            let oldest_deployment = self.deployment.entries.front().map(|entry| entry.seq);
            let dropped = match (oldest_root, oldest_deployment) {
                (Some((root_seq, root)), Some(deployment_seq)) if root_seq < deployment_seq => {
                    self.roots.get_mut(&root).is_some_and(RootEvents::drop_oldest)
                }
                (Some(_), Some(_)) | (None, Some(_)) => self.deployment.drop_oldest(),
                (Some((_, root)), None) => self.roots.get_mut(&root).is_some_and(RootEvents::drop_oldest),
                (None, None) => false,
            };
            if !dropped {
                break;
            }
            self.roots
                .retain(|_, events| !events.entries.is_empty() || events.dropped > 0);
        }
    }
}

/// What a reader gets: one trajectory's entries, the deployment-wide ones that fall in the
/// same span, and what the bounds took from each.
///
/// The two lists are reported separately because their truncation is separate: a reader must
/// be able to tell "this trajectory's early hooks were evicted" from "a reload was evicted".
#[derive(Debug, Default)]
pub(crate) struct Events {
    pub(crate) entries: Vec<RecordedEvent>,
    pub(crate) dropped: u64,
    pub(crate) dropped_through_seq: Option<u64>,
    pub(crate) deployment: Vec<RecordedEvent>,
    pub(crate) deployment_dropped: u64,
    pub(crate) deployment_dropped_through_seq: Option<u64>,
}

/// Which trajectory a caller who named none meant.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recent {
    One {
        root: String,
        age: std::time::Duration,
    },
    /// More than one trajectory was active in the window. Guessing between them would put
    /// one session's diagnostic in another session's report.
    Ambiguous,
    None,
}

impl EventLog {
    /// One trajectory's account, plus every deployment-wide entry from that trajectory's
    /// first retained sequence *through the log's current position* — not through the
    /// trajectory's own last entry. A reload that lands after a trajectory's final hook is
    /// exactly the kind of thing its report needs to show.
    pub(crate) fn events(&self, root: &TrajectoryId) -> Events {
        let Some(events) = self.roots.get(&root.0) else {
            return Events::default();
        };
        let from = events.entries.front().map_or(u64::MAX, |entry| entry.seq);
        let deployment: Vec<RecordedEvent> = self
            .deployment
            .entries
            .iter()
            .filter(|entry| entry.seq >= from)
            .cloned()
            .collect();
        Events {
            entries: events.entries.iter().cloned().collect(),
            dropped: events.dropped,
            dropped_through_seq: events.dropped_through_seq,
            deployment,
            deployment_dropped: self.deployment.dropped,
            deployment_dropped_through_seq: self.deployment.dropped_through_seq,
        }
    }

    /// The trajectory a caller who named none most likely means: the one that was active in
    /// the window, when there is exactly one.
    pub(crate) fn recent_root(&self, window: std::time::Duration) -> Recent {
        let now = SystemTime::now();
        let mut inside = self.roots.iter().filter_map(|(root, events)| {
            let last = events.entries.back()?;
            let age = now.duration_since(last.at).ok()?;
            (age <= window).then(|| (root.clone(), age))
        });
        match (inside.next(), inside.next()) {
            (Some((root, age)), None) => Recent::One { root, age },
            (Some(_), Some(_)) => Recent::Ambiguous,
            (None, _) => Recent::None,
        }
    }
}

/// Clamp the two fields an untrusted hook body can grow. `/hook` accepts an unbounded body,
/// so without this a count cap bounds records rather than memory.
fn clamp(event: RuntimeEvent) -> RuntimeEvent {
    match event {
        RuntimeEvent::Hook {
            event: kind,
            tool,
            dispatch,
            outcome,
            mut offers,
        } => {
            offers.truncate(MAX_OFFERS);
            RuntimeEvent::Hook {
                event: kind,
                tool: tool.map(clamp_name),
                dispatch,
                outcome,
                offers,
            }
        }
        RuntimeEvent::External {
            role,
            name,
            outcome,
            duration_ms,
            offer,
            dispatch,
        } => RuntimeEvent::External {
            role,
            name: clamp_name(name),
            outcome,
            duration_ms,
            offer,
            dispatch,
        },
        other => other,
    }
}

fn clamp_name(name: String) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name;
    }
    let mut cut = MAX_NAME_BYTES - ELISION.len();
    while cut > 0 && !name.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut clamped = name[..cut].to_string();
    clamped.push_str(ELISION);
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(tool: &str) -> RuntimeEvent {
        RuntimeEvent::Hook {
            event: HookKind::ToolCall,
            tool: Some(tool.to_string()),
            dispatch: None,
            outcome: HookOutcome::Allowed,
            offers: Vec::new(),
        }
    }

    fn root(name: &str) -> TrajectoryId {
        TrajectoryId(name.to_string())
    }

    /// The map's key for a trajectory, for the assertions that read the log directly.
    fn key(name: &str) -> String {
        name.to_string()
    }

    /// The whole point of allocating the sequence under the insert's lock.
    #[test]
    fn sequences_are_dense_and_ordered_across_lists() {
        let mut log = EventLog::default();
        for index in 0..10 {
            match index % 2 {
                0 => log.record(Some(&root("a")), hook("Read")),
                _ => log.record(None, hook("Read")),
            }
        }
        let mut seqs: Vec<u64> = log.roots[&key("a")]
            .entries
            .iter()
            .chain(log.deployment.entries.iter())
            .map(|entry| entry.seq)
            .collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn the_entry_cap_drops_oldest_and_says_how_far() {
        let mut log = EventLog::default();
        for _ in 0..(MAX_ENTRIES_PER_LIST + 1) {
            log.record(Some(&root("a")), hook("Read"));
        }
        let events = &log.roots[&key("a")];
        assert_eq!(events.entries.len(), MAX_ENTRIES_PER_LIST);
        assert_eq!(events.dropped, 1);
        assert_eq!(events.dropped_through_seq, Some(0));
        assert_eq!(events.entries.front().expect("the list is not empty").seq, 1);
    }

    #[test]
    fn the_root_cap_evicts_the_coldest_trajectory_whole() {
        let mut log = EventLog::default();
        for index in 0..=MAX_ROOTS {
            log.record(Some(&root(&format!("t{index}"))), hook("Read"));
        }
        assert_eq!(log.roots.len(), MAX_ROOTS);
        assert!(!log.roots.contains_key(&key("t0")), "the coldest root is gone");
        assert!(log.roots.contains_key(&format!("t{MAX_ROOTS}")));
    }

    #[test]
    fn a_long_tool_name_is_clamped_at_admission() {
        let mut log = EventLog::default();
        log.record(Some(&root("a")), hook(&"x".repeat(300)));
        let RuntimeEvent::Hook { tool, .. } = &log.roots[&key("a")].entries[0].event else {
            panic!("a hook event was recorded");
        };
        let tool = tool.as_ref().expect("the hook named a tool");
        assert!(tool.len() <= MAX_NAME_BYTES);
        assert!(tool.ends_with(ELISION));
    }

    /// Clamping must not split a character, whatever the byte budget lands on.
    #[test]
    fn clamping_respects_character_boundaries() {
        let clamped = clamp_name("é".repeat(200));
        assert!(clamped.len() <= MAX_NAME_BYTES);
        assert!(clamped.ends_with(ELISION));
        assert!(clamped[..clamped.len() - ELISION.len()].chars().all(|c| c == 'é'));
    }

    #[test]
    fn offers_are_clamped_at_admission() {
        let mut log = EventLog::default();
        log.record(
            Some(&root("a")),
            RuntimeEvent::Hook {
                event: HookKind::ToolCall,
                tool: None,
                dispatch: None,
                outcome: HookOutcome::Denied,
                offers: (0..100).map(|index| index.to_string()).collect(),
            },
        );
        let RuntimeEvent::Hook { offers, .. } = &log.roots[&key("a")].entries[0].event else {
            panic!("a hook event was recorded");
        };
        assert_eq!(offers.len(), MAX_OFFERS);
    }

    /// An `Option` that is `None` and an empty `Vec` must not reach the wire: a reader of a
    /// report should see the fields an event actually carries.
    #[test]
    fn absent_fields_are_omitted_from_the_wire() {
        let rendered = serde_json::to_value(RuntimeEvent::Hook {
            event: HookKind::SessionStart,
            tool: None,
            dispatch: None,
            outcome: HookOutcome::Acked,
            offers: Vec::new(),
        })
        .expect("a runtime event serializes");
        let object = rendered.as_object().expect("an event renders as an object");
        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, vec!["event", "kind", "outcome"]);
    }

    /// Nested enums keep their own casing unless each one says otherwise, and a report's
    /// reader should not have to know which of them were tagged.
    #[test]
    fn nested_enums_render_in_snake_case() {
        let rendered = serde_json::to_value(RuntimeEvent::External {
            role: ExternalRole::AudienceSource,
            name: "directory".to_string(),
            outcome: ExternalOutcome::NoAnswer(NoAnswerClass::Unreachable),
            duration_ms: 12,
            offer: None,
            dispatch: None,
        })
        .expect("a runtime event serializes");
        assert_eq!(rendered["kind"], "external");
        assert_eq!(rendered["role"], "audience_source");
        assert_eq!(rendered["outcome"]["no_answer"], "unreachable");
    }

    /// The byte budget, not the entry caps: filling with tiny events hits the per-list count
    /// cap long before 64 MiB, so this fills with the largest entry the clamps allow.
    #[test]
    fn the_byte_budget_evicts_across_both_lists_oldest_first() {
        fn fat() -> RuntimeEvent {
            RuntimeEvent::Hook {
                event: HookKind::ToolCall,
                tool: Some("x".repeat(MAX_NAME_BYTES)),
                dispatch: None,
                outcome: HookOutcome::Denied,
                offers: (0..MAX_OFFERS).map(|index| format!("{index:064}")).collect(),
            }
        }

        let mut log = EventLog::default();
        log.record(Some(&root("a")), fat());
        log.record(None, fat());
        // Spread across many roots, because one root is capped at MAX_ENTRIES_PER_LIST.
        let mut filled = 0;
        while log.bytes() < MAX_TOTAL_BYTES && filled < MAX_ROOTS * MAX_ENTRIES_PER_LIST {
            log.record(Some(&root(&format!("f{}", filled % (MAX_ROOTS - 2)))), fat());
            filled += 1;
        }
        assert!(log.bytes() <= MAX_TOTAL_BYTES, "the budget is enforced");
        assert!(
            log.roots.get(&key("a")).is_none_or(|events| events.entries.is_empty()),
            "the oldest entry went first"
        );
    }
}

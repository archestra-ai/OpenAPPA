//! The classification inventory: one table per aggregate a fact can reach.
//!
//! Every entry describes the **emitted JSON**, not the Rust struct. Where the two differ —
//! and several engine types make them differ deliberately — the comment says so. The rules
//! themselves, and why a missing table is safe rather than a leak, are in [`super::strip`].
//!
//! `Fact` is externally tagged, so a serialized fact is a one-key object whose key is the
//! variant name. [`FACT`] is that outer table; every value under it recurses into a table
//! below. Enums appear the same way, except that a *unit* variant is a bare string — those are
//! listed with [`Rule::Keep`], which is what the walk demands of them.

use serde_json::Value;

use super::strip::{Rule, Table};
use super::tokens::Class;

/// The harness's own session id, which names the machine and often the person.
const TRAJECTORY: Rule = Rule::Token(Class::Trajectory);
/// A content digest: the correlation key a reader reconstructs the decision sequence from.
///
/// Tokenized, not carried as it stands. Within one report a token correlates exactly as the
/// hex does; outside one, the hex is an unsalted SHA-256 of the very tool name, arguments and
/// output bytes this export promises not to carry, and a recipient who guesses them can
/// confirm the guess. The correlation is what a reader needs; the oracle is not.
const DIGEST: Rule = Rule::Token(Class::Digest);
/// A counter, a rank, a version, a boolean: engine arithmetic with no name in it.
const NUMBER: Rule = Rule::Keep;

// ---------------------------------------------------------------- labels and audiences

static CLAUSE: Table = Table {
    name: "Clause",
    entries: &[
        // A closed set the engine defines: `self`, `internal`.
        ("chain", Rule::Keep),
        // `GroupRef` serializes as one packed string, always `@`-marked.
        ("groups", Rule::Each(&Rule::PackedName)),
        // Reader ids are people: email addresses, account ids.
        ("readers", Rule::Elements(Class::Reader)),
    ],
};

static LABEL: Table = Table {
    name: "Label",
    entries: &[
        // `Trust` is a numeric rank on the wire; only the policy names the ranks.
        ("trust", NUMBER),
        // `Audience` is `#[serde(transparent)]` over its clause set, so the wrapper level does
        // not exist in the JSON: this is an array of clauses directly.
        ("audience", Rule::Each(&Rule::Table(&CLAUSE))),
    ],
};

static DECLARED_AUDIENCE: Table = Table {
    name: "DeclaredAudience",
    entries: &[("Public", Rule::Keep), ("Union", Rule::Table(&CLAUSE))],
};

static NARROWING: Table = Table {
    name: "Narrowing",
    entries: &[("from", Rule::Table(&LABEL)), ("to", Rule::Table(&LABEL))],
};

// ---------------------------------------------------------------- audience evidence

/// One member as an audience source described them. Both fields name the same person in two
/// spellings, and both are that person's identity.
static MEMBER_CLAIMS: Table = Table {
    name: "MemberClaims",
    entries: &[
        ("id", Rule::Token(Class::Reader)),
        ("verified_email", Rule::Token(Class::Reader)),
    ],
};

static SOURCE_CLAIMS: Table = Table {
    name: "SourceClaims",
    entries: &[
        ("provider", Rule::Token(Class::Source)),
        ("selector", Rule::Token(Class::Selector)),
        ("members", Rule::Each(&Rule::Table(&MEMBER_CLAIMS))),
    ],
};

static MEMBER_LOOKUP: Table = Table {
    name: "MemberLookup",
    entries: &[
        ("provider", Rule::Token(Class::Source)),
        ("member", Rule::Token(Class::Reader)),
        ("claims", Rule::Table(&MEMBER_CLAIMS)),
    ],
};

static IDENTITY_MAPPING: Table = Table {
    name: "IdentityMapping",
    entries: &[
        ("id", Rule::Token(Class::Reader)),
        ("principal", Rule::Token(Class::Reader)),
    ],
};

static EVIDENCE: Table = Table {
    name: "AudienceEvidence",
    entries: &[
        ("sources", Rule::Each(&Rule::Table(&SOURCE_CLAIMS))),
        ("lookups", Rule::Each(&Rule::Table(&MEMBER_LOOKUP))),
        ("identity", Rule::Each(&Rule::Table(&IDENTITY_MAPPING))),
    ],
};

// ---------------------------------------------------------------- identities and subjects

static DISPATCH_ID: Table = Table {
    name: "DispatchId",
    entries: &[("trajectory", TRAJECTORY), ("digest", DIGEST), ("occurrence", NUMBER)],
};

static CHILD_RETURN_ID: Table = Table {
    name: "ChildReturnId",
    entries: &[("child", TRAJECTORY), ("occurrence", NUMBER)],
};

static SUBJECT_CALL: Table = Table {
    name: "SubjectKey::Call",
    entries: &[("trajectory", TRAJECTORY), ("batch", DIGEST), ("position", NUMBER)],
};

static SUBJECT_KEY: Table = Table {
    name: "SubjectKey",
    entries: &[
        ("Call", Rule::Table(&SUBJECT_CALL)),
        ("Approval", DIGEST),
        ("ConfinedResult", Rule::Table(&DISPATCH_ID)),
        ("Return", Rule::Table(&CHILD_RETURN_ID)),
    ],
};

static DECIDED_ACT: Table = Table {
    name: "DecidedAct",
    entries: &[
        ("Proposals", DIGEST),
        ("Outcome", Rule::Table(&DISPATCH_ID)),
        ("ChildReturn", Rule::Table(&CHILD_RETURN_ID)),
        // `ForkId` is a newtype over `DispatchId`, so it serializes as one.
        ("Binding", Rule::Table(&DISPATCH_ID)),
        ("Offer", DIGEST),
    ],
};

static POLICY_BASIS: Table = Table {
    name: "PolicyBasis",
    entries: &[("family", NUMBER), ("flow", NUMBER), ("subject", NUMBER)],
};

static BASIS_ADVANCE: Table = Table {
    name: "BasisAdvance",
    entries: &[
        ("family", NUMBER),
        ("flows", Rule::Elements(Class::Trajectory)),
        ("subjects", Rule::Each(&Rule::Table(&SUBJECT_KEY))),
    ],
};

// ---------------------------------------------------------------- annotations

static RECIPIENT_SPEC: Table = Table {
    name: "RecipientSpec",
    entries: &[
        ("Static", Rule::Table(&DECLARED_AUDIENCE)),
        // The tool argument the recipients are read from. The policy names it, but it names
        // the same key space a call's arguments do, so it is numbered with them.
        ("Placeholder", Rule::Token(Class::Argument)),
    ],
};

static AUDIENCE_REQUIREMENT: Table = Table {
    name: "AudienceRequirement",
    entries: &[
        ("Includes", Rule::Table(&RECIPIENT_SPEC)),
        ("Cap", Rule::Table(&DECLARED_AUDIENCE)),
    ],
};

static LABEL_REQUIREMENTS: Table = Table {
    name: "LabelRequirements",
    entries: &[
        ("trust_floor", NUMBER),
        ("audience", Rule::Each(&Rule::Table(&AUDIENCE_REQUIREMENT))),
    ],
};

static HISTORY_REQUIREMENT: Table = Table {
    name: "HistoryRequirement",
    entries: &[
        ("Prior", Rule::Token(Class::Effect)),
        ("NoPrior", Rule::Token(Class::Effect)),
    ],
};

static REQUIRES: Table = Table {
    name: "Requires",
    entries: &[
        ("label", Rule::Table(&LABEL_REQUIREMENTS)),
        ("history", Rule::Each(&Rule::Table(&HISTORY_REQUIREMENT))),
        ("attention", Rule::Elements(Class::Mark)),
    ],
};

static DELTA: Table = Table {
    name: "Delta",
    entries: &[("trust", NUMBER), ("audience", Rule::Table(&DECLARED_AUDIENCE))],
};

static PRODUCED_ANNOTATION: Table = Table {
    name: "ProducedAnnotation",
    entries: &[
        ("delta", Rule::Table(&DELTA)),
        // `EffectSet` is `#[serde(transparent)]` over its vector of names.
        ("emits", Rule::Elements(Class::Effect)),
        ("requires", Rule::Table(&REQUIRES)),
    ],
};

/// `PinnedAnnotation` is `#[serde(transparent)]` over its parts, so these are the parts' keys.
static PINNED_ANNOTATION: Table = Table {
    name: "PinnedAnnotation",
    entries: &[
        ("annotator", Rule::Token(Class::Annotator)),
        ("call", DIGEST),
        ("produced", Rule::Table(&PRODUCED_ANNOTATION)),
    ],
};

static RESOLVED_CALL: Table = Table {
    name: "ResolvedCall",
    entries: &[
        ("tool", Rule::VouchedTool),
        ("declaration", NUMBER),
        // `CanonicalArguments` serializes as a scalar string of canonical JSON, so the rule
        // parses it before taking its keys.
        ("arguments", Rule::ArgumentKeys),
        ("annotation", Rule::Table(&PINNED_ANNOTATION)),
    ],
};

// ---------------------------------------------------------------- gaps, plans, rulings

static GAP_TRUST_FLOOR: Table = Table {
    name: "Gap::TrustFloor",
    entries: &[("required", NUMBER), ("actual", NUMBER)],
};

static GAP_INCLUDES: Table = Table {
    name: "Gap::Includes",
    entries: &[("recipients", Rule::Table(&DECLARED_AUDIENCE))],
};

static GAP_CAP: Table = Table {
    name: "Gap::Cap",
    entries: &[("cap", Rule::Table(&DECLARED_AUDIENCE))],
};

static GAP: Table = Table {
    name: "Gap",
    entries: &[
        ("TrustFloor", Rule::Table(&GAP_TRUST_FLOOR)),
        ("Includes", Rule::Table(&GAP_INCLUDES)),
        ("Cap", Rule::Table(&GAP_CAP)),
        ("Prior", Rule::Token(Class::Effect)),
        ("NoPrior", Rule::Token(Class::Effect)),
        ("Attention", Rule::Token(Class::Mark)),
    ],
};

static REMEDY_STEP: Table = Table {
    name: "RemedyStep",
    entries: &[
        ("Authorize", Rule::Token(Class::Authority)),
        ("Accept", Rule::Table(&NARROWING)),
        ("Sanitize", Rule::Token(Class::Sanitizer)),
        ("Derive", Rule::Token(Class::Sanitizer)),
        ("Return", Rule::Token(Class::Sanitizer)),
    ],
};

static REQUIRED_RULING: Table = Table {
    name: "RequiredRuling",
    entries: &[
        ("authority", Rule::Token(Class::Authority)),
        ("covers", Rule::Each(&Rule::Table(&GAP))),
    ],
};

static EXECUTABLE_REMEDY_PLAN: Table = Table {
    name: "ExecutableRemedyPlan",
    entries: &[
        ("id", NUMBER),
        ("steps", Rule::Each(&Rule::Table(&REMEDY_STEP))),
        ("required", Rule::Each(&Rule::Table(&REQUIRED_RULING))),
    ],
};

static AUTHORITY_REVIEW: Table = Table {
    name: "AuthorityReview",
    entries: &[("tool", Rule::VouchedTool), ("trajectory_label", Rule::Table(&LABEL))],
};

static AUTHORITY_EVIDENCE: Table = Table {
    name: "AuthorityEvidence",
    entries: &[
        ("offer", DIGEST),
        ("authority", Rule::Token(Class::Authority)),
        ("covers", Rule::Each(&Rule::Table(&GAP))),
        ("reviewed", Rule::Table(&AUTHORITY_REVIEW)),
    ],
};

// ---------------------------------------------------------------- values and candidates

static LABELED_VALUE: Table = Table {
    name: "LabeledValue",
    entries: &[
        // `ValueBody` serializes as a bare string; only its length crosses.
        ("body", Rule::BodyBytes),
        ("label", Rule::Table(&LABEL)),
    ],
};

static CONFINED_FROM: Table = Table {
    name: "ConfinedFrom",
    entries: &[("Bound", Rule::Keep), ("Offer", DIGEST)],
};

static CANDIDATE_CALL: Table = Table {
    name: "DerivedCandidate::Call",
    entries: &[
        ("source", DIGEST),
        ("from", DIGEST),
        ("call", Rule::Table(&RESOLVED_CALL)),
        ("label", Rule::Table(&LABEL)),
    ],
};

static CANDIDATE_RESULT: Table = Table {
    name: "DerivedCandidate::Result",
    entries: &[
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        ("source", DIGEST),
        ("from", Rule::Table(&CONFINED_FROM)),
        ("value", Rule::Table(&LABELED_VALUE)),
        ("residual", Rule::Table(&NARROWING)),
    ],
};

static CANDIDATE_RETURN: Table = Table {
    name: "DerivedCandidate::Return",
    entries: &[("source", DIGEST), ("value", Rule::Table(&LABELED_VALUE))],
};

static DERIVED_CANDIDATE: Table = Table {
    name: "DerivedCandidate",
    entries: &[
        ("Call", Rule::Table(&CANDIDATE_CALL)),
        ("Result", Rule::Table(&CANDIDATE_RESULT)),
        ("Return", Rule::Table(&CANDIDATE_RETURN)),
    ],
};

static PROVENANCE_TOOL_RESULT: Table = Table {
    name: "Provenance::ToolResult",
    entries: &[("dispatch", Rule::Table(&DISPATCH_ID))],
};

static PROVENANCE_CHILD_RETURN: Table = Table {
    name: "Provenance::ChildReturn",
    entries: &[("child", TRAJECTORY), ("id", Rule::Table(&CHILD_RETURN_ID))],
};

static PROVENANCE_PROVIDER_RUN: Table = Table {
    name: "Provenance::ProviderRun",
    entries: &[
        ("tool", Rule::VouchedTool),
        ("batch", DIGEST),
        ("position", NUMBER),
        ("effects", Rule::Elements(Class::Effect)),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

static PROVENANCE: Table = Table {
    name: "Provenance",
    entries: &[
        ("ToolResult", Rule::Table(&PROVENANCE_TOOL_RESULT)),
        ("ChildReturn", Rule::Table(&PROVENANCE_CHILD_RETURN)),
        ("ProviderRun", Rule::Table(&PROVENANCE_PROVIDER_RUN)),
    ],
};

// ---------------------------------------------------------------- forks and returns

static RETURN_SANITIZER: Table = Table {
    name: "ReturnSanitizer",
    entries: &[
        // `ReturnShape` has a hand-written `Serialize` that emits a JSON Schema document.
        ("Attest", Rule::ReturnSchema),
        ("Named", Rule::Token(Class::Sanitizer)),
    ],
};

static RETURN_POLICY: Table = Table {
    name: "ReturnPolicy",
    entries: &[
        ("floor", Rule::Table(&LABEL)),
        ("sanitizer", Rule::Table(&RETURN_SANITIZER)),
    ],
};

static RETURN_DERIVATION_SANITIZED: Table = Table {
    name: "ReturnDerivation::Sanitized",
    entries: &[("sanitizer", Rule::Token(Class::Sanitizer)), ("raw_digest", DIGEST)],
};

static RETURN_DERIVATION: Table = Table {
    name: "ReturnDerivation",
    entries: &[
        ("Raw", Rule::Keep),
        ("Sanitized", Rule::Table(&RETURN_DERIVATION_SANITIZED)),
    ],
};

static FORK_SNAPSHOT: Table = Table {
    name: "ForkSnapshot",
    entries: &[
        ("base", Rule::Table(&LABEL)),
        // `ValueId` is a counter, not a name.
        ("inherited", Rule::Each(&NUMBER)),
        ("seed", Rule::Table(&LABEL)),
    ],
};

static BOUNDARY_MERGE: Table = Table {
    name: "BoundaryKind::Merge",
    entries: &[("child_return", Rule::Table(&CHILD_RETURN_ID))],
};

static BOUNDARY_RESUME: Table = Table {
    name: "BoundaryKind::Resume",
    entries: &[("seed", Rule::Table(&LABEL))],
};

static BOUNDARY_KIND: Table = Table {
    name: "BoundaryKind",
    entries: &[
        ("Merge", Rule::Table(&BOUNDARY_MERGE)),
        ("VoidReturn", Rule::Keep),
        ("Resume", Rule::Table(&BOUNDARY_RESUME)),
    ],
};

// ---------------------------------------------------------------- the deployment profile

static PROFILE: Table = Table {
    name: "DeploymentProfile",
    entries: &[
        ("starting_label", Rule::Table(&LABEL)),
        ("context_control", NUMBER),
        // `ExecutorClass`, `SurfaceMode` and `BindingMode` are closed snake_case enums.
        ("dispatch", Rule::Keep),
        ("executor_exceptions", Rule::MapKeys(Class::Tool, &Rule::Keep)),
        // A `BTreeSet<ToolName>` serializes to an array: no map-key rule reaches it.
        ("confined_results", Rule::Elements(Class::Tool)),
        ("provider_surfaces", Rule::MapKeys(Class::Surface, &Rule::Keep)),
        ("binding", Rule::Keep),
    ],
};

static OPEN_VECTOR_TOOL: Table = Table {
    name: "OpenVector::tool",
    entries: &[("tool", Rule::VouchedTool)],
};

static OPEN_VECTOR_SURFACE: Table = Table {
    name: "OpenVector::surface",
    entries: &[("surface", Rule::Token(Class::Surface))],
};

static OPEN_VECTOR: Table = Table {
    name: "OpenVector",
    entries: &[
        ("AssumedExecutor", Rule::Table(&OPEN_VECTOR_TOOL)),
        ("ProviderRunDispatch", Rule::Table(&OPEN_VECTOR_TOOL)),
        ("OpenProviderSurface", Rule::Table(&OPEN_VECTOR_SURFACE)),
    ],
};

// ---------------------------------------------------------------- the facts themselves

static TRAJECTORY_OPENED: Table = Table {
    name: "TrajectoryOpened",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("dialect", NUMBER),
        ("profile", Rule::Table(&PROFILE)),
        // Neither names a person, but together they fingerprint the deployment, which is
        // exactly what a pseudonymized report must not carry from one report to the next.
        ("policy_digest", Rule::Fingerprint),
        ("policy_file_key", Rule::Fingerprint),
        ("open_vectors", Rule::Each(&Rule::Table(&OPEN_VECTOR))),
    ],
};

static VALUE_ADMITTED: Table = Table {
    name: "ValueAdmitted",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("value", Rule::Table(&LABELED_VALUE)),
        ("provenance", Rule::Table(&PROVENANCE)),
    ],
};

static DISPATCH_OPENED: Table = Table {
    name: "DispatchOpened",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        ("tool", Rule::VouchedTool),
        ("declaration", NUMBER),
        ("arguments", Rule::ArgumentKeys),
        ("proposed_label", Rule::Table(&LABEL)),
        ("receiving", Rule::Table(&LABEL)),
        ("proposed_effects", Rule::Elements(Class::Effect)),
        ("annotation", Rule::Table(&PINNED_ANNOTATION)),
        ("evidence", Rule::Table(&EVIDENCE)),
        ("subject", Rule::Table(&SUBJECT_KEY)),
    ],
};

static OBSERVED_RESULT: Table = Table {
    name: "ObservedResult",
    entries: &[("Available", DIGEST), ("Unavailable", Rule::Keep)],
};

static DISPATCH_SUCCEEDED: Table = Table {
    name: "DispatchSucceeded",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        ("effects", Rule::Elements(Class::Effect)),
        ("observed", Rule::Table(&OBSERVED_RESULT)),
    ],
};

static CLOSE_SUCCESS: Table = Table {
    name: "CloseOutcome::Success",
    entries: &[("effects", Rule::Elements(Class::Effect))],
};

static CLOSE_OUTCOME: Table = Table {
    name: "CloseOutcome",
    entries: &[
        ("Success", Rule::Table(&CLOSE_SUCCESS)),
        ("Failure", Rule::Keep),
        ("Indeterminate", Rule::Keep),
    ],
};

static DISPATCH_CLOSED: Table = Table {
    name: "DispatchClosed",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        ("outcome", Rule::Table(&CLOSE_OUTCOME)),
    ],
};

static RULING: Table = Table {
    name: "Ruling",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        ("plan", NUMBER),
        ("authority", Rule::Token(Class::Authority)),
        ("covers", Rule::Each(&Rule::Table(&GAP))),
        ("reviewed", Rule::Table(&AUTHORITY_REVIEW)),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

static DENIAL: Table = Table {
    name: "Denial",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("digest", DIGEST),
        ("authority", Rule::Token(Class::Authority)),
    ],
};

static ACCEPTANCE: Table = Table {
    name: "Acceptance",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        ("plan", NUMBER),
        ("narrowing", Rule::Table(&NARROWING)),
    ],
};

static OUTPUT_SANITIZER_BOUND: Table = Table {
    name: "OutputSanitizerBound",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        ("plan", NUMBER),
        ("sanitizer", Rule::Token(Class::Sanitizer)),
        ("contribution", Rule::Table(&LABEL)),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

static CANDIDATE_DERIVED: Table = Table {
    name: "CandidateDerived",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("subject", Rule::Table(&SUBJECT_KEY)),
        ("sanitizer", Rule::Token(Class::Sanitizer)),
        ("derived", Rule::Table(&DERIVED_CANDIDATE)),
        // `SanitizerLineage` serializes through `into = "Vec<SanitizerName>"`.
        ("lineage", Rule::Elements(Class::Sanitizer)),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

static CANDIDATE_ACCEPTED: Table = Table {
    name: "CandidateAccepted",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("subject", Rule::Table(&SUBJECT_KEY)),
        ("offer", DIGEST),
        ("narrowing", Rule::Table(&NARROWING)),
    ],
};

static CHILD_RETURN: Table = Table {
    name: "ChildReturn",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("id", Rule::Table(&CHILD_RETURN_ID)),
        ("value", Rule::Table(&LABELED_VALUE)),
        ("derivation", Rule::Table(&RETURN_DERIVATION)),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

static PROPOSAL_BATCH_DECIDED: Table = Table {
    name: "ProposalBatchDecided",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("batch", DIGEST),
        ("proposals", Rule::Each(&Rule::Table(&RESOLVED_CALL))),
        // `SpawnMark` is the position of the marked proposal.
        ("spawn", NUMBER),
        ("released", Rule::Each(&Rule::Table(&DISPATCH_ID))),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

static OFFER_OPENED: Table = Table {
    name: "OfferOpened",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("offer", DIGEST),
        ("block", DIGEST),
        ("act", Rule::Table(&DECIDED_ACT)),
        ("call", DIGEST),
        ("subject", Rule::Table(&SUBJECT_KEY)),
        ("plan", Rule::Table(&EXECUTABLE_REMEDY_PLAN)),
        ("basis", Rule::Table(&POLICY_BASIS)),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

/// `OfferAccepted` and `OfferInvalidated` carry the same two fields. The name is the shared
/// one, not either variant's: it is what a drift report prints, and naming one of the two
/// would file the other's drift under a fact it did not come from.
static OFFER_LIFECYCLE: Table = Table {
    name: "OfferLifecycle",
    entries: &[("trajectory", TRAJECTORY), ("offer", DIGEST)],
};

static OFFER_DENIED: Table = Table {
    name: "OfferDenied",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("offer", DIGEST),
        ("authority", Rule::Token(Class::Authority)),
    ],
};

static CALL_APPROVED: Table = Table {
    name: "CallApproved",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("offer", DIGEST),
        ("call", Rule::Table(&RESOLVED_CALL)),
        ("plan", NUMBER),
        ("acceptance", Rule::Table(&NARROWING)),
        ("rulings", Rule::Each(&Rule::Table(&AUTHORITY_EVIDENCE))),
        ("sanitizer", Rule::Token(Class::Sanitizer)),
        ("return_policy", Rule::Table(&RETURN_POLICY)),
        ("basis", Rule::Table(&POLICY_BASIS)),
        ("evidence", Rule::Table(&EVIDENCE)),
    ],
};

static CALL_APPROVAL_CONSUMED: Table = Table {
    name: "CallApprovalConsumed",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("offer", DIGEST),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
    ],
};

static BASIS_ADVANCED: Table = Table {
    name: "BasisAdvanced",
    entries: &[
        ("trajectory", TRAJECTORY),
        ("act", Rule::Table(&DECIDED_ACT)),
        ("advance", Rule::Table(&BASIS_ADVANCE)),
    ],
};

static FORK_PREPARED: Table = Table {
    name: "ForkPrepared",
    entries: &[
        ("trajectory", TRAJECTORY),
        // `ForkId` is a newtype over `DispatchId`.
        ("fork", Rule::Table(&DISPATCH_ID)),
        ("snapshot", Rule::Table(&FORK_SNAPSHOT)),
        ("return_policy", Rule::Table(&RETURN_POLICY)),
    ],
};

static FORK_OPENED: Table = Table {
    name: "ForkOpened",
    entries: &[("trajectory", TRAJECTORY), ("fork", Rule::Table(&DISPATCH_ID))],
};

static BOUNDARY: Table = Table {
    name: "Boundary",
    entries: &[("trajectory", TRAJECTORY), ("kind", Rule::Table(&BOUNDARY_KIND))],
};

/// The outer table: `Fact` is externally tagged, so each key is a variant name. A variant
/// added to the engine and not added here becomes `"<unclassified>"` with its name recorded,
/// which the inventory test turns into a failure naming the variant.
pub(crate) static FACT: Table = Table {
    name: "Fact",
    entries: &[
        ("TrajectoryOpened", Rule::Table(&TRAJECTORY_OPENED)),
        ("ValueAdmitted", Rule::Table(&VALUE_ADMITTED)),
        ("DispatchOpened", Rule::Table(&DISPATCH_OPENED)),
        ("DispatchSucceeded", Rule::Table(&DISPATCH_SUCCEEDED)),
        ("DispatchClosed", Rule::Table(&DISPATCH_CLOSED)),
        ("Ruling", Rule::Table(&RULING)),
        ("Denial", Rule::Table(&DENIAL)),
        ("Acceptance", Rule::Table(&ACCEPTANCE)),
        ("OutputSanitizerBound", Rule::Table(&OUTPUT_SANITIZER_BOUND)),
        ("CandidateDerived", Rule::Table(&CANDIDATE_DERIVED)),
        ("CandidateAccepted", Rule::Table(&CANDIDATE_ACCEPTED)),
        ("ChildReturn", Rule::Table(&CHILD_RETURN)),
        ("ProposalBatchDecided", Rule::Table(&PROPOSAL_BATCH_DECIDED)),
        ("OfferOpened", Rule::Table(&OFFER_OPENED)),
        ("OfferAccepted", Rule::Table(&OFFER_LIFECYCLE)),
        ("OfferDenied", Rule::Table(&OFFER_DENIED)),
        ("OfferInvalidated", Rule::Table(&OFFER_LIFECYCLE)),
        ("CallApproved", Rule::Table(&CALL_APPROVED)),
        ("CallApprovalConsumed", Rule::Table(&CALL_APPROVAL_CONSUMED)),
        ("BasisAdvanced", Rule::Table(&BASIS_ADVANCED)),
        ("ForkPrepared", Rule::Table(&FORK_PREPARED)),
        ("ForkOpened", Rule::Table(&FORK_OPENED)),
        ("Boundary", Rule::Table(&BOUNDARY)),
    ],
};

// ------------------------------------------------------------- the runtime's own events

// `RuntimeEvent` is *internally* tagged, so every variant's fields share one object with the
// `kind` key. A single table would have to give `outcome` one rule, and `outcome` is three
// unrelated enums across the five variants — so the table is chosen by the tag instead, in
// [`event_table`]. An external's `name` needs the tag *and* its role, because the name belongs
// to whichever registry the role points at, and a sanitizer numbered among the authorities
// would be a token nobody can cross-reference.

static NON_SUCCESS: Table = Table {
    name: "NoAnswerClass::NonSuccess",
    entries: &[("status", NUMBER)],
};

static NO_ANSWER_CLASS: Table = Table {
    name: "NoAnswerClass",
    entries: &[
        ("unregistered", Rule::Keep),
        ("unreachable", Rule::Keep),
        ("dismissed", Rule::Keep),
        ("non_success", Rule::Table(&NON_SUCCESS)),
        ("timeout", Rule::Keep),
        ("transport", Rule::Keep),
        ("malformed", Rule::Keep),
        ("oversized", Rule::Keep),
        ("unsupported_version", Rule::Keep),
        ("module_error", Rule::Keep),
        ("module_panicked", Rule::Keep),
    ],
};

static EXTERNAL_OUTCOME: Table = Table {
    name: "ExternalOutcome",
    entries: &[("answered", Rule::Keep), ("no_answer", Rule::Table(&NO_ANSWER_CLASS))],
};

static HOOK_EVENT: Table = Table {
    name: "RuntimeEvent::Hook",
    entries: &[
        ("kind", Rule::Keep),
        ("event", Rule::Keep),
        ("tool", Rule::VouchedTool),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
        // Every `HookOutcome` is a unit variant, so this is always a closed-set string.
        ("outcome", Rule::Keep),
        // Offer ids: content digests, like every other correlation key.
        ("offers", Rule::Each(&DIGEST)),
    ],
};

/// The five external tables differ only in what kind of name `name` is.
macro_rules! external_table {
    ($ident:ident, $name:literal, $class:expr) => {
        static $ident: Table = Table {
            name: $name,
            entries: &[
                ("kind", Rule::Keep),
                ("role", Rule::Keep),
                ("name", Rule::Token($class)),
                ("outcome", Rule::Table(&EXTERNAL_OUTCOME)),
                ("duration_ms", NUMBER),
                ("offer", DIGEST),
                ("dispatch", Rule::Table(&DISPATCH_ID)),
            ],
        };
    };
}

external_table!(
    EXTERNAL_AUTHORITY,
    "RuntimeEvent::External(authority)",
    Class::Authority
);
external_table!(
    EXTERNAL_SANITIZER,
    "RuntimeEvent::External(sanitizer)",
    Class::Sanitizer
);
external_table!(
    EXTERNAL_ANNOTATOR,
    "RuntimeEvent::External(annotator)",
    Class::Annotator
);
external_table!(
    EXTERNAL_SOURCE,
    "RuntimeEvent::External(audience_source)",
    Class::Source
);
external_table!(EXTERNAL_IDENTITY, "RuntimeEvent::External(identity)", Class::Identity);

static CONTROL_CALL: Table = Table {
    name: "ControlCall",
    entries: &[
        // `ControlCall` is internally tagged on `tool`, and the tool is APPA's own.
        ("tool", Rule::Keep),
        ("offer", DIGEST),
        ("dispatch", Rule::Table(&DISPATCH_ID)),
    ],
};

static CONTROL_EVENT: Table = Table {
    name: "RuntimeEvent::Control",
    entries: &[
        ("kind", Rule::Keep),
        ("call", Rule::Table(&CONTROL_CALL)),
        ("outcome", Rule::Keep),
        ("duration_ms", NUMBER),
    ],
};

static RELOAD_EVENT: Table = Table {
    name: "RuntimeEvent::Reload",
    entries: &[
        ("kind", Rule::Keep),
        ("policy_key", Rule::Fingerprint),
        ("changed", Rule::Keep),
    ],
};

static STORE_ERROR_EVENT: Table = Table {
    name: "RuntimeEvent::StoreError",
    entries: &[("kind", Rule::Keep), ("operation", Rule::Keep), ("class", Rule::Keep)],
};

/// The table for one serialized runtime event, read from the tag it carries. `None` when the
/// tag names no variant this inventory knows — drift the caller reports and carries nothing of.
pub(crate) fn event_table(event: &Value) -> Option<&'static Table> {
    match event.get("kind")?.as_str()? {
        "hook" => Some(&HOOK_EVENT),
        "external" => match event.get("role")?.as_str()? {
            "authority" => Some(&EXTERNAL_AUTHORITY),
            "sanitizer" => Some(&EXTERNAL_SANITIZER),
            "annotator" => Some(&EXTERNAL_ANNOTATOR),
            "audience_source" => Some(&EXTERNAL_SOURCE),
            "identity" => Some(&EXTERNAL_IDENTITY),
            _ => None,
        },
        "control" => Some(&CONTROL_EVENT),
        "reload" => Some(&RELOAD_EVENT),
        "store_error" => Some(&STORE_ERROR_EVENT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    use crate::events::{
        ControlCall, ControlOutcome, ExternalOutcome, ExternalRole, HookKind, HookOutcome, NoAnswerClass, RuntimeEvent,
        StoreOperation,
    };
    use crate::yell::strip::strip;
    use crate::yell::tokens::{Mode, Tokens};

    fn dispatch() -> appa_engine::value::DispatchId {
        appa_engine::value::DispatchId::new(
            appa_engine::value::TrajectoryId::new("cc:session"),
            serde_json::from_value(serde_json::json!(
                "38142c4d026dba0e8f82124bf7d95f1edd7f8ab8e348f41fd276ec1af59c1a63"
            ))
            .expect("a hex digest parses"),
            0,
        )
    }

    /// Every variant of the runtime's own account, with every optional field present.
    ///
    /// The recorded-session fixture in `yell::diagnostic` covers hook events only: a live
    /// session reaches no external in process, never reloads, and does not fail a store. Those
    /// four tables would otherwise ship unverified, and a wrong key in one of them is a
    /// classified field silently turned into drift.
    fn every_event() -> Vec<RuntimeEvent> {
        let external = |role, outcome| RuntimeEvent::External {
            role,
            name: "the-name".to_string(),
            outcome,
            duration_ms: 12,
            offer: Some("f610dbd5610171965d4de357b2e0acbe".to_string()),
            dispatch: Some(dispatch()),
        };
        vec![
            RuntimeEvent::Hook {
                event: HookKind::ToolCall,
                tool: Some("Bash".to_string()),
                dispatch: Some(dispatch()),
                outcome: HookOutcome::Denied,
                offers: vec!["f86ce7ba8e2e552d".to_string()],
            },
            external(ExternalRole::Authority, ExternalOutcome::Answered),
            external(
                ExternalRole::Sanitizer,
                ExternalOutcome::NoAnswer(NoAnswerClass::Timeout),
            ),
            external(
                ExternalRole::Annotator,
                // The one no-answer class carrying a payload of its own.
                ExternalOutcome::NoAnswer(NoAnswerClass::NonSuccess { status: 502 }),
            ),
            external(
                ExternalRole::AudienceSource,
                ExternalOutcome::NoAnswer(NoAnswerClass::ModulePanicked),
            ),
            external(ExternalRole::Identity, ExternalOutcome::Answered),
            RuntimeEvent::Control {
                call: ControlCall::Remedy {
                    offer: "f610dbd5610171965d4de357b2e0acbe".to_string(),
                    dispatch: Some(dispatch()),
                },
                outcome: ControlOutcome::Declined,
                duration_ms: 7,
            },
            RuntimeEvent::Reload {
                policy_key: "a91f".to_string(),
                changed: true,
            },
            RuntimeEvent::StoreError {
                operation: StoreOperation::Append,
                class: appa_eventlog::StoreErrorClass::Conflict,
            },
        ]
    }

    #[test]
    fn every_runtime_event_is_classified() {
        let mut tokens = Tokens::default();
        for event in every_event() {
            let value = serde_json::to_value(&event).expect("a runtime event serializes");
            let table = event_table(&value).unwrap_or_else(|| panic!("no table for {value}"));
            let stripped = strip(&value, table, &mut tokens, Mode::Pseudonymized, &BTreeSet::new());
            assert!(
                stripped.unclassified.is_empty(),
                "{} is not covered: {:?}",
                table.name,
                stripped.unclassified
            );
            // The status of a non-success is the one scalar buried two enums deep, so seeing
            // it proves the nested tables were walked rather than merely matched.
            if let Some(status) = value.pointer("/outcome/no_answer/non_success/status") {
                assert_eq!(
                    stripped.value.pointer("/outcome/no_answer/non_success/status"),
                    Some(status)
                );
            }
        }
    }

    /// An external's name belongs to whichever registry its role points at, so the same
    /// spelling under two roles must not become one token.
    #[test]
    fn an_external_name_is_numbered_in_its_own_role_s_class() {
        let mut tokens = Tokens::default();
        let mut stripped = |event: &RuntimeEvent| {
            let value = serde_json::to_value(event).expect("serializes");
            let table = event_table(&value).expect("a table");
            strip(&value, table, &mut tokens, Mode::Pseudonymized, &BTreeSet::new()).value["name"]
                .as_str()
                .expect("a token")
                .to_string()
        };
        let events = every_event();
        let authority = stripped(&events[1]);
        let sanitizer = stripped(&events[2]);
        assert_eq!(authority, "authority-1");
        assert_eq!(sanitizer, "sanitizer-1");
    }
}

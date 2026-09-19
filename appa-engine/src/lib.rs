//! # appa-engine — the OpenAPPA pure decision core
//!
//! The engine is a *function of the event log*: it converts untrusted tool-call bytes into a
//! [`ResolvedCall`](crate::value::ResolvedCall), then evaluates that call against the log's cached
//! views. It returns a decision plus a validated batch of facts to append. It performs no IO,
//! reads no clock, and never mutates a store — an outer runtime owns state and appends the batch.
//!
//! This crate is the reference for engine concepts and semantics: what a term means here is
//! what it means across APPA.
//!
//! The model is two monoids: a **checked** monoid of label actions
//! (audience × trust) and a **free** monoid of events. Propagation folds the label
//! restrictively (min trust, intersect audience) into the trajectory's one concrete label
//! ([`label::Label`] — there is no partial or pending label state anywhere in the algebra);
//! checking is the sink-side comparison against that label. The two are never conflated.
//!
//! The audience dimension is **symbolic**: a canonical intersection of union clauses
//! ([`label::Audience`]) over the built-in audience chain `self` ⊆ `internal` ⊆ `public`,
//! group references (`@finance`, `@slack:user-group/eng`), and literal readers. Symbols
//! survive in labels and durable events. A check answers from a sound derivability calculus
//! over policy-declared facts — the chain, `within` assertions — where that suffices, and
//! otherwise evaluates the exact denotation from the operation's pinned evidence: primitive
//! source answers and member lookups ([`audience`]), from which principal substitution,
//! union, and the symmetric `within` closure are recomputed on replay. A failed
//! derivation never denies; a missing answer is a membership ask, never a label state.
//!
//! Every released tool call carries one complete concrete annotation
//! ([`contract::ToolAnnotation`]): its delta, its requirements, and the effects it emits.
//! The [`contract::ToolDeclaration`] names the annotation's one producer — `Declared`, the
//! declaration is its own annotation, or `Annotated`, a registered Annotator consulted per
//! call whose answer is bounded by its mandate and pinned ([`contract::PinnedAnnotation`])
//! to the producing Annotator and the call's canonical digest, so a rewrite is annotated
//! afresh and replay never consults again. The
//! wildcard declaration (`"*"`) routes every call the policy does not name through an
//! Annotator; a call nothing covers is refused before it runs, and an annotation that fails
//! to arrive is an operational refusal, never a policy denial.
//!
//! ## Two fork lifecycles
//!
//! A **subagent fork** branches within one family log. A released spawn records
//! [`fact::Fact::ForkPrepared`]; [`fact::Fact::ForkOpened`] binds its child trajectory.
//! Its [`fact::ForkSnapshot`] refers to source values in that same log, and a child return
//! crosses back to its parent only through the checked return path. Family-wide effect
//! history remains shared.
//!
//! A **root fork** opens an independent family from an existing trajectory. Its
//! [`fact::RootForkOrigin`] is recorded on the new root's [`fact::Fact::TrajectoryOpened`],
//! not on a `ForkOpened` child binding. It freezes the source label, family effects and
//! unsettled reservations, and source denials. Replay reads that opening record without
//! reading the parent log; later activity on either side does not update the other.
//! No spawn dispatch or child-return contract is created. The runtime must also preserve
//! the source family's opening policy when it creates the new log.
//!
//! ## File content: a pinned basis, and two labels
//!
//! A call that touches a file the runtime tracks carries a host-pinned [`value::FileBasis`]
//! on its [`value::ResolvedCall`]: the version, digest and Label of the content it reads,
//! replaces, edits, copies, moves or processes, all supplied by the trusted harness and
//! never by model arguments. The basis is part of the call's proposal-batch identity, so the
//! same rendered call over different content is a different act, while the call's own
//! canonical digest stays the digest of its tool and arguments.
//!
//! Two labels follow from it, and they are not the same label:
//!
//! - [`value::ResolvedCall::output_label`] is what the call returns to the trajectory. A
//!   read returns content and carries the file's Label into the trajectory; a write or a
//!   transfer returns a constant acknowledgement, so its content does not enter the
//!   trajectory's Label and only a later read does.
//! - [`value::ResolvedCall::file_output_label`] is what the call publishes as file content.
//!   A copy or move puts the source's Label there even though the trajectory never sees the
//!   bytes, and a replacement drops the Label of the content it destroyed.
//!
//! Checking a call's requirements reads both: the committed label a successful call would
//! leave the trajectory at, folded with the content the call would publish. So a copy into a
//! destination is checked against the source's Label, and no narrowing of the trajectory can
//! make that requirement go away. A file call that fails admits its error text at the same
//! value Label a success would have used ([`transition::ToolOutcome::FailureWithBody`]), so
//! an error can never say more than the call it came from.
//!
pub mod admit;
pub mod audience;
pub mod authority;
pub mod basis;
pub mod branch;
pub mod candidate;
pub mod check;
pub mod contract;
pub mod engine;
pub mod execute;
pub mod fact;
mod hex32;
pub mod label;
pub mod names;
pub mod params;
pub mod plan;
pub mod profile;
pub mod projection;
pub mod registry;
pub mod route;
pub mod shape;
pub mod transition;
pub mod value;

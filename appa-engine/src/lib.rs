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
//! source answers, member lookups, and identity mappings ([`audience`]), from which identity
//! application, union, and the symmetric `within` closure are recomputed on replay. A failed
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

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
//! restrictively (min trust, intersect audience) into a partial label — an established bound
//! plus the unresolved source identities per dimension; checking is the
//! sink-side adequacy relation, unresolved at exactly the consumers that care. The
//! two are never conflated.
//!
pub mod admit;
pub mod authority;
pub mod basis;
pub mod branch;
pub mod candidate;
pub mod check;
pub mod contract;
pub mod engine;
pub mod execute;
pub mod fact;
pub mod groups;
pub mod label;
pub mod names;
pub mod params;
pub mod plan;
pub mod profile;
pub mod projection;
pub mod registry;
pub mod shape;
pub mod transition;
pub mod value;

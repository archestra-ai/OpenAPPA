//! # appa-engine — the OpenAPPA pure decision core
//!
//! The engine is a *function of the event log*: given the log's cached views and a
//! [`ResolvedCall`](crate::value::ResolvedCall) the runtime has already resolved, it returns a
//! decision plus a validated batch of facts to append. It performs no IO, reads no clock, and
//! never mutates a store — the outer [`appa-runtime`] layer owns state and appends the batch.
//!
//! This crate is the reference for engine concepts and semantics (per `CLAUDE.md` document
//! precedence). It implements `docs/spec.md`, which is normative: where the two disagree, the
//! spec is right and this crate has drift to close.
//!
//! The model is two monoids (see `docs/spec.md`): a **checked** monoid of label actions
//! (audience × trust) and a **free** monoid of events. Propagation folds the label
//! restrictively (min trust, intersect audience); checking is the sink-side adequacy relation.
//! The two are never conflated.
//!
//! [`appa-runtime`]: ../appa_runtime/index.html

pub mod admit;
pub mod authority;
pub mod branch;
pub mod check;
pub mod contract;
pub mod engine;
pub mod execute;
pub mod fact;
pub mod label;
pub mod names;
pub mod plan;
pub mod projection;
pub mod registry;
pub mod value;

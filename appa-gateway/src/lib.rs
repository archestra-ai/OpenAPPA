//! # appa-gateway - OpenAPPA's compatibility HTTP adapter
//!
//! This crate preserves the OpenAI-compatible `/v1/chat/completions` north contract, strict
//! admission profile, tenant/session ownership, and disconnect cancellation behavior. Canonical
//! mediation and trajectory state live in [`appa_runtime::Mediator`]; inference orchestration lives
//! in [`appa_agent::Agent`].

pub mod admission;
pub mod drive;
pub mod inference;
pub mod runtime;
pub mod server;

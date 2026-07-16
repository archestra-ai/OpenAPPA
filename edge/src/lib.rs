//! # appa-edge
//!
//! The layer between protocol adapters and `appa-core`. Protocol-agnostic:
//! it knows conversations and verdicts, never wire formats.
//!
//! Every embedding of appa-core hand-rolls the same logic: build a
//! trajectory from conversation history, label ingress, translate proposed
//! tool calls into requests, run `pursue`, act on the verdict, drive the
//! dispatch cycle. This is the code where a mistake is a security hole;
//! appa-edge implements it once.
//!
//! - appa-edge is the only caller of the engine. Engine construction is
//!   encapsulated in [`Session::new`] — the single seam contracts pass
//!   through.
//! - appa-edge owns the I/O around the engine: the outbound
//!   [`AuthorityResolver`] leg and the dispatch cycle around an adapter's
//!   executor. appa-core stays pure, synchronous, and never calls anyone.
//! - Protocol translation stays in the adapters; they drive a [`Session`]
//!   directly and render its typed [`Verdict`]s into their own wire text.
//! - Every label enters at ingress as a required argument — the edge has no
//!   default. Every failure fails closed: no ruling is ever fabricated, a
//!   cancelled in-flight operation poisons the session, and a blocked
//!   outcome never degrades to a permit.

pub mod error;
pub mod resolver;
pub mod session;

pub use error::EdgeError;
pub use resolver::{AuthorityResolver, NoResolver, ResolveError, WebhookResolver};
pub use session::{ProposedCall, Session, Verdict, describe};

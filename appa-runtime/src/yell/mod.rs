//! Reporting that APPA is in the way.
//!
//! A report carries what APPA decided and nothing about what the agent was working on. The
//! boundary is enforced in one place: [`strip`] walks a serialized fact or runtime event
//! against the deny-by-default inventory in [`tables`], the same walk reads the deployment's
//! own policy against the second inventory in [`policy`], [`tokens`] holds the report-local
//! substitution that replaces the names a deployment chose, and [`diagnostic`] assembles one
//! trajectory's export from them.

pub(crate) mod diagnostic;
pub(crate) mod policy;
pub(crate) mod strip;
pub(crate) mod tables;
pub(crate) mod tokens;

pub(crate) use diagnostic::{Diagnostic, OmittedReason, RECENT_WINDOW, Selection, Source, branches, build, resolve};
pub(crate) use tokens::Mode;

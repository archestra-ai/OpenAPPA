//! Reporting that APPA is in the way.
//!
//! A report carries what APPA decided and nothing about what the agent was working on. The
//! boundary is enforced in one place: [`strip`] walks a serialized fact or runtime event
//! against the deny-by-default inventory in [`tables`], the same walk reads the deployment's
//! own policy against the second inventory in [`policy`], [`tokens`] holds the report-local
//! substitution that replaces the names a deployment chose, [`diagnostic`] assembles one
//! trajectory's export from them, and [`report`] puts that export in the envelope a receiver
//! accepts.

pub mod cli;
pub(crate) mod client;
pub(crate) mod diagnostic;
pub(crate) mod policy;
pub(crate) mod report;
pub(crate) mod strip;
pub(crate) mod tables;
pub(crate) mod tokens;

pub(crate) use diagnostic::{
    Budget, Diagnostic, OmittedReason, Projection, RECENT_WINDOW, Selection, Source, branches, build, resolve,
};
pub(crate) use report::{Author, Finished, Origin, Oversize, Report, ReportId, ReportRequest, YellMessage};
pub(crate) use tokens::Mode;

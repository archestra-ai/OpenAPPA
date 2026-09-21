//! Reporting that APPA is in the way.
//!
//! A report carries what APPA decided and nothing about what the agent was working on. The
//! boundary is enforced in one place: [`strip`] walks a serialized fact or runtime event
//! against the deny-by-default inventory in [`tables`], the same walk reads the deployment's
//! own policy against the second inventory in [`policy`], [`tokens`] holds the report-local
//! substitution that replaces the names a deployment chose, [`diagnostic`] assembles one
//! trajectory's export from them, and [`report`] puts that export in the envelope a receiver
//! accepts.

pub(crate) mod agent;
#[cfg(feature = "daemon")]
pub mod cli;
#[cfg(feature = "daemon")]
pub(crate) mod client;
#[cfg(feature = "daemon")]
pub(crate) mod diagnostic;
#[cfg(feature = "daemon")]
pub mod embedded;
#[cfg(feature = "daemon")]
pub(crate) mod policy;
#[cfg(feature = "daemon")]
pub(crate) mod report;
#[cfg(feature = "daemon")]
pub(crate) mod strip;
#[cfg(feature = "daemon")]
pub(crate) mod tables;
#[cfg(feature = "daemon")]
pub(crate) mod tokens;

pub(crate) use agent::YellArgs;
#[cfg(feature = "daemon")]
pub(crate) use diagnostic::{
    Budget, Diagnostic, OmittedReason, Projection, RECENT_WINDOW, Selection, Source, branches, build, resolve,
};
#[cfg(feature = "daemon")]
pub use report::Harness;
#[cfg(feature = "daemon")]
pub(crate) use report::{Author, Finished, Origin, Oversize, Report, ReportId, ReportRequest, YellMessage};
#[cfg(feature = "daemon")]
pub(crate) use tokens::Mode;

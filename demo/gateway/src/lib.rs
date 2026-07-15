//! baton-demo: the ad-hoc demo harnesses for the baton prototype, kept out of
//! the shared workspace so the parked approval flow cannot break the build.

pub mod gateway;

#[cfg(feature = "approver")]
pub mod approval;

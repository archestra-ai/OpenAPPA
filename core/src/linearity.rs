//! Compile-time proof that the linear capabilities stay linear.
//!
//! The capability family — [`ExecutionToken`](crate::engine::ExecutionToken),
//! [`DispatchReceipt`](crate::engine::DispatchReceipt),
//! [`StepCapability`](crate::engine::StepCapability),
//! [`PendingApproval`](crate::approval::PendingApproval) — is `Serialize`-only
//! and non-`Clone`, and deserializing one would forge the linearity these
//! types exist to enforce. That rule is load-bearing and was previously carried
//! by prose alone: the `compile_fail` doctests move a capability twice, which
//! proves only that it is not `Copy`. A `Clone` derive would leave those
//! doctests passing (Rust never clones implicitly), and nothing at all pinned
//! the absence of `Deserialize`.
//!
//! Stable Rust has no negative bound (`T: !Clone`), so each assertion below
//! resolves a marker trait that becomes ambiguous exactly when the type
//! implements the forbidden trait — the type checker then refuses to pick an
//! impl, and the crate stops compiling.

use serde::{Deserialize, Serialize};

use crate::approval::PendingApproval;
use crate::engine::{DispatchReceipt, ExecutionToken, StepCapability};

macro_rules! assert_not_impl {
    ($ty:ty: $bound:path) => {
        const _: () = {
            trait Marker<A> {
                const PROOF: () = ();
            }
            struct Forbidden;
            impl<T: ?Sized> Marker<()> for T {}
            impl<T: ?Sized + $bound> Marker<Forbidden> for T {}
            let _ = <$ty as Marker<_>>::PROOF;
        };
    };
}

macro_rules! assert_impl {
    ($ty:ty: $bound:path) => {
        const _: () = {
            const fn proof<T: ?Sized + $bound>() {}
            proof::<$ty>();
        };
    };
}

assert_impl!(ExecutionToken: Serialize);
assert_impl!(DispatchReceipt: Serialize);
assert_impl!(StepCapability: Serialize);
assert_impl!(PendingApproval: Serialize);

assert_not_impl!(ExecutionToken: Clone);
assert_not_impl!(DispatchReceipt: Clone);
assert_not_impl!(StepCapability: Clone);
assert_not_impl!(PendingApproval: Clone);

assert_not_impl!(ExecutionToken: Deserialize<'static>);
assert_not_impl!(DispatchReceipt: Deserialize<'static>);
assert_not_impl!(StepCapability: Deserialize<'static>);
assert_not_impl!(PendingApproval: Deserialize<'static>);

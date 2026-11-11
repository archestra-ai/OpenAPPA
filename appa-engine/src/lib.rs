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
//! source answers and member lookups ([`audience`]), from which principal substitution,
//! union, and the symmetric `within` closure are recomputed on replay. A failed
//! derivation never denies; a missing answer is a membership ask, never a label state.
//!
//! Every released tool call carries one complete concrete annotation
//! ([`contract::ToolAnnotation`]): its delta, what it forwards, its requirements, and the
//! effects it emits. The delta labels the call's result and narrows the trajectory once the
//! call succeeds; it never gates the call. `forwards` labels content the call itself sends
//! beyond the agent's arguments, such as a file a tool reads server-side and mails.
//! The [`contract::ToolDeclaration`] names the annotation's one producer — `Declared`, the
//! declaration is its own annotation, or `Annotated`, a registered Annotator consulted per
//! call whose answer is bounded by its mandate and pinned ([`contract::PinnedAnnotation`])
//! to the producing Annotator and the call's canonical digest, so a rewrite is annotated
//! afresh and replay never consults again. The
//! wildcard declaration (`"*"`) routes every call without a matching explicit contract through an
//! Annotator; a call nothing covers is refused before it runs, and an annotation that fails
//! to arrive is an operational refusal, never a policy denial.
//!
//! ## Two fork lifecycles
//!
//! A **subagent fork** branches within one family log. A released spawn records
//! [`fact::Fact::ForkPrepared`]; [`fact::Fact::ForkOpened`] binds its child trajectory.
//! Its [`fact::ForkSnapshot`] refers to source values in that same log, and a child return
//! crosses back to its parent only through the checked return path. Family-wide effect
//! history remains shared.
//!
//! A marked spawn releases by spending an approval that carries its child's **return
//! declaration**. Declaring asks no authority: a blocked spawn with no declaration is offered
//! the declarations alone, and the approval one of them prepares releases the spawn when nothing
//! else blocks it. Otherwise the retry is planned on its remaining gaps, and the approval that
//! covers them carries the declaration that stands; the release that spends it spends the
//! declaration too, so a declaration covers one spawn. A spawn whose other gaps have no plan
//! but those naming an authority that denied it is offered no declaration. Like any approval, a
//! declaration stops being current when the parent's label moves.
//!
//! A **fan-out spawn** ([`transition::SpawnKind::FanOut`]) starts any number of children under
//! the one return declaration its release consumed. Its own fork binds no child. Each child it
//! starts gets a member fork ([`value::ForkId::Member`]), prepared by the engine's binding
//! decision under that declaration and seeded at the parent's label as it stands at the child's
//! start, so a child started after a sibling's return crossed starts at the label that return
//! left on the parent. Every member's return crosses separately, exactly as a single spawn's
//! does.
//!
//! A **root fork** opens an independent family from an existing trajectory. Its
//! [`fact::RootForkOrigin`] is recorded on the new root's [`fact::Fact::TrajectoryOpened`],
//! not on a `ForkOpened` child binding. It freezes the source label, family effects and
//! unsettled reservations, and source denials. Replay reads that opening record without
//! reading the parent log; later activity on either side does not update the other.
//! No spawn dispatch or child-return contract is created. The new family belongs to the
//! source family's deployment.
//!
//! ## Recorders: what a call publishes
//!
//! The engine knows no file, object, or store. Labels on objects that live outside the
//! trajectory — a file on disk, a row in a database — belong to two external components
//! the policy names on a tool declaration. Its **Annotator** reads the object's stored label
//! and answers the call's annotation: a read's `delta` carries the label into the
//! trajectory, an edit's `forwards` carries its predecessor into what the call sends. Its
//! **recorder** ([`names::RecorderName`]) receives the call's records after they are
//! appended, and stores what the call published. The recorder is named by the declaration,
//! never chosen by the Annotator, and a recorder is never consulted: it cannot deny, delay,
//! or grant anything.
//!
//! [`engine::Engine::published_label`] is what a released call publishes:
//! `trajectory ⊓ forwards ⊓ delta`. `trajectory` is the label the call's
//! [`fact::Fact::DispatchOpened`] pinned as it was released — the substituted label, for a
//! call an input sanitizer rewrote — so a write publishes everything the agent could have put
//! in its arguments, and a copy publishes its source's label through `delta` even though the
//! trajectory never sees the bytes. The label derives from the dispatch's own record, so it
//! is the same on every replay; a recorder that loses state rebuilds it from the log.
//!
//! An Annotator answer may carry opaque `evidence` ([`contract::AnnotationEvidence`]): the
//! version of the object it read a label from, for instance. The evidence is pinned and
//! hashed with the answer, so the same rendered call judged over two versions of an object
//! is two proposal batches, and it reaches the recorder with the call's records.
//!
//! The one host-pinned content the engine still knows is a peer message the runtime holds
//! ([`value::HeldMessage`]): a call that reads it returns it at the held label, and a call
//! that fails admits its error text at that label
//! ([`transition::ToolOutcome::FailureWithBody`]), so an error never says more than the
//! message it came from.
//!
//! ## Policy revisions
//!
//! A **deployment** ([`profile::DeploymentName`]) is a policy line that advances through numbered
//! **policy revisions** ([`profile::PolicyRevision`]). Every family follows a deployment: its
//! opening record names the deployment and the revision that opened it. An [`engine::Engine`]
//! decides only at a revision ([`engine::Engine::at`]); an engine [`engine::Engine::open`] compiled
//! and no revision placed opens, replays and decides nothing. A deciding engine stamps every batch
//! it seals with its revision, on the `BasisAdvanced` record the batch begins with, so the family's
//! next decision is always made at the deployment's head and a family's stamps never decrease.
//!
//! [`engine::Engine::replay`] judges each batch under the engine of the revision stamped on it,
//! so a later revision never changes what an earlier record meant. An earlier revision's engine
//! only validates; replay consults nothing. Only a batch of host observations alone carries no
//! stamp; a batch that holds engine records and names no revision is refused. No decision is made
//! on a view stamped later than the deciding engine's revision.
//!
//! Labels index the trust chain of the revision that wrote them. Where a later revision's chain
//! names other ranks, the labels a view holds cross into it by rank name
//! ([`label::TrustCrossing`]): a value at a rank the new chain drops falls to its lowest rank,
//! and a return floor at such a rank rises to its highest. Audience atoms are symbolic and cross
//! unchanged.
//!
//! Deferred authorizations do not cross. A view that crosses to a later revision advances its
//! family basis, so no offer or approval an earlier revision recorded is current: executing such
//! an offer consults nothing, an approval releases nothing, and the call is decided afresh under
//! the later revision. A derived candidate keeps its derivation, and the call or result it stands
//! for is planned again from it.
//!
//! A call released under one revision and reported under a later one is judged by the
//! [`fact::ReleaseTerms`] its `DispatchOpened` record pinned, met with the later revision's
//! declaration of the call: the result's label is the meet of both, and the effects are those
//! the release reserved. A result whose release bound an output sanitizer that the later
//! revision no longer registers is withheld.
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

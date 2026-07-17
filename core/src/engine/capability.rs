use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::ToolName;
use crate::approval::PendingApproval;
use crate::audit::AuthorityName;
use crate::contract::{AudienceRule, Requirements, Violation};
use crate::dimension::{Effect, Effects};
use crate::plan::{NonEmptyVec, RemedyPlan};
use crate::request::{ArgumentName, ArgumentSchema};
use crate::revision::{ActionId, PlanId, Revision, ValueId};
use crate::turn::TrajectoryId;
use crate::value::ValueLabel;

use super::EngineId;

pub(crate) const RESPONSE_SINK: &str = "assistant.response";

/// A tool's annotation: what it demands of a flow, the intrinsic label its
/// results wear, the effects running it proposes, and where its argument
/// tree carries typed roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolContract {
    pub name: ToolName,
    /// What the trajectory must satisfy before this tool runs. `None` means
    /// the requirements were never stated — every call escalates as
    /// [`crate::contract::Unprovable::RequirementsUnknown`], fail closed;
    /// `Some(Requirements::default())` means considered, nothing required.
    pub requires: Option<Requirements>,
    pub output_label: ValueLabel,
    /// Effects one dispatch of this tool proposes; committed to the monotone
    /// past when dispatch begins.
    pub effects: Effects,
    pub arguments: ArgumentSchema,
}

impl ToolContract {
    /// A pure read: no requirements, no effects, opaque arguments. A
    /// dependency-free call's output wears exactly `output_label`; argument
    /// and control dependencies fold in and can only worsen it.
    pub fn source(name: impl Into<String>, output_label: ValueLabel) -> Self {
        Self {
            name: ToolName::new(name),
            requires: Some(Requirements::default()),
            output_label,
            effects: Effects::none(),
            arguments: ArgumentSchema::opaque(),
        }
    }

    /// An egress sink: recipients are read from the top-level argument
    /// `recipients_arg` and must lie within the flow's audience; one dispatch
    /// proposes an `Egress` effect. The output wears the identity label.
    pub fn egress_sink(name: impl Into<String>, recipients_arg: impl Into<String>) -> Self {
        Self {
            name: ToolName::new(name),
            requires: Some(Requirements {
                audience: AudienceRule::FromRecipients,
                ..Requirements::default()
            }),
            output_label: ValueLabel::identity(),
            effects: Effects::declared([Effect::Egress]),
            arguments: ArgumentSchema::with_recipients(ArgumentName::new(recipients_arg)),
        }
    }
}

/// Proof that the engine authorized one tool call — the only way to append a
/// tool result to a [`Trajectory`](crate::turn::Trajectory). Bound to the trajectory, its exact
/// revision, and the pending action, so any state change invalidates it.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ExecutionToken {
    pub(super) action: ActionId,
    pub(super) tool: ToolName,
    pub(super) intrinsic: ValueLabel,
    pub(super) arguments: BTreeSet<ValueId>,
    pub(super) control: BTreeSet<ValueId>,
    pub(super) proposed_effects: Effects,
    pub(super) trajectory: TrajectoryId,
    pub(super) revision: Revision,
}

pub(crate) struct TokenParts {
    pub(crate) action: ActionId,
    pub(crate) tool: ToolName,
    pub(crate) intrinsic: ValueLabel,
    pub(crate) arguments: BTreeSet<ValueId>,
    pub(crate) control: BTreeSet<ValueId>,
    pub(crate) proposed_effects: Effects,
    pub(crate) trajectory: TrajectoryId,
    pub(crate) revision: Revision,
}

impl ExecutionToken {
    pub fn action(&self) -> ActionId {
        self.action
    }

    pub(crate) fn into_parts(self) -> TokenParts {
        TokenParts {
            action: self.action,
            tool: self.tool,
            intrinsic: self.intrinsic,
            arguments: self.arguments,
            control: self.control,
            proposed_effects: self.proposed_effects,
            trajectory: self.trajectory,
            revision: self.revision,
        }
    }
}

/// The owned, canonically rendered request handed to the adapter at release
/// time. Produced from the exact argument tree the engine checked; adapters
/// execute this and never re-render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalRequest {
    pub action: ActionId,
    pub tool: ToolName,
    /// Deterministic rendering of the checked argument tree: the engine
    /// renders once at release and adapters execute this verbatim.
    pub rendered: String,
}

/// The linear receipt minted at release: the only way to admit the dispatched
/// tool's output — or declare its failure — and close the action. Bound to
/// the trajectory and to the action's `Released` phase, deliberately *not* to
/// the revision: a receipt closes an external dispatch that already happened,
/// and refusing it cannot undo that dispatch, so an unrelated later mutation
/// (a checked emission, a new value) must never wedge the action open.
/// Tokens, step capabilities, and approvals authorize *future* state changes
/// and stay revision-bound; the receipt records a past one. Linearity
/// (non-`Clone`, consumed on use) plus the `Released`-phase check keep one
/// receipt closing one action exactly once.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct DispatchReceipt {
    action: ActionId,
    tool: ToolName,
    intrinsic: ValueLabel,
    arguments: BTreeSet<ValueId>,
    control: BTreeSet<ValueId>,
    trajectory: TrajectoryId,
}

pub(crate) struct ReceiptParts {
    pub(crate) action: ActionId,
    pub(crate) tool: ToolName,
    pub(crate) intrinsic: ValueLabel,
    pub(crate) arguments: BTreeSet<ValueId>,
    pub(crate) control: BTreeSet<ValueId>,
    pub(crate) trajectory: TrajectoryId,
}

impl DispatchReceipt {
    pub fn action(&self) -> ActionId {
        self.action
    }

    pub(crate) fn from_token_parts(parts: TokenParts) -> Self {
        Self {
            action: parts.action,
            tool: parts.tool,
            intrinsic: parts.intrinsic,
            arguments: parts.arguments,
            control: parts.control,
            trajectory: parts.trajectory,
        }
    }

    pub(crate) fn into_parts(self) -> ReceiptParts {
        ReceiptParts {
            action: self.action,
            tool: self.tool,
            intrinsic: self.intrinsic,
            arguments: self.arguments,
            control: self.control,
            trajectory: self.trajectory,
        }
    }
}

/// A linear capability ([`ExecutionToken`] or [`DispatchReceipt`]) was
/// refused: it no longer (or never did) describe that trajectory's state, so
/// the flow must be re-evaluated. The capability is consumed either way.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RejectedToken {
    #[error("token was minted for {minted_for}, not {this}")]
    ForeignTrajectory {
        minted_for: TrajectoryId,
        this: TrajectoryId,
    },
    #[error("token minted at {minted_at}, but the trajectory is now at {current}")]
    Stale { minted_at: Revision, current: Revision },
    #[error("action {action} is not pending on this trajectory")]
    ActionNotPending { action: ActionId },
}

/// The linear capability to apply one plan step. Bound to the trajectory,
/// its exact revision, the checked flow, and the exact plan and step; minted
/// by [`PolicyEngine::mint_step`](crate::engine::PolicyEngine::mint_step) and consumed — success or failure — by
/// [`PolicyEngine::apply_step`](crate::engine::PolicyEngine::apply_step).
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct StepCapability {
    pub(super) plan: PlanId,
    pub(super) step: usize,
    pub(super) flow: crate::revision::FlowId,
    pub(super) trajectory: TrajectoryId,
    pub(super) revision: Revision,
    pub(super) engine: EngineId,
}

/// A step or approval interaction was refused without touching state: the
/// capability never described this trajectory's current state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepRefused {
    #[error("no stored plan {plan}")]
    UnknownPlan { plan: PlanId },
    #[error("plan minted at {basis}, but the trajectory is now at {current}")]
    StalePlan { basis: Revision, current: Revision },
    #[error("{plan} has no step {step}")]
    NoSuchStep { plan: PlanId, step: usize },
    #[error("only the plan's head step is executable; step {step} is predictive")]
    NotNextStep { step: usize },
    #[error("capability was minted for {minted_for}, not {this}")]
    ForeignTrajectory {
        minted_for: TrajectoryId,
        this: TrajectoryId,
    },
    #[error("capability was minted under {minted_by}, not {this}")]
    ForeignEngine { minted_by: EngineId, this: EngineId },
    #[error("flow {flow} has no pending proposal on this trajectory")]
    FlowNotPending { flow: crate::revision::FlowId },
}

/// The outcome of applying one plan step: a continuation within the
/// remediable flow, never a new policy outcome kind.
#[derive(Debug, Serialize)]
#[must_use = "a dropped StepOutcome loses the flow's continuation"]
pub enum StepOutcome {
    Advanced(FlowOutcome<FlowPermit>),
    NeedsApproval(PendingApproval),
    Failed(crate::audit::TransitionFailure),
}

/// [`PolicyEngine::register`](crate::engine::PolicyEngine::register) refused a contract: a contract for that tool is
/// already registered. Contracts are the policy boundary, so a silent replace
/// could weaken policy unnoticed — registration fails loudly instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a contract for `{tool}` is already registered")]
pub struct DuplicateContract {
    pub tool: ToolName,
}

/// Why a flow is terminally blocked: a *policy* outcome — the flow was
/// well-formed and the policy proved (or an authority ruled) it cannot
/// proceed. Protocol/state defects are [`FlowRefusal`]s, never here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum BlockReason {
    RequiresStructuralFix,
    NoRemedy,
    DeniedByAuthority { authority: AuthorityName, reason: String },
    PostconditionFailed,
    NoAuthorityRuled,
}

impl fmt::Display for BlockReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequiresStructuralFix => {
                write!(f, "a structural violation nothing may override")
            }
            Self::NoRemedy => write!(f, "the flow escalated and no remedy applies"),
            Self::DeniedByAuthority { authority, reason } => {
                write!(f, "denied by {authority}: {reason}")
            }
            Self::PostconditionFailed => {
                write!(f, "an applied remedy did not clear the checks it targeted")
            }
            Self::NoAuthorityRuled => {
                write!(f, "every competent authority abstained; no ruling was produced")
            }
        }
    }
}

/// An invalid, stale, foreign, or conflicting proposal, refused before any
/// policy judgment — outside the tri-state, touching no state (no revision
/// advance, no event, no cleared slot). Distinct from a [`BlockReason`]:
/// a refusal says "this request does not describe the trajectory's current
/// state", not "policy forbids this flow".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
pub enum FlowRefusal {
    #[error("{pending} is already pending on this trajectory")]
    ActionAlreadyPending { pending: ActionId },
    #[error("emission {flow} is already pending on this trajectory")]
    EmissionAlreadyPending { flow: crate::revision::FlowId },
    #[error("request references {value}, which this trajectory never admitted")]
    UnknownValueReferenced { value: ValueId },
    #[error("proposal composed at {composed_at}, but the trajectory is now at {current}")]
    StaleBasis { composed_at: Revision, current: Revision },
}

/// The one policy outcome of checking a well-formed flow proposal against
/// the current state, for every emission sink alike (a tool dispatch or an
/// assistant emission — `P` is the sink's permit payload).
#[derive(Debug, PartialEq, Eq, Serialize)]
#[must_use = "a dropped FlowOutcome loses the permit or the flow's continuation"]
pub enum FlowOutcome<P> {
    AllowedNow(P),
    Remediable {
        violations: Vec<Violation>,
        plans: NonEmptyVec<RemedyPlan>,
    },
    Terminal {
        violations: Vec<Violation>,
        reason: BlockReason,
    },
}

impl<P> FlowOutcome<P> {
    pub(crate) fn map_allowed<Q>(self, f: impl FnOnce(P) -> Q) -> FlowOutcome<Q> {
        match self {
            Self::AllowedNow(permit) => FlowOutcome::AllowedNow(f(permit)),
            Self::Remediable { violations, plans } => FlowOutcome::Remediable { violations, plans },
            Self::Terminal { violations, reason } => FlowOutcome::Terminal { violations, reason },
        }
    }
}

/// An emitted assistant response: the harness sends `rendered` — bytes
/// produced from the exact checked tree — and nothing else; there is no
/// separate raw model string that may be returned after the check.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[must_use = "a dropped Emitted loses the only bytes the check permitted"]
pub struct Emitted {
    pub value: ValueId,
    pub rendered: String,
}

/// The permit payload of a flow whose sink kind is dynamic (plan application
/// and approval re-entry work over stored plans, which may target a tool
/// action or a pending emission).
#[derive(Debug, PartialEq, Eq, Serialize)]
#[must_use = "a dropped FlowPermit means the flow was authorized but never carried out"]
pub enum FlowPermit {
    Execute(ExecutionToken),
    Emit(Emitted),
}

/// Policy for the reserved assistant-response sink: what an emission flow
/// must satisfy, and who reads the conversation (the sink's recipients).
/// Registration routes emissions through the same shared evaluation
/// pipeline as every tool sink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResponsePolicy {
    pub requires: Requirements,
    pub readers: BTreeSet<crate::dimension::UserId>,
}

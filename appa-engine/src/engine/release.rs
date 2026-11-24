//! Released content: the root's answer recorded at its label, the receipts that name released
//! records, and the judgment of a release into a sink.

use super::*;
use crate::check::{CallReads, Gap};
use crate::contract::ToolDeclaration;
use crate::fact::EffectKind;
use crate::label::Evaluation;
use crate::transition::AnswerReport;
use appa_runtime_api::ReleaseDigest;

/// The released record a receipt is asked for: the value admitted for one dispatch's result,
/// the latest return a child crossed into its parent, or the root's latest answer.
#[derive(Clone, Copy, Debug)]
pub enum Receipted<'a> {
    Result(&'a DispatchId),
    Return { child: &'a TrajectoryId },
    Answer { root: &'a TrajectoryId },
}

/// Why a receipt names no label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ReceiptRefusal {
    #[error("no released record stands at this position of the family log")]
    NotReleased,
    #[error("the released record at this position holds other bytes")]
    OtherBytes,
    #[error("the view stands at a policy revision this engine does not judge")]
    OtherRevision,
}

/// Why a sink is not a call a release may judge. A release judges no trajectory, so it has no
/// history to read and no proposal to annotate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum SinkRefusal {
    #[error("no declaration names the sink; the wildcard covers none")]
    Undeclared,
    #[error("the sink's declaration names an annotator")]
    Annotated,
    #[error("the sink's contract requires a history")]
    HistoryRequirement,
}

/// Why a release could not be judged.
#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error(transparent)]
    Sink(#[from] SinkRefusal),
    #[error(transparent)]
    Call(#[from] EngineError),
    #[error("the release reads symbolic audiences without pinned answers")]
    MembershipNeeded { needed: Vec<SymbolicAtom> },
    #[error(transparent)]
    Evidence(#[from] TransitionError),
}

/// The judgment of one release: the content's label meets the sink's requirements, alone or
/// with stock `approve` rulings over the gaps it misses; or it does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseJudgment {
    Released { approved_by: Vec<AuthorityName> },
    Withheld { gaps: Vec<Gap> },
}

impl Engine {
    /// Record the root's answer at the label the root holds. The same bytes at the same label
    /// as the root's latest answer stand recorded already.
    pub(super) fn decide_answer(
        &self,
        view: &EngineView,
        report: &AnswerReport,
    ) -> Result<EngineDecision, TransitionError> {
        let projection = view.projection();
        let trajectory = &report.trajectory;
        let views = projection.view(trajectory);
        if views.has_ended(trajectory) {
            return Err(TransitionError::BranchEnded);
        }
        let label = views.current_label();
        let standing = projection.answer_at(trajectory).filter(|position| {
            projection
                .released(*position)
                .is_some_and(|(digest, recorded)| digest == report.digest && *recorded == label)
        });
        if let Some(position) = standing {
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Answer { position },
            });
        }
        let answer = Fact::AnswerReleased {
            trajectory: trajectory.clone(),
            digest: report.digest,
            label,
        };
        let batch = self.decided(view, crate::basis::DecidedAct::Answer(trajectory.clone()), vec![answer])?;
        let offset = batch
            .facts()
            .iter()
            .position(|fact| matches!(fact, Fact::AnswerReleased { .. }))
            .expect("an answer's batch holds its answer");
        Ok(EngineDecision {
            follow_up: FollowUp::Answer {
                position: projection.folded() + offset as u64,
            },
            append: Some(batch),
        })
    }

    /// The position and digest of the released record `receipted` names.
    pub fn receipt_of(&self, view: &EngineView, receipted: Receipted<'_>) -> Option<(u64, ReleaseDigest)> {
        let projection = view.projection();
        let position = match receipted {
            Receipted::Result(dispatch) => projection.admitted_result_at(dispatch),
            Receipted::Return { child } => projection.crossed_return_at(child),
            Receipted::Answer { root } => projection.answer_at(root),
        }?;
        projection.released(position).map(|(digest, _)| (position, digest))
    }

    /// The label of the released record at `position` of the family log `view` replays, under
    /// this engine's trust chain: a rank crosses by its name.
    pub fn released_label(
        &self,
        view: &EngineView,
        position: u64,
        digest: &ReleaseDigest,
    ) -> Result<Label, ReceiptRefusal> {
        let view = self.current(view).map_err(|_| ReceiptRefusal::OtherRevision)?;
        match view.projection().released(position) {
            None => Err(ReceiptRefusal::NotReleased),
            Some((recorded, _)) if recorded != *digest => Err(ReceiptRefusal::OtherBytes),
            Some((_, label)) => Ok(label.clone()),
        }
    }

    /// Judge content at `label` into the sink call `tool` with `raw_arguments`, under this
    /// engine's registry. The sink's contract is checked as a call's would be, against no
    /// history; a gap stands lifted only when an authority `approves` admits covers it. Nothing
    /// is recorded.
    pub fn judge_release(
        &self,
        label: &Label,
        tool: ToolName,
        raw_arguments: &[u8],
        audience: AudienceEvidence,
        principal: Option<ReaderId>,
        approves: impl Fn(&AuthorityName) -> bool,
    ) -> Result<ReleaseJudgment, ReleaseError> {
        if self.registry.classify(&tool) != Some(ToolKind::Declared) {
            return Err(SinkRefusal::Undeclared.into());
        }
        let (call, declaration) = select_call(&self.registry, tool, raw_arguments)?;
        if let ToolDeclaration::Annotated { .. } = declaration {
            return Err(SinkRefusal::Annotated.into());
        }
        let contract = self.validated_contract(&call)?;
        if !contract.requires.history.is_empty() {
            return Err(SinkRefusal::HistoryRequirement.into());
        }
        let act = self.act_evidence(audience, AudienceEvidence::default(), principal)?;
        let context = self.context(&act);
        let no_history = |_: &EffectKind| false;
        let raw = check::evaluate_state(
            &contract,
            label,
            &no_history,
            &no_history,
            CallReads::Resolved(&call),
            &CallStage::default(),
            &context,
        )
        .map_err(|needed| ReleaseError::MembershipNeeded { needed: needed.needed })?;
        let judgment = match raw.is_hard() {
            true => ReleaseJudgment::Withheld {
                gaps: raw.requirement_gaps,
            },
            false => self.approved(&raw.requirement_gaps, &contract.tags, &context, &approves)?,
        };
        act.settle(self.registry.audience()).map_err(TransitionError::from)?;
        Ok(judgment)
    }

    /// Each gap covered by an authority `approves` admits, or the gaps none covers.
    fn approved(
        &self,
        gaps: &[Gap],
        tags: &[crate::names::TagName],
        context: &MembershipContext<'_>,
        approves: &impl Fn(&AuthorityName) -> bool,
    ) -> Result<ReleaseJudgment, ReleaseError> {
        let (mut approved_by, mut uncovered, mut needed) = (Vec::new(), Vec::new(), Vec::new());
        for gap in gaps {
            let mut cover = None;
            for authority in self
                .registry
                .authorities()
                .iter()
                .filter(|authority| approves(&authority.name))
            {
                match plan::gap_cover(authority, gap, tags, context) {
                    Evaluation::Holds => {
                        cover = Some(authority.name.clone());
                        break;
                    }
                    Evaluation::Needs(ask) => needed.extend(ask.needed),
                    Evaluation::Fails => {}
                }
            }
            match cover {
                Some(name) => approved_by.push(name),
                None => uncovered.push(gap.clone()),
            }
        }
        Ok(match (uncovered.is_empty(), needed.is_empty()) {
            (true, _) => {
                approved_by.sort();
                approved_by.dedup();
                ReleaseJudgment::Released { approved_by }
            }
            (false, true) => ReleaseJudgment::Withheld { gaps: uncovered },
            (false, false) => {
                needed.sort();
                needed.dedup();
                return Err(ReleaseError::MembershipNeeded { needed });
            }
        })
    }
}

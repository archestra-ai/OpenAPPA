//! Released content: the root's answer recorded at its label, an offer's review recorded at its
//! block's, the receipts that name released records, and the judgment of a release into a sink.

use super::*;
use crate::check::{CallReads, Gap};
use crate::contract::ToolDeclaration;
use crate::fact::EffectKind;
use crate::label::Evaluation;
use crate::transition::{Admitted, AnswerReport, ReleasedAt, ReviewReport, ValidatedFactBatch};
use appa_runtime_api::ReleaseDigest;

/// The value `batch` admits as a result or a crossed return, beside the position it lands at
/// once appended to the log `view` replays.
pub(super) fn admitted_in(view: &EngineView, batch: &ValidatedFactBatch) -> Option<Admitted> {
    batch.facts().iter().enumerate().find_map(|(offset, fact)| match fact {
        Fact::ValueAdmitted {
            value,
            provenance: Provenance::ToolResult { .. } | Provenance::ChildReturn { .. },
            ..
        } => Some(Admitted {
            body: value.body.clone(),
            position: view.projection().folded() + offset as u64,
        }),
        _ => None,
    })
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
    /// Answering the membership of any one alternative lets the judgment go on; the next round
    /// judges again with its answer.
    #[error("the release reads symbolic audiences without pinned answers")]
    MembershipNeeded { alternatives: Vec<Vec<SymbolicAtom>> },
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
    /// Record the root's answer at the label the root holds. The same bytes over the same terms
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
        if let Some(position) = projection.standing_answer(trajectory, &report.digest) {
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Released {
                    at: ReleasedAt {
                        position,
                        digest: report.digest,
                    },
                },
            });
        }
        let answer = Fact::AnswerReleased {
            trajectory: trajectory.clone(),
            digest: report.digest,
            label: views.current_label(),
        };
        let batch = self.decided(view, crate::basis::DecidedAct::Answer(trajectory.clone()), vec![answer])?;
        let offset = batch
            .facts()
            .iter()
            .position(|fact| matches!(fact, Fact::AnswerReleased { .. }))
            .expect("an answer's batch holds its answer");
        Ok(EngineDecision {
            follow_up: FollowUp::Released {
                at: ReleasedAt {
                    position: projection.folded() + offset as u64,
                    digest: report.digest,
                },
            },
            append: Some(batch),
        })
    }

    /// Record an offer's review at the label its blocked call was judged under. Only the bytes
    /// whose digest the offer's opening records are recorded; bytes recorded already for the
    /// offer stand as recorded.
    pub(super) fn decide_review(
        &self,
        view: &EngineView,
        report: &ReviewReport,
    ) -> Result<EngineDecision, TransitionError> {
        let projection = view.projection();
        let released = |position| FollowUp::Released {
            at: ReleasedAt {
                position,
                digest: report.digest,
            },
        };
        if let Some(position) = projection.standing_review(&report.offer, &report.digest) {
            return Ok(EngineDecision {
                append: None,
                follow_up: released(position),
            });
        }
        let (trajectory, label) = projection
            .offered_at(&report.offer)
            .ok_or(TransitionError::UnknownOffer)?;
        if projection.review_digest(&report.offer) != Some(report.digest) {
            return Err(TransitionError::OtherReview);
        }
        let review = Fact::ReviewReleased {
            trajectory,
            offer: report.offer,
            digest: report.digest,
            label,
        };
        let batch = self.decided(view, crate::basis::DecidedAct::Review(report.offer), vec![review])?;
        let offset = batch
            .facts()
            .iter()
            .position(|fact| matches!(fact, Fact::ReviewReleased { .. }))
            .expect("a review's batch holds its review");
        Ok(EngineDecision {
            follow_up: released(projection.folded() + offset as u64),
            append: Some(batch),
        })
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
            Some((_, label)) => Ok(label),
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
        .map_err(|needed| ReleaseError::MembershipNeeded {
            alternatives: vec![needed.needed],
        })?;
        let judgment = match raw.is_hard() {
            true => ReleaseJudgment::Withheld {
                gaps: raw.requirement_gaps,
            },
            false => self.approved(&raw.requirement_gaps, &contract.tags, &context, &approves)?,
        };
        act.settle(self.registry.audience()).map_err(TransitionError::from)?;
        Ok(judgment)
    }

    /// Each gap covered by an authority `approves` admits, or withheld at the first gap none of
    /// them can cover. Where the first gap left unresolved has only covers that read membership
    /// not yet answered, each such cover's membership is one alternative: answering any one of
    /// them lets the next round judge again.
    fn approved(
        &self,
        gaps: &[Gap],
        tags: &[crate::names::TagName],
        context: &MembershipContext<'_>,
        approves: &impl Fn(&AuthorityName) -> bool,
    ) -> Result<ReleaseJudgment, ReleaseError> {
        let eligible: Vec<_> = self
            .registry
            .authorities()
            .iter()
            .filter(|authority| approves(&authority.name))
            .collect();
        let (mut approved_by, mut unresolved) = (Vec::new(), None);
        for gap in gaps {
            let (mut cover, mut open) = (None, Vec::new());
            for authority in &eligible {
                match plan::gap_cover(authority, gap, tags, context) {
                    Evaluation::Holds => {
                        cover.get_or_insert_with(|| authority.name.clone());
                    }
                    Evaluation::Needs(ask) => open.push(ask.needed),
                    Evaluation::Fails => {}
                }
            }
            match (cover, open.is_empty()) {
                (Some(name), _) => approved_by.push(name),
                (None, false) => {
                    unresolved.get_or_insert(open);
                }
                (None, true) => {
                    return Ok(ReleaseJudgment::Withheld {
                        gaps: vec![gap.clone()],
                    });
                }
            }
        }
        match unresolved {
            Some(alternatives) => Err(ReleaseError::MembershipNeeded { alternatives }),
            None => {
                approved_by.sort();
                approved_by.dedup();
                Ok(ReleaseJudgment::Released { approved_by })
            }
        }
    }
}

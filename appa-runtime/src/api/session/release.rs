use super::*;
use appa_engine::engine::Receipted;
use appa_engine::label::Label;
use appa_runtime_api::{ReleaseDigest, ReleaseReceipt, ReleaseRefusal, RetryHint};

use crate::engine::ReleaseRound;

impl Session {
    /// Where this family's log records what `receipted` names.
    pub(crate) fn receipt(&self, receipted: Receipted<'_>) -> Result<Option<ReleaseReceipt>, EventError> {
        self.with_view(|context| Ok(context.engine().receipt(context.view, receipted)))
    }

    /// The receipt of the return this child's decision crossed into its parent.
    pub(crate) fn return_receipt(&self, decision: &ChildReturnDecision) -> Result<Option<ReleaseReceipt>, EventError> {
        match decision {
            ChildReturnDecision::Returned { .. } => self.receipt(Receipted::Return {
                child: &self.trajectory,
            }),
            ChildReturnDecision::Staged { .. }
            | ChildReturnDecision::NoValue
            | ChildReturnDecision::Deferred
            | ChildReturnDecision::Blocked { .. } => Ok(None),
        }
    }

    /// Record the root's final answer of a turn at the label the root holds, and return its
    /// receipt. The same bytes at the same label as the root's latest answer record nothing.
    pub(crate) async fn release_answer(&self, answer: &str) -> Result<Option<ReleaseReceipt>, EventError> {
        let digest = ReleaseDigest::of(answer.as_bytes());
        self.drive_with_evidence(|_, _| Ok(EngineEvent::Answer { digest }), None, None, Occasion::Other)
            .await?;
        self.receipt(Receipted::Answer { root: &self.trajectory })
    }

    /// Judge content at `label` into `sink(arguments)` under the head registry, asking the
    /// membership the judgment reads. Nothing is appended.
    pub(crate) async fn judge_release(
        &self,
        label: &Label,
        principal: Option<&ReaderId>,
        sink: &str,
        arguments: &[u8],
    ) -> Result<(), ReleaseRefusal> {
        let deployment = self.deployment();
        let approves =
            |authority: &appa_engine::names::AuthorityName| deployment.externals.is_stock_approve(authority.as_str());
        let mut evidence = Vec::new();
        for _ in 0..RESOLUTION_ROUNDS {
            match deployment
                .resident
                .judge_release(label, sink, arguments, principal, &evidence, approves)
            {
                ReleaseRound::Judged(judged) => return judged,
                ReleaseRound::Consult(requests) => {
                    for request in requests {
                        match self.consult(request, None, None, Occasion::Other).await {
                            Ok(answer) => evidence.push(answer),
                            Err(error) => return Err(missing_or_integrity(&error)),
                        }
                    }
                }
            }
        }
        Err(ReleaseRefusal::MissingFact(RetryHint::Later))
    }
}

/// A consult that failed outright: a fact not obtained where that is the failure, otherwise
/// a deployment fault.
fn missing_or_integrity(error: &EventError) -> ReleaseRefusal {
    match error.missing_fact() {
        Some(unobtained) => ReleaseRefusal::MissingFact(unobtained.retry),
        None => {
            tracing::warn!(%error, "a release judgment's membership consult failed");
            ReleaseRefusal::Integrity
        }
    }
}

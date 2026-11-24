use super::*;
use appa_engine::label::Label;
use appa_runtime_api::{ReleaseDigest, ReleaseReceipt, RetryHint};

use crate::engine::{ReleaseRound, Unreleased};

impl Session {
    /// Record the root's final answer of a turn at the label the root holds, and return the
    /// receipt of the record that stands for it: the one this decision appended, or the root's
    /// latest answer where that holds the same bytes at the same label.
    pub(crate) async fn release_answer(&self, answer: &str) -> Result<ReleaseReceipt, EventError> {
        let digest = ReleaseDigest::of(answer.as_bytes());
        let decision = self
            .drive_with_evidence(|_, _| Ok(EngineEvent::Answer { digest }), None, None, Occasion::Other)
            .await?;
        match decision.then {
            Next::Released { receipt } => Ok(receipt),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// Record the review `offer` shows a person as the bytes `digest` names, and return the
    /// receipt of the record that stands for it. The review is rendered here as the block's
    /// feedback renders it; other bytes are refused.
    pub(crate) async fn release_review(
        &self,
        quoted: &OfferId,
        digest: ReleaseDigest,
    ) -> Result<ReleaseReceipt, EventError> {
        let review = |context: &Decided<'_>, _| {
            let offer = crate::engine::resolve_rendered(context.log, quoted).ok_or(EventError::UnknownOffer)?;
            let pending = context
                .offer_pursuer(&offer)
                .map(|pursuer| context.engine().offer_reviews(context.view, &pursuer, &offer))
                .unwrap_or_default();
            let rendered = super::remedy::reviews(&pending, &self.deployment().externals)
                .into_iter()
                .next()
                .map(|review| ReleaseDigest::of(review.text.as_bytes()));
            Ok(EngineEvent::Review {
                offer,
                digest,
                rendered,
            })
        };
        let decision = self.drive_with_evidence(review, None, None, Occasion::Other).await?;
        match decision.then {
            Next::Released { receipt } => Ok(receipt),
            _ => Err(EventError::UnexpectedDecision),
        }
    }
}

/// Judge content at `label` into `sink(arguments)` under `deployment`'s registry, reading the
/// membership the judgment needs now, each round's reads together. No consult is recorded
/// and nothing is appended.
pub(in crate::api) async fn judge_release(
    deployment: &Deployment,
    label: &Label,
    principal: Option<&ReaderId>,
    sink: &str,
    arguments: &[u8],
) -> Result<(), Unreleased> {
    let approves =
        |authority: &appa_engine::names::AuthorityName| deployment.externals.is_stock_approve(authority.as_str());
    let mut evidence = Vec::new();
    // One judgment beyond the last batch reads that batch's answers.
    for consulted in 0..=RESOLUTION_ROUNDS {
        match deployment
            .resident
            .judge_release(label, sink, arguments, principal, &evidence, approves)
        {
            ReleaseRound::Judged(judged) => return judged,
            ReleaseRound::Consult(_) if consulted == RESOLUTION_ROUNDS => break,
            ReleaseRound::Consult(requests) => {
                let asked = requests.iter().map(|request| membership(deployment, request));
                let mut learned = false;
                for answered in crate::external::settle_batch(asked).await {
                    let answered = answered?;
                    if !evidence.contains(&answered) {
                        evidence.push(answered);
                        learned = true;
                    }
                }
                // A round that learns nothing asks the same batch again.
                if !learned {
                    break;
                }
            }
        }
    }
    Err(Unreleased::Unobtained(RetryHint::Later))
}

/// One membership read a judgment asks, straight from the deployment's externals.
async fn membership(deployment: &Deployment, request: &ExternalRequest) -> Result<ExternalEvidence, Unreleased> {
    let consult = super::consult::membership_consult(request).ok_or(Unreleased::Unjudgeable)?;
    let outcome = Box::pin(deployment.externals.consult(&consult, None, None)).await;
    super::consult::membership_evidence(request, outcome).ok_or(Unreleased::Unjudgeable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use appa_engine::engine::SinkRefusal;

    const SINKS: &str = r#"
[policy]
version = 2

[[policy.annotator]]
name = "classifier"

[[policy.tool]]
name = "audited"
requires = { effects = { excludes = ["egress"] } }
delta = {}

[[policy.tool]]
name = "classified"
annotator = "classifier"

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.annotators.classifier]
url = "http://127.0.0.1:9/annotate"
"#;

    #[tokio::test]
    async fn a_sink_no_release_may_be_judged_into_is_refused_for_its_own_reason() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, SINKS).expect("the fixture writes");
        let runtime = crate::api::Runtime::open(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the runtime opens");
        let deployment = runtime.inner.deployment();
        for (sink, why) in [
            ("audited", SinkRefusal::HistoryRequirement),
            ("classified", SinkRefusal::Annotated),
            ("mail", SinkRefusal::Undeclared),
        ] {
            assert_eq!(
                judge_release(&deployment, &Label::top(), None, sink, b"{}").await,
                Err(Unreleased::Sink(why)),
                "{sink}"
            );
        }
    }
}

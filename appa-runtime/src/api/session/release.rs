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

    /// Record the review `quoted` showed a person as the bytes `digest` names, and return the
    /// receipt of the record that stands for it. Only the review its block showed is recorded,
    /// and only for the trajectory that pursues the offer. A recorded review answers the
    /// trajectory that opened the offer or pursues it, live or ended.
    pub(crate) async fn release_review(
        &self,
        quoted: &OfferId,
        digest: ReleaseDigest,
    ) -> Result<ReleaseReceipt, EventError> {
        let trajectory = &self.trajectory;
        let resolved =
            |log: &appa_eventlog::Log| crate::engine::resolve_rendered(log, quoted).ok_or(EventError::UnknownOffer);
        let standing = self.with_view(|context| {
            let offer = resolved(context.log)?;
            let engine = context.engine();
            let owns = engine.offer_opener(context.view, &offer).as_ref() == Some(trajectory)
                || context.offer_pursuer(&offer).as_ref() == Some(trajectory);
            Ok(owns
                .then(|| engine.standing_review(context.view, &offer, digest))
                .flatten())
        })?;
        if let Some(receipt) = standing {
            return Ok(receipt);
        }
        let review = |context: &Decided<'_>, _| {
            let offer = resolved(context.log)?;
            if context.offer_pursuer(&offer).as_ref() != Some(trajectory) {
                return Err(EventError::UnknownOffer);
            }
            let shown = crate::engine::shown_review(context.log, &offer);
            Ok(EngineEvent::Review { offer, digest, shown })
        };
        let decision = self.drive_with_evidence(review, None, None, Occasion::Other).await?;
        match decision.then {
            Next::Released { receipt } => Ok(receipt),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// Record the digest of the review each offer of this decision's block shows a person: the
    /// bytes a later release of that review is held to. A record that does not land leaves the
    /// review unreleasable.
    pub(super) fn record_shown_reviews(&self, decision: &EngineDecision) {
        let Some(facts) = decision.append.as_ref() else {
            return;
        };
        let pending: Vec<PendingReview> = match &decision.then {
            Next::ModelResponse { feedback, .. } => feedback.iter().flat_map(|entry| entry.review.clone()).collect(),
            Next::PresentToModel(Presentation::Blocked { review, .. }) => review.clone(),
            _ => return,
        };
        for review in super::remedy::reviews(&pending, &self.deployment().externals) {
            let Some(offer) = crate::engine::opened_in(facts, &review.offer) else {
                continue;
            };
            let shown = appa_eventlog::HostObservation::ReviewShown {
                offer,
                digest: ReleaseDigest::of(review.text.as_bytes()),
            };
            if let Err(error) = self.inner.append_host(&self.root, &shown) {
                tracing::warn!(root = %self.root.0, %error, "the review a block showed was not recorded");
            }
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

    #[cfg(feature = "daemon")]
    const NARROWING: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "publish"
delta = { trust = "suspicious" }
requires = { attention = ["signoff"] }

[[policy.tool]]
name = "deploy"
delta = {}
requires = { trust = "trusted" }

[[policy.authority]]
name = "operator"

[policy.authority.permits]
trust_below = "trusted"
attention = ["signoff"]

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.authorities.operator]
builtin = "hitl"
"#;

    /// The block shows the tools a narrowing limits; an execution of the same offer renders the
    /// narrowing without them. Only the bytes the block showed are released.
    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn a_narrowing_review_releases_as_its_block_showed_it_and_never_as_rendered_again() {
        use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall};
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, NARROWING).expect("the fixture writes");
        let runtime = crate::api::Runtime::open(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the runtime opens");
        let root = appa_runtime_api::TrajectoryId("cc:narrowing".to_string());
        let started = crate::hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root.clone(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
                settings: Default::default(),
            },
        )
        .await;
        assert_eq!(started, HookDecision::Ack);
        let blocked = crate::hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: root.clone(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "publish".to_string(),
                    arguments: serde_json::value::to_raw_value(&serde_json::json!({ "body": "figures" }))
                        .expect("the arguments serialize"),
                    cwd: None,
                },
                call_id: None,
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        let HookDecision::DenyCall { review, .. } = blocked else {
            panic!("the publish blocks on the operator: {blocked:?}");
        };
        let [shown] = review.as_slice() else {
            panic!("the block shows one review: {review:?}");
        };
        let quoted = OfferId(shown.offer.clone());
        let session = runtime
            .session_with_presentation(&root, &root, Default::default())
            .expect("the root is open");
        let rendered_again = session
            .with_view(|context| {
                let offer = crate::engine::resolve_rendered(context.log, &quoted).expect("the offer resolves");
                let pending = context.engine().offer_reviews(context.view, &root, &offer);
                Ok(super::super::remedy::reviews(&pending, &session.deployment().externals))
            })
            .expect("the view reads");
        let [rendered_again] = rendered_again.as_slice() else {
            panic!("the execution renders one review: {rendered_again:?}");
        };
        assert_ne!(rendered_again.text, shown.text, "the two renders of a narrowing differ");

        let substituted = session
            .release_review(&quoted, ReleaseDigest::of(rendered_again.text.as_bytes()))
            .await;
        assert!(matches!(substituted, Err(EventError::OtherReview)), "{substituted:?}");
        let receipt = session
            .release_review(&quoted, ReleaseDigest::of(shown.text.as_bytes()))
            .await
            .expect("the review the block showed releases");
        assert_eq!(receipt.digest, ReleaseDigest::of(shown.text.as_bytes()));
    }
}

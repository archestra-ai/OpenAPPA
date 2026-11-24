use super::*;
use appa_engine::fact::Fact;
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

    /// Record the review `quoted` asks about as the bytes `digest` names, and return the
    /// receipt of the record that stands for it. Only the review text the offer's block
    /// rendered is recorded ([`stamp_review_digests`]), and only for the trajectory that
    /// pursues the offer. A recorded review answers the trajectory that opened the offer or
    /// pursues it, live or ended: read without the family's writer first, then again inside
    /// the step that would record it.
    ///
    /// Any path that returns the block's review returns these bytes. A rendering made at
    /// execution instead, such as the message of an MCP elicitation form, is other bytes and
    /// releases nothing.
    pub(crate) async fn release_review(
        &self,
        quoted: &OfferId,
        digest: ReleaseDigest,
    ) -> Result<ReleaseReceipt, EventError> {
        let trajectory = &self.trajectory;
        let release = ReviewRelease { offer: quoted, digest };
        if let Some(receipt) = self.with_view(|context| Ok(standing_review(context, trajectory, release)))? {
            return Ok(receipt);
        }
        let review = |context: &Decided<'_>, _| {
            let offer = crate::engine::resolve_rendered(context.log, quoted).ok_or(EventError::UnknownOffer)?;
            if context.offer_pursuer(&offer).as_ref() != Some(trajectory) {
                return Err(EventError::UnknownOffer);
            }
            Ok(EngineEvent::Review { offer, digest })
        };
        let decision = self
            .drive_with_evidence(review, None, None, Occasion::Review(release))
            .await?;
        match decision.then {
            Next::Released { receipt } => Ok(receipt),
            _ => Err(EventError::UnexpectedDecision),
        }
    }
}

/// Record on each offer `facts` open the digest of the review the block renders for it, as
/// `then` carries it to the people its `hitl` authorities ask: the bytes a release of that
/// review may name. The only writer of [`Fact::OfferOpened`]'s `review_digest`, run on a
/// decided batch before it is appended.
pub(super) fn stamp_review_digests(facts: &mut [Fact], then: &Next, externals: &crate::external::ExternalServices) {
    let rendered = match then {
        Next::ModelResponse { feedback, .. } => {
            super::remedy::reviews(feedback.iter().flat_map(|entry| &entry.review), externals)
        }
        Next::PresentToModel(Presentation::Blocked { review, .. }) => super::remedy::reviews(review, externals),
        _ => return,
    };
    for fact in facts {
        if let Fact::OfferOpened {
            offer, review_digest, ..
        } = fact
        {
            let id = crate::engine::offer_id(offer);
            *review_digest = rendered
                .iter()
                .find(|review| review.offer == id.0)
                .map(|review| ReleaseDigest::of(review.text.as_bytes()));
        }
    }
}

/// The receipt `release` already stands under in the log `context` reads, where `trajectory`
/// opened its offer or pursues it.
pub(super) fn standing_review(
    context: &Decided<'_>,
    trajectory: &TrajectoryId,
    release: ReviewRelease<'_>,
) -> Option<ReleaseReceipt> {
    let offer = crate::engine::resolve_rendered(context.log, release.offer)?;
    let engine = context.engine();
    let owns = engine.offer_opener(context.view, &offer).as_ref() == Some(trajectory)
        || context.offer_pursuer(&offer).as_ref() == Some(trajectory);
    owns.then(|| engine.standing_review(context.view, &offer, release.digest))
        .flatten()
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

    const REVIEWED: &str = r#"
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

[[policy.tool]]
name = "countersigned"
delta = {}
requires = { attention = ["countersign"] }

[[policy.tool]]
name = "stamped"
delta = {}
requires = { attention = ["stamp"] }

[[policy.authority]]
name = "operator"

[policy.authority.permits]
trust_below = "trusted"
attention = ["signoff", "countersign", "stamp"]

[[policy.authority]]
name = "auditor"

[policy.authority.permits]
attention = ["countersign"]

[[policy.authority]]
name = "stock"

[policy.authority.permits]
attention = ["stamp"]

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.authorities.operator]
builtin = "hitl"

[externals.authorities.auditor]
builtin = "hitl"

[externals.authorities.stock]
builtin = "approve"
"#;

    async fn started(dir: &std::path::Path) -> (crate::api::Runtime, TrajectoryId) {
        let path = dir.join("appa.toml");
        std::fs::write(&path, REVIEWED).expect("the fixture writes");
        let runtime = crate::api::Runtime::open(
            Config::load(&path).expect("the fixture validates"),
            dir.join("appa.db"),
            None,
        )
        .expect("the runtime opens");
        let root = TrajectoryId("cc:reviewed".to_string());
        let start = appa_runtime_api::HookEvent::SessionStart {
            root: root.clone(),
            principal: None,
            address: None,
            title: None,
            launch: None,
            start: None,
            settings: Default::default(),
        };
        assert_eq!(
            crate::hooks::handle(&runtime, start).await,
            appa_runtime_api::HookDecision::Ack
        );
        (runtime, root)
    }

    fn proposed(tool: &str) -> appa_runtime_api::ProposedCall {
        appa_runtime_api::ProposedCall {
            tool: tool.to_string(),
            arguments: serde_json::value::to_raw_value(&serde_json::json!({ "body": "figures" }))
                .expect("the arguments serialize"),
            cwd: None,
        }
    }

    async fn called(
        runtime: &crate::api::Runtime,
        root: &TrajectoryId,
        tool: &str,
        call_id: Option<&str>,
    ) -> appa_runtime_api::HookDecision {
        let call = appa_runtime_api::HookEvent::ToolCall {
            actor: appa_runtime_api::Actor {
                root: root.clone(),
                child: None,
            },
            call: proposed(tool),
            call_id: call_id.map(str::to_string),
            spawn: None,
            prompt: None,
            ruling: None,
            declares: None,
        };
        crate::hooks::handle(runtime, call).await
    }

    fn denied_reviews(decision: appa_runtime_api::HookDecision) -> Vec<appa_runtime_api::Review> {
        match decision {
            appa_runtime_api::HookDecision::DenyCall { review, .. } => review,
            other => panic!("the call is denied: {other:?}"),
        }
    }

    async fn released(
        runtime: &crate::api::Runtime,
        root: &TrajectoryId,
        review: &appa_runtime_api::Review,
    ) -> Result<ReleaseReceipt, EventError> {
        runtime
            .session_with_presentation(root, root, Default::default())
            .expect("the root is open")
            .release_review(
                &OfferId(review.offer.clone()),
                ReleaseDigest::of(review.text.as_bytes()),
            )
            .await
    }

    /// The runtime applies the stock plan itself: the offer it applies asks no person, so its
    /// opening records no review and nothing releases as one.
    #[tokio::test]
    async fn the_offer_the_runtime_applies_itself_records_no_review() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (runtime, root) = started(dir.path()).await;
        let decision = called(&runtime, &root, "stamped", None).await;
        assert!(
            matches!(decision, appa_runtime_api::HookDecision::AllowCall { .. }),
            "{decision:?}"
        );
        let log = runtime.inner.log(&root).expect("the family log reads");
        let accepted = log
            .facts()
            .iter()
            .find_map(|fact| match fact {
                Fact::OfferAccepted { offer, .. } => Some(*offer),
                _ => None,
            })
            .expect("the runtime applied an offer");
        let opening = log.facts().iter().find_map(|fact| match fact {
            Fact::OfferOpened {
                offer, review_digest, ..
            } if *offer == accepted => Some(*review_digest),
            _ => None,
        });
        assert_eq!(opening, Some(None));
        let quoted = crate::engine::offer_id(&accepted);
        let refused = runtime
            .session_with_presentation(&root, &root, Default::default())
            .expect("the root is open")
            .release_review(&quoted, ReleaseDigest::of(b""))
            .await;
        assert!(matches!(refused, Err(EventError::OtherReview)), "{refused:?}");
    }

    /// A block that asks two officers renders a review for each offer, and each releases.
    #[tokio::test]
    async fn each_alternative_offers_review_releases() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (runtime, root) = started(dir.path()).await;
        let reviews = denied_reviews(called(&runtime, &root, "countersigned", None).await);
        assert_eq!(
            reviews.len(),
            2,
            "each officer is asked through its own offer: {reviews:?}"
        );
        for review in &reviews {
            released(&runtime, &root, review).await.expect("each review releases");
        }
    }

    /// The block shows the tools a narrowing limits; an execution of the same offer renders the
    /// narrowing without them. Only the bytes the block showed are released.
    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn a_narrowing_review_releases_as_its_block_showed_it_and_never_as_rendered_again() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (runtime, root) = started(dir.path()).await;
        let [shown] = denied_reviews(called(&runtime, &root, "publish", None).await)
            .try_into()
            .expect("the block shows one review");
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
        let receipt = released(&runtime, &root, &shown)
            .await
            .expect("the review the block showed releases");
        assert_eq!(receipt.digest, ReleaseDigest::of(shown.text.as_bytes()));
    }
}

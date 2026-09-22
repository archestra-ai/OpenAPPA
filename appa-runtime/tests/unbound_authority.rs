//! An authority the deployment leaves unbound answers nothing: the deployment opens, and a
//! remedy that names the authority leaves its offer standing.

mod common;
use common::{actor, offer_of, propose, raw, root};

use std::sync::Arc;

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};

const POLICY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "publish"
delta = {}
requires = { attention = ["signoff"] }

[[policy.authority]]
name = "operator"

[policy.authority.permits]
attention = ["signoff"]

[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#;

fn publish() -> ProposedCall {
    ProposedCall {
        tool: "publish".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

#[tokio::test]
async fn a_remedy_naming_an_unbound_authority_gives_no_answer_and_the_offer_stands() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );

    let offer = offer_of(&propose(&runtime, publish()).await);
    for _ in 0..2 {
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer.clone()).await,
            RemedyOutcome::NoAnswer { .. }
        ));
    }
    assert!(
        matches!(propose(&runtime, publish()).await, HookDecision::DenyCall { .. }),
        "nothing was released"
    );
}

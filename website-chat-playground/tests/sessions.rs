use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use website_chat_playground::session::{CreateError, Sessions};
use website_chat_playground::systems::System;

const MODEL: &str = "openai/gpt-4o";
const PRESET: &str = include_str!("../policies/default.toml");

fn sessions(dir: &tempfile::TempDir, ttl: Duration) -> Sessions {
    Sessions::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("world"),
        dir.path().join("sessions"),
        ttl,
        appa_example_agent::Endpoint::new("http://127.0.0.1:1/v1"),
    )
}

fn all() -> BTreeSet<System> {
    System::ALL.into_iter().collect()
}

#[tokio::test]
async fn the_preset_opens_with_every_component_this_host_bound() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let sessions = sessions(&dir, Duration::from_secs(600));

    let session = sessions
        .create(PRESET, &all(), MODEL)
        .await
        .expect("the preset opens a runtime");

    assert_eq!(session.tool_count, 8);
    assert_eq!(session.boundary.trust, "trusted");
    assert_eq!(session.boundary.audience, "public");
    assert!(session.runtime.audit(&session.trajectory).is_none());
}

#[tokio::test]
async fn an_authority_the_host_never_heard_of_is_still_bound() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let sessions = sessions(&dir, Duration::from_secs(600));
    let policy = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "create_issue"
requires = { attention = ["release-review"] }
delta = {}

[[authority]]
name = "some-desk-the-service-has-never-seen"
hint = "You are the release desk."
permits = { attention = ["release-review"] }
"#;

    sessions
        .create(policy, &[System::Github].into_iter().collect(), MODEL)
        .await
        .expect("a visitor-registered authority is bound to this session's desk");
}

#[tokio::test]
async fn a_policy_this_deployment_cannot_run_is_refused_at_creation() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let sessions = sessions(&dir, Duration::from_secs(600));
    // The playground routes every named annotator to its own handler, and a
    // builtin annotator takes no such binding — so a policy naming one cannot
    // run here and is refused when the session is created.
    let policy = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[annotator]]
name = "classify"
builtin = "llm"

[[tool]]
name = "list_customers"
annotator = "classify"
"#;

    let Err(error) = sessions
        .create(policy, &[System::Crm].into_iter().collect(), MODEL)
        .await
    else {
        panic!("a builtin annotator would run on the demo host itself, so the policy must be refused");
    };
    assert!(matches!(error, CreateError::Policy(_)), "got: {error}");
}

#[tokio::test]
async fn an_annotator_the_playground_does_not_implement_refuses_at_create() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let sessions = sessions(&dir, Duration::from_secs(600));
    let policy = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[annotator]]
name = "classify"

[[tool]]
name = "list_customers"
annotator = "classify"
"#;

    let Err(refusal) = sessions
        .create(policy, &[System::Crm].into_iter().collect(), MODEL)
        .await
    else {
        panic!("the playground implements no annotator named classify, so the open refuses");
    };
    let text = format!("{refusal}");
    assert!(text.contains("classify"), "the refusal names the annotator: {text}");
}

#[tokio::test]
async fn two_sessions_keep_their_own_policy_and_their_own_log() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let sessions = sessions(&dir, Duration::from_secs(600));

    let full = sessions.create(PRESET, &all(), MODEL).await.expect("the preset opens");
    let crm_only = sessions
        .create(PRESET, &[System::Crm].into_iter().collect(), MODEL)
        .await
        .expect("a narrower world opens");

    assert_eq!(full.tool_count, 8);
    assert_eq!(crm_only.tool_count, 2, "only the enabled system's tools exist");
    assert_ne!(full.id, crm_only.id);
    assert_ne!(full.trajectory, crm_only.trajectory);
}

#[tokio::test]
async fn an_expired_session_releases_its_database_with_its_world() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let sessions = sessions(&dir, Duration::ZERO);

    let session = sessions.create(PRESET, &all(), MODEL).await.expect("the preset opens");
    let id = session.id.clone();
    let session_dir = dir.path().join("sessions").join(&id);
    assert!(session_dir.join("appa.db").is_file(), "the runtime opened a database");
    assert!(session_dir.join("data").is_dir(), "and the world was copied");

    assert_eq!(sessions.expire_idle(), 1);
    assert!(sessions.get(&id).is_none(), "an expired session is not handed out");
    drop(session);
    assert!(!session_dir.exists(), "the session's whole footprint is released");
}

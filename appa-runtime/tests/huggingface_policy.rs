//! Hugging Face battery: each repository's readers bound the read, writes need trusted data
//! its readers may see, code run with the token needs a person, and nothing records an effect.
mod common;

use appa_runtime::{
    api::{AuditEvent, RemedyOutcome, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, routing::post};
use common::{actor, offer_of, propose, ran, raw, repo_root, root, serve};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/huggingface/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

/// A loopback standing in for `repo-visibility.py`: it answers the shapes the script's
/// tests pin, keyed on the repository the call names — `acme/private` is a private
/// repository of the viewer's, everything else public.
async fn serve_annotators() -> String {
    let router = Router::new().route(
        "/annotate",
        post(|body: String| async move {
            let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
            let arguments = &request["artifact"]["args"]["arguments"];
            let private = arguments.to_string().contains("acme/private");
            let readers = if private { serde_json::json!(["self"]) } else { serde_json::json!("public") };
            let answer = match request["name"].as_str() {
                Some("huggingface.repo-visibility") => serde_json::json!({
                    "delta": { "trust": "suspicious", "audience": readers },
                    "requires": { "history": [], "attention": [] },
                    "emits": [],
                }),
                Some("huggingface.repo-readers") => serde_json::json!({
                    "delta": {},
                    "requires": { "trust": "trusted", "audience": { "contains": readers }, "history": [], "attention": [] },
                    "emits": [],
                }),
                other => panic!("unexpected annotator {other:?}"),
            };
            serde_json::json!({ "version": 1, "answer": answer }).to_string()
        }),
    );
    format!("{}/annotate", serve(router).await)
}

/// A loopback `huggingface` audience source: the viewer is one address.
async fn serve_source() -> String {
    let router = Router::new().route(
        "/audience",
        post(|_body: String| async move {
            serde_json::json!({ "version": 1, "answer": { "members": ["viewer@corp.example"] } }).to_string()
        }),
    );
    format!("{}/audience", serve(router).await)
}

/// The shipped battery with its two helper processes swapped for loopback services, under
/// a root that maps `self` onto the viewer and permits the review mark.
async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let target = dir.path().join("marketplace/batteries/huggingface");
    std::fs::create_dir_all(&target).unwrap();
    let mut policy = std::fs::read_to_string(repo_root().join("marketplace/batteries/huggingface/appa.toml")).unwrap();
    let annotators = serve_annotators().await;
    let source = serve_source().await;
    for (binding, url) in [
        (
            "command = [\"python3\", \"repo-visibility.py\"]\ntoken_env = \"APPA_PROVIDER_HUGGINGFACE_TOKEN\"\n",
            &annotators,
        ),
        (
            "command = [\"python3\", \"audience-source.py\"]\ntoken_env = \"APPA_PROVIDER_HUGGINGFACE_TOKEN\"\n",
            &source,
        ),
    ] {
        assert!(policy.contains(binding), "the battery binds its helpers as documented");
        policy = policy.replace(binding, &format!("url = \"{url}\"\n"));
    }
    std::fs::write(target.join("appa.toml"), policy).unwrap();
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        r#"include = ["marketplace/batteries/huggingface/appa.toml"]

[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

[policy.audience]
self = ["huggingface:viewer"]

[[policy.authority]]
name = "huggingface-operator"
permits = { trust_below = "trusted", attention = ["huggingface-review"] }

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.huggingface-operator]
builtin = "approve"
"#,
    )
    .unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

/// Runs the call, taking the remedy the runtime offers for a narrowing first.
async fn admit(runtime: &Arc<Runtime>, call: ProposedCall) {
    let decision = propose(runtime, call.clone()).await;
    if !matches!(decision, HookDecision::AllowCall { .. }) {
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(runtime, call.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            call.tool
        );
    }
    ran(runtime, call).await;
}

fn released_effects(runtime: &Runtime) -> Vec<Vec<String>> {
    runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/huggingface/") => Some(effects),
            _ => None,
        })
        .collect()
}

/// Public repository content stays public but is suspicious: a further public read runs
/// outright, a write back into the Hub does not.
#[tokio::test]
async fn a_public_read_stays_public_and_cannot_be_written_back() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    admit(
        &runtime,
        call(
            "hub_repo_details",
            serde_json::json!({ "repo_ids": ["openai-community/gpt2"], "repo_type": "model" }),
        ),
    )
    .await;
    assert_eq!(
        propose(
            &runtime,
            call(
                "hf_fs",
                serde_json::json!({ "operations": [{ "cmd": "ls", "args": ["hf://papers"] }] })
            )
        )
        .await,
        HookDecision::AllowCall { spawn: None }
    );
    let write = call(
        "hf_fs_write",
        serde_json::json!({ "cmd": "put", "args": ["hf://models/openai-community/gpt2/README.md"], "content": "x" }),
    );
    assert!(!matches!(
        propose(&runtime, write).await,
        HookDecision::AllowCall { .. }
    ));
}

/// Private repository content narrows the trajectory to the viewer: a write into a public
/// repository is refused, the viewer's own account read and a search still run.
#[tokio::test]
async fn a_private_read_narrows_to_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    admit(
        &runtime,
        call(
            "hf_fs",
            serde_json::json!({ "operations": [{ "cmd": "cat", "args": ["hf://models/acme/private/config.json"] }] }),
        ),
    )
    .await;
    let write = call(
        "hf_fs_write",
        serde_json::json!({ "cmd": "put", "args": ["hf://models/openai-community/gpt2/README.md"], "content": "x" }),
    );
    assert!(!matches!(
        propose(&runtime, write).await,
        HookDecision::AllowCall { .. }
    ));
    for read in [
        call("hf_whoami", serde_json::json!({})),
        call("hub_repo_search", serde_json::json!({ "query": "private" })),
    ] {
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
        ran(&runtime, read).await;
    }
}

/// A fresh trajectory holds trusted data everyone may see: a file write runs outright, while
/// invoking a Space or a job waits for the reviewer. Nothing records an effect.
#[tokio::test]
async fn code_run_with_the_token_needs_review_and_records_no_effect() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let write = call(
        "hf_fs_write",
        serde_json::json!({ "cmd": "put", "args": ["hf://models/openai-community/gpt2/README.md"], "content": "x" }),
    );
    assert_eq!(
        propose(&runtime, write.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, write).await;

    for reviewed in [
        call(
            "dynamic_space",
            serde_json::json!({ "operation": "invoke", "space_name": "mcp-tools/Z-Image-Turbo", "parameters": "{}" }),
        ),
        call(
            "hf_jobs",
            serde_json::json!({ "operation": "run", "command": ["python", "train.py"] }),
        ),
    ] {
        let decision = propose(&runtime, reviewed.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }), "{}", reviewed.tool);
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(&runtime, reviewed.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            reviewed.tool
        );
        ran(&runtime, reviewed).await;
    }
    let effects = released_effects(&runtime);
    assert_eq!(effects.len(), 3);
    assert!(effects.iter().all(Vec::is_empty), "{effects:?}");
}

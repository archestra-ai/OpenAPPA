mod common;
use common::{actor, audit_len, last_offer, propose, ran, raw, root, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::{ProbeError, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

const POLICY: &str = r#"
[policy]
version = 2

[policy.audience.group.team]
from = ["slack:user-group/team"]

[policy.audience.group.nobody]
from = ["slack:user-group/nobody"]

[[policy.tool]]
name = "read_hr"
delta = { audience = ["alice@corp.example", "bob@corp.example"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { contains = ["$to"] } }
effects = ["egress"]
delta = {}

[[policy.tool]]
name = "send_capped"
requires = { audience = { within = ["alice@corp.example", "@team"] } }
effects = ["egress"]
delta = {}

[[policy.tool]]
name = "send_direct"
requires = { audience = { within = ["@slack:user-group/direct"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "full-members", feeds = "internal" }, { template = "user-group/<handle>" }]
"#;

/// The `[externals.audience]` block of `POLICY`, for the variants that replace it.
const SLACK_BINDING: &str = "[externals.audience.slack]\nurl = \"AUDIENCE_URL\"\nselectors = [{ template = \"viewer\", feeds = \"self\" }, { template = \"full-members\", feeds = \"internal\" }, { template = \"user-group/<handle>\" }]\n";

/// Slack's member lookups answered in process from a roster.
const ROSTER_BINDINGS: &str = r#"
[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "full-members", feeds = "internal" }, { template = "user-group/<handle>" }]
lookup = "people"

[externals.audience.people]
readers = { "slack:U-bob" = "bob@corp.example" }
"#;

/// Slack's member lookups answered by a second HTTP entry — the same stub, under another
/// name.
const LOOKUP_URL_BINDINGS: &str = r#"
[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "full-members", feeds = "internal" }, { template = "user-group/<handle>" }]
lookup = "people"

[externals.audience.people]
url = "AUDIENCE_URL"
"#;

/// What the stub answers: the members of any selector, and the principal of any member.
/// `None` on either answers a 500.
#[derive(Clone, Default)]
struct Answers {
    members: Option<Vec<&'static str>>,
    principal: Option<Option<&'static str>>,
}

#[derive(Clone)]
struct Source {
    answers: Arc<Mutex<Answers>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Source {
    fn members(&self, members: Option<Vec<&'static str>>) {
        self.answers.lock().unwrap().members = members;
    }

    fn principal(&self, principal: Option<Option<&'static str>>) {
        self.answers.lock().unwrap().principal = principal;
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_source() -> (String, Source) {
    let source = Source {
        answers: Arc::new(Mutex::new(Answers {
            members: Some(vec![]),
            principal: None,
        })),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route(
            "/audience",
            post(|State(source): State<Source>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let lookup = request["artifact"]["member"].is_string();
                source.requests.lock().unwrap().push(request);
                let answers = source.answers.lock().unwrap().clone();
                let answer = if lookup {
                    answers
                        .principal
                        .map(|principal| serde_json::json!({ "principal": principal }))
                } else {
                    answers.members.map(|members| serde_json::json!({ "members": members }))
                };
                match answer {
                    Some(answer) => (
                        axum::http::StatusCode::OK,
                        serde_json::json!({ "version": 1, "answer": answer }).to_string(),
                    ),
                    None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(source.clone());
    (format!("{}/audience", serve(router).await), source)
}

fn read_hr() -> ProposedCall {
    ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
    }
}

fn send(to: &str) -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({ "to": to })),
    }
}

async fn narrowed(dir: &tempfile::TempDir, audience_url: &str) -> Arc<Runtime> {
    narrowed_under(dir, POLICY, audience_url).await
}

/// The runtime opened on `policy`, its audience narrowed to Alice and Bob by the `read_hr`
/// call every scenario starts from.
async fn narrowed_under(dir: &tempfile::TempDir, policy: &str, audience_url: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy.replace("AUDIENCE_URL", audience_url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    let blocked = propose(&runtime, read_hr()).await;
    let HookDecision::DenyCall { feedback, .. } = blocked else {
        panic!("the narrowing read is offered for acceptance, got {blocked:?}");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_hr()).await;
    runtime
}

#[tokio::test]
async fn a_group_argument_is_checked_against_the_sources_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // The source reports Alice by the address the delta wrote: the same reader.
    source.members(Some(vec!["alice@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["version"], 1);
    assert_eq!(requests[0]["kind"], "audience");
    assert_eq!(requests[0]["name"], "slack");
    assert_eq!(
        requests[0]["declaration"]["templates"],
        serde_json::json!(["viewer", "full-members", "user-group/<handle>"])
    );
    assert_eq!(
        requests[0]["artifact"],
        serde_json::json!({ "selector": "user-group/team" })
    );

    // A member reported by provider id is that qualified reader, which the narrowed
    // audience does not hold.
    source.members(Some(vec!["alice@corp.example", "slack:U-carol"]));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 2);

    // An empty member list is a complete answer.
    source.members(Some(vec![]));
    assert_eq!(
        propose(&runtime, send("@nobody")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@nobody")).await;
    assert_eq!(
        source.requests()[2]["artifact"],
        serde_json::json!({ "selector": "user-group/nobody" })
    );
}

#[tokio::test]
async fn no_answer_leaves_the_call_unchecked_and_the_log_unchanged() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    source.members(None);
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), before, "no answer is no engine act");

    source.members(Some(vec!["bob@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(audit_len(&runtime) > before);
    ran(&runtime, send("@team")).await;
}

#[tokio::test]
async fn an_unconfigured_group_argument_fails_operationally() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // `@offsite` is supplied dynamically and configured nowhere: an operational
    // refusal that consults nothing and decides nothing.
    assert!(matches!(
        propose(&runtime, send("@offsite")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty());
    assert_eq!(audit_len(&runtime), before);
}

#[tokio::test]
async fn public_and_literal_arguments_never_consult_the_source() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    assert!(matches!(
        propose(&runtime, send("public")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(
        propose(&runtime, send("alice@corp.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("alice@corp.example")).await;
    assert!(matches!(
        propose(&runtime, send("mallory")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty(), "no spelling here names a group");
}

/// The probe reads every selector the policy references, once each, and refuses the first
/// source that fails or reports a reader outside the shape rule — naming the provider and
/// the selector.
#[tokio::test]
async fn the_probe_reads_every_referenced_selector_and_refuses_a_malformed_source() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY.replace("AUDIENCE_URL", &url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

    source.members(Some(vec!["alice@corp.example", "slack:U-carol"]));
    runtime
        .probe_sources()
        .await
        .expect("well-formed answers pass the probe");
    let mut probed: Vec<String> = source
        .requests()
        .iter()
        .map(|request| {
            request["artifact"]["selector"]
                .as_str()
                .expect("a selector read")
                .to_string()
        })
        .collect();
    probed.sort();
    assert_eq!(probed, ["user-group/direct", "user-group/nobody", "user-group/team"]);

    source.members(Some(vec!["alice@corp.example", "finance"]));
    let refused = runtime
        .probe_sources()
        .await
        .expect_err("a malformed reader refuses the probe");
    let ProbeError::Selector { provider, selector, .. } = &refused else {
        panic!("a malformed member names its selector: {refused}");
    };
    assert_eq!(provider, "slack");
    assert!(selector.starts_with("user-group/"), "{refused}");

    source.members(None);
    assert!(
        runtime.probe_sources().await.is_err(),
        "a source that is down refuses the probe"
    );
}

/// The probe also asks the lookup entry for one qualified member the selectors report,
/// whatever their number, and holds the principal to the shape rule.
#[tokio::test]
async fn the_probe_asks_each_lookup_entry_for_one_owed_member() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let path = dir.path().join("appa.toml");
    let policy = POLICY.replace(SLACK_BINDING, LOOKUP_URL_BINDINGS);
    std::fs::write(&path, policy.replace("AUDIENCE_URL", &url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

    source.members(Some(vec!["alice@corp.example", "slack:U-bob", "slack:U-carol"]));
    source.principal(Some(Some("bob@corp.example")));
    runtime
        .probe_sources()
        .await
        .expect("a well-formed principal passes the probe");
    let lookups: Vec<serde_json::Value> = source
        .requests()
        .into_iter()
        .filter(|request| request["artifact"]["member"].is_string())
        .collect();
    assert_eq!(lookups.len(), 1, "two owed members under one provider cost one lookup");
    assert_eq!(lookups[0]["name"], "people");
    assert_eq!(lookups[0]["artifact"], serde_json::json!({ "member": "slack:U-bob" }));

    source.principal(Some(Some("github:bob")));
    let refused = runtime
        .probe_sources()
        .await
        .expect_err("a principal outside the member's namespace refuses the probe");
    let ProbeError::Lookup { provider, member, .. } = &refused else {
        panic!("a malformed principal names its member: {refused}");
    };
    assert_eq!((provider.as_str(), member.as_str()), ("slack", "slack:U-bob"));

    source.principal(None);
    assert!(
        matches!(runtime.probe_sources().await, Err(ProbeError::Lookup { .. })),
        "a lookup entry that is down refuses the probe"
    );
}

/// A reload is prepared, probed, then installed: a probe that fails leaves the previous
/// deployment serving, and the next successful reload of the same file is the change.
#[tokio::test]
async fn a_refused_reload_probe_leaves_the_previous_deployment_serving() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY.replace("AUDIENCE_URL", &url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

    let revised = || {
        let text = POLICY
            .replace("AUDIENCE_URL", &url)
            .replace("name = \"send_capped\"", "name = \"send_within\"");
        std::fs::write(&path, text).expect("the revision writes");
        Config::load(&path).expect("the revision validates")
    };
    source.members(Some(vec!["finance"]));
    let prepared = runtime.prepare_reload(revised()).expect("the revision loads");
    assert!(
        prepared.probe_sources().await.is_err(),
        "the revision's source answers garbage"
    );
    drop(prepared);

    source.members(Some(vec!["bob@corp.example"]));
    let prepared = runtime.prepare_reload(revised()).expect("the revision loads again");
    prepared.probe_sources().await.expect("the source recovered");
    let reloaded = runtime.install(prepared);
    assert!(
        reloaded.changed,
        "the refused probe installed nothing, so this reload is the change"
    );
}

/// A trajectory opened under one routing keeps it after a reload retargets the provider:
/// its lookups still reach the entry its stored policy file names, while a trajectory opened
/// under the reloaded deployment reaches the new one.
#[tokio::test]
async fn a_retired_trajectory_keeps_the_lookup_routing_it_opened_under() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let policy = POLICY.replace(SLACK_BINDING, LOOKUP_URL_BINDINGS);
    let runtime = narrowed_under(&dir, &policy, &url).await;

    // Slack's lookups move to `directory`; a new GitHub group keeps `people` bound.
    let retargeted = format!(
        "{}\n[externals.audience.directory]\nurl = \"{url}\"\n\n[externals.audience.github]\nurl = \"{url}\"\nlookup = \"people\"\nselectors = [{{ template = \"org/<org>/members\", feeds = \"internal\" }}]\n",
        policy
            .replace("AUDIENCE_URL", &url)
            .replace("lookup = \"people\"", "lookup = \"directory\"")
            .replace(
                "[policy.audience.group.nobody]\n",
                "[policy.audience.group.engineers]\nfrom = [\"github:org/acme/members\"]\n\n[policy.audience.group.nobody]\n"
            )
    );
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, retargeted).expect("the revision writes");
    let revised = Config::load(&path).expect("the revision validates");
    source.members(Some(vec!["slack:U-bob"]));
    source.principal(Some(Some("bob@corp.example")));
    let reloaded = runtime.reload(revised).expect("the revision installs");
    assert!(reloaded.changed);

    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let lookups: Vec<serde_json::Value> = source
        .requests()
        .into_iter()
        .filter(|request| request["artifact"]["member"].is_string())
        .collect();
    assert_eq!(lookups.len(), 1);
    assert_eq!(
        lookups[0]["name"], "people",
        "the retired trajectory's lookup goes where its own policy file sends it"
    );
}

#[test]
fn a_referenced_audience_source_must_be_bound() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    let unbound = POLICY.replace(SLACK_BINDING, "");
    std::fs::write(&path, unbound).expect("the fixture writes");
    // The binding carries the provider's declaration, so without it the policy's own
    // `slack:` references have no provider to resolve against.
    let config = Config::load(&path).expect("the file validates");
    assert!(matches!(
        Runtime::open(config, dir.path().join("appa.db"), None),
        Err(appa_runtime::api::OpenError::Policy(error))
            if matches!(*error, appa_policy::ConfigError::UndeclaredProvider { .. })
    ));
}

#[tokio::test]
async fn a_qualified_recipient_is_checked_through_a_member_lookup() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // The lookup canonicalizes the recipient to the principal the narrowed audience holds.
    source.principal(Some(Some("bob@corp.example")));
    assert_eq!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("slack:U-bob")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1, "one act, one lookup per member");
    assert_eq!(requests[0]["kind"], "audience");
    assert_eq!(requests[0]["artifact"], serde_json::json!({ "member": "slack:U-bob" }));
}

#[tokio::test]
async fn a_malformed_principal_is_refused_and_not_re_asked() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // A principal that is neither an address nor a slack-qualified id is a broken answer:
    // the call is refused operationally after exactly one consult, and no decision is
    // recorded.
    source.principal(Some(Some("finance")));
    assert!(matches!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 1, "a broken answer is not re-asked");
    assert_eq!(audit_len(&runtime), before);

    // A principal in another provider's namespace is refused the same way.
    source.principal(Some(Some("github:bob")));
    assert!(matches!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 2);
    assert_eq!(audit_len(&runtime), before);
}

#[tokio::test]
async fn a_malformed_member_refuses_the_whole_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // One member that is neither an address nor a slack-qualified id refuses the answer:
    // the call is refused operationally after one consult, and no decision is recorded.
    for members in [
        vec!["alice@corp.example", "finance"],
        vec!["github:alice"],
        vec!["public"],
    ] {
        let asked = source.requests().len();
        source.members(Some(members));
        assert!(matches!(
            propose(&runtime, send("@team")).await,
            HookDecision::DenyCall { .. }
        ));
        assert_eq!(source.requests().len(), asked + 1, "a broken answer is not re-asked");
        assert_eq!(audit_len(&runtime), before);
    }
}

/// Under `lookup`, every member a source reports by provider id is looked up before it
/// seats, and the principal the lookup names is the reader the audience holds.
#[tokio::test]
async fn a_redirected_providers_qualified_members_seat_through_a_roster() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let policy = POLICY.replace(SLACK_BINDING, ROSTER_BINDINGS);
    let runtime = narrowed_under(&dir, &policy, &url).await;

    // The roster maps Bob to the address the narrowed audience holds, in process.
    source.members(Some(vec!["slack:U-bob"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    assert_eq!(source.requests().len(), 1, "a roster lookup reaches no service");

    // A member the roster does not list keeps its qualified identity.
    source.members(Some(vec!["slack:U-carol"]));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 2);

    // An address needs no lookup.
    source.members(Some(vec!["alice@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
}

#[tokio::test]
async fn a_redirected_providers_lookups_go_to_the_named_entry() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let policy = POLICY.replace(SLACK_BINDING, LOOKUP_URL_BINDINGS);
    let runtime = narrowed_under(&dir, &policy, &url).await;
    let before = audit_len(&runtime);

    source.members(Some(vec!["slack:U-bob"]));
    source.principal(Some(Some("bob@corp.example")));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 2, "one selector read, one lookup");
    assert_eq!(requests[0]["name"], "slack");
    assert_eq!(
        requests[1]["name"], "people",
        "the lookup is asked of the entry slack names"
    );
    assert_eq!(requests[1]["artifact"], serde_json::json!({ "member": "slack:U-bob" }));

    // `null` is a definitive answer: the member keeps its qualified identity.
    source.principal(Some(None));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 4);

    // No answer from the lookup entry is an operational refusal and no decision.
    let decided = audit_len(&runtime);
    source.principal(None);
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), decided);
    assert!(decided > before);
    drop(runtime);

    // The reopened deployment replays the pinned lookups without consulting anyone; only
    // the fresh act reads the source and the lookup entry again.
    let config = Config::load(&dir.path().join("appa.toml")).expect("the fixture validates");
    let reopened = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment reopens"));
    source.principal(Some(Some("bob@corp.example")));
    assert_eq!(
        propose(&reopened, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(source.requests().len(), 8);
}

#[tokio::test]
async fn a_bare_provider_prefix_recipient_is_a_literal_reader() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // "slack:" names no member, so it denotes itself: the check decides without any
    // consult instead of looping against the source.
    assert!(matches!(
        propose(&runtime, send("slack:")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty(), "a bare prefix is never looked up");
}

fn send_capped() -> ProposedCall {
    ProposedCall {
        tool: "send_capped".to_string(),
        arguments: raw(serde_json::json!({})),
    }
}

#[tokio::test]
async fn a_cap_written_with_a_group_is_read_per_act_from_the_source() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    source.members(Some(vec!["bob@corp.example"]));
    assert_eq!(
        propose(&runtime, send_capped()).await,
        HookDecision::AllowCall { spawn: None }
    );
    let requests = source.requests();
    assert_eq!(requests.len(), 1, "one act, one consult per selector");
    assert_eq!(
        requests[0]["artifact"],
        serde_json::json!({ "selector": "user-group/team" })
    );
    ran(&runtime, send_capped()).await;

    source.members(Some(vec!["carol@corp.example"]));
    let HookDecision::DenyCall { feedback, .. } = propose(&runtime, send_capped()).await else {
        panic!("the moved cap blocks");
    };
    assert!(
        !feedback.contains("carol"),
        "the model hears a directory member: {feedback}"
    );
    assert_eq!(source.requests().len(), 2);

    let undecided = audit_len(&runtime);
    source.members(None);
    assert!(matches!(
        propose(&runtime, send_capped()).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), undecided);
    assert!(audit_len(&runtime) > before);

    let audit = serde_json::to_string(&runtime.audit(&root()).expect("the audit reads")).expect("the audit serializes");
    assert!(!audit.contains("carol"), "the audit leaks a directory member: {audit}");
    drop(runtime);

    // The reopened deployment replays the log — its pinned answers included — without
    // consulting the source; only the fresh act reads it again.
    let config = Config::load(&dir.path().join("appa.toml")).expect("the fixture validates");
    let reopened = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment reopens"));
    source.members(Some(vec!["bob@corp.example"]));
    assert_eq!(
        propose(&reopened, send_capped()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(source.requests().len(), 4);
}

/// `POLICY` with a channel-keyed sink: posting to a channel requires that channel's members
/// among the current readers, the channel read from the call.
const CHANNEL_POLICY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["alice@corp.example", "bob@corp.example"] }

[[policy.tool]]
name = "post"
parameters = { type = "object", properties = { channel_id = { type = "string" }, text = { type = "string" } }, required = ["channel_id", "text"] }
requires = { audience = { contains = ["@slack:channel/$channel_id"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "channel/<id>" }]
"#;

fn post_to(channel_id: &str) -> ProposedCall {
    ProposedCall {
        tool: "post".to_string(),
        arguments: raw(serde_json::json!({ "channel_id": channel_id, "text": "hi" })),
    }
}

/// A selector placeholder is instantiated from each call: the source is asked for exactly the
/// collection the call's argument spells, and its answer is checked as a static mention's is.
#[tokio::test]
async fn a_selector_placeholder_asks_the_source_for_the_collection_the_call_spells() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed_under(&dir, CHANNEL_POLICY, &url).await;

    source.members(Some(vec!["alice@corp.example"]));
    assert_eq!(
        propose(&runtime, post_to("C1")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, post_to("C1")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["declaration"]["templates"],
        serde_json::json!(["viewer", "channel/<id>"])
    );
    assert_eq!(requests[0]["artifact"], serde_json::json!({ "selector": "channel/C1" }));

    // Another channel is another collection, asked for by its own selector.
    source.members(Some(vec!["alice@corp.example", "slack:U-carol"]));
    assert!(matches!(
        propose(&runtime, post_to("C2")).await,
        HookDecision::DenyCall { .. }
    ));
    let requests = source.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["artifact"], serde_json::json!({ "selector": "channel/C2" }));
    assert!(audit_len(&runtime) > 0, "the decided acts read back");
}

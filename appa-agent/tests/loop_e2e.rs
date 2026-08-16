
mod harness;

use std::sync::Arc;

use appa_agent::wire::WireMessage;
use appa_agent::{
    Agent, ArgumentKey, Limits, OpenAiCompatible, OpenAiConfig, Outcome, SpawnTool, ToolCatalogue, ToolName, ToolShim,
    TranscriptHead,
};
use appa_runtime_v2::api::TrajectoryId;
use harness::{Decisions, Provider, ToolHost, runtime, tool};

const NEUTRAL: &str = r#"
version = 1

[[policy.tool]]
name = "read_hr"

[[policy.tool]]
name = "send_email"
"#;

async fn agent(runtime: appa_runtime_v2::api::Runtime, provider: &Provider, host: &ToolHost, tools: &[&str]) -> Agent {
    let endpoint = provider.clone().serve().await;
    let shim = format!("{}/tools", host.clone().serve().await);
    Agent::new(
        Arc::new(runtime),
        OpenAiCompatible::with_http_client(
            OpenAiConfig::new(endpoint, "fixture/model", "test-key"),
            appa_agent::HttpClient::loopback(),
        ),
        ToolShim::new(shim),
        ToolCatalogue::new(tools.iter().map(|name| tool(name)).collect()),
    )
    .with_head(TranscriptHead::new(vec![WireMessage::system("You are a fixture.")]))
    .with_limits(Limits {
        max_inference_rounds: 8,
        ..Limits::default()
    })
}

fn root() -> TrajectoryId {
    TrajectoryId("agent-test".to_string())
}

#[tokio::test]
async fn a_released_call_runs_and_its_output_reaches_the_model() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .says("Alice is a Staff Engineer.");
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer");

    let agent = agent(runtime(&dir, NEUTRAL, ""), &provider, &host, &["read_hr"]).await;
    let outcome = agent.run(root(), "Who is Alice?", Default::default()).await;

    assert_eq!(outcome, Outcome::Answer("Alice is a Staff Engineer.".to_string()));
    assert_eq!(
        provider.tool_result(1, "call_0"),
        "Alice Chen, Staff Engineer",
        "the released call's output crosses as produced",
    );
    assert_eq!(
        host.calls(),
        vec![serde_json::json!({"tool": "read_hr", "arguments": {"who": "alice"}})],
        "the host ran exactly the call the model proposed",
    );
}

#[tokio::test]
async fn the_agent_owns_the_transcript_it_shows_the_model() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider.says("done");
    let host = ToolHost::default();

    let agent = agent(runtime(&dir, NEUTRAL, ""), &provider, &host, &["read_hr"]).await;
    agent.run(root(), "Who is Alice?", Default::default()).await;

    let transcript = provider.transcript(0);
    assert_eq!(transcript.len(), 2);
    assert_eq!(transcript[0]["role"], "system");
    assert_eq!(
        transcript[1],
        serde_json::json!({"role": "user", "content": "Who is Alice?"})
    );

    let requests = provider.requests();
    let advertised: Vec<&str> = requests[0]["tools"]
        .as_array()
        .expect("the agent advertises its catalogue")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        advertised,
        vec!["read_hr", "execute_remedy_plan"],
        "the control tool is advertised beside the host's own",
    );
}

#[tokio::test]
async fn a_denied_call_never_runs_and_its_feedback_reaches_the_model() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let policy = r#"
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["staff"] } }

[[policy.tool]]
name = "send_email"
requires = { audience = { includes = ["public"] } }
delta = {}
"#;
    let provider = Provider::default();
    provider
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .calls("send_email", serde_json::json!({"to": "all"}))
        .says("I could not send it.");
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer")
        .answers("send_email", "sent");

    let agent = agent(runtime(&dir, policy, ""), &provider, &host, &["read_hr", "send_email"]).await;
    let outcome = agent
        .run(root(), "Email Alice's record to everyone.", Default::default())
        .await;

    assert_eq!(outcome, Outcome::Answer("I could not send it.".to_string()));
    assert_eq!(
        host.calls(),
        vec![serde_json::json!({"tool": "send_email", "arguments": {"to": "all"}})],
        "the read was denied, so it never reached the host — and, never having run, it left the \
         audience the mail needs intact",
    );
    assert!(
        !provider.tool_result(1, "call_0").is_empty(),
        "the denial's feedback is what the model gets for the call that did not run",
    );
}

#[tokio::test]
async fn a_denied_call_is_recorded_for_the_host_in_the_engines_own_words() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let policy = r#"
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["staff"] } }

[[policy.tool]]
name = "send_email"
requires = { audience = { includes = ["public"] } }
delta = {}
"#;
    let provider = Provider::default();
    provider
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .calls("send_email", serde_json::json!({"to": "all"}))
        .says("I could not send it.");
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer")
        .answers("send_email", "sent");

    let agent = agent(runtime(&dir, policy, ""), &provider, &host, &["read_hr", "send_email"]).await;
    let decisions = Decisions::recording();
    agent
        .run(root(), "Email Alice's record to everyone.", Default::default())
        .await;

    let recorded = decisions.recorded();
    let feedback = provider.tool_result(1, "call_0").replace('\n', "\\n");
    assert!(
        recorded.iter().any(|line| line.contains(&feedback)),
        "the block the model was told about is in the host's record too. Recorded: {recorded:?}",
    );
    assert!(
        recorded.iter().any(|line| line.contains("send_email")),
        "and so is the call that did run, so a host sees the whole conversation. Recorded: {recorded:?}",
    );
}

#[tokio::test]
async fn a_duplicate_key_proposal_is_refused_before_the_host_sees_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider
        .calls_raw("read_hr", r#"{"who":"alice","who":"mallory"}"#)
        .says("I could not read it.");
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer");

    let agent = agent(runtime(&dir, NEUTRAL, ""), &provider, &host, &["read_hr"]).await;
    let outcome = agent.run(root(), "Who is Alice?", Default::default()).await;

    assert_eq!(outcome, Outcome::Answer("I could not read it.".to_string()));
    assert_eq!(
        host.calls(),
        Vec::<serde_json::Value>::new(),
        "a call the engine would not read must never run. Ran: {:?}",
        host.calls(),
    );
    assert!(
        !provider.tool_result(1, "call_0").is_empty(),
        "and the model is told why, so it can propose something readable",
    );
}

#[tokio::test]
async fn a_narrowing_read_is_blocked_before_it_runs_and_the_options_reach_the_model() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let policy = r#"
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["hr"] } }

[policy.boundary]
audience = { exactly = ["public"] }
"#;
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, SSN 4821-9930");

    let provider = Provider::default();
    provider
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .says("I could not read it without narrowing this session.");

    let agent = agent(runtime(&dir, policy, ""), &provider, &host, &["read_hr"]).await;
    agent.run(root(), "Who is Alice?", Default::default()).await;

    assert!(
        host.calls().is_empty(),
        "a blocked call must never reach the host. Ran: {:?}",
        host.calls(),
    );
    let feedback = provider.tool_result(1, "call_0");
    assert!(
        harness::offer_id(&feedback).is_some(),
        "the block must surface an offer id the model can pursue. Got: {feedback}",
    );
}

#[tokio::test]
async fn an_uncarried_output_crosses_as_the_engine_s_account_of_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let host = ToolHost::default();
    host.answers("read_hr", &format!("SSN 4821-9930{}", "x".repeat(70_000)));

    let provider = Provider::default();
    provider
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .says("The record was too large to read.");

    let agent = agent(runtime(&dir, NEUTRAL, ""), &provider, &host, &["read_hr"]).await;
    agent.run(root(), "Who is Alice?", Default::default()).await;

    let shown = provider.tool_result(1, "call_0");
    assert!(
        !shown.contains("4821-9930"),
        "an uncarried body must not reach the model. Got {} bytes",
        shown.len(),
    );
    assert_eq!(shown, "[appa] the result was not carried; nothing was admitted");
    assert_eq!(host.calls().len(), 1, "the call ran; only its body did not cross");
}

const FORKING: &str = r#"
version = 1

[[policy.tool]]
name = "delegate"
parameters = { type = "object", properties = { task = { type = "string" } } }

# A neutral delta establishes the output's label. Without one the value
# is unknown, and the child's return blocks on missing facts rather than
# on anything this test is about.
[[policy.tool]]
name = "read_hr"
delta = {}

[policy.deployment]
context_control = true
"#;

fn delegating(agent: Agent) -> Agent {
    agent.with_spawn_tool(SpawnTool {
        name: ToolName::new("delegate"),
        errand: ArgumentKey::new("task"),
    })
}

#[tokio::test]
async fn a_child_runs_its_own_trajectory_and_its_final_message_crosses_back() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider
        .calls("delegate", serde_json::json!({"task": "Look up Alice."}))
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .says("Alice Chen is a Staff Engineer.")
        .says("The lookup says: Alice Chen is a Staff Engineer.");
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer");

    let agent = delegating(agent(runtime(&dir, FORKING, ""), &provider, &host, &["delegate", "read_hr"]).await);
    let outcome = agent.run(root(), "Find out about Alice.", Default::default()).await;

    assert_eq!(
        outcome,
        Outcome::Answer("The lookup says: Alice Chen is a Staff Engineer.".to_string()),
    );
    assert_eq!(
        host.calls().len(),
        1,
        "the spawn call opens a child; it is not dispatched to the host",
    );
    let child_opening = provider.transcript(1);
    assert_eq!(
        child_opening.last().expect("the child gets its errand"),
        &serde_json::json!({"role": "user", "content": "Look up Alice."}),
    );
    assert_eq!(
        provider.tool_result(3, "call_0"),
        "Alice Chen is a Staff Engineer.",
        "the parent's spawn call is answered with what crossed the return channel",
    );
}

#[tokio::test]
async fn a_child_that_says_nothing_returns_no_value() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider
        .calls("delegate", serde_json::json!({"task": "Do it yourself."}))
        .says_nothing()
        .says("The child handled it.");
    let host = ToolHost::default();

    let agent = delegating(agent(runtime(&dir, FORKING, ""), &provider, &host, &["delegate"]).await);
    let outcome = agent.run(root(), "Delegate the work.", Default::default()).await;

    assert_eq!(outcome, Outcome::Answer("The child handled it.".to_string()));
    assert_eq!(
        provider.tool_result(2, "call_0"),
        "[appa] the result was not carried; nothing was admitted",
        "a void return admits no value, so the spawn call closes carrying none",
    );
}

#[tokio::test]
async fn the_fork_depth_ceiling_refuses_a_spawn_below_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    let delegate = || serde_json::json!({"task": "Go deeper."});
    provider
        .calls("delegate", delegate())
        .calls("delegate", delegate())
        .calls("delegate", delegate())
        .says("I could not delegate further.")
        .says("child done")
        .says("Everything is done.");
    let host = ToolHost::default();

    let agent =
        delegating(agent(runtime(&dir, FORKING, ""), &provider, &host, &["delegate"]).await).with_limits(Limits {
            max_inference_rounds: 12,
            max_forks: 8,
            max_fork_depth: 2,
            ..Limits::default()
        });
    let outcome = agent
        .run(root(), "Delegate as deep as you can.", Default::default())
        .await;

    assert_eq!(outcome, Outcome::Answer("Everything is done.".to_string()));
    let refusal = provider.tool_result(3, "call_2");
    assert_eq!(
        refusal, "no child was opened: this run's fork budget is spent",
        "the third spawn sits at the ceiling and opens no child",
    );
}

#[tokio::test]
async fn a_sanitized_child_return_reaches_the_parent_redacted() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let policy = r#"
version = 1

[[policy.tool]]
name = "delegate"
parameters = { type = "object", properties = { task = { type = "string" } } }

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["internal"] } }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[policy.child]
return_sanitizer = "scrub"

[policy.deployment]
context_control = true
confined_child_return = true
"#;
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, SSN 4821-9930")
        .sanitizes_to("Alice Chen, a staff member.");
    let sanitizer_url = format!("{}/sanitizer", host.clone().serve().await);
    let externals = format!("[externals.sanitizers.scrub]\nurl = \"{sanitizer_url}\"\n");

    let provider = Provider::default();
    provider
        .calls("delegate", serde_json::json!({"task": "Look up Alice."}))
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .says("Alice Chen, SSN 4821-9930")
        .says("Understood.");

    let agent = delegating(
        agent(
            runtime(&dir, policy, &externals),
            &provider,
            &host,
            &["delegate", "read_hr"],
        )
        .await,
    );
    agent.run(root(), "Find out about Alice.", Default::default()).await;

    let crossed = provider.tool_result(3, "call_0");
    assert_eq!(
        crossed, "Alice Chen, a staff member.",
        "the admitted return crosses, not the child's own words",
    );
}

#[tokio::test]
async fn a_child_return_over_the_output_cap_still_crosses_whole() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let long = format!("Alice Chen. {}", "detail. ".repeat(10_000));
    assert!(long.len() > 65_536, "the return must exceed the deployment's body cap");

    let provider = Provider::default();
    provider
        .calls("delegate", serde_json::json!({"task": "Report in full."}))
        .says(&long)
        .says("Got the full report.");
    let host = ToolHost::default();

    let agent = delegating(agent(runtime(&dir, FORKING, ""), &provider, &host, &["delegate"]).await);
    let outcome = agent.run(root(), "Get the full report.", Default::default()).await;

    assert_eq!(outcome, Outcome::Answer("Got the full report.".to_string()));
    assert_eq!(
        provider.tool_result(2, "call_0"),
        long,
        "the whole return crosses: the byte cap governs tool outputs, not the return channel",
    );
}

const APPROVED_WIRE: &str = r#"
version = 1

[[policy.tool]]
name = "wire"
parameters = { type = "object", properties = { amount = { type = "integer" } } }
requires = { attention = ["irreversible"] }
delta = {}

[[policy.authority]]
name = "approver"
[policy.authority.mandate]
attends = ["irreversible"]
"#;

#[tokio::test]
async fn pursuing_a_surfaced_offer_authorizes_the_exact_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let host = ToolHost::default();
    host.answers("wire", "wired").rules("approve");
    let authority_url = format!("{}/authority", host.clone().serve().await);
    let externals = format!("[externals.authorities.approver]\nurl = \"{authority_url}\"\n");

    let provider = Provider::default();
    provider
        .calls("wire", serde_json::json!({"amount": 100}))
        .pursues_the_offer()
        .calls("wire", serde_json::json!({"amount": 100}))
        .says("The transfer went through.");

    let agent = agent(runtime(&dir, APPROVED_WIRE, &externals), &provider, &host, &["wire"]).await;
    let outcome = agent.run(root(), "Wire 100.", Default::default()).await;

    assert_eq!(outcome, Outcome::Answer("The transfer went through.".to_string()));
    assert_eq!(
        host.calls().iter().filter(|call| call["tool"] == "wire").count(),
        1,
        "the call runs once, and only after the offer authorized it",
    );
    assert_eq!(
        provider.tool_result(3, "call_2"),
        "wired",
        "the authorized re-proposal runs and its output crosses",
    );
}

const NARROWING: &str = r#"
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["staff"] } }

[[policy.tool]]
name = "send_email"
requires = { audience = { includes = ["public"] } }
delta = {}
"#;

#[tokio::test]
async fn a_second_turn_continues_where_the_first_one_left_the_trajectory() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .pursues_the_offer()
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .says("Alice is a Staff Engineer.")
        .calls("read_hr", serde_json::json!({"who": "bob"}))
        .says("Bob is a Principal Engineer.");
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer");

    let agent = agent(
        runtime(&dir, NARROWING, ""),
        &provider,
        &host,
        &["read_hr", "send_email"],
    )
    .await;
    let mut transcript = appa_agent::Transcript::default();
    let first = agent
        .turn(root(), &mut transcript, "Who is Alice?", Default::default())
        .await;
    let second = agent
        .turn(root(), &mut transcript, "And Bob?", Default::default())
        .await;

    assert_eq!(first, Outcome::Answer("Alice is a Staff Engineer.".to_string()));
    assert_eq!(second, Outcome::Answer("Bob is a Principal Engineer.".to_string()));
    assert_eq!(
        provider.tool_result(5, "call_4"),
        "Alice Chen, Staff Engineer",
        "the second turn's read runs on the first turn's accepted label, unblocked",
    );
    let opening = provider.transcript(4);
    assert!(
        opening.iter().any(|message| message["content"] == "Who is Alice?"),
        "and it sees the first turn's conversation. Showed: {opening:?}",
    );
}

#[tokio::test]
async fn a_stopped_turn_leaves_its_transcript_behind() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider.calls("read_hr", serde_json::json!({"who": "alice"}));
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer");

    let agent = agent(runtime(&dir, NEUTRAL, ""), &provider, &host, &["read_hr"])
        .await
        .with_limits(Limits {
            max_inference_rounds: 1,
            ..Limits::default()
        });
    let mut transcript = appa_agent::Transcript::default();
    let outcome = agent
        .turn(root(), &mut transcript, "Who is Alice?", Default::default())
        .await;

    assert!(matches!(outcome, Outcome::Stopped(_)), "the round ceiling ends it");
    let said: Vec<&str> = transcript
        .messages()
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect();
    assert!(
        said.contains(&"Who is Alice?") && said.contains(&"Alice Chen, Staff Engineer"),
        "the task and what the call returned both survive the stop. Left: {said:?}",
    );
}

#[tokio::test]
async fn an_observer_receives_every_record_typed() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let provider = Provider::default();
    provider
        .calls("read_hr", serde_json::json!({"who": "alice"}))
        .says("I could not read it.");
    let host = ToolHost::default();
    host.answers("read_hr", "Alice Chen, Staff Engineer");

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let agent = agent(
        runtime(&dir, NARROWING, ""),
        &provider,
        &host,
        &["read_hr", "send_email"],
    )
    .await
    .with_observer(tx);
    agent.run(root(), "Who is Alice?", Default::default()).await;
    drop(agent);

    let mut records = Vec::new();
    while let Some(recorded) = rx.recv().await {
        assert_eq!(recorded.trajectory, root(), "a root run records under its root");
        records.push(recorded.record);
    }

    let proposed = records
        .iter()
        .find_map(|record| match record {
            appa_agent::Record::Proposes { call, tool, .. } => Some((call.clone(), tool.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the proposal is recorded. Recorded: {records:?}"));
    assert_eq!(
        proposed,
        (appa_agent::CallId("call_0".to_string()), "read_hr".to_string())
    );

    let blocked = records
        .iter()
        .find_map(|record| match record {
            appa_agent::Record::Blocked { call, feedback, .. } => Some((call.clone(), feedback.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the block is recorded. Recorded: {records:?}"));
    assert_eq!(blocked.0, proposed.0, "under the call id the host is rendering against");
    assert_eq!(
        blocked.1,
        provider.tool_result(1, "call_0"),
        "in the same words the model was given",
    );
    assert!(
        records.contains(&appa_agent::Record::Answers {
            text: "I could not read it.".to_string()
        }),
        "and the run's answer closes the record. Recorded: {records:?}",
    );
}

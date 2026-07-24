use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use appa_agent::{Agent, Endpoint, ModelId, OpenAiCompatible, OpenAiConfig, Outcome, ProviderError, TenantId};
use appa_engine::fact::{BoundaryKind, Fact};
use appa_engine::value::{ToolName, TrajectoryId};
use appa_runtime::tool::{FORK, RenderedCall, SUBMIT_RESULT};
use appa_runtime::wire::{ChatCompletionRequest, ChatCompletionResponse, WireFunctionCall, WireMessage, WireToolCall};
use appa_runtime::{Config, Limits, Mediator};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "test/model";
const API_KEY: &str = "test-api-key";

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl CapturedRequest {
    fn completion(&self) -> ChatCompletionRequest {
        serde_json::from_slice(&self.body).expect("request body is a chat completion")
    }
}

fn provider(endpoint: impl Into<Endpoint>) -> OpenAiCompatible {
    OpenAiCompatible::new(
        OpenAiConfig::new(endpoint, ModelId::new(MODEL), API_KEY).with_request_timeout(Duration::from_secs(5)),
    )
}

fn response(message: WireMessage) -> String {
    serde_json::to_string(&ChatCompletionResponse::single("cmpl-test", message, "stop")).expect("response serializes")
}

fn call(id: &str, name: &str, arguments: &str) -> WireToolCall {
    WireToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: WireFunctionCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

fn tool_completion(calls: Vec<WireToolCall>) -> WireMessage {
    WireMessage::assistant_tool_calls(calls)
}

fn mediator(policy: &str) -> Arc<Mediator> {
    Arc::new(
        Mediator::new(Config::from_toml_str(policy).expect("policy parses"), BTreeMap::new())
            .expect("mediator assembles"),
    )
}

fn tool_names(request: &ChatCompletionRequest) -> Vec<&str> {
    request
        .tools
        .as_ref()
        .expect("agent always supplies its runtime tool surface")
        .iter()
        .map(|tool| tool.function.name.as_str())
        .collect()
}

fn facts(mediator: &Mediator, tenant: &TenantId, session: &TrajectoryId) -> Vec<Fact> {
    mediator.snapshot(tenant, session).expect("session snapshot").0
}

fn turn_end_counts(log: &[Fact]) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for fact in log {
        if let Fact::Boundary {
            trajectory,
            kind: BoundaryKind::TurnEnd,
        } = fact
        {
            *counts.entry(trajectory.as_str()).or_default() += 1;
        }
    }
    counts
}

async fn spawn_model(responses: Vec<String>) -> (Endpoint, tokio::task::JoinHandle<Vec<CapturedRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind model server");
    let address = listener.local_addr().expect("model server address");
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in responses {
            let (mut socket, _) = listener.accept().await.expect("accept model request");
            requests.push(read_request(&mut socket).await);
            write_response(&mut socket, "200 OK", &[], &body).await;
        }
        requests
    });
    (Endpoint::new(format!("http://{address}/v1")), handle)
}

async fn spawn_response(
    status: &'static str,
    headers: Vec<(String, String)>,
    body: String,
) -> (Endpoint, tokio::task::JoinHandle<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind response server");
    let address = listener.local_addr().expect("response server address");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept request");
        let request = read_request(&mut socket).await;
        write_response(&mut socket, status, &headers, &body).await;
        request
    });
    (Endpoint::new(format!("http://{address}/v1")), handle)
}

async fn read_request(socket: &mut TcpStream) -> CapturedRequest {
    let mut received = Vec::new();
    let mut buffer = [0u8; 8192];
    let (header_end, body_len) = loop {
        let count = socket.read(&mut buffer).await.expect("read request");
        assert_ne!(count, 0, "connection closed before request completed");
        received.extend_from_slice(&buffer[..count]);
        if let Some(header_end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&received[..header_end]).expect("headers are UTF-8");
            let body_len = headers
                .lines()
                .skip(1)
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("valid content length"))
                })
                .unwrap_or_default();
            if received.len() >= header_end + 4 + body_len {
                break (header_end, body_len);
            }
        }
    };

    let head = std::str::from_utf8(&received[..header_end]).expect("headers are UTF-8");
    let mut lines = head.lines();
    let request_line = lines.next().expect("request line");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .expect("request path")
        .to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let body_start = header_end + 4;
    CapturedRequest {
        path,
        headers,
        body: received[body_start..body_start + body_len].to_vec(),
    }
}

async fn write_response(socket: &mut TcpStream, status: &str, headers: &[(String, String)], body: &str) {
    let extra_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.expect("write response");
}

#[tokio::test]
async fn provider_sends_pinned_configuration_and_parses_final_and_tool_call_completions() {
    let expected_call = call("call-1", "inspect", r#"{"id":7}"#);
    let (endpoint, server) = spawn_model(vec![
        response(WireMessage::assistant("provider final")),
        response(tool_completion(vec![expected_call.clone()])),
    ])
    .await;
    let provider = provider(endpoint);
    let final_completion = provider
        .complete(ChatCompletionRequest {
            model: "caller-model".to_string(),
            messages: vec![WireMessage::user("request")],
            tools: None,
            stream: Some(true),
        })
        .await
        .expect("final completion");
    assert_eq!(final_completion.content.as_deref(), Some("provider final"));
    assert!(final_completion.tool_calls.is_empty());

    let tool_completion = provider
        .complete(ChatCompletionRequest {
            model: String::new(),
            messages: vec![WireMessage::user("tool request")],
            tools: None,
            stream: None,
        })
        .await
        .expect("tool-call completion");
    assert_eq!(tool_completion.content, None);
    assert_eq!(tool_completion.tool_calls, [expected_call]);
    let requests = server.await.expect("model server joins");
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-api-key")
        );
        assert_eq!(request.completion().model, MODEL);
        assert_eq!(request.completion().stream, None);
    }
    assert_eq!(requests[0].completion().messages, [WireMessage::user("request")]);
    assert_eq!(requests[1].completion().messages, [WireMessage::user("tool request")]);
}

#[tokio::test]
async fn provider_faults_redirects_and_bad_bodies_fail_closed() {
    let target = TcpListener::bind("127.0.0.1:0").await.expect("bind redirect target");
    let target_address = target.local_addr().expect("redirect target address");
    drop(target);
    let (redirect_endpoint, redirect_server) = spawn_response(
        "307 Temporary Redirect",
        vec![(
            "Location".to_string(),
            format!("http://{target_address}/v1/chat/completions"),
        )],
        String::new(),
    )
    .await;
    let request = ChatCompletionRequest {
        model: String::new(),
        messages: vec![WireMessage::user("request")],
        tools: None,
        stream: None,
    };
    assert_eq!(
        provider(redirect_endpoint).complete(request.clone()).await,
        Err(ProviderError::Status)
    );
    redirect_server.await.expect("redirect server joins");

    let (malformed_endpoint, malformed_server) = spawn_response("200 OK", vec![], "not-json".to_string()).await;
    assert_eq!(
        provider(malformed_endpoint).complete(request.clone()).await,
        Err(ProviderError::Malformed)
    );
    malformed_server.await.expect("malformed server joins");

    let (oversized_endpoint, oversized_server) = spawn_response("200 OK", vec![], "x".repeat(17)).await;
    let oversized =
        OpenAiCompatible::new(OpenAiConfig::new(oversized_endpoint, MODEL, API_KEY).with_response_body_cap_bytes(16));
    assert_eq!(oversized.complete(request.clone()).await, Err(ProviderError::Malformed));
    oversized_server.await.expect("oversized server joins");

    let no_choice = serde_json::to_string(&ChatCompletionResponse {
        id: "cmpl-empty".to_string(),
        object: "chat.completion".to_string(),
        choices: vec![],
    })
    .expect("empty response serializes");
    let (empty_endpoint, empty_server) = spawn_response("200 OK", vec![], no_choice).await;
    assert_eq!(
        provider(empty_endpoint).complete(request.clone()).await,
        Err(ProviderError::NoChoice)
    );
    empty_server.await.expect("empty server joins");

    let unused = TcpListener::bind("127.0.0.1:0").await.expect("bind unused port");
    let unused_address = unused.local_addr().expect("unused address");
    drop(unused);
    assert_eq!(
        provider(format!("http://{unused_address}/v1")).complete(request).await,
        Err(ProviderError::Transport)
    );
}

#[tokio::test]
async fn agent_returns_a_root_final_answer() {
    let (endpoint, server) = spawn_model(vec![response(WireMessage::assistant("root answer"))]).await;
    let mediator = mediator("version = 1\n");
    let agent = Agent::new(mediator.clone(), provider(endpoint), Limits::default());
    let tenant = TenantId::new("tenant-final");
    let session = agent.create_session(tenant.clone());
    let outcome = agent
        .run_existing(tenant.clone(), session.clone(), "root task", CancellationToken::new())
        .await
        .expect("agent run");

    assert_eq!(outcome, Outcome::Final("root answer".to_string()));
    let requests = server.await.expect("model server joins");
    assert_eq!(requests[0].completion().messages, [WireMessage::user("root task")]);
    assert_eq!(
        turn_end_counts(&facts(&mediator, &tenant, &session)),
        [(session.as_str(), 1)].into()
    );
}

#[tokio::test]
async fn ordinary_tool_result_enters_the_next_request_only_through_the_runtime_transcript() {
    let (tool_endpoint, tool_server) = spawn_response("200 OK", vec![], "tool value".to_string()).await;
    let policy = format!(
        r#"
version = 1
[[tool]]
name = "lookup"
delta = {{}}
[tool.implementation.http]
url = "{}/invoke"
"#,
        tool_endpoint.as_str().trim_end_matches("/v1")
    );
    let first_call = call("lookup-1", "lookup", r#"{"key":"alpha"}"#);
    let (model_endpoint, model_server) = spawn_model(vec![
        response(tool_completion(vec![first_call.clone()])),
        response(WireMessage::assistant("root answer")),
    ])
    .await;
    let mediator = mediator(&policy);
    let agent = Agent::new(mediator, provider(model_endpoint), Limits::default());
    let outcome = agent
        .run_new(TenantId::new("tenant-tool"), "use the tool", CancellationToken::new())
        .await
        .expect("agent run")
        .1;
    assert_eq!(outcome, Outcome::Final("root answer".to_string()));

    let tool_request = tool_server.await.expect("tool server joins");
    assert_eq!(tool_request.path, "/invoke");
    assert_eq!(
        serde_json::from_slice::<RenderedCall>(&tool_request.body).expect("rendered tool call"),
        RenderedCall {
            tool: ToolName::new("lookup"),
            arguments: serde_json::json!({ "key": "alpha" }),
        }
    );

    let model_requests = model_server.await.expect("model server joins");
    assert_eq!(model_requests.len(), 2);
    let first = model_requests[0].completion();
    assert_eq!(first.messages, [WireMessage::user("use the tool")]);
    assert_eq!(tool_names(&first), ["lookup", "execute_remedy_plan", FORK]);
    let second = model_requests[1].completion();
    assert_eq!(
        second.messages,
        [
            WireMessage::user("use the tool"),
            WireMessage::assistant_tool_calls(vec![first_call]),
            WireMessage::tool_result("lookup-1", "tool value"),
        ]
    );
}

#[tokio::test]
async fn serial_fork_joins_only_the_admitted_child_return_and_an_outcome_specific_fork_response() {
    let fork_call = call("fork-1", FORK, r#"{"task":"child task"}"#);
    let submit_call = call("submit-1", SUBMIT_RESULT, r#"{"value":"child finding"}"#);
    let (endpoint, server) = spawn_model(vec![
        response(tool_completion(vec![fork_call.clone()])),
        response(tool_completion(vec![submit_call])),
        response(WireMessage::assistant("root answer")),
    ])
    .await;
    let mediator = mediator("version = 1\n");
    let agent = Agent::new(mediator.clone(), provider(endpoint), Limits::default());
    let tenant = TenantId::new("tenant-fork");
    let (root, outcome) = agent
        .run_new(tenant.clone(), "root task", CancellationToken::new())
        .await
        .expect("agent run");
    assert_eq!(outcome, Outcome::Final("root answer".to_string()));

    let requests = server.await.expect("model server joins");
    assert_eq!(requests.len(), 3);
    let root_first = requests[0].completion();
    assert_eq!(root_first.messages, [WireMessage::user("root task")]);
    assert_eq!(tool_names(&root_first), ["execute_remedy_plan", FORK]);

    let child = requests[1].completion();
    assert_eq!(child.messages.len(), 2);
    assert_eq!(child.messages[0], WireMessage::user("root task"));
    assert_eq!(child.messages[1].role, "user");
    assert!(
        child.messages[1]
            .content
            .as_deref()
            .is_some_and(|content| content.starts_with("child task"))
    );
    assert_eq!(tool_names(&child), ["execute_remedy_plan", FORK, SUBMIT_RESULT]);

    let parent = requests[2].completion();
    assert_eq!(parent.messages.len(), 4);
    assert_eq!(parent.messages[0], WireMessage::user("root task"));
    assert_eq!(parent.messages[1], WireMessage::assistant_tool_calls(vec![fork_call]));
    assert_eq!(parent.messages[2].role, "tool");
    assert_eq!(parent.messages[2].tool_call_id.as_deref(), Some("fork-1"));
    assert!(parent.messages[2].content.is_some());
    assert_eq!(parent.messages[3], WireMessage::user("child finding"));
    assert_eq!(tool_names(&parent), ["execute_remedy_plan", FORK]);

    let log = facts(&mediator, &tenant, &root);
    assert_eq!(turn_end_counts(&log).len(), 2);
    assert!(turn_end_counts(&log).values().all(|count| *count == 1));
}

#[tokio::test]
async fn child_final_prose_without_submit_result_never_enters_the_parent_request() {
    let fork_call = call("fork-1", FORK, r#"{"task":"child task"}"#);
    let child_final = "child-only final";
    let (endpoint, server) = spawn_model(vec![
        response(tool_completion(vec![fork_call.clone()])),
        response(WireMessage::assistant(child_final)),
        response(WireMessage::assistant("root answer")),
    ])
    .await;
    let agent = Agent::new(mediator("version = 1\n"), provider(endpoint), Limits::default());
    let outcome = agent
        .run_new(
            TenantId::new("tenant-child-final"),
            "root task",
            CancellationToken::new(),
        )
        .await
        .expect("agent run")
        .1;
    assert_eq!(outcome, Outcome::Final("root answer".to_string()));

    let requests = server.await.expect("model server joins");
    let parent = requests[2].completion();
    assert_eq!(parent.messages.len(), 3);
    assert_eq!(parent.messages[0], WireMessage::user("root task"));
    assert_eq!(parent.messages[1], WireMessage::assistant_tool_calls(vec![fork_call]));
    assert_eq!(parent.messages[2].role, "tool");
    assert_eq!(parent.messages[2].tool_call_id.as_deref(), Some("fork-1"));
    assert!(
        parent
            .messages
            .iter()
            .all(|message| message.content.as_deref() != Some(child_final))
    );
}

#[tokio::test]
async fn shared_inference_and_fork_budgets_stop_every_started_turn_once() {
    let root_fork = call("fork-1", FORK, r#"{"task":"child task"}"#);
    let (inference_endpoint, inference_server) =
        spawn_model(vec![response(tool_completion(vec![root_fork.clone()]))]).await;
    let inference_mediator = mediator("version = 1\n");
    let inference_agent = Agent::new(
        inference_mediator.clone(),
        provider(inference_endpoint),
        Limits {
            max_inference_rounds: 1,
            ..Limits::default()
        },
    );
    let tenant = TenantId::new("tenant-inference-budget");
    let (root, outcome) = inference_agent
        .run_new(tenant.clone(), "root task", CancellationToken::new())
        .await
        .expect("budgeted run");
    assert!(matches!(outcome, Outcome::PolicyStop(_)));
    assert_eq!(inference_server.await.expect("model server joins").len(), 1);
    let inference_log = facts(&inference_mediator, &tenant, &root);
    let inference_ends = turn_end_counts(&inference_log);
    assert_eq!(inference_ends.len(), 2);
    assert!(inference_ends.values().all(|count| *count == 1));

    let (fork_endpoint, fork_server) = spawn_model(vec![response(tool_completion(vec![root_fork]))]).await;
    let fork_mediator = mediator("version = 1\n");
    let fork_agent = Agent::new(
        fork_mediator.clone(),
        provider(fork_endpoint),
        Limits {
            max_forks: 0,
            ..Limits::default()
        },
    );
    let tenant = TenantId::new("tenant-fork-budget");
    let (root, outcome) = fork_agent
        .run_new(tenant.clone(), "root task", CancellationToken::new())
        .await
        .expect("budgeted run");
    assert!(matches!(outcome, Outcome::PolicyStop(_)));
    let fork_requests = fork_server.await.expect("model server joins");
    assert_eq!(fork_requests.len(), 1);
    assert_eq!(tool_names(&fork_requests[0].completion()), ["execute_remedy_plan"]);
    assert_eq!(turn_end_counts(&facts(&fork_mediator, &tenant, &root)).len(), 1);
}

#[tokio::test]
async fn hidden_fork_at_the_depth_limit_creates_no_grandchild() {
    let root_fork = call("fork-1", FORK, r#"{"task":"child task"}"#);
    let hidden_child_fork = call("fork-2", FORK, r#"{"task":"grandchild task"}"#);
    let (endpoint, server) = spawn_model(vec![
        response(tool_completion(vec![root_fork])),
        response(tool_completion(vec![hidden_child_fork])),
    ])
    .await;
    let mediator = mediator("version = 1\n");
    let agent = Agent::new(
        mediator.clone(),
        provider(endpoint),
        Limits {
            max_fork_depth: 1,
            ..Limits::default()
        },
    );
    let tenant = TenantId::new("tenant-depth-limit");
    let (root, outcome) = agent
        .run_new(tenant.clone(), "root task", CancellationToken::new())
        .await
        .expect("depth-limited run");

    assert!(matches!(outcome, Outcome::PolicyStop(_)));
    let requests = server.await.expect("model server joins");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_names(&requests[1].completion()),
        ["execute_remedy_plan", SUBMIT_RESULT]
    );

    let log = facts(&mediator, &tenant, &root);
    let fork_count = log
        .iter()
        .filter(|fact| {
            matches!(
                fact,
                Fact::Boundary {
                    kind: BoundaryKind::Fork { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(fork_count, 1, "the hidden fork must not create a grandchild");
    assert_eq!(turn_end_counts(&log).len(), 2);
}

#[tokio::test]
async fn cancellation_during_child_provider_wait_unwinds_the_whole_family() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind model server");
    let address = listener.local_addr().expect("model address");
    let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let fork_response = response(tool_completion(vec![call("fork-1", FORK, r#"{"task":"child task"}"#)]));
    let server = tokio::spawn(async move {
        let (mut root_socket, _) = listener.accept().await.expect("accept root request");
        let root_request = read_request(&mut root_socket).await;
        write_response(&mut root_socket, "200 OK", &[], &fork_response).await;

        let (mut child_socket, _) = listener.accept().await.expect("accept child request");
        let child_request = read_request(&mut child_socket).await;
        waiting_tx.send(()).ok();
        release_rx.await.ok();
        let final_response = response(WireMessage::assistant("late child answer"));
        let _ = child_socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{final_response}",
                    final_response.len()
                )
                .as_bytes(),
            )
            .await;
        [root_request, child_request]
    });

    let mediator = mediator("version = 1\n");
    let agent = Agent::new(
        mediator.clone(),
        provider(format!("http://{address}/v1")),
        Limits::default(),
    );
    let tenant = TenantId::new("tenant-cancel");
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run_tenant = tenant.clone();
    let run = tokio::spawn(async move {
        agent
            .run_new(run_tenant, "root task", run_cancel)
            .await
            .expect("cancelled run")
    });
    waiting_rx.await.expect("child provider request started");
    cancel.cancel();
    let (root, outcome) = run.await.expect("agent task joins");
    assert!(matches!(outcome, Outcome::PolicyStop(_)));
    release_tx.send(()).ok();

    let requests = server.await.expect("model server joins");
    assert_eq!(requests.len(), 2);
    let child = requests[1].completion();
    assert_eq!(child.messages.len(), 2);
    assert_eq!(child.messages[0], WireMessage::user("root task"));
    assert_eq!(child.messages[1].role, "user");
    assert!(
        child.messages[1]
            .content
            .as_deref()
            .is_some_and(|content| content.starts_with("child task"))
    );
    let log = facts(&mediator, &tenant, &root);
    let ends = turn_end_counts(&log);
    assert_eq!(ends.len(), 2);
    assert!(ends.values().all(|count| *count == 1));
    let ended_trajectories = ends.keys().copied().collect::<BTreeSet<_>>();
    let forked_trajectories = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::Boundary {
                trajectory,
                kind: BoundaryKind::Fork { .. },
            } => Some(trajectory.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(forked_trajectories.len(), 1);
    assert!(forked_trajectories.is_subset(&ended_trajectories));
}

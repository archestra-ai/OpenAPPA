//! Authenticated kagent proxy protocol with durable request receipts.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use sha2::{Digest, Sha256};

use appa_eventlog::{
    ProxyApprovalAdmission, ProxyBatchBinding, ProxyBatchPosition, ProxyDispatchBinding, ProxyEventAdmission,
    ProxyEventCompletion, ProxyOfferBinding,
};
use appa_runtime_api::{
    Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, Ruling, SpawnBinding, SpawnRef, ToolOutcome,
    TrajectoryId,
};

use crate::api::{BatchCallDecision, OfferId, OfferKind, RemedyArguments, RemedyOutcome, Runtime, ToolResultDecision};

const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = 128 * 1024;

#[derive(Clone)]
pub(crate) struct ProxyState {
    runtime: Arc<Runtime>,
    boot_owner: String,
    active: Arc<Mutex<BTreeMap<(String, String), String>>>,
    root_gates: Arc<Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    approval_secret: Option<Arc<str>>,
}

impl ProxyState {
    pub(crate) fn new(runtime: Arc<Runtime>, approval_secret: Option<Arc<str>>) -> Self {
        Self {
            runtime,
            boot_owner: uuid::Uuid::new_v4().to_string(),
            active: Arc::new(Mutex::new(BTreeMap::new())),
            root_gates: Arc::new(Mutex::new(BTreeMap::new())),
            approval_secret,
        }
    }
}

pub(crate) fn router(runtime: Arc<Runtime>, approval_secret: Option<Arc<str>>) -> Router {
    let state = ProxyState::new(runtime, approval_secret);
    Router::new()
        .route("/capabilities", get(capabilities))
        .route("/openapi.json", get(openapi))
        .route(
            "/events",
            post(event).layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST_BYTES)),
        )
        .route(
            "/checkpoints",
            post(checkpoint).layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST_BYTES)),
        )
        .with_state(state)
}

async fn capabilities(State(state): State<ProxyState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "protocol_version": 1,
        "completed_event_replay": true,
        "typed_offers": true,
        "restriction_acceptance": true,
        "acceptance_settlement": true,
        "human_approval": state.approval_secret.is_some(),
        "approval_grants": state.approval_secret.is_some(),
        "legacy_hooks": false,
        "parallel_calls": true,
        "held_batches": true,
        "position_bound_batch_offers": true,
        "batch_commit": true,
        "input_rewrite_holds_dispatch": true,
        "dispatch_call_mapping": true,
        "sanitized_results": true,
        "child_workflows": true,
        "child_actor_targeting": true,
        "durable_checkpoints": true,
        "review_provenance": true,
        "openapi": true,
    }))
}

async fn openapi() -> Json<serde_json::Value> {
    let schema = |value| serde_json::to_value(value).expect("runtime API schemas serialize");
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": { "title": "OpenAPPA authenticated proxy", "version": "1" },
        "security": [{ "bearerAuth": [] }],
        "paths": {
            "/proxy/v1/capabilities": {
                "get": { "responses": { "200": { "description": "Supported protocol features" } } }
            },
            "/proxy/v1/openapi.json": {
                "get": { "responses": { "200": { "description": "This OpenAPI document" } } }
            },
            "/proxy/v1/events": {
                "post": {
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProxyEventEnvelope" } } } },
                    "responses": { "200": { "description": "Durable event decision" }, "400": { "description": "Malformed event" }, "409": { "description": "Conflicting event" }, "503": { "description": "Event receipt is in progress or unavailable" } }
                }
            },
            "/proxy/v1/checkpoints": {
                "post": {
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CheckpointRequest" } } } },
                    "responses": { "200": { "description": "Checkpoint created or forked" }, "400": { "description": "Malformed request" }, "409": { "description": "Checkpoint cannot be created or forked" }, "413": { "description": "Request exceeds the 128 KiB proxy payload limit" } }
                }
            }
        },
        "components": {
            "securitySchemes": { "bearerAuth": { "type": "http", "scheme": "bearer" } },
            "schemas": {
                "ProxyEventEnvelope": schema(schemars::schema_for!(Envelope)),
                "ProxyHookEvent": schema(schemars::schema_for!(ProxyHookEvent)),
                "ToolCalls": schema(schemars::schema_for!(ToolCalls)),
                "PrepareBatch": schema(schemars::schema_for!(PrepareBatch)),
                "CommitBatch": schema(schemars::schema_for!(CommitBatch)),
                "CancelBatch": schema(schemars::schema_for!(CancelBatch)),
                "ToolResult": schema(schemars::schema_for!(ToolResult)),
                "ResolveOffer": schema(schemars::schema_for!(ResolveOffer)),
                "CheckpointRequest": crate::checkpoint::schema(),
            }
        }
    }))
}

async fn checkpoint(State(state): State<ProxyState>, body: Bytes) -> Response {
    if body.len() > MAX_REQUEST_BYTES {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "checkpoint request exceeds the payload limit",
        );
    }
    let (status, response) = crate::checkpoint::answer(&state.runtime, appa_adapter_kagent::adapter(), &body);
    (
        StatusCode::from_u16(status).expect("checkpoint answers carry valid status codes"),
        Json(response),
    )
        .into_response()
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Envelope {
    event_id: String,
    #[schemars(with = "serde_json::Value")]
    event: Box<serde_json::value::RawValue>,
}

#[derive(serde::Deserialize)]
struct EventHeader {
    event: String,
    root_id: String,
}

#[derive(serde::Deserialize)]
struct GenericToolCall {
    event: String,
    #[serde(default)]
    ruling: Option<serde_json::Value>,
}

/// The proxy owns this compact event shape independently of the runtime hook wire. It names
/// canonical tool ids directly because the proxy, not a kagent adapter, authenticates its peer.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ProxyHookEvent {
    event: String,
    #[serde(default)]
    root_id: Option<String>,
    #[serde(default)]
    child_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    arguments: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    spawn: Option<bool>,
    #[serde(default)]
    ruling: Option<String>,
    #[serde(default)]
    outcome: Option<ProxyOutcome>,
    #[serde(default)]
    spawned_id: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    spawn_binding: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToolCalls {
    event: String,
    root_id: String,
    /// The client child-thread id. Its APPA trajectory is scoped under `root_id`.
    #[serde(default)]
    child_id: Option<String>,
    calls: Vec<BatchCall>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PrepareBatch {
    event: String,
    root_id: String,
    #[serde(default)]
    child_id: Option<String>,
    batch_id: String,
    calls: Vec<BatchCall>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommitBatch {
    event: String,
    root_id: String,
    #[serde(default)]
    child_id: Option<String>,
    batch_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CancelBatch {
    event: String,
    root_id: String,
    #[serde(default)]
    child_id: Option<String>,
    batch_id: String,
    reason: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BatchCall {
    call_id: String,
    tool: String,
    #[schemars(with = "serde_json::Value")]
    arguments: Box<serde_json::value::RawValue>,
    #[serde(default)]
    spawn: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToolResult {
    event: String,
    root_id: String,
    #[serde(default)]
    child_id: Option<String>,
    call_id: String,
    dispatch_id: String,
    outcome: ProxyOutcome,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProxyOutcome {
    status: String,
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    body: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolveOffer {
    event: String,
    root_id: String,
    #[serde(default)]
    child_id: Option<String>,
    offer_id: String,
    tool: String,
    arguments_sha256: String,
    resolution: String,
    /// Required for context-controlled spawn offers; ignored by ordinary remedy plans.
    #[serde(default)]
    label: Option<WireLabel>,
    #[serde(default)]
    return_schema: Option<serde_json::Value>,
    #[serde(default)]
    approval: Option<ApprovalGrant>,
    #[serde(default)]
    batch_id: Option<String>,
    #[serde(default)]
    position: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WireLabel {
    #[serde(default)]
    trust: Option<String>,
    #[serde(default)]
    audience: Option<Vec<String>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApprovalGrant {
    approval_id: String,
    reviewer_id: String,
    root_id: String,
    offer_id: String,
    tool: String,
    arguments_sha256: String,
    resolution: String,
    expires_at: i64,
    signature: String,
    #[serde(default)]
    batch_id: Option<String>,
    #[serde(default)]
    position: Option<u32>,
}

async fn event(State(state): State<ProxyState>, body: Bytes) -> Response {
    if body.len() > MAX_REQUEST_BYTES {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "proxy event exceeds the payload limit",
        );
    }
    let envelope: Envelope = match serde_json::from_slice(&body) {
        Ok(envelope) => envelope,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_event",
                "proxy event is not a valid envelope",
            );
        }
    };
    if uuid::Uuid::parse_str(&envelope.event_id).is_err() {
        return error(StatusCode::BAD_REQUEST, "invalid_event", "event_id must be a UUID");
    }
    let header: EventHeader = match serde_json::from_str(envelope.event.get()) {
        Ok(header) => header,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_event", "event must name its root_id"),
    };
    let digest = format!("{:x}", Sha256::digest(&body));
    let key = (header.root_id.clone(), envelope.event_id.clone());
    {
        let active = state.active.lock().expect("proxy event active set is never poisoned");
        match active.get(&key) {
            Some(active_digest) if active_digest != &digest => {
                return error(
                    StatusCode::CONFLICT,
                    "event_conflict",
                    "event_id was already received with different bytes",
                );
            }
            Some(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "event_in_progress",
                    "event is executing",
                );
            }
            None => {}
        }
    }
    let root_gate = state
        .root_gates
        .lock()
        .expect("proxy root gates are never poisoned")
        .entry(header.root_id.clone())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _root_gate = root_gate.lock().await;
    state
        .active
        .lock()
        .expect("proxy event active set is never poisoned")
        .insert(key.clone(), digest.clone());
    let admission =
        match state
            .runtime
            .begin_proxy_event(&header.root_id, &envelope.event_id, &digest, &state.boot_owner)
        {
            Ok(admission) => admission,
            Err(_) => {
                remove_active(&state, &key);
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "event_uncertain",
                    "event receipt storage is unavailable",
                );
            }
        };
    match admission {
        ProxyEventAdmission::Replay(response) => {
            remove_active(&state, &key);
            return cached(response);
        }
        ProxyEventAdmission::Conflict => {
            remove_active(&state, &key);
            return error(
                StatusCode::CONFLICT,
                "event_conflict",
                "event_id was already received with different bytes",
            );
        }
        ProxyEventAdmission::InProgress | ProxyEventAdmission::Uncertain | ProxyEventAdmission::RootPending => {
            remove_active(&state, &key);
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "event_uncertain",
                "event was left pending and will not be re-executed",
            );
        }
        ProxyEventAdmission::BudgetExceeded => {
            remove_active(&state, &key);
            return error(
                StatusCode::CONFLICT,
                "event_budget_exhausted",
                "proxy receipt budget is exhausted for this root",
            );
        }
        ProxyEventAdmission::Started => {}
    }

    let (decision, bindings, dispatch_bindings, approval_id) = if header.event == "resolve_offer"
        || header.event == "resolve_batch_offer"
    {
        let (decision, bindings, approval_id) = resolve_offer(
            &state,
            envelope.event.get(),
            &header.root_id,
            &envelope.event_id,
            &digest,
        )
        .await;
        (decision, bindings, Vec::new(), approval_id)
    } else if header.event == "prepare_batch" {
        let (decision, bindings) = prepare_batch(&state.runtime, envelope.event.get()).await;
        (decision, bindings, Vec::new(), None)
    } else if header.event == "commit_batch" {
        let (decision, dispatches) = commit_batch(&state.runtime, envelope.event.get()).await;
        (decision, Vec::new(), dispatches, None)
    } else if header.event == "cancel_batch" {
        let decision = cancel_batch(&state.runtime, envelope.event.get());
        (decision, Vec::new(), Vec::new(), None)
    } else if header.event == "tool_calls" {
        let (decision, bindings, dispatch_bindings) = dispatch_tool_calls(&state.runtime, envelope.event.get()).await;
        (decision, bindings, dispatch_bindings, None)
    } else if header.event == "tool_result" {
        let (decision, bindings) = dispatch_tool_result(&state.runtime, envelope.event.get()).await;
        (decision, bindings, Vec::new(), None)
    } else {
        let (decision, bindings) = dispatch_kagent(&state.runtime, &header.root_id, envelope.event.get()).await;
        (decision, bindings, Vec::new(), None)
    };
    let response = envelope_response(&envelope.event_id, &digest, decision);
    let response = response_or_refusal(&envelope.event_id, &digest, response);
    let completed = state.runtime.complete_proxy_event(&ProxyEventCompletion {
        root_id: &header.root_id,
        event_id: &envelope.event_id,
        body_digest: &digest,
        response: &response,
        bindings: &bindings,
        dispatch_bindings: &dispatch_bindings,
        approval_id: approval_id.as_deref(),
    });
    remove_active(&state, &key);
    if completed.is_err() {
        // The intent remains a tombstone. Replaying after this point is unsafe because the
        // engine and receipt commits are deliberately separate transactions.
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "event_uncertain",
            "event completion could not be recorded",
        );
    }
    cached(response)
}

fn remove_active(state: &ProxyState, key: &(String, String)) {
    state
        .active
        .lock()
        .expect("proxy event active set is never poisoned")
        .remove(key);
}

async fn dispatch_kagent(runtime: &Runtime, root_id: &str, raw: &str) -> (serde_json::Value, Vec<ProxyOfferBinding>) {
    let generic: GenericToolCall = match serde_json::from_str(raw) {
        Ok(event) => event,
        Err(_) => return (refuse("event is not a valid kagent event"), Vec::new()),
    };
    if generic.event == "tool_call" && generic.ruling.is_some() {
        return (
            refuse("generic tool_call rulings are not accepted; resolve a typed human approval offer"),
            Vec::new(),
        );
    }
    let parsed = match parse_kagent_event(raw) {
        Ok(Some(event)) => event,
        Ok(None) => return (serde_json::json!({"decision": "ack"}), Vec::new()),
        Err(_) => return (refuse("kagent event was refused"), Vec::new()),
    };
    let original = tool_call(&parsed);
    let decision = crate::hooks::handle_recorded(runtime, parsed.clone()).await;
    let mut rendered = render_kagent_decision(&decision);
    let bindings = match (original, &decision) {
        (Some((tool, arguments)), HookDecision::DenyCall { offers, .. }) => {
            let arguments_sha256 = match stable_arguments_sha256(arguments) {
                Some(digest) => digest,
                None => return (refuse("blocked tool arguments are not JSON"), Vec::new()),
            };
            let actual_root = kagent_root(root_id);
            let typed: Vec<(serde_json::Value, ProxyOfferBinding)> = offers
                .iter()
                .map(|offer| {
                    let kind = runtime
                        .offer_kind(&actual_root, &OfferId(offer.id.clone()))
                        .map(|kind| typed_kind(runtime, kind))
                        .unwrap_or("unsupported");
                    (
                        serde_json::json!({
                            "offer_id": offer.id,
                            "kind": kind,
                            "root_id": root_id,
                            "tool": tool,
                            "arguments_sha256": arguments_sha256,
                        }),
                        ProxyOfferBinding {
                            offer_id: offer.id.clone(),
                            root_id: root_id.to_string(),
                            tool: tool.to_string(),
                            arguments_sha256: arguments_sha256.clone(),
                            kind: kind.to_string(),
                            deployment_fingerprint: runtime.serving_policy_key(),
                            batch_id: None,
                            position: None,
                        },
                    )
                })
                .collect();
            if let Some(object) = rendered.as_object_mut() {
                object.insert(
                    "offers".to_string(),
                    serde_json::Value::Array(typed.iter().map(|(offer, _)| offer.clone()).collect()),
                );
            }
            typed.into_iter().map(|(_, binding)| binding).collect()
        }
        _ => Vec::new(),
    };
    (rendered, bindings)
}

/// Decide every call in a model response through one core `ProposalBatch`. This path must not
/// call the singleton hook once per entry: sibling calls compose against the same trajectory view.
async fn dispatch_tool_calls(
    runtime: &Runtime,
    raw: &str,
) -> (serde_json::Value, Vec<ProxyOfferBinding>, Vec<ProxyDispatchBinding>) {
    let batch: ToolCalls = match serde_json::from_str::<ToolCalls>(raw) {
        Ok(batch) if batch.event == "tool_calls" && !batch.calls.is_empty() => batch,
        _ => return (refuse("tool_calls is malformed"), Vec::new(), Vec::new()),
    };
    let mut ids = std::collections::BTreeSet::new();
    if batch
        .calls
        .iter()
        .any(|call| call.call_id.is_empty() || !ids.insert(&call.call_id))
    {
        return (
            refuse("tool_calls requires unique non-empty call_id values"),
            Vec::new(),
            Vec::new(),
        );
    }
    let root = kagent_root(&batch.root_id);
    let lane = match kagent_lane(&batch.root_id, batch.child_id.as_deref()) {
        Some(lane) => lane,
        None => return (refuse("child_id must not be empty"), Vec::new(), Vec::new()),
    };
    let session = match runtime.session(&root, &lane) {
        Ok(session) => session,
        Err(_) => return (refuse("root is not open"), Vec::new(), Vec::new()),
    };
    let calls: Vec<(ProposedCall, bool)> = batch
        .calls
        .iter()
        .map(|call| {
            (
                ProposedCall {
                    tool: call.tool.clone(),
                    arguments: call.arguments.clone(),
                },
                call.spawn,
            )
        })
        .collect();
    let decisions = match session.on_tool_calls(calls).await {
        Ok(decisions) => decisions,
        Err(error) => return (refuse(&error.to_string()), Vec::new(), Vec::new()),
    };
    let mut rendered = Vec::with_capacity(decisions.len());
    let mut offers = Vec::new();
    let mut dispatches = Vec::new();
    let mut denied = false;
    for (call, decision) in batch.calls.iter().zip(decisions) {
        let arguments_sha256 = match stable_arguments_sha256(call.arguments.get()) {
            Some(digest) => digest,
            None => return (refuse("tool_calls arguments are not JSON"), Vec::new(), Vec::new()),
        };
        match decision {
            BatchCallDecision::Allow { release } => {
                let dispatch = match serde_json::to_string(&release.dispatch) {
                    Ok(dispatch) => dispatch,
                    Err(_) => return (refuse("dispatch could not be encoded"), Vec::new(), Vec::new()),
                };
                let spawn_binding = release.fork.as_ref().map(|binding| binding.0.clone());
                dispatches.push(ProxyDispatchBinding {
                    root_id: batch.root_id.clone(),
                    lane_id: lane.0.clone(),
                    call_id: call.call_id.clone(),
                    tool: release.tool.clone(),
                    arguments_sha256: arguments_sha256.clone(),
                    dispatch: dispatch.clone(),
                    spawn_binding: spawn_binding.clone(),
                    deployment_fingerprint: runtime.serving_policy_key(),
                    batch_id: None,
                    position: None,
                });
                let mut allowed = serde_json::json!({
                    "call_id": call.call_id,
                    "dispatch_id": dispatch,
                });
                if let Some(binding) = spawn_binding {
                    allowed["spawn_binding"] = serde_json::Value::String(binding);
                }
                rendered.push(allowed);
            }
            BatchCallDecision::Deny { feedback } => {
                denied = true;
                let (typed, bindings) = typed_offers(
                    runtime,
                    &root,
                    &batch.root_id,
                    &call.tool,
                    &arguments_sha256,
                    &feedback.offers,
                );
                offers.extend(bindings);
                rendered.push(serde_json::json!({
                    "call_id": call.call_id,
                    "decision": "deny_call",
                    "feedback": feedback.text,
                    "offers": typed,
                    "review": feedback.review.into_iter().map(|review| serde_json::json!({"offer_id": review.offer.0, "text": review.text})).collect::<Vec<_>>(),
                }));
            }
        }
    }
    let decision = match denied {
        false => serde_json::json!({"decision": "allow_calls", "calls": rendered}),
        true => serde_json::json!({"decision": "deny_calls", "calls": rendered}),
    };
    (decision, offers, dispatches)
}

/// A held batch is one core proposal batch. The proxy persists its ordered client positions and
/// deliberately withholds all dispatch ids. Therefore an allowed sibling cannot escape while a
/// different position still needs a remedy.
async fn prepare_batch(runtime: &Runtime, raw: &str) -> (serde_json::Value, Vec<ProxyOfferBinding>) {
    let batch: PrepareBatch = match serde_json::from_str::<PrepareBatch>(raw) {
        Ok(batch) if batch.event == "prepare_batch" && !batch.calls.is_empty() => batch,
        _ => return (refuse("prepare_batch is malformed"), Vec::new()),
    };
    if uuid::Uuid::parse_str(&batch.batch_id).is_err() {
        return (refuse("batch_id must be a UUID"), Vec::new());
    }
    let mut call_ids = std::collections::BTreeSet::new();
    if batch
        .calls
        .iter()
        .any(|call| call.call_id.is_empty() || !call_ids.insert(&call.call_id))
    {
        return (
            refuse("prepare_batch requires unique non-empty call_id values"),
            Vec::new(),
        );
    }
    let root = kagent_root(&batch.root_id);
    let lane = match kagent_lane(&batch.root_id, batch.child_id.as_deref()) {
        Some(lane) => lane,
        None => return (refuse("child_id must not be empty"), Vec::new()),
    };
    let session = match runtime.session(&root, &lane) {
        Ok(session) => session,
        Err(_) => return (refuse("root is not open"), Vec::new()),
    };
    let calls = batch
        .calls
        .iter()
        .map(|call| {
            (
                ProposedCall {
                    tool: call.tool.clone(),
                    arguments: call.arguments.clone(),
                },
                call.spawn,
            )
        })
        .collect();
    let decisions = match session.on_tool_calls_with_id(Some(batch.batch_id.clone()), calls).await {
        Ok(decisions) => decisions,
        Err(error) => return (refuse(&error.to_string()), Vec::new()),
    };
    let mut offered = Vec::new();
    let mut positions = Vec::with_capacity(decisions.len());
    let mut rendered = Vec::with_capacity(decisions.len());
    for (position, (call, decision)) in batch.calls.iter().zip(decisions).enumerate() {
        let Some(arguments_sha256) = stable_arguments_sha256(call.arguments.get()) else {
            return (refuse("prepare_batch arguments are not JSON"), Vec::new());
        };
        let Some(arguments) = canonical_arguments(call.arguments.get()) else {
            return (refuse("prepare_batch arguments are not JSON"), Vec::new());
        };
        let position = position as u32;
        let mut binding = ProxyBatchPosition {
            batch_id: batch.batch_id.clone(),
            position,
            call_id: call.call_id.clone(),
            tool: call.tool.clone(),
            arguments_sha256: arguments_sha256.clone(),
            arguments: arguments.clone(),
            effective_tool: call.tool.clone(),
            effective_arguments_sha256: arguments_sha256.clone(),
            effective_arguments: arguments,
            dispatch: None,
            spawn: call.spawn,
            spawn_binding: None,
            authorized: false,
        };
        match decision {
            BatchCallDecision::Allow { release } => {
                binding.dispatch = serde_json::to_string(&release.dispatch).ok();
                binding.spawn_binding = release.fork.as_ref().map(|binding| binding.0.clone());
                binding.authorized = binding.dispatch.is_some();
                rendered.push(serde_json::json!({
                    "position": position,
                    "call_id": call.call_id,
                    "state": "held",
                    "tool": call.tool,
                    "arguments_sha256": arguments_sha256,
                }));
            }
            BatchCallDecision::Deny { feedback } => {
                let (mut offers, mut bindings) = typed_offers(
                    runtime,
                    &root,
                    &batch.root_id,
                    &call.tool,
                    &arguments_sha256,
                    &feedback.offers,
                );
                for offer in &mut bindings {
                    offer.batch_id = Some(batch.batch_id.clone());
                    offer.position = Some(position);
                }
                for offer in &mut offers {
                    offer["batch_id"] = serde_json::Value::String(batch.batch_id.clone());
                    offer["position"] = serde_json::json!(position);
                }
                offered.extend(bindings);
                rendered.push(serde_json::json!({
                    "position": position,
                    "call_id": call.call_id,
                    "state": "blocked",
                    "tool": call.tool,
                    "arguments_sha256": arguments_sha256,
                    "feedback": feedback.text,
                    "offers": offers,
                    "review": feedback.review.into_iter().map(|review| serde_json::json!({"offer_id": review.offer.0, "text": review.text})).collect::<Vec<_>>(),
                }));
            }
        }
        positions.push(binding);
    }
    let binding = ProxyBatchBinding {
        batch_id: batch.batch_id.clone(),
        root_id: batch.root_id.clone(),
        lane_id: lane.0,
        core_batch_id: batch.batch_id.clone(),
        positions: positions.len() as u32,
        basis: match runtime.proxy_basis(&root) {
            Ok(basis) => basis,
            Err(_) => return (refuse("batch basis storage is unavailable"), Vec::new()),
        },
        deployment_fingerprint: runtime.serving_policy_key(),
    };
    match runtime.create_proxy_batch(&binding, &positions) {
        Ok(true) => {}
        Ok(false) => return (refuse("batch_id is already bound to different positions"), Vec::new()),
        Err(_) => return (refuse("batch mapping storage is unavailable"), Vec::new()),
    }
    (
        serde_json::json!({
            "decision": "batch_prepared",
            "batch_id": batch.batch_id,
            "root_id": batch.root_id,
            "positions": rendered,
            "next": "resolve_batch_offer or commit_batch",
        }),
        offered,
    )
}

/// Commit is the client publication boundary. It first opens any position whose accepted APPA
/// remedy prepared an approval but no dispatch, while keeping those dispatches inside the proxy.
/// Only after every position has an engine dispatch does it return the complete ordered release.
async fn commit_batch(runtime: &Runtime, raw: &str) -> (serde_json::Value, Vec<ProxyDispatchBinding>) {
    let commit: CommitBatch = match serde_json::from_str::<CommitBatch>(raw) {
        Ok(commit) if commit.event == "commit_batch" && uuid::Uuid::parse_str(&commit.batch_id).is_ok() => commit,
        _ => return (refuse("commit_batch is malformed"), Vec::new()),
    };
    let root = kagent_root(&commit.root_id);
    let lane = match kagent_lane(&commit.root_id, commit.child_id.as_deref()) {
        Some(lane) => lane,
        None => return (refuse("child_id must not be empty"), Vec::new()),
    };
    let Some(mut batch) = (match runtime.proxy_batch(&commit.batch_id) {
        Ok(batch) => batch,
        Err(_) => return (refuse("batch mapping storage is unavailable"), Vec::new()),
    }) else {
        return (refuse("batch mapping does not exist"), Vec::new());
    };
    match runtime.proxy_batch_quarantined(&commit.batch_id) {
        Ok(true) => return (refuse("batch is quarantined and cannot be committed"), Vec::new()),
        Ok(false) => {}
        Err(_) => return (refuse("batch quarantine storage is unavailable"), Vec::new()),
    }
    if !batch_matches_actor(runtime, &batch, &commit.root_id, &root, &lane) {
        return (refuse("batch belongs to a different actor or deployment"), Vec::new());
    }
    if let Err(detail) = batch_basis_is_current(runtime, &batch, &root, &lane) {
        return (refuse(detail), Vec::new());
    }
    let mut positions = match runtime.proxy_batch_positions(&commit.batch_id) {
        Ok(positions) if positions.len() == batch.positions as usize => positions,
        Ok(_) => return (refuse("batch positions are incomplete"), Vec::new()),
        Err(_) => return (refuse("batch mapping storage is unavailable"), Vec::new()),
    };
    if positions.iter().any(|position| !position.authorized) {
        return (refuse("batch has unresolved positions"), Vec::new());
    }
    let session = match runtime.session(&root, &lane) {
        Ok(session) => session,
        Err(_) => return (refuse("root is not open"), Vec::new()),
    };
    for position in &mut positions {
        if position.dispatch.is_some() {
            continue;
        }
        if let Err(detail) = batch_basis_is_current(runtime, &batch, &root, &lane) {
            return (refuse(detail), Vec::new());
        }
        let arguments = match serde_json::value::RawValue::from_string(position.effective_arguments.clone()) {
            Ok(arguments) => arguments,
            Err(_) => return (refuse("stored effective arguments are invalid"), Vec::new()),
        };
        let call = ProposedCall {
            tool: position.effective_tool.clone(),
            arguments,
        };
        // A one-position core batch consumes this exact prepared approval. It is not published
        // until this function has collected dispatches for every original batch position.
        let (dispatch, spawn_binding) = match session.on_tool_calls(vec![(call, position.spawn)]).await {
            Ok(decisions) => match decisions.as_slice() {
                [BatchCallDecision::Allow { release }] => match serde_json::to_string(&release.dispatch) {
                    Ok(dispatch) => (dispatch, release.fork.as_ref().map(|binding| binding.0.clone())),
                    Err(_) => return (refuse("commit dispatch could not be encoded"), Vec::new()),
                },
                _ => return (refuse("an authorized batch position no longer releases"), Vec::new()),
            },
            Err(_) => return (refuse("batch commit could not open its held dispatch"), Vec::new()),
        };
        if let Err(detail) = advance_batch_basis(runtime, &mut batch, &root, &lane) {
            quarantine_batch(
                runtime,
                &batch.batch_id,
                "batch commit transition could not be verified",
            );
            return (refuse(detail), Vec::new());
        }
        position.dispatch = Some(dispatch);
        position.spawn_binding = spawn_binding;
        if runtime.update_proxy_batch_position(position).is_err() {
            quarantine_batch(
                runtime,
                &batch.batch_id,
                "batch commit dispatch mapping could not be stored",
            );
            return (refuse("batch dispatch mapping storage is unavailable"), Vec::new());
        }
    }
    if let Err(detail) = batch_basis_is_current(runtime, &batch, &root, &lane) {
        return (refuse(detail), Vec::new());
    }
    let mut dispatches = Vec::with_capacity(positions.len());
    let mut released = Vec::with_capacity(positions.len());
    for position in positions {
        let Some(dispatch) = position.dispatch else {
            return (refuse("batch position has no held dispatch"), Vec::new());
        };
        dispatches.push(ProxyDispatchBinding {
            root_id: commit.root_id.clone(),
            lane_id: lane.0.clone(),
            call_id: position.call_id.clone(),
            tool: position.effective_tool.clone(),
            arguments_sha256: position.effective_arguments_sha256.clone(),
            dispatch: dispatch.clone(),
            spawn_binding: position.spawn_binding.clone(),
            deployment_fingerprint: runtime.serving_policy_key(),
            batch_id: Some(commit.batch_id.clone()),
            position: Some(position.position),
        });
        released.push(serde_json::json!({
            "position": position.position,
            "call_id": position.call_id,
            "dispatch_id": dispatch,
            "tool": position.effective_tool,
            "arguments_sha256": position.effective_arguments_sha256,
            "arguments": serde_json::from_str::<serde_json::Value>(&position.effective_arguments).unwrap_or(serde_json::Value::Null),
            "spawn_binding": position.spawn_binding,
        }));
    }
    (
        serde_json::json!({
            "decision": "batch_committed",
            "batch_id": commit.batch_id,
            "calls": released,
        }),
        dispatches,
    )
}

fn cancel_batch(runtime: &Runtime, raw: &str) -> serde_json::Value {
    let cancel: CancelBatch = match serde_json::from_str::<CancelBatch>(raw) {
        Ok(cancel)
            if cancel.event == "cancel_batch"
                && uuid::Uuid::parse_str(&cancel.batch_id).is_ok()
                && !cancel.reason.trim().is_empty() =>
        {
            cancel
        }
        _ => return refuse("cancel_batch is malformed"),
    };
    let lane = match kagent_lane(&cancel.root_id, cancel.child_id.as_deref()) {
        Some(lane) => lane,
        None => return refuse("child_id must not be empty"),
    };
    let Some(batch) = (match runtime.proxy_batch(&cancel.batch_id) {
        Ok(batch) => batch,
        Err(_) => return refuse("batch mapping storage is unavailable"),
    }) else {
        return refuse("batch mapping does not exist");
    };
    if batch.root_id != cancel.root_id || batch.lane_id != lane.0 {
        return refuse("batch belongs to a different actor");
    }
    if runtime
        .quarantine_proxy_batch(&cancel.batch_id, &cancel.reason)
        .is_err()
    {
        return refuse("batch quarantine storage is unavailable");
    }
    serde_json::json!({
        "decision": "batch_quarantined",
        "batch_id": cancel.batch_id,
        "reason": cancel.reason,
    })
}

async fn dispatch_tool_result(runtime: &Runtime, raw: &str) -> (serde_json::Value, Vec<ProxyOfferBinding>) {
    let result: ToolResult = match serde_json::from_str::<ToolResult>(raw) {
        Ok(result) if result.event == "tool_result" => result,
        _ => return (refuse("tool_result is malformed"), Vec::new()),
    };
    let root = kagent_root(&result.root_id);
    let lane = match kagent_lane(&result.root_id, result.child_id.as_deref()) {
        Some(lane) => lane,
        None => return (refuse("child_id must not be empty"), Vec::new()),
    };
    let Some(binding) = (match runtime.proxy_dispatch_binding(&result.root_id, &lane.0, &result.call_id) {
        Ok(binding) => binding,
        Err(_) => return (refuse("dispatch binding storage is unavailable"), Vec::new()),
    }) else {
        return (
            refuse("tool_result call_id does not name a released dispatch"),
            Vec::new(),
        );
    };
    if binding.deployment_fingerprint != runtime.serving_policy_key() {
        return (refuse("dispatch was issued by a different deployment"), Vec::new());
    }
    if binding.dispatch != result.dispatch_id {
        return (refuse("tool_result dispatch_id does not match call_id"), Vec::new());
    }
    let dispatch = match serde_json::from_str(&binding.dispatch) {
        Ok(dispatch) => dispatch,
        Err(_) => return (refuse("stored dispatch binding is invalid"), Vec::new()),
    };
    let outcome = match proxy_outcome(result.outcome) {
        Some(outcome) => outcome,
        None => return (refuse("tool_result outcome is malformed"), Vec::new()),
    };
    let session = match runtime.session(&root, &lane) {
        Ok(session) => session,
        Err(_) => return (refuse("root is not open"), Vec::new()),
    };
    let presentation = model_presentation(&outcome);
    match session.on_dispatch_result(dispatch, outcome).await {
        Ok(ToolResultDecision::Keep) => match presentation {
            Some(presentation) => (
                serde_json::json!({
                    "decision": "result_admitted",
                    "call_id": result.call_id,
                    "presentation": presentation,
                }),
                Vec::new(),
            ),
            None => (
                serde_json::json!({"decision": "ack", "call_id": result.call_id}),
                Vec::new(),
            ),
        },
        Ok(ToolResultDecision::Deliver { value }) => (
            serde_json::json!({
                "decision": "result_admitted",
                "call_id": result.call_id,
                "presentation": value,
            }),
            Vec::new(),
        ),
        Ok(ToolResultDecision::Replace { placeholder, offers }) => {
            let (offers, bindings) = typed_offers(
                runtime,
                &root,
                &result.root_id,
                &binding.tool,
                &binding.arguments_sha256,
                &offers,
            );
            (
                serde_json::json!({
                    "decision": "result_admitted",
                    "call_id": result.call_id,
                    "presentation": placeholder,
                    "offers": offers,
                }),
                bindings,
            )
        }
        Err(error) => (refuse(&error.to_string()), Vec::new()),
    }
}

/// The proxy only exposes a result as one canonical JSON string. The raw harness spelling
/// never crosses the boundary, so equal values have one model-visible presentation.
fn model_presentation(outcome: &ToolOutcome) -> Option<String> {
    if matches!(outcome, ToolOutcome::Failure { .. }) {
        return Some("Tool execution failed. The raw tool result was not admitted.".to_owned());
    }
    let ToolOutcome::Success {
        body: OutcomeBody::Available(body),
    } = outcome
    else {
        return None;
    };
    match serde_json::from_str(body) {
        Ok(value) => String::from_utf8(appa_engine::params::canonical_bytes(&value)).ok(),
        Err(_) => Some(body.clone()),
    }
}

fn proxy_outcome(outcome: ProxyOutcome) -> Option<ToolOutcome> {
    match outcome.status.as_str() {
        "success" => outcome.body.and_then(|body| {
            let value: serde_json::Value = serde_json::from_str(body.get()).ok()?;
            let body = match value {
                // APPA result artifacts are text; unwrap a JSON-string transport envelope.
                serde_json::Value::String(text) => text,
                value => String::from_utf8(appa_engine::params::canonical_bytes(&value)).ok()?,
            };
            Some(ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            })
        }),
        "failure" => outcome.message.map(|message| ToolOutcome::Failure { message }),
        "indeterminate" if outcome.body.is_none() && outcome.message.is_none() => Some(ToolOutcome::Indeterminate),
        _ => None,
    }
}

fn typed_offers(
    runtime: &Runtime,
    actual_root: &TrajectoryId,
    root_id: &str,
    tool: &str,
    arguments_sha256: &str,
    offers: &[appa_runtime_api::OfferedRemedy],
) -> (Vec<serde_json::Value>, Vec<ProxyOfferBinding>) {
    offers
        .iter()
        .map(|offer| {
            let kind = runtime
                .offer_kind(actual_root, &OfferId(offer.id.clone()))
                .map(|kind| typed_kind(runtime, kind))
                .unwrap_or("unsupported");
            (
                serde_json::json!({
                    "offer_id": offer.id,
                    "kind": kind,
                    "root_id": root_id,
                    "tool": tool,
                    "arguments_sha256": arguments_sha256,
                }),
                ProxyOfferBinding {
                    offer_id: offer.id.clone(),
                    root_id: root_id.to_string(),
                    tool: tool.to_string(),
                    arguments_sha256: arguments_sha256.to_string(),
                    kind: kind.to_string(),
                    deployment_fingerprint: runtime.serving_policy_key(),
                    batch_id: None,
                    position: None,
                },
            )
        })
        .unzip()
}

/// A held batch is bound to exactly one policy-sensitive actor lane. The protocol's family log
/// has one position for every lane, so a change in any sibling lane invalidates a held batch
/// rather than reusing an offer under an unexamined family state.
fn batch_matches_actor(
    runtime: &Runtime,
    batch: &ProxyBatchBinding,
    root_id: &str,
    root: &TrajectoryId,
    lane: &TrajectoryId,
) -> bool {
    batch.root_id == root_id
        && batch.lane_id == lane.0
        && batch.deployment_fingerprint == runtime.serving_policy_key()
        && root == &kagent_root(root_id)
}

/// Rebuild the bound actor and compare its family-log position before an authority, sanitizer,
/// or held dispatch can run. This is deliberately not used by ordinary `tool_calls`, which keeps
/// its single-event core batch fast path.
fn batch_basis_is_current(
    runtime: &Runtime,
    batch: &ProxyBatchBinding,
    root: &TrajectoryId,
    lane: &TrajectoryId,
) -> Result<(), &'static str> {
    match runtime.proxy_actor_basis(root, lane) {
        Ok(current) if current == batch.basis => Ok(()),
        Ok(_) => Err("batch basis changed before it could be committed"),
        Err(_) => Err("batch actor context is no longer live"),
    }
}

/// A resolving remedy and a deferred approval release each append exactly one core event. The
/// stored expectation moves only after that event is observed at the next family-log position.
/// Any larger jump includes a writer outside this batch and is unsafe to attribute to it.
fn advance_batch_basis(
    runtime: &Runtime,
    batch: &mut ProxyBatchBinding,
    root: &TrajectoryId,
    lane: &TrajectoryId,
) -> Result<(), &'static str> {
    let expected_next = batch.basis.checked_add(1).ok_or("batch basis cannot advance")?;
    let current = runtime
        .proxy_actor_basis(root, lane)
        .map_err(|_| "batch actor context is no longer live")?;
    if current != expected_next {
        return Err("batch transition was not the only next log event");
    }
    match runtime.advance_proxy_batch_basis(&batch.batch_id, batch.basis, current) {
        Ok(true) => {
            batch.basis = current;
            Ok(())
        }
        Ok(false) => Err("batch basis changed while its transition was being recorded"),
        Err(_) => Err("batch basis storage is unavailable"),
    }
}

fn quarantine_batch(runtime: &Runtime, batch_id: &str, reason: &str) {
    let _ = runtime.quarantine_proxy_batch(batch_id, reason);
}

async fn resolve_offer(
    state: &ProxyState,
    raw: &str,
    root_id: &str,
    event_id: &str,
    body_digest: &str,
) -> (serde_json::Value, Vec<ProxyOfferBinding>, Option<String>) {
    let runtime = &state.runtime;
    let resolve: ResolveOffer = match serde_json::from_str::<ResolveOffer>(raw) {
        Ok(resolve) if resolve.event == "resolve_offer" || resolve.event == "resolve_batch_offer" => resolve,
        _ => return (refuse("resolve_offer is malformed"), Vec::new(), None),
    };
    let Some(binding) = (match runtime.proxy_offer_binding(&resolve.offer_id) {
        Ok(binding) => binding,
        Err(_) => return (refuse("offer binding storage is unavailable"), Vec::new(), None),
    }) else {
        return (refuse("offer binding does not exist"), Vec::new(), None);
    };
    if binding.root_id != resolve.root_id
        || binding.tool != resolve.tool
        || binding.arguments_sha256 != resolve.arguments_sha256
    {
        return (
            refuse("offer binding does not match this root, tool, or arguments"),
            Vec::new(),
            None,
        );
    }
    if binding.deployment_fingerprint != runtime.serving_policy_key() {
        return (refuse("offer was issued by a different deployment"), Vec::new(), None);
    }
    let actual_root = kagent_root(&resolve.root_id);
    let lane = match kagent_lane(&resolve.root_id, resolve.child_id.as_deref()) {
        Some(lane) => lane,
        None => return (refuse("child_id must not be empty"), Vec::new(), None),
    };
    let mut held_batch = None;
    let batch_position = if resolve.event == "resolve_batch_offer" {
        let (Some(batch_id), Some(position)) = (resolve.batch_id.as_deref(), resolve.position) else {
            return (
                refuse("resolve_batch_offer requires batch_id and position"),
                Vec::new(),
                None,
            );
        };
        if binding.batch_id.as_deref() != Some(batch_id) || binding.position != Some(position) {
            return (refuse("offer does not belong to this batch position"), Vec::new(), None);
        }
        let Some(batch) = (match runtime.proxy_batch(batch_id) {
            Ok(batch) => batch,
            Err(_) => return (refuse("batch mapping storage is unavailable"), Vec::new(), None),
        }) else {
            return (refuse("batch mapping does not exist"), Vec::new(), None);
        };
        match runtime.proxy_batch_quarantined(batch_id) {
            Ok(true) => {
                return (
                    refuse("batch is quarantined and cannot resolve offers"),
                    Vec::new(),
                    None,
                );
            }
            Ok(false) => {}
            Err(_) => return (refuse("batch quarantine storage is unavailable"), Vec::new(), None),
        }
        if !batch_matches_actor(runtime, &batch, &resolve.root_id, &actual_root, &lane) {
            return (
                refuse("batch belongs to a different deployment or root"),
                Vec::new(),
                None,
            );
        }
        if let Err(detail) = batch_basis_is_current(runtime, &batch, &actual_root, &lane) {
            return (refuse(detail), Vec::new(), None);
        }
        match runtime.proxy_batch_positions(batch_id) {
            Ok(positions) => match positions.into_iter().find(|candidate| candidate.position == position) {
                Some(position)
                    if position.effective_tool == resolve.tool
                        && position.effective_arguments_sha256 == resolve.arguments_sha256 =>
                {
                    held_batch = Some(batch);
                    Some(position)
                }
                _ => return (refuse("batch position does not match effective call"), Vec::new(), None),
            },
            Err(_) => return (refuse("batch mapping storage is unavailable"), Vec::new(), None),
        }
    } else {
        None
    };
    let offer = OfferId(resolve.offer_id.clone());
    let Some((_, pursuer)) = runtime.resolve_in(&actual_root, &offer) else {
        return (refuse("offer is stale"), Vec::new(), None);
    };
    if pursuer != lane {
        return (refuse("offer belongs to a different actor"), Vec::new(), None);
    }
    let Some(kind) = runtime
        .offer_kind(&actual_root, &offer)
        .map(|kind| typed_kind(runtime, kind))
    else {
        return (refuse("offer is no longer live"), Vec::new(), None);
    };
    if kind != binding.kind {
        return (refuse("offer kind changed"), Vec::new(), None);
    }
    let (ruling, approval_id) = match (kind, resolve.resolution.as_str()) {
        // `Accept` is an APPA narrowing settlement, not a generic restriction classification.
        // Keep the old spelling accepted for singleton clients while advertising `acceptance`.
        ("acceptance", "accept" | "accept_restriction") => (None, None),
        ("sanitizer", "apply_sanitizer") => (None, None),
        ("human_approval", "approve" | "deny") => {
            let Some(secret) = state.approval_secret.as_deref() else {
                return (refuse("approval grants are unavailable"), Vec::new(), None);
            };
            let Some(approval) = resolve.approval.as_ref() else {
                return (refuse("approval grant is required"), Vec::new(), None);
            };
            if let Err(detail) = validate_approval_grant(secret, approval, &resolve) {
                return (refuse(detail), Vec::new(), None);
            }
            match runtime.begin_proxy_approval_grant(&approval.approval_id, root_id, event_id, body_digest) {
                Ok(ProxyApprovalAdmission::Started) => {}
                Ok(ProxyApprovalAdmission::Consumed) => {
                    return (refuse("approval grant was already consumed"), Vec::new(), None);
                }
                Err(_) => return (refuse("approval grant storage is unavailable"), Vec::new(), None),
            }
            let ruling = match resolve.resolution.as_str() {
                "approve" => Ruling::Approve,
                "deny" => Ruling::Deny,
                _ => unreachable!("the resolution was matched above"),
            };
            (Some(ruling), Some(approval.approval_id.clone()))
        }
        _ => return (refuse("resolution is not permitted for this offer"), Vec::new(), None),
    };
    let arguments = RemedyArguments {
        label: resolve.label.as_ref().map(|label| crate::engine::LabelSpelling {
            trust: label.trust.clone(),
            audience: label.audience.clone(),
        }),
        return_schema: resolve.return_schema.clone(),
    };
    let outcome = runtime
        .remedy(
            &Actor {
                root: actual_root.clone(),
                child: (lane != kagent_root(&resolve.root_id)).then_some(lane.clone()),
            },
            offer,
            arguments,
            None,
            ruling,
        )
        .await;
    if matches!(
        outcome,
        RemedyOutcome::Authorized { .. }
            | RemedyOutcome::Substituted { .. }
            | RemedyOutcome::Returned { .. }
            | RemedyOutcome::Declined { .. }
    ) && let Some(batch) = held_batch.as_mut()
        && let Err(detail) = advance_batch_basis(runtime, batch, &actual_root, &lane)
    {
        quarantine_batch(
            runtime,
            &batch.batch_id,
            "batch remedy transition could not be verified",
        );
        return (refuse(detail), Vec::new(), approval_id);
    }
    match (kind, resolve.resolution.as_str(), outcome) {
        ("acceptance", "accept" | "accept_restriction", RemedyOutcome::Authorized { .. }) => {
            if let Some(mut position) = batch_position {
                position.authorized = true;
                if runtime.update_proxy_batch_position(&position).is_err() {
                    quarantine_batch(runtime, &position.batch_id, "batch acceptance could not be stored");
                    return (
                        refuse("batch mapping completion is unavailable"),
                        Vec::new(),
                        approval_id,
                    );
                }
            }
            (resolved(&resolve, "acceptance", "accepted"), Vec::new(), approval_id)
        }
        ("sanitizer", "apply_sanitizer", RemedyOutcome::Returned { value }) => (
            serde_json::json!({
                "decision": "sanitized_output",
                "offer_id": resolve.offer_id,
                "tool": resolve.tool,
                "arguments_sha256": resolve.arguments_sha256,
                "output": value,
            }),
            Vec::new(),
            approval_id,
        ),
        // An output sanitizer selected before its call runs binds the sanitizer to the future
        // dispatch. The subsequent result drives the registered external implementation.
        ("sanitizer", "apply_sanitizer", RemedyOutcome::Authorized { .. }) => {
            if let Some(mut position) = batch_position {
                position.authorized = true;
                if runtime.update_proxy_batch_position(&position).is_err() {
                    quarantine_batch(
                        runtime,
                        &position.batch_id,
                        "batch sanitizer binding could not be stored",
                    );
                    return (
                        refuse("batch mapping completion is unavailable"),
                        Vec::new(),
                        approval_id,
                    );
                }
            }
            (resolved(&resolve, "sanitizer", "bound"), Vec::new(), approval_id)
        }
        ("sanitizer", "apply_sanitizer", RemedyOutcome::Substituted { call }) => {
            let Some(mut position) = batch_position else {
                return (
                    serde_json::json!({
                        "decision": "substituted_call",
                        "offer_id": resolve.offer_id,
                        "tool": call.tool,
                        "arguments": serde_json::from_str::<serde_json::Value>(call.arguments.get()).unwrap_or(serde_json::Value::Null),
                    }),
                    Vec::new(),
                    approval_id,
                );
            };
            let Some(arguments_sha256) = stable_arguments_sha256(call.arguments.get()) else {
                return (refuse("sanitizer returned invalid arguments"), Vec::new(), approval_id);
            };
            let session = match runtime.session(&actual_root, &lane) {
                Ok(session) => session,
                Err(_) => return (refuse("root is not open"), Vec::new(), approval_id),
            };
            let dispatch = match session.on_tool_call(call.clone(), false).await {
                Ok(crate::api::ToolCallDecision::Allow { dispatch, .. }) => match serde_json::to_string(&dispatch) {
                    Ok(dispatch) => dispatch,
                    Err(_) => {
                        return (
                            refuse("substituted dispatch could not be encoded"),
                            Vec::new(),
                            approval_id,
                        );
                    }
                },
                _ => return (refuse("substituted call did not remain held"), Vec::new(), approval_id),
            };
            position.effective_tool = call.tool.clone();
            position.effective_arguments_sha256 = arguments_sha256;
            position.effective_arguments =
                canonical_arguments(call.arguments.get()).expect("a substituted call has JSON arguments");
            position.dispatch = Some(dispatch);
            position.authorized = true;
            if runtime.update_proxy_batch_position(&position).is_err() {
                quarantine_batch(
                    runtime,
                    &position.batch_id,
                    "batch substitution mapping could not be stored",
                );
                return (
                    refuse("batch mapping completion is unavailable"),
                    Vec::new(),
                    approval_id,
                );
            }
            (resolved(&resolve, "sanitizer", "substituted"), Vec::new(), approval_id)
        }
        ("human_approval", "approve", RemedyOutcome::Authorized { .. }) => {
            if let Some(mut position) = batch_position {
                position.authorized = true;
                if runtime.update_proxy_batch_position(&position).is_err() {
                    quarantine_batch(runtime, &position.batch_id, "batch approval could not be stored");
                    return (
                        refuse("batch mapping completion is unavailable"),
                        Vec::new(),
                        approval_id,
                    );
                }
            }
            (
                resolved(&resolve, "human_approval", "approved"),
                Vec::new(),
                approval_id,
            )
        }
        ("human_approval", "deny", RemedyOutcome::Declined { .. }) => {
            (resolved(&resolve, "human_approval", "denied"), Vec::new(), approval_id)
        }
        (_, _, RemedyOutcome::Refused { detail }) => (refuse(&detail), Vec::new(), approval_id),
        (_, _, RemedyOutcome::Declined { .. }) => (refuse("runtime declined the offer"), Vec::new(), approval_id),
        (_, _, RemedyOutcome::NoAnswer { .. }) => {
            if let Some(position) = batch_position {
                let _ = runtime.quarantine_proxy_batch(&position.batch_id, "external remedy outcome is unknown");
            }
            (refuse("runtime received no authority answer"), Vec::new(), approval_id)
        }
        (_, _, RemedyOutcome::Returned { .. } | RemedyOutcome::Substituted { .. }) => (
            refuse("runtime returned a non-call remedy result"),
            Vec::new(),
            approval_id,
        ),
        (_, _, RemedyOutcome::Authorized { .. }) => (
            refuse("runtime authorized an incompatible offer"),
            Vec::new(),
            approval_id,
        ),
    }
}

fn parse_kagent_event(raw: &str) -> Result<Option<HookEvent>, ()> {
    let event: ProxyHookEvent = serde_json::from_str(raw).map_err(|_| ())?;
    let root = || -> Result<TrajectoryId, ()> {
        event
            .root_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(kagent_root)
            .ok_or(())
    };
    let actor = || -> Result<Actor, ()> {
        let root = root()?;
        let child = event
            .child_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|child| TrajectoryId(format!("{}:{child}", root.0)));
        Ok(Actor { root, child })
    };
    let call = || -> Result<ProposedCall, ()> {
        match (event.tool.clone(), event.arguments.clone()) {
            (Some(tool), Some(arguments)) => Ok(ProposedCall { tool, arguments }),
            _ => Err(()),
        }
    };
    let outcome = || -> Result<ToolOutcome, ()> {
        match event.outcome.as_ref() {
            Some(outcome) => proxy_outcome(ProxyOutcome {
                status: outcome.status.clone(),
                body: outcome.body.clone(),
                message: outcome.message.clone(),
            })
            .ok_or(()),
            None => Err(()),
        }
    };
    match event.event.as_str() {
        "ping" => Ok(None),
        "session_start" => Ok(Some(HookEvent::SessionStart { root: root()? })),
        "prompt" => Ok(Some(HookEvent::Prompt {
            actor: actor()?,
            text: event.text.clone().ok_or(())?,
        })),
        "turn_end" => Ok(Some(HookEvent::TurnEnd { actor: actor()? })),
        "tool_call" => {
            let ruling = match event.ruling.as_deref() {
                None => None,
                Some("approve") => Some(Ruling::Approve),
                Some("deny") => Some(Ruling::Deny),
                Some(_) => return Err(()),
            };
            Ok(Some(HookEvent::ToolCall {
                actor: actor()?,
                call: call()?,
                spawn: event.spawn.ok_or(())?,
                ruling,
            }))
        }
        "tool_result" => Ok(Some(HookEvent::ToolResult {
            actor: actor()?,
            call: call()?,
            outcome: outcome()?,
        })),
        "spawn_result" => {
            let actor = actor()?;
            Ok(Some(HookEvent::SpawnResult {
                call: call()?,
                outcome: outcome()?,
                child: event
                    .spawned_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .map(|child| TrajectoryId(format!("{}:{child}", actor.root.0))),
                value: event.value.clone().filter(|value| !value.is_empty()),
                actor,
            }))
        }
        "child_start" => {
            let root = root()?;
            let child = event
                .child_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(|child| TrajectoryId(format!("{}:{child}", root.0)))
                .ok_or(())?;
            let spawn = match event.spawn_binding.clone() {
                Some(binding) if binding.is_empty() => return Err(()),
                Some(binding) => SpawnRef::Binding(SpawnBinding(binding)),
                None => SpawnRef::InFlight,
            };
            Ok(Some(HookEvent::ChildStart { root, child, spawn }))
        }
        "child_end" => {
            let root = root()?;
            let child = event
                .child_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(|child| TrajectoryId(format!("{}:{child}", root.0)))
                .ok_or(())?;
            Ok(Some(HookEvent::ChildEnd {
                root,
                child,
                value: event.value.clone().filter(|value| !value.is_empty()),
            }))
        }
        _ => Err(()),
    }
}

fn render_kagent_decision(decision: &HookDecision) -> serde_json::Value {
    match decision {
        HookDecision::Ack => serde_json::json!({"decision": "ack"}),
        HookDecision::AllowCall { spawn } => match spawn {
            Some(binding) => serde_json::json!({"decision": "allow_call", "spawn_binding": binding.0}),
            None => serde_json::json!({"decision": "allow_call"}),
        },
        HookDecision::PassControl => serde_json::json!({"decision": "pass_control"}),
        HookDecision::DenyCall {
            feedback,
            offers,
            review,
        } => serde_json::json!({
            "decision": "deny_call",
            "feedback": feedback,
            "offers": offers.iter().map(|offer| serde_json::json!({"offer_id": offer.id})).collect::<Vec<_>>(),
            "review": review.iter().map(|review| serde_json::json!({"offer_id": review.offer, "text": review.text})).collect::<Vec<_>>(),
        }),
        HookDecision::Block { reason } => serde_json::json!({"decision": "block", "reason": reason}),
        HookDecision::ReplaceOutput { output } => serde_json::json!({"decision": "replace_output", "output": output}),
        HookDecision::DeliverValue { value } => serde_json::json!({"decision": "deliver_value", "value": value}),
        HookDecision::ChildReturn { value } => serde_json::json!({"decision": "child_return", "value": value}),
        HookDecision::Context { text } => serde_json::json!({"decision": "context", "text": text}),
        HookDecision::Refuse { detail } => serde_json::json!({"decision": "refuse", "detail": detail}),
    }
}

fn tool_call(event: &HookEvent) -> Option<(&str, &str)> {
    match event {
        HookEvent::ToolCall { call, .. } => Some((&call.tool, call.arguments.get())),
        _ => None,
    }
}

fn typed_kind(runtime: &Runtime, kind: OfferKind) -> &'static str {
    match kind {
        OfferKind::Accept => "acceptance",
        OfferKind::Authority { names } if names.iter().any(|name| runtime.is_hitl_authority(name)) => "human_approval",
        OfferKind::Authority { .. } => "authority",
        OfferKind::Sanitizer { .. } => "sanitizer",
    }
}

fn stable_arguments_sha256(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let canonical = appa_engine::params::canonical_bytes(&value);
    Some(format!("{:x}", Sha256::digest(canonical)))
}

fn canonical_arguments(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    String::from_utf8(appa_engine::params::canonical_bytes(&value)).ok()
}

fn validate_approval_grant<'a>(
    secret: &str,
    approval: &'a ApprovalGrant,
    resolve: &ResolveOffer,
) -> Result<(), &'a str> {
    if uuid::Uuid::parse_str(&approval.approval_id).is_err() {
        return Err("approval grant id must be a UUID");
    }
    if approval.reviewer_id.trim().is_empty() {
        return Err("approval grant reviewer_id is required");
    }
    if approval.root_id != resolve.root_id
        || approval.offer_id != resolve.offer_id
        || approval.tool != resolve.tool
        || approval.arguments_sha256 != resolve.arguments_sha256
        || approval.resolution != resolve.resolution
    {
        return Err("approval grant does not match this offer resolution");
    }
    if resolve.event == "resolve_batch_offer"
        && (approval.batch_id != resolve.batch_id || approval.position != resolve.position)
    {
        return Err("approval grant does not match this batch position");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(i64::MAX);
    if approval.expires_at <= now {
        return Err("approval grant has expired");
    }
    if approval.expires_at > now.saturating_add(5 * 60 * 1000) {
        return Err("approval grant expiry exceeds five minutes");
    }
    let signed = serde_json::json!({
        "approval_id": approval.approval_id,
        "reviewer_id": approval.reviewer_id,
        "root_id": approval.root_id,
        "offer_id": approval.offer_id,
        "tool": approval.tool,
        "arguments_sha256": approval.arguments_sha256,
        "resolution": approval.resolution,
        "expires_at": approval.expires_at,
    });
    let mut signed = signed;
    if resolve.event == "resolve_batch_offer" {
        signed["batch_id"] = serde_json::json!(approval.batch_id);
        signed["position"] = serde_json::json!(approval.position);
    }
    let bytes = appa_engine::params::canonical_bytes(&signed);
    if approval_signature_matches(secret, &bytes, &approval.signature) {
        Ok(())
    } else {
        Err("approval grant signature is invalid")
    }
}

fn approval_signature_matches(secret: &str, bytes: &[u8], signature: &str) -> bool {
    use hmac::{Hmac, Mac};

    type HmacSha256 = Hmac<Sha256>;
    if signature.len() != 64 || !signature.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return false;
    }
    let signature = (0..signature.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&signature[offset..offset + 2], 16))
        .collect::<Result<Vec<_>, _>>();
    let Ok(signature) = signature else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(bytes);
    mac.verify_slice(&signature).is_ok()
}

fn kagent_root(root_id: &str) -> TrajectoryId {
    TrajectoryId(format!("kagent:{root_id}"))
}

/// Batch events keep the platform's family root for receipt serialization, while a non-empty
/// child id selects the APPA trajectory created by `child_start` under that family.
fn kagent_lane(root_id: &str, child_id: Option<&str>) -> Option<TrajectoryId> {
    let root = kagent_root(root_id);
    match child_id {
        None => Some(root),
        Some("") => None,
        Some(child) => Some(TrajectoryId(format!("{}:{child}", root.0))),
    }
}

fn resolved(resolve: &ResolveOffer, kind: &str, resolution: &str) -> serde_json::Value {
    let mut response = serde_json::json!({
        "decision": if resolve.event == "resolve_batch_offer" { "batch_offer_resolved" } else { "offer_resolved" },
        "offer_id": resolve.offer_id,
        "kind": kind,
        "resolution": resolution,
        "tool": resolve.tool,
        "arguments_sha256": resolve.arguments_sha256,
    });
    if resolve.event == "resolve_batch_offer" {
        response["batch_id"] = serde_json::json!(resolve.batch_id);
        response["position"] = serde_json::json!(resolve.position);
    }
    response
}

fn refuse(detail: &str) -> serde_json::Value {
    serde_json::json!({"decision": "refuse", "detail": detail})
}

fn envelope_response(event_id: &str, request_sha256: &str, decision: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "protocol_version": 1,
        "event_id": event_id,
        "request_sha256": request_sha256,
        "decision": decision,
    }))
    .expect("proxy responses contain only serializable JSON")
}

fn response_or_refusal(event_id: &str, request_sha256: &str, response: Vec<u8>) -> Vec<u8> {
    if response.len() <= MAX_RESPONSE_BYTES {
        return response;
    }
    serde_json::to_vec(&serde_json::json!({
        "protocol_version": 1,
        "event_id": event_id,
        "request_sha256": request_sha256,
        "decision": {"decision": "refuse", "detail": "proxy response exceeds the response limit"},
    }))
    .expect("compact proxy refusal serializes")
}

fn cached(response: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        response,
    )
        .into_response()
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::config::Config;

    #[test]
    fn known_failure_has_a_safe_presentation_but_uncertainty_does_not() {
        let presentation = model_presentation(&ToolOutcome::Failure {
            message: "SYNTHETIC_PRIVATE_ERROR_DETAILS".to_owned(),
        })
        .expect("a completed failure has a bounded model-visible notice");
        assert!(presentation.contains("failed"));
        assert!(!presentation.contains("SYNTHETIC_PRIVATE_ERROR_DETAILS"));
        assert!(model_presentation(&ToolOutcome::Indeterminate).is_none());
    }

    fn runtime(directory: &tempfile::TempDir) -> Arc<Runtime> {
        runtime_with_policy(
            directory,
            r#"
                [policy]
                version = 2
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        )
    }

    fn runtime_with_policy(directory: &tempfile::TempDir, policy: &str) -> Arc<Runtime> {
        let config_path = directory.path().join("appa.toml");
        std::fs::write(&config_path, policy).expect("fixture config writes");
        Arc::new(
            Runtime::open(
                Config::load(&config_path).expect("config parses"),
                directory.path().join("appa.db"),
                None,
            )
            .expect("runtime opens"),
        )
    }

    fn session_start(id: &str) -> String {
        format!(r#"{{"event_id":"{id}","event":{{"event":"session_start","root_id":"root-1"}}}}"#)
    }

    async fn post(app: Router, body: String) -> Response {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/events")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request builds"),
        )
        .await
        .expect("router responds")
    }

    async fn decision(app: Router, body: String) -> serde_json::Value {
        let response = post(app, body).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
            .await
            .expect("response body reads");
        serde_json::from_slice(&body).expect("response is JSON")
    }

    fn wrapped(id: &str, event: serde_json::Value) -> String {
        serde_json::json!({"event_id": id, "event": event}).to_string()
    }

    #[tokio::test]
    async fn proxy_checkpoint_and_openapi_routes_share_the_kagent_protocol_surface() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime(&directory);
        let app = router(Arc::clone(&runtime), None);

        let started = post(app.clone(), session_start("123e4567-e89b-12d3-a456-426614174140")).await;
        assert_eq!(started.status(), StatusCode::OK);

        let checkpoint = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/checkpoints")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "protocol": appa_runtime_api::PROTOCOL,
                            "adapter": "kagent",
                            "operation": "create",
                            "root_id": "root-1",
                        })
                        .to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(checkpoint.status(), StatusCode::OK);
        let checkpoint = axum::body::to_bytes(checkpoint.into_body(), MAX_RESPONSE_BYTES)
            .await
            .expect("checkpoint body reads");
        let checkpoint: serde_json::Value = serde_json::from_slice(&checkpoint).expect("checkpoint is JSON");
        let fork = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/checkpoints")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "protocol": appa_runtime_api::PROTOCOL,
                            "adapter": "kagent",
                            "operation": "fork",
                            "checkpoint_id": checkpoint["checkpoint_id"],
                            "root_id": "fork-1",
                        })
                        .to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(fork.status(), StatusCode::OK);

        let capabilities = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/capabilities")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        let capabilities = axum::body::to_bytes(capabilities.into_body(), MAX_RESPONSE_BYTES)
            .await
            .expect("capabilities body reads");
        let capabilities: serde_json::Value = serde_json::from_slice(&capabilities).expect("capabilities are JSON");
        assert_eq!(capabilities["durable_checkpoints"], true);
        assert_eq!(capabilities["review_provenance"], true);
        assert_eq!(capabilities["openapi"], true);

        let schema = app
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(schema.status(), StatusCode::OK);
        let schema = axum::body::to_bytes(schema.into_body(), MAX_RESPONSE_BYTES)
            .await
            .expect("schema body reads");
        let schema: serde_json::Value = serde_json::from_slice(&schema).expect("schema is JSON");
        assert_eq!(schema["openapi"], "3.1.0");
        assert!(schema["paths"].get("/proxy/v1/events").is_some());
        assert!(schema["paths"].get("/proxy/v1/checkpoints").is_some());
        assert!(schema["components"]["schemas"].get("ProxyEventEnvelope").is_some());
        assert!(schema["components"]["schemas"].get("CheckpointRequest").is_some());
        assert!(schema["components"]["schemas"].get("ToolCalls").is_some());
        assert!(schema["components"]["schemas"].get("PrepareBatch").is_some());
        assert!(
            schema["paths"]["/proxy/v1/checkpoints"]["post"]["responses"]
                .get("413")
                .is_some()
        );
    }

    #[tokio::test]
    async fn oversized_checkpoint_request_is_rejected_before_durable_checkpoint_admission() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime(&directory);
        let host_root = format!("checkpoint-limit-{}", "x".repeat(MAX_REQUEST_BYTES));
        let root = kagent_root(&host_root);
        runtime.create_session(root.clone()).expect("root opens");
        let before = runtime.audit(&root).expect("root audit reads");
        let app = router(Arc::clone(&runtime), None);
        let oversized = format!(
            "{{\"protocol\":{},\"adapter\":\"kagent\",\"operation\":\"create\",\"root_id\":\"{}\"}}",
            appa_runtime_api::PROTOCOL,
            host_root,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/checkpoints")
                    .header("content-type", "application/json")
                    .body(Body::from(oversized))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(runtime.audit(&root).expect("root audit reads"), before);
    }

    fn approval(
        secret: &str,
        root_id: &str,
        offer: &serde_json::Value,
        tool: &str,
        resolution: &str,
    ) -> serde_json::Value {
        use hmac::{Hmac, Mac};

        type HmacSha256 = Hmac<Sha256>;
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock follows the epoch")
            .as_millis() as i64
            + 60_000;
        let grant = serde_json::json!({
            "approval_id": uuid::Uuid::new_v4().to_string(),
            "reviewer_id": "reviewer-1",
            "root_id": root_id,
            "offer_id": offer["offer_id"],
            "tool": tool,
            "arguments_sha256": offer["arguments_sha256"],
            "resolution": resolution,
            "expires_at": expires_at,
        });
        let bytes = appa_engine::params::canonical_bytes(&grant);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts the fixture key");
        mac.update(&bytes);
        let signature = mac.finalize().into_bytes();
        let mut signed = grant;
        signed["signature"] = serde_json::Value::String(signature.iter().map(|byte| format!("{byte:02x}")).collect());
        signed
    }

    fn batch_approval(
        secret: &str,
        root_id: &str,
        offer: &serde_json::Value,
        tool: &str,
        resolution: &str,
        batch_id: &str,
        position: u32,
    ) -> serde_json::Value {
        use hmac::{Hmac, Mac};

        type HmacSha256 = Hmac<Sha256>;
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock follows the epoch")
            .as_millis() as i64
            + 60_000;
        let grant = serde_json::json!({
            "approval_id": uuid::Uuid::new_v4().to_string(),
            "reviewer_id": "reviewer-1",
            "root_id": root_id,
            "offer_id": offer["offer_id"],
            "tool": tool,
            "arguments_sha256": offer["arguments_sha256"],
            "resolution": resolution,
            "expires_at": expires_at,
            "batch_id": batch_id,
            "position": position,
        });
        let bytes = appa_engine::params::canonical_bytes(&grant);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts the fixture key");
        mac.update(&bytes);
        let signature = mac.finalize().into_bytes();
        let mut signed = grant;
        signed["signature"] = serde_json::Value::String(signature.iter().map(|byte| format!("{byte:02x}")).collect());
        signed
    }

    #[tokio::test]
    async fn a_completed_event_replays_after_a_real_sqlite_reopen() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let body = session_start("123e4567-e89b-12d3-a456-426614174000");
        let first = runtime(&directory);
        assert_eq!(
            post(router(Arc::clone(&first), None), body.clone()).await.status(),
            StatusCode::OK
        );
        assert_eq!(first.log_basis(&kagent_root("root-1")), 1);
        drop(first);

        let reopened = runtime(&directory);
        assert_eq!(
            post(router(Arc::clone(&reopened), None), body).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            reopened.log_basis(&kagent_root("root-1")),
            1,
            "the replay did not reach the engine"
        );
    }

    #[tokio::test]
    async fn unequal_exact_body_bytes_conflict_without_reexecution() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime(&directory);
        let body = session_start("123e4567-e89b-12d3-a456-426614174001");
        assert_eq!(
            post(router(Arc::clone(&runtime), None), body.clone()).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            post(router(Arc::clone(&runtime), None), format!("{body} "))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(runtime.log_basis(&kagent_root("root-1")), 1);
    }

    #[tokio::test]
    async fn a_pending_receipt_blocks_a_new_event_for_its_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime(&directory);
        let state = ProxyState::new(Arc::clone(&runtime), None);
        let pending = session_start("123e4567-e89b-12d3-a456-426614174002");
        let digest = format!("{:x}", Sha256::digest(pending.as_bytes()));
        assert_eq!(
            runtime
                .begin_proxy_event(
                    "root-1",
                    "123e4567-e89b-12d3-a456-426614174002",
                    &digest,
                    &state.boot_owner
                )
                .expect("intent persists"),
            ProxyEventAdmission::Started
        );
        let body = session_start("123e4567-e89b-12d3-a456-426614174003");
        let response = event(State(state), Bytes::from(body)).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            runtime
                .begin_proxy_event("root-1", "123e4567-e89b-12d3-a456-426614174003", &digest, "new-boot")
                .expect("pending root is readable"),
            ProxyEventAdmission::RootPending
        );
    }

    #[tokio::test]
    async fn root_gate_serializes_distinct_events_and_cancellation_keeps_the_root_frozen() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime(&directory);
        let state = ProxyState::new(Arc::clone(&runtime), None);
        let pending = session_start("123e4567-e89b-12d3-a456-426614174004");
        let digest = format!("{:x}", Sha256::digest(pending.as_bytes()));
        runtime
            .begin_proxy_event(
                "root-1",
                "123e4567-e89b-12d3-a456-426614174004",
                &digest,
                &state.boot_owner,
            )
            .expect("pending intent persists");
        let gate = state
            .root_gates
            .lock()
            .expect("gates lock")
            .entry("root-1".to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let held = gate.lock().await;
        let waiting_state = state.clone();
        let waiting = tokio::spawn(async move {
            event(
                State(waiting_state),
                Bytes::from(session_start("123e4567-e89b-12d3-a456-426614174005")),
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "the second root event waits behind the root gate"
        );
        waiting.abort();
        drop(held);
        let response = event(
            State(state),
            Bytes::from(session_start("123e4567-e89b-12d3-a456-426614174006")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn no_approval_secret_keeps_basic_proxy_features_but_hides_human_approval() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let response = router(runtime(&directory), None)
            .oneshot(
                Request::builder()
                    .uri("/capabilities")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        let body = axum::body::to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
            .await
            .expect("capability body reads");
        let capabilities: serde_json::Value = serde_json::from_slice(&body).expect("capabilities are JSON");
        assert_eq!(capabilities["completed_event_replay"], true);
        assert_eq!(capabilities["human_approval"], false);
        assert_eq!(capabilities["approval_grants"], false);
        assert_eq!(capabilities["legacy_hooks"], false);
        assert_eq!(capabilities["restriction_acceptance"], true);
        assert_eq!(capabilities["parallel_calls"], true);
        assert_eq!(capabilities["dispatch_call_mapping"], true);
        assert_eq!(capabilities["child_actor_targeting"], true);
    }

    #[tokio::test]
    async fn tool_calls_release_one_core_batch_and_results_follow_call_ids() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "first"
                delta = {}
                [[policy.tool]]
                name = "second"
                delta = {}
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174050",
            serde_json::json!({"event":"session_start", "root_id":"root-batch"}),
        )
        .await;
        let batch = serde_json::json!({
            "event":"tool_calls",
            "root_id":"root-batch",
            "calls":[
                {"call_id":"first-call", "tool":"first", "arguments":{}, "spawn":false},
                {"call_id":"second-call", "tool":"second", "arguments":{}, "spawn":false}
            ]
        });
        let released = start("123e4567-e89b-12d3-a456-426614174051", batch.clone()).await;
        assert_eq!(released["decision"]["decision"], "allow_calls");
        assert_eq!(released["decision"]["calls"].as_array().map(Vec::len), Some(2));
        assert_eq!(released["decision"]["calls"][0]["call_id"], "first-call");
        assert_eq!(released["decision"]["calls"][1]["call_id"], "second-call");
        assert!(released["decision"]["calls"][0]["dispatch_id"].is_string());
        assert!(released["decision"]["calls"][1]["dispatch_id"].is_string());
        assert!(released["decision"]["calls"][0].get("tool").is_none());
        assert!(released["decision"]["calls"][0].get("arguments").is_none());
        assert_eq!(
            runtime.log_basis(&kagent_root("root-batch")),
            2,
            "session opening plus one proposal-batch append; calls were not dispatched sequentially",
        );
        let replayed = start("123e4567-e89b-12d3-a456-426614174051", batch).await;
        assert_eq!(replayed, released, "completed batch receipts replay exactly");

        let second = start(
            "123e4567-e89b-12d3-a456-426614174052",
            serde_json::json!({
                "event":"tool_result",
                "root_id":"root-batch",
                "call_id":"second-call",
                "dispatch_id":released["decision"]["calls"][1]["dispatch_id"],
                "outcome":{"status":"success", "body":{"ok":true}}
            }),
        )
        .await;
        assert_eq!(second["decision"]["decision"], "result_admitted");
        assert_eq!(second["decision"]["call_id"], "second-call");
        assert_eq!(second["decision"]["presentation"], r#"{"ok":true}"#);
        let mismatched = start(
            "123e4567-e89b-12d3-a456-426614174053",
            serde_json::json!({
                "event":"tool_result",
                "root_id":"root-batch",
                "call_id":"first-call",
                "dispatch_id":released["decision"]["calls"][1]["dispatch_id"],
                "outcome":{"status":"success", "body":{"ok":true}}
            }),
        )
        .await;
        assert_eq!(mismatched["decision"]["decision"], "refuse");
        let first = start(
            "123e4567-e89b-12d3-a456-426614174054",
            serde_json::json!({
                "event":"tool_result",
                "root_id":"root-batch",
                "call_id":"first-call",
                "dispatch_id":released["decision"]["calls"][0]["dispatch_id"],
                "outcome":{"status":"success", "body":{"ok":true}}
            }),
        )
        .await;
        assert_eq!(first["decision"]["decision"], "result_admitted");
        assert_eq!(first["decision"]["call_id"], "first-call");
    }

    #[tokio::test]
    async fn held_batch_withholds_allowed_dispatches_until_its_commit() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "same"
                delta = {}
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174090",
            serde_json::json!({"event":"session_start","root_id":"root-held"}),
        )
        .await;
        let batch_id = "123e4567-e89b-12d3-a456-426614174091";
        let prepared = start(
            "123e4567-e89b-12d3-a456-426614174092",
            serde_json::json!({
                "event":"prepare_batch",
                "root_id":"root-held",
                "batch_id":batch_id,
                "calls":[
                    {"call_id":"one","tool":"same","arguments":{},"spawn":false},
                    {"call_id":"two","tool":"same","arguments":{},"spawn":false}
                ]
            }),
        )
        .await;
        assert_eq!(prepared["decision"]["decision"], "batch_prepared");
        assert!(
            prepared["decision"]["positions"]
                .as_array()
                .expect("positions are ordered")
                .iter()
                .all(|position| position.get("dispatch_id").is_none()),
            "prepare must not publish a business dispatch: {prepared}"
        );
        let committed = start(
            "123e4567-e89b-12d3-a456-426614174093",
            serde_json::json!({"event":"commit_batch","root_id":"root-held","batch_id":batch_id}),
        )
        .await;
        assert_eq!(committed["decision"]["decision"], "batch_committed");
        assert_eq!(committed["decision"]["calls"].as_array().map(Vec::len), Some(2));
        assert!(
            committed["decision"]["calls"]
                .as_array()
                .unwrap()
                .iter()
                .all(|call| call["dispatch_id"].is_string())
        );
        let replayed = start(
            "123e4567-e89b-12d3-a456-426614174093",
            serde_json::json!({"event":"commit_batch","root_id":"root-held","batch_id":batch_id}),
        )
        .await;
        assert_eq!(replayed, committed, "commit receipt replays without another release");
    }

    #[tokio::test]
    async fn held_batch_binds_distinct_positions_and_advances_basis_after_approval() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "plain"
                parameters = { type = "object", properties = { value = { type = "integer" } }, required = ["value"] }
                delta = {}
                [[policy.tool]]
                name = "wire"
                parameters = { type = "object", properties = { amount = { type = "integer" } }, required = ["amount"] }
                requires = { attention = ["human"] }
                delta = {}
                [[policy.authority]]
                name = "oncall"
                [policy.authority.permits]
                attention = ["human"]
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
                [externals.authorities.oncall]
                builtin = "hitl"
            "#,
        );
        let secret: Arc<str> = Arc::from("01234567890123456789012345678901");
        let start = |id, event| {
            decision(
                router(Arc::clone(&runtime), Some(Arc::clone(&secret))),
                wrapped(id, event),
            )
        };
        let batch_id = "123e4567-e89b-12d3-a456-426614174094";
        start(
            "123e4567-e89b-12d3-a456-426614174095",
            serde_json::json!({"event":"session_start","root_id":"root-mixed"}),
        )
        .await;
        let prepared = start(
            "123e4567-e89b-12d3-a456-426614174096",
            serde_json::json!({
                "event":"prepare_batch", "root_id":"root-mixed", "batch_id":batch_id,
                "calls":[
                    {"call_id":"plain-1","tool":"plain","arguments":{"value":1},"spawn":false},
                    {"call_id":"wire-2","tool":"wire","arguments":{"amount":2},"spawn":false}
                ]
            }),
        )
        .await;
        assert_eq!(prepared["decision"]["decision"], "batch_prepared", "{prepared}");
        assert_eq!(
            prepared["decision"]["positions"][0]["arguments_sha256"],
            stable_arguments_sha256(r#"{"value":1}"#).expect("fixture arguments are JSON")
        );
        let offer = &prepared["decision"]["positions"][1]["offers"][0];
        assert_eq!(offer["kind"], "human_approval");
        let wrong_position = start(
            "123e4567-e89b-12d3-a456-426614174097",
            serde_json::json!({
                "event":"resolve_batch_offer", "root_id":"root-mixed", "batch_id":batch_id, "position":1,
                "offer_id":offer["offer_id"], "tool":"wire", "arguments_sha256":offer["arguments_sha256"], "resolution":"approve",
                "approval":batch_approval(&secret, "root-mixed", offer, "wire", "approve", batch_id, 0)
            }),
        )
        .await;
        assert_eq!(wrong_position["decision"]["decision"], "refuse");
        let approved = start(
            "123e4567-e89b-12d3-a456-426614174098",
            serde_json::json!({
                "event":"resolve_batch_offer", "root_id":"root-mixed", "batch_id":batch_id, "position":1,
                "offer_id":offer["offer_id"], "tool":"wire", "arguments_sha256":offer["arguments_sha256"], "resolution":"approve",
                "approval":batch_approval(&secret, "root-mixed", offer, "wire", "approve", batch_id, 1)
            }),
        )
        .await;
        assert_eq!(approved["decision"]["resolution"], "approved", "{approved}");
        assert_eq!(
            runtime
                .proxy_batch(batch_id)
                .expect("batch reads")
                .expect("batch exists")
                .basis,
            runtime.log_basis(&kagent_root("root-mixed")),
            "the approval's verified transition moved the stored expected basis"
        );
        assert!(
            runtime
                .proxy_dispatch_binding("root-mixed", "kagent:root-mixed", "plain-1")
                .expect("binding reads")
                .is_none(),
            "the allowed sibling is still private before commit"
        );
        let committed = start(
            "123e4567-e89b-12d3-a456-426614174099",
            serde_json::json!({"event":"commit_batch","root_id":"root-mixed","batch_id":batch_id}),
        )
        .await;
        assert_eq!(committed["decision"]["decision"], "batch_committed", "{committed}");
        assert_eq!(committed["decision"]["calls"][0]["call_id"], "plain-1");
        assert_eq!(
            committed["decision"]["calls"][0]["arguments"],
            serde_json::json!({"value":1})
        );
        assert_eq!(committed["decision"]["calls"][1]["call_id"], "wire-2");
        assert_eq!(
            committed["decision"]["calls"][1]["arguments"],
            serde_json::json!({"amount":2})
        );
    }

    #[tokio::test]
    async fn a_serial_controller_can_prepare_approve_and_commit_one_held_call() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "wire"
                parameters = { type = "object", properties = { amount = { type = "integer" } }, required = ["amount"] }
                requires = { attention = ["human"] }
                delta = {}
                [[policy.authority]]
                name = "oncall"
                [policy.authority.permits]
                attention = ["human"]
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
                [externals.authorities.oncall]
                builtin = "hitl"
            "#,
        );
        let secret: Arc<str> = Arc::from("01234567890123456789012345678901");
        let start = |id, event| {
            decision(
                router(Arc::clone(&runtime), Some(Arc::clone(&secret))),
                wrapped(id, event),
            )
        };
        let batch_id = "123e4567-e89b-12d3-a456-426614174141";
        start(
            "123e4567-e89b-12d3-a456-426614174142",
            serde_json::json!({"event":"session_start","root_id":"root-serial-held"}),
        )
        .await;
        let prepared = start(
            "123e4567-e89b-12d3-a456-426614174143",
            serde_json::json!({
                "event":"prepare_batch", "root_id":"root-serial-held", "batch_id":batch_id,
                "calls":[{"call_id":"wire-1","tool":"wire","arguments":{"amount":2},"spawn":false}]
            }),
        )
        .await;
        assert_eq!(prepared["decision"]["decision"], "batch_prepared", "{prepared}");
        let offer = &prepared["decision"]["positions"][0]["offers"][0];
        assert_eq!(offer["kind"], "human_approval");

        let approved = start(
            "123e4567-e89b-12d3-a456-426614174144",
            serde_json::json!({
                "event":"resolve_batch_offer", "root_id":"root-serial-held", "batch_id":batch_id, "position":0,
                "offer_id":offer["offer_id"], "tool":"wire", "arguments_sha256":offer["arguments_sha256"], "resolution":"approve",
                "approval":batch_approval(&secret, "root-serial-held", offer, "wire", "approve", batch_id, 0)
            }),
        )
        .await;
        assert_eq!(approved["decision"]["resolution"], "approved", "{approved}");

        let committed = start(
            "123e4567-e89b-12d3-a456-426614174145",
            serde_json::json!({"event":"commit_batch","root_id":"root-serial-held","batch_id":batch_id}),
        )
        .await;
        assert_eq!(committed["decision"]["decision"], "batch_committed", "{committed}");
        assert_eq!(committed["decision"]["calls"].as_array().map(Vec::len), Some(1));
        assert_eq!(committed["decision"]["calls"][0]["call_id"], "wire-1");
        assert!(committed["decision"]["calls"][0]["dispatch_id"].is_string());
    }

    #[tokio::test]
    async fn held_batch_refuses_changed_arguments_and_an_unrelated_basis_change() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "taint"
                parameters = { type = "object", properties = { a = { type = "integer" } }, required = ["a"] }
                delta = { trust = "suspicious" }
                [[policy.tool]]
                name = "plain"
                delta = {}
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        let batch_id = "123e4567-e89b-12d3-a456-426614174100";
        start(
            "123e4567-e89b-12d3-a456-426614174101",
            serde_json::json!({"event":"session_start","root_id":"root-basis"}),
        )
        .await;
        let prepared = start(
            "123e4567-e89b-12d3-a456-426614174102",
            serde_json::json!({"event":"prepare_batch","root_id":"root-basis","batch_id":batch_id,"calls":[{"call_id":"taint","tool":"taint","arguments":{"a":1},"spawn":false}]}),
        )
        .await;
        let offer = &prepared["decision"]["positions"][0]["offers"][0];
        let changed_arguments = start(
            "123e4567-e89b-12d3-a456-426614174103",
            serde_json::json!({"event":"resolve_batch_offer","root_id":"root-basis","batch_id":batch_id,"position":0,"offer_id":offer["offer_id"],"tool":"taint","arguments_sha256":stable_arguments_sha256(r#"{"a":2}"#),"resolution":"accept"}),
        )
        .await;
        assert_eq!(changed_arguments["decision"]["decision"], "refuse");
        let unrelated = start(
            "123e4567-e89b-12d3-a456-426614174104",
            serde_json::json!({"event":"tool_calls","root_id":"root-basis","calls":[{"call_id":"other","tool":"plain","arguments":{},"spawn":false}]}),
        )
        .await;
        assert_eq!(unrelated["decision"]["decision"], "allow_calls");
        let stale = start(
            "123e4567-e89b-12d3-a456-426614174105",
            serde_json::json!({"event":"resolve_batch_offer","root_id":"root-basis","batch_id":batch_id,"position":0,"offer_id":offer["offer_id"],"tool":"taint","arguments_sha256":offer["arguments_sha256"],"resolution":"accept"}),
        )
        .await;
        assert_eq!(stale["decision"]["decision"], "refuse");
        assert_eq!(
            stale["decision"]["detail"],
            "batch basis changed before it could be committed"
        );
    }

    #[tokio::test]
    async fn cancelling_a_held_batch_quarantines_its_commit() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "plain"
                delta = {}
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        let batch_id = "123e4567-e89b-12d3-a456-426614174106";
        start(
            "123e4567-e89b-12d3-a456-426614174107",
            serde_json::json!({"event":"session_start","root_id":"root-cancel"}),
        )
        .await;
        start(
            "123e4567-e89b-12d3-a456-426614174108",
            serde_json::json!({"event":"prepare_batch","root_id":"root-cancel","batch_id":batch_id,"calls":[{"call_id":"plain","tool":"plain","arguments":{},"spawn":false}]}),
        )
        .await;
        let cancelled = start(
            "123e4567-e89b-12d3-a456-426614174109",
            serde_json::json!({"event":"cancel_batch","root_id":"root-cancel","batch_id":batch_id,"reason":"client abandoned batch"}),
        )
        .await;
        assert_eq!(cancelled["decision"]["decision"], "batch_quarantined");
        let commit = start(
            "123e4567-e89b-12d3-a456-426614174110",
            serde_json::json!({"event":"commit_batch","root_id":"root-cancel","batch_id":batch_id}),
        )
        .await;
        assert_eq!(commit["decision"]["decision"], "refuse");
        assert_eq!(
            commit["decision"]["detail"],
            "batch is quarantined and cannot be committed"
        );
    }

    #[tokio::test]
    async fn held_batch_preserves_its_spawn_binding_until_commit() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "fetch"
                delta = {}
                [policy.deployment]
                context_control = true
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        let batch_id = "123e4567-e89b-12d3-a456-426614174111";
        start(
            "123e4567-e89b-12d3-a456-426614174112",
            serde_json::json!({"event":"session_start","root_id":"root-batch-spawn"}),
        )
        .await;
        let declaration = start(
            "123e4567-e89b-12d3-a456-426614174116",
            serde_json::json!({"event":"tool_calls","root_id":"root-batch-spawn","calls":[{"call_id":"declare","tool":"fetch","arguments":{},"spawn":true}]}),
        )
        .await;
        let offer = &declaration["decision"]["calls"][0]["offers"][0];
        start(
            "123e4567-e89b-12d3-a456-426614174117",
            serde_json::json!({"event":"resolve_offer","root_id":"root-batch-spawn","offer_id":offer["offer_id"],"tool":"fetch","arguments_sha256":offer["arguments_sha256"],"resolution":"accept","label":{"trust":"trusted"}}),
        )
        .await;
        let prepared = start(
            "123e4567-e89b-12d3-a456-426614174113",
            serde_json::json!({"event":"prepare_batch","root_id":"root-batch-spawn","batch_id":batch_id,"calls":[{"call_id":"spawn","tool":"fetch","arguments":{},"spawn":true}]}),
        )
        .await;
        assert_eq!(prepared["decision"]["decision"], "batch_prepared", "{prepared}");
        assert!(prepared["decision"]["positions"][0].get("spawn_binding").is_none());
        let committed = start(
            "123e4567-e89b-12d3-a456-426614174114",
            serde_json::json!({"event":"commit_batch","root_id":"root-batch-spawn","batch_id":batch_id}),
        )
        .await;
        let binding = &committed["decision"]["calls"][0]["spawn_binding"];
        assert!(
            binding.is_string(),
            "commit must preserve the prepared fork binding: {committed}"
        );
        let child = start(
            "123e4567-e89b-12d3-a456-426614174115",
            serde_json::json!({"event":"child_start","root_id":"root-batch-spawn","child_id":"child","spawn_binding":binding}),
        )
        .await;
        assert_ne!(
            child["decision"]["decision"], "refuse",
            "the committed binding starts its child: {child}"
        );
    }

    #[tokio::test]
    async fn input_substitution_stays_held_until_its_batch_commits() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "read_hr"
                delta = { audience = ["hr"] }
                [[policy.tool]]
                name = "send"
                parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
                requires = { audience = { contains = ["public"] } }
                delta = {}
                [[policy.sanitizer]]
                name = "redactor"
                on = ["tool_input"]
                permits = { audience = { from = ["hr"], to = ["public"] } }
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
                [externals.sanitizers.redactor]
                builtin = "redact-email"
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174116",
            serde_json::json!({"event":"session_start","root_id":"root-input"}),
        )
        .await;
        let read = start(
            "123e4567-e89b-12d3-a456-426614174117",
            serde_json::json!({"event":"tool_calls","root_id":"root-input","calls":[{"call_id":"read","tool":"read_hr","arguments":{},"spawn":false}]}),
        )
        .await;
        let accept = &read["decision"]["calls"][0]["offers"][0];
        start(
            "123e4567-e89b-12d3-a456-426614174118",
            serde_json::json!({"event":"resolve_offer","root_id":"root-input","offer_id":accept["offer_id"],"tool":"read_hr","arguments_sha256":accept["arguments_sha256"],"resolution":"accept"}),
        )
        .await;
        let read = start(
            "123e4567-e89b-12d3-a456-426614174119",
            serde_json::json!({"event":"tool_calls","root_id":"root-input","calls":[{"call_id":"read-run","tool":"read_hr","arguments":{},"spawn":false}]}),
        )
        .await;
        start(
            "123e4567-e89b-12d3-a456-426614174120",
            serde_json::json!({"event":"tool_result","root_id":"root-input","call_id":"read-run","dispatch_id":read["decision"]["calls"][0]["dispatch_id"],"outcome":{"status":"success","body":"Alice Chen"}}),
        )
        .await;
        let batch_id = "123e4567-e89b-12d3-a456-426614174121";
        let prepared = start(
            "123e4567-e89b-12d3-a456-426614174122",
            serde_json::json!({"event":"prepare_batch","root_id":"root-input","batch_id":batch_id,"calls":[{"call_id":"send","tool":"send","arguments":{"body":"mail alice@corp.example today"},"spawn":false}]}),
        )
        .await;
        let sanitizer = &prepared["decision"]["positions"][0]["offers"][0];
        let substituted = start(
            "123e4567-e89b-12d3-a456-426614174123",
            serde_json::json!({"event":"resolve_batch_offer","root_id":"root-input","batch_id":batch_id,"position":0,"offer_id":sanitizer["offer_id"],"tool":"send","arguments_sha256":sanitizer["arguments_sha256"],"resolution":"apply_sanitizer"}),
        )
        .await;
        assert_eq!(
            substituted["decision"]["decision"], "batch_offer_resolved",
            "{substituted}"
        );
        assert!(
            runtime
                .proxy_dispatch_binding("root-input", "kagent:root-input", "send")
                .expect("binding reads")
                .is_none(),
            "the substituted dispatch is not client-reachable before commit"
        );
        let committed = start(
            "123e4567-e89b-12d3-a456-426614174124",
            serde_json::json!({"event":"commit_batch","root_id":"root-input","batch_id":batch_id}),
        )
        .await;
        assert_eq!(committed["decision"]["decision"], "batch_committed", "{committed}");
        assert_eq!(
            committed["decision"]["calls"][0]["arguments"],
            serde_json::json!({"body":"mail [redacted-email] today"})
        );
    }

    #[tokio::test]
    async fn no_answer_from_a_batch_input_sanitizer_quarantines_the_batch() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "read_hr"
                delta = { audience = ["hr"] }
                [[policy.tool]]
                name = "send"
                parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
                requires = { audience = { contains = ["public"] } }
                delta = {}
                [[policy.sanitizer]]
                name = "offline"
                on = ["tool_input"]
                permits = { audience = { from = ["hr"], to = ["public"] } }
                [externals]
                timeout_ms = 100
                max_body_bytes = 4096
                [externals.sanitizers.offline]
                url = "http://127.0.0.1:1/sanitize"
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174125",
            serde_json::json!({"event":"session_start","root_id":"root-no-answer"}),
        )
        .await;
        let read = start(
            "123e4567-e89b-12d3-a456-426614174126",
            serde_json::json!({"event":"tool_calls","root_id":"root-no-answer","calls":[{"call_id":"read","tool":"read_hr","arguments":{},"spawn":false}]}),
        )
        .await;
        let accept = &read["decision"]["calls"][0]["offers"][0];
        start(
            "123e4567-e89b-12d3-a456-426614174127",
            serde_json::json!({"event":"resolve_offer","root_id":"root-no-answer","offer_id":accept["offer_id"],"tool":"read_hr","arguments_sha256":accept["arguments_sha256"],"resolution":"accept"}),
        )
        .await;
        let read = start(
            "123e4567-e89b-12d3-a456-426614174128",
            serde_json::json!({"event":"tool_calls","root_id":"root-no-answer","calls":[{"call_id":"read-run","tool":"read_hr","arguments":{},"spawn":false}]}),
        )
        .await;
        start(
            "123e4567-e89b-12d3-a456-426614174129",
            serde_json::json!({"event":"tool_result","root_id":"root-no-answer","call_id":"read-run","dispatch_id":read["decision"]["calls"][0]["dispatch_id"],"outcome":{"status":"success","body":"Alice Chen"}}),
        )
        .await;
        let batch_id = "123e4567-e89b-12d3-a456-426614174130";
        let prepared = start(
            "123e4567-e89b-12d3-a456-426614174131",
            serde_json::json!({"event":"prepare_batch","root_id":"root-no-answer","batch_id":batch_id,"calls":[{"call_id":"send","tool":"send","arguments":{"body":"mail alice@corp.example today"},"spawn":false}]}),
        )
        .await;
        let offer = &prepared["decision"]["positions"][0]["offers"][0];
        let no_answer = start(
            "123e4567-e89b-12d3-a456-426614174132",
            serde_json::json!({"event":"resolve_batch_offer","root_id":"root-no-answer","batch_id":batch_id,"position":0,"offer_id":offer["offer_id"],"tool":"send","arguments_sha256":offer["arguments_sha256"],"resolution":"apply_sanitizer"}),
        )
        .await;
        assert_eq!(no_answer["decision"]["decision"], "refuse", "{no_answer}");
        assert_eq!(no_answer["decision"]["detail"], "runtime received no authority answer");
        assert!(runtime.proxy_batch_quarantined(batch_id).expect("quarantine reads"));
        let commit = start(
            "123e4567-e89b-12d3-a456-426614174133",
            serde_json::json!({"event":"commit_batch","root_id":"root-no-answer","batch_id":batch_id}),
        )
        .await;
        assert_eq!(
            commit["decision"]["detail"],
            "batch is quarantined and cannot be committed"
        );
    }

    #[tokio::test]
    async fn a_singleton_batch_preserves_its_acceptance_offer_and_retries_with_a_dispatch() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "taint"
                parameters = { type = "object", properties = { a = { type = "integer" } } }
                delta = { trust = "suspicious" }
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174060",
            serde_json::json!({"event":"session_start", "root_id":"root-batch-accept"}),
        )
        .await;
        let proposed = serde_json::json!({
            "event":"tool_calls",
            "root_id":"root-batch-accept",
            "calls":[{"call_id":"taint-1", "tool":"taint", "arguments":{"a":1}, "spawn":false}]
        });
        let denied = start("123e4567-e89b-12d3-a456-426614174061", proposed).await;
        assert_eq!(denied["decision"]["decision"], "deny_calls");
        assert_eq!(denied["decision"]["calls"][0]["decision"], "deny_call");
        assert_eq!(denied["decision"]["calls"][0]["call_id"], "taint-1");
        let offer = &denied["decision"]["calls"][0]["offers"][0];
        assert_eq!(offer["kind"], "acceptance");
        assert_eq!(offer["root_id"], "root-batch-accept");
        assert_eq!(offer["tool"], "taint");
        let accepted = start(
            "123e4567-e89b-12d3-a456-426614174062",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-batch-accept",
                "offer_id":offer["offer_id"],
                "tool":"taint",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"accept"
            }),
        )
        .await;
        assert_eq!(accepted["decision"]["resolution"], "accepted");
        let retried = start(
            "123e4567-e89b-12d3-a456-426614174063",
            serde_json::json!({
                "event":"tool_calls",
                "root_id":"root-batch-accept",
                "calls":[{"call_id":"taint-2", "tool":"taint", "arguments":{"a":1}, "spawn":false}]
            }),
        )
        .await;
        assert_eq!(retried["decision"]["decision"], "allow_calls");
        assert_eq!(retried["decision"]["calls"][0]["call_id"], "taint-2");
        assert!(retried["decision"]["calls"][0]["dispatch_id"].is_string());
    }

    #[tokio::test]
    async fn a_proxy_output_sanitizer_offer_invokes_its_registered_appa_implementation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "leak"
                parameters = { type = "object", properties = { q = { type = "string" } } }
                delta = { audience = ["insider"] }
                [[policy.sanitizer]]
                name = "scrub"
                on = ["tool_output"]
                [policy.sanitizer.permits]
                audience = { from = ["insider"], to = ["public"] }
                [policy.deployment]
                confined_results = ["leak"]
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
                [externals.sanitizers.scrub]
                builtin = "redact-email"
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174070",
            serde_json::json!({"event":"session_start", "root_id":"root-sanitize"}),
        )
        .await;
        let proposed = serde_json::json!({
            "event":"tool_calls",
            "root_id":"root-sanitize",
            "calls":[{"call_id":"leak-1", "tool":"leak", "arguments":{"q":"all"}, "spawn":false}]
        });
        let blocked = start("123e4567-e89b-12d3-a456-426614174071", proposed).await;
        assert_eq!(
            blocked["decision"]["calls"][0]["decision"], "deny_call",
            "blocked response: {blocked}"
        );
        let sanitizer = blocked["decision"]["calls"][0]["offers"]
            .as_array()
            .and_then(|offers| offers.iter().find(|offer| offer["kind"] == "sanitizer"))
            .expect("the confined call offers its named sanitizer");
        let bound = start(
            "123e4567-e89b-12d3-a456-426614174072",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-sanitize",
                "offer_id":sanitizer["offer_id"],
                "tool":"leak",
                "arguments_sha256":sanitizer["arguments_sha256"],
                "resolution":"apply_sanitizer"
            }),
        )
        .await;
        assert_eq!(bound["decision"]["resolution"], "bound");
        let released = start(
            "123e4567-e89b-12d3-a456-426614174073",
            serde_json::json!({
                "event":"tool_calls",
                "root_id":"root-sanitize",
                "calls":[{"call_id":"leak-2", "tool":"leak", "arguments":{"q":"all"}, "spawn":false}]
            }),
        )
        .await;
        let dispatch_id = &released["decision"]["calls"][0]["dispatch_id"];
        let staged = start(
            "123e4567-e89b-12d3-a456-426614174074",
            serde_json::json!({
                "event":"tool_result",
                "root_id":"root-sanitize",
                "call_id":"leak-2",
                "dispatch_id":dispatch_id,
                "outcome":{"status":"success", "body":"mail alice@corp.example today"}
            }),
        )
        .await;
        assert_eq!(staged["decision"]["decision"], "result_admitted");
        assert!(
            !staged["decision"]["presentation"]
                .as_str()
                .unwrap_or_default()
                .contains("alice@corp.example"),
            "the real sanitizer derivation, not the raw tool result, was admitted: {staged}"
        );
    }

    #[tokio::test]
    async fn child_batch_events_target_the_bound_child_lane_not_the_family_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "fetch"
                parameters = { type = "object", properties = {} }
                delta = {}
                [policy.deployment]
                context_control = true
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174080",
            serde_json::json!({"event":"session_start", "root_id":"root-child-lane"}),
        )
        .await;
        let spawned = start(
            "123e4567-e89b-12d3-a456-426614174081",
            serde_json::json!({
                "event":"tool_calls",
                "root_id":"root-child-lane",
                "calls":[{"call_id":"spawn", "tool":"fetch", "arguments":{}, "spawn":true}]
            }),
        )
        .await;
        let spawn_offer = &spawned["decision"]["calls"][0]["offers"][0];
        let declared = start(
            "123e4567-e89b-12d3-a456-426614174086",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-child-lane",
                "offer_id":spawn_offer["offer_id"],
                "tool":"fetch",
                "arguments_sha256":spawn_offer["arguments_sha256"],
                "resolution":"accept",
                "label":{"trust":"trusted"}
            }),
        )
        .await;
        assert_eq!(
            declared["decision"]["resolution"], "accepted",
            "spawn declaration: {declared}"
        );
        let spawned = start(
            "123e4567-e89b-12d3-a456-426614174087",
            serde_json::json!({
                "event":"tool_calls",
                "root_id":"root-child-lane",
                "calls":[{"call_id":"call-1", "tool":"fetch", "arguments":{}, "spawn":true}]
            }),
        )
        .await;
        let spawn_binding = &spawned["decision"]["calls"][0]["spawn_binding"];
        let parent_dispatch_id = &spawned["decision"]["calls"][0]["dispatch_id"];
        assert!(spawn_binding.is_string(), "spawn response: {spawned}");
        let child_start = start(
            "123e4567-e89b-12d3-a456-426614174082",
            serde_json::json!({
                "event":"child_start",
                "root_id":"root-child-lane",
                "child_id":"thread-7",
                "spawn_binding":spawn_binding
            }),
        )
        .await;
        assert_ne!(
            child_start["decision"]["decision"], "refuse",
            "child start: {child_start}"
        );
        let child_call = start(
            "123e4567-e89b-12d3-a456-426614174083",
            serde_json::json!({
                "event":"tool_calls",
                "root_id":"root-child-lane",
                "child_id":"thread-7",
                "calls":[{"call_id":"call-1", "tool":"fetch", "arguments":{}, "spawn":false}]
            }),
        )
        .await;
        let dispatch_id = &child_call["decision"]["calls"][0]["dispatch_id"];
        assert!(
            dispatch_id.as_str().is_some_and(|id| id.contains("thread-7")),
            "child call: {child_call}"
        );
        let wrong_lane = start(
            "123e4567-e89b-12d3-a456-426614174088",
            serde_json::json!({
                "event":"tool_result",
                "root_id":"root-child-lane",
                "child_id":"thread-7",
                "call_id":"call-1",
                "dispatch_id":parent_dispatch_id,
                "outcome":{"status":"success", "body":{"ok":true}}
            }),
        )
        .await;
        assert_eq!(
            wrong_lane["decision"]["decision"], "refuse",
            "a parent dispatch cannot settle the child call with the same id: {wrong_lane}"
        );
        let wrong_lane = start(
            "123e4567-e89b-12d3-a456-426614174084",
            serde_json::json!({
                "event":"tool_result",
                "root_id":"root-child-lane",
                "call_id":"call-1",
                "dispatch_id":dispatch_id,
                "outcome":{"status":"success", "body":{"ok":true}}
            }),
        )
        .await;
        assert_eq!(wrong_lane["decision"]["decision"], "refuse");
        let admitted = start(
            "123e4567-e89b-12d3-a456-426614174085",
            serde_json::json!({
                "event":"tool_result",
                "root_id":"root-child-lane",
                "child_id":"thread-7",
                "call_id":"call-1",
                "dispatch_id":dispatch_id,
                "outcome":{"status":"success", "body":{"ok":true}}
            }),
        )
        .await;
        assert_eq!(admitted["decision"]["decision"], "result_admitted");
        assert_eq!(admitted["decision"]["call_id"], "call-1");
    }

    #[test]
    fn only_real_hitl_authorities_are_typed_as_human_approval() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime(&directory);
        assert_eq!(typed_kind(&runtime, OfferKind::Accept), "acceptance");
        assert_eq!(
            typed_kind(
                &runtime,
                OfferKind::Authority {
                    names: vec!["not-hitl".to_string()]
                }
            ),
            "authority"
        );
        assert_eq!(
            typed_kind(
                &runtime,
                OfferKind::Sanitizer {
                    name: "redact".to_string()
                }
            ),
            "sanitizer"
        );
    }

    #[tokio::test]
    async fn a_typed_restriction_accepts_only_the_bound_original_call() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "taint"
                parameters = { type = "object", properties = { a = { type = "integer" } } }
                delta = { trust = "suspicious" }
                [[policy.tool]]
                name = "send"
                requires = { trust = "trusted" }
                delta = {}
                [policy.deployment]
                context_control = true
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
            "#,
        );
        let start = |id, event| decision(router(Arc::clone(&runtime), None), wrapped(id, event));
        start(
            "123e4567-e89b-12d3-a456-426614174010",
            serde_json::json!({"event":"session_start", "root_id":"root-typed"}),
        )
        .await;
        let taint = start(
            "123e4567-e89b-12d3-a456-426614174011",
            serde_json::json!({"event":"tool_call", "root_id":"root-typed", "tool":"taint", "arguments":{"a":1}, "spawn":false}),
        )
        .await;
        let taint_offer = &taint["decision"]["offers"][0];
        assert_eq!(taint_offer["kind"], "acceptance", "taint response: {taint}");
        let taint_accepted = start(
            "123e4567-e89b-12d3-a456-426614174012",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-typed",
                "offer_id":taint_offer["offer_id"],
                "tool":"taint",
                "arguments_sha256":taint_offer["arguments_sha256"],
                "resolution":"accept_restriction"
            }),
        )
        .await;
        assert_eq!(taint_accepted["decision"]["resolution"], "accepted");
        let wrong_tool = start(
            "123e4567-e89b-12d3-a456-426614174017",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-typed",
                "offer_id":taint_offer["offer_id"],
                "tool":"other",
                "arguments_sha256":taint_offer["arguments_sha256"],
                "resolution":"accept_restriction"
            }),
        )
        .await;
        assert_eq!(wrong_tool["decision"]["decision"], "refuse");
    }

    #[tokio::test]
    async fn hitl_offers_require_dedicated_approval_and_never_autoaccept() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = runtime_with_policy(
            &directory,
            r#"
                [policy]
                version = 2
                [[policy.tool]]
                name = "wire"
                requires = { attention = ["human"] }
                delta = {}
                [[policy.authority]]
                name = "oncall"
                [policy.authority.permits]
                attention = ["human"]
                [externals]
                timeout_ms = 1000
                max_body_bytes = 4096
                [externals.authorities.oncall]
                builtin = "hitl"
            "#,
        );
        let secret: Arc<str> = Arc::from("01234567890123456789012345678901");
        let start = |id, event| {
            decision(
                router(Arc::clone(&runtime), Some(Arc::clone(&secret))),
                wrapped(id, event),
            )
        };
        start(
            "123e4567-e89b-12d3-a456-426614174020",
            serde_json::json!({"event":"session_start", "root_id":"root-hitl"}),
        )
        .await;
        let denied = start(
            "123e4567-e89b-12d3-a456-426614174021",
            serde_json::json!({"event":"tool_call", "root_id":"root-hitl", "tool":"wire", "arguments":{"amount":1}, "spawn":false}),
        )
        .await;
        let offer = &denied["decision"]["offers"][0];
        assert_eq!(offer["kind"], "human_approval");
        let autoaccept = start(
            "123e4567-e89b-12d3-a456-426614174022",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"accept_restriction"
            }),
        )
        .await;
        assert_eq!(autoaccept["decision"]["decision"], "refuse");
        let unsigned = start(
            "123e4567-e89b-12d3-a456-426614174023",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"approve"
            }),
        )
        .await;
        assert_eq!(unsigned["decision"]["decision"], "refuse");
        let wrong_tool = start(
            "123e4567-e89b-12d3-a456-426614174032",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":offer["offer_id"],
                "tool":"other",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"approve",
                "approval": approval(&secret, "root-hitl", offer, "other", "approve")
            }),
        )
        .await;
        assert_eq!(wrong_tool["decision"]["decision"], "refuse");
        let wrong_offer = start(
            "123e4567-e89b-12d3-a456-426614174033",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":"other-offer",
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"approve",
                "approval": approval(&secret, "root-hitl", offer, "wire", "approve")
            }),
        )
        .await;
        assert_eq!(wrong_offer["decision"]["decision"], "refuse");
        let wrong_action = start(
            "123e4567-e89b-12d3-a456-426614174034",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"deny",
                "approval": approval(&secret, "root-hitl", offer, "wire", "approve")
            }),
        )
        .await;
        assert_eq!(wrong_action["decision"]["decision"], "refuse");
        let mut forged = approval(&secret, "root-hitl", offer, "wire", "approve");
        forged["signature"] = serde_json::Value::String("00".repeat(32));
        let forged = start(
            "123e4567-e89b-12d3-a456-426614174027",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"approve",
                "approval": forged
            }),
        )
        .await;
        assert_eq!(forged["decision"]["decision"], "refuse");
        let mut expired = approval(&secret, "root-hitl", offer, "wire", "approve");
        expired["expires_at"] = serde_json::json!(0);
        let expired = start(
            "123e4567-e89b-12d3-a456-426614174028",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"approve",
                "approval": expired
            }),
        )
        .await;
        assert_eq!(expired["decision"]["decision"], "refuse");
        let mut wrong_root = approval(&secret, "root-hitl", offer, "wire", "approve");
        wrong_root["root_id"] = serde_json::json!("other-root");
        let wrong_root = start(
            "123e4567-e89b-12d3-a456-426614174029",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl",
                "offer_id":offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"approve",
                "approval": wrong_root
            }),
        )
        .await;
        assert_eq!(wrong_root["decision"]["decision"], "refuse");
        let approval_event = serde_json::json!({
            "event":"resolve_offer",
            "root_id":"root-hitl",
            "offer_id":offer["offer_id"],
            "tool":"wire",
            "arguments_sha256":offer["arguments_sha256"],
            "resolution":"approve",
            "approval": approval(&secret, "root-hitl", offer, "wire", "approve")
        });
        let approved = start("123e4567-e89b-12d3-a456-426614174030", approval_event.clone()).await;
        assert_eq!(
            approved["decision"]["resolution"], "approved",
            "approval response: {approved}"
        );
        let replayed = start("123e4567-e89b-12d3-a456-426614174030", approval_event.clone()).await;
        assert_eq!(replayed, approved, "the completed grant event replays from its receipt");
        let duplicate = start("123e4567-e89b-12d3-a456-426614174031", approval_event).await;
        assert_eq!(duplicate["decision"]["decision"], "refuse");
        let approved_batch = start(
            "123e4567-e89b-12d3-a456-426614174035",
            serde_json::json!({
                "event":"tool_calls",
                "root_id":"root-hitl",
                "calls":[{"call_id":"wire-approved", "tool":"wire", "arguments":{"amount":1}, "spawn":false}]
            }),
        )
        .await;
        assert_eq!(approved_batch["decision"]["decision"], "allow_calls");
        assert_eq!(approved_batch["decision"]["calls"][0]["call_id"], "wire-approved");
        assert!(approved_batch["decision"]["calls"][0]["dispatch_id"].is_string());

        start(
            "123e4567-e89b-12d3-a456-426614174024",
            serde_json::json!({"event":"session_start", "root_id":"root-hitl-deny"}),
        )
        .await;
        let denied = start(
            "123e4567-e89b-12d3-a456-426614174025",
            serde_json::json!({"event":"tool_call", "root_id":"root-hitl-deny", "tool":"wire", "arguments":{"amount":2}, "spawn":false}),
        )
        .await;
        let denied_offer = &denied["decision"]["offers"][0];
        let human_denied = start(
            "123e4567-e89b-12d3-a456-426614174026",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-hitl-deny",
                "offer_id":denied_offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":denied_offer["arguments_sha256"],
                "resolution":"deny",
                "approval": approval(&secret, "root-hitl-deny", denied_offer, "wire", "deny")
            }),
        )
        .await;
        assert_eq!(human_denied["decision"]["resolution"], "denied");
    }

    #[tokio::test]
    async fn an_offer_issued_before_a_reload_cannot_be_reinterpreted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let policy = r#"
            [policy]
            version = 2
            [[policy.tool]]
            name = "wire"
            requires = { attention = ["human"] }
            delta = {}
            [[policy.authority]]
            name = "oncall"
            [policy.authority.permits]
            attention = ["human"]
            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
            [externals.authorities.oncall]
            url = "http://127.0.0.1:1/authority"
        "#;
        let runtime = runtime_with_policy(&directory, policy);
        let secret: Arc<str> = Arc::from("01234567890123456789012345678901");
        let start = |id, event| {
            decision(
                router(Arc::clone(&runtime), Some(Arc::clone(&secret))),
                wrapped(id, event),
            )
        };
        start(
            "123e4567-e89b-12d3-a456-426614174040",
            serde_json::json!({"event":"session_start", "root_id":"root-reload"}),
        )
        .await;
        let denied = start(
            "123e4567-e89b-12d3-a456-426614174041",
            serde_json::json!({"event":"tool_call", "root_id":"root-reload", "tool":"wire", "arguments":{"amount":1}, "spawn":false}),
        )
        .await;
        let offer = &denied["decision"]["offers"][0];
        let reloaded_path = directory.path().join("reloaded.toml");
        let reloaded_policy = policy.replace("url = \"http://127.0.0.1:1/authority\"", "builtin = \"hitl\"");
        std::fs::write(
            &reloaded_path,
            format!("{reloaded_policy}\n[[policy.tool]]\nname = \"unrelated\"\ndelta = {{}}\n"),
        )
        .expect("reloaded policy writes");
        runtime
            .reload(Config::load(&reloaded_path).expect("reloaded policy parses"))
            .expect("runtime reloads");
        let result = start(
            "123e4567-e89b-12d3-a456-426614174042",
            serde_json::json!({
                "event":"resolve_offer",
                "root_id":"root-reload",
                "offer_id":offer["offer_id"],
                "tool":"wire",
                "arguments_sha256":offer["arguments_sha256"],
                "resolution":"approve",
                "approval": approval(&secret, "root-reload", offer, "wire", "approve")
            }),
        )
        .await;
        assert_eq!(result["decision"]["decision"], "refuse");
        assert_eq!(
            result["decision"]["detail"],
            "offer was issued by a different deployment"
        );
    }
}

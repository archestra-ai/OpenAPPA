//! The HTTP face: four JSON endpoints and one SSE stream per turn.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use appa_example_agent::{Agent, Limits, Outcome, Recorded, ToolShim};
use axum::Json;
use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::events::{AuditReader, WireEvent, record_event};
use crate::lint::check_policy;
use crate::session::{CreateError, DemoSession, Sessions};
use crate::systems::System;

const MAX_POLICY_BYTES: usize = 32 * 1024;
const MAX_MESSAGE_BYTES: usize = 2 * 1024;

/// How long one inference round may take. The turn's wall clock is unbounded
/// on purpose, but a single provider call that never answers is a hang, not a
/// visitor thinking.
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(120);

const PUMP_INTERVAL: Duration = Duration::from_millis(150);

const EFFECTIVELY_UNBOUNDED: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Per-turn ceilings. Wall-clock is deliberately unbounded so a parked
/// approval waits for the visitor's click, not a timer: a runaway loop costs
/// a visitor 16 rounds, a session the turn cap, and an abandoned tab is
/// reclaimed by the pump cancelling the turn when the stream drops. A zero
/// fork budget keeps the playground single-trajectory: no spawn tool is
/// installed, so the story stays one visible thread.
fn demo_limits() -> Limits {
    Limits {
        max_inference_rounds: 16,
        max_tool_calls: 32,
        run_deadline: EFFECTIVELY_UNBOUNDED,
        max_forks: 0,
        max_fork_depth: 0,
    }
}

#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<Sessions>,
    pub origins: Arc<Vec<String>>,
    pub openrouter_key: Option<Arc<String>>,
    pub max_turns: u32,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/preset", get(preset))
        .route("/policy/check", post(policy_check))
        .route("/session", post(session_create))
        .route("/session/{id}", delete(session_delete))
        .route("/session/{id}/message", post(session_message))
        .route("/session/{id}/approval/{approval}", post(session_approval))
        .layer(middleware::from_fn_with_state(state.clone(), cors))
        .with_state(state)
}

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("policy too large (max {MAX_POLICY_BYTES} bytes)")]
    PolicyTooLarge,
    #[error("message too large (max {MAX_MESSAGE_BYTES} bytes)")]
    MessageTooLarge,
    #[error("no session {0:?} (it may have expired)")]
    UnknownSession(String),
    #[error("a turn is already streaming on this session")]
    TurnInFlight,
    #[error(transparent)]
    UnknownApproval(#[from] crate::approvals::UnknownApproval),
    #[error("the service has no OpenRouter key configured")]
    NoKey,
    #[error("this session has used its {0} turns; start a new chat")]
    TurnLimit(u32),
    #[error("unknown system {:?}; valid systems: {}", .0, System::ALL.map(System::id).join(", "))]
    UnknownSystem(String),
    #[error(transparent)]
    Create(#[from] CreateError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::PolicyTooLarge | ApiError::MessageTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::UnknownSession(_) => StatusCode::NOT_FOUND,
            ApiError::UnknownApproval(_) => StatusCode::NOT_FOUND,
            ApiError::TurnInFlight => StatusCode::CONFLICT,
            ApiError::NoKey => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::TurnLimit(_) => StatusCode::TOO_MANY_REQUESTS,
            ApiError::UnknownSystem(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Create(CreateError::AtCapacity) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Create(CreateError::World(_)) | ApiError::Create(CreateError::Deployment(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ApiError::Create(_) => StatusCode::UNPROCESSABLE_ENTITY,
        };
        (status, Json(serde_json::json!({ "error": self.to_string() }))).into_response()
    }
}

// ---- /preset ---------------------------------------------------------------

/// What a fresh playground starts from. The client holds no copy of any of it:
/// the shipped policy and the world's systems are facts about this build, and a
/// second copy in the website would be a second answer nothing keeps honest.
#[derive(Serialize)]
struct PresetResponse {
    policy: &'static str,
    systems: Vec<PresetSystem>,
}

#[derive(Serialize)]
struct PresetSystem {
    id: &'static str,
    tools: &'static [&'static str],
}

async fn preset() -> Json<PresetResponse> {
    Json(PresetResponse {
        policy: include_str!("../policies/default.toml"),
        systems: System::ALL
            .into_iter()
            .map(|system| PresetSystem {
                id: system.id(),
                tools: system.tools(),
            })
            .collect(),
    })
}

// ---- /policy/check ---------------------------------------------------------

#[derive(Deserialize)]
struct CheckRequest {
    policy: String,
    #[serde(default)]
    systems: Vec<String>,
}

#[derive(Serialize)]
struct CheckResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unconstrained: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ignored: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    boundary: Option<Boundary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Boundary {
    trust: String,
    audience: String,
}

async fn policy_check(Json(request): Json<CheckRequest>) -> Result<Json<CheckResponse>, ApiError> {
    if request.policy.len() > MAX_POLICY_BYTES {
        return Err(ApiError::PolicyTooLarge);
    }
    let enabled = parse_systems(&request.systems)?;
    Ok(Json(match check_policy(&request.policy, &enabled) {
        Ok(checked) => {
            let boundary = crate::events::LabelText::of(
                checked.config.boundary_label(),
                &checked.config.registry_config().trust_chain,
            );
            CheckResponse {
                ok: true,
                tools: Some(checked.tool_count),
                unconstrained: checked.defaulted,
                ignored: checked.dropped,
                boundary: Some(Boundary {
                    trust: boundary.trust,
                    audience: boundary.audience,
                }),
                error: None,
            }
        }
        Err(error) => CheckResponse {
            ok: false,
            tools: None,
            unconstrained: Vec::new(),
            ignored: Vec::new(),
            boundary: None,
            error: Some(error.to_string()),
        },
    }))
}

fn parse_systems(names: &[String]) -> Result<BTreeSet<System>, ApiError> {
    names
        .iter()
        .map(|name| System::parse(name).map_err(|_| ApiError::UnknownSystem(name.clone())))
        .collect()
}

// ---- /session --------------------------------------------------------------

#[derive(Deserialize)]
struct CreateRequest {
    policy: String,
    #[serde(default)]
    systems: Vec<String>,
    model: String,
}

#[derive(Serialize)]
struct CreateResponse {
    session: String,
    tools: usize,
    trust: String,
    audience: String,
}

async fn session_create(
    State(state): State<AppState>,
    Json(request): Json<CreateRequest>,
) -> Result<Json<CreateResponse>, ApiError> {
    if request.policy.len() > MAX_POLICY_BYTES {
        return Err(ApiError::PolicyTooLarge);
    }
    let enabled = parse_systems(&request.systems)?;
    let session = state.sessions.create(&request.policy, &enabled, &request.model).await?;
    Ok(Json(CreateResponse {
        session: session.id.clone(),
        tools: session.tool_count,
        trust: session.boundary.trust.clone(),
        audience: session.boundary.audience.clone(),
    }))
}

async fn session_delete(State(state): State<AppState>, Path(id): Path<String>) -> StatusCode {
    match state.sessions.remove(&id) {
        Some(_) => StatusCode::NO_CONTENT,
        None => StatusCode::NOT_FOUND,
    }
}

// ---- /session/{id}/approval/{approval} -------------------------------------

#[derive(Deserialize)]
struct ApprovalRequest {
    approve: bool,
}

async fn session_approval(
    State(state): State<AppState>,
    Path((id, approval)): Path<(String, String)>,
    Json(request): Json<ApprovalRequest>,
) -> Result<StatusCode, ApiError> {
    let session = state
        .sessions
        .get(&id)
        .ok_or_else(|| ApiError::UnknownSession(id.clone()))?;
    session.approvals.resolve(&approval, request.approve)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- /session/{id}/message -------------------------------------------------

#[derive(Deserialize)]
struct MessageRequest {
    text: String,
}

async fn session_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<MessageRequest>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, axum::Error>>>, ApiError> {
    if request.text.len() > MAX_MESSAGE_BYTES {
        return Err(ApiError::MessageTooLarge);
    }
    let key = (*state.openrouter_key.as_deref().ok_or(ApiError::NoKey)?).clone();
    let session = state
        .sessions
        .get(&id)
        .ok_or_else(|| ApiError::UnknownSession(id.clone()))?;

    let mut transcript = Arc::clone(&session.turn_gate)
        .try_lock_owned()
        .map_err(|_| ApiError::TurnInFlight)?;

    if session.turns_spent() >= state.max_turns {
        return Err(ApiError::TurnLimit(state.max_turns));
    }
    session.spend_turn();

    let (tx, rx) = tokio::sync::mpsc::channel::<WireEvent>(64);
    let (records_tx, records) = tokio::sync::mpsc::channel::<Recorded>(64);
    let cancel = CancellationToken::new();
    session.derivations.arm(key.clone());
    let agent = Agent::new(
        Arc::clone(&session.runtime),
        crate::session::provider(&session.inference, &session.model, key.clone(), INFERENCE_TIMEOUT),
        ToolShim::new(session.tools_url.clone()),
        session.catalogue.clone(),
    )
    .with_head(crate::session::head())
    .with_limits(demo_limits())
    .with_observer(records_tx);

    let mut turn = tokio::spawn({
        let session = session.clone();
        let cancel = cancel.clone();
        let text = request.text;
        async move {
            let outcome = agent
                .turn(session.trajectory.clone(), &mut transcript, text, cancel)
                .await;
            drop(transcript);
            outcome
        }
    });

    tokio::spawn(async move {
        let mut pump = Pump {
            session: &session,
            tx: &tx,
            records,
            audit_seen: 0,
            audit: AuditReader::default(),
            last_label: None,
        };
        let mut ticker = tokio::time::interval(PUMP_INTERVAL);

        let finished = loop {
            tokio::select! {
                result = &mut turn => break Some(result),
                _ = ticker.tick() => {
                    session.touch();
                    if pump.drain().await.is_err() {
                        pump.records.close();
                        session.approvals.abandon();
                        cancel.cancel();
                        break None;
                    }
                }
            }
        };

        let outcome = match finished {
            Some(outcome) => outcome,
            None => turn.await,
        };
        // The turn is over, so everything it recorded is already buffered.
        pump.records.close();
        let _ = pump.drain().await;
        let final_event = match outcome {
            Ok(Outcome::Answer(text)) => WireEvent::Answer { text },
            Ok(Outcome::Stopped(reason)) => WireEvent::Stopped {
                text: reason.to_string(),
            },
            Err(join) => WireEvent::Error {
                message: format!("the turn task failed: {join}"),
            },
        };
        let _ = tx.send(final_event).await;
        session.derivations.disarm();
        session.touch();
    });

    let stream = ReceiverStream::new(rx).map(|event| Event::default().json_data(&event));
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}

struct Pump<'a> {
    session: &'a DemoSession,
    tx: &'a tokio::sync::mpsc::Sender<WireEvent>,
    records: tokio::sync::mpsc::Receiver<Recorded>,
    audit_seen: usize,
    audit: AuditReader,
    last_label: Option<(String, String)>,
}

struct ClientGone;

impl Pump<'_> {
    async fn drain(&mut self) -> Result<(), ClientGone> {
        // A hangup is noticed on its own, not only when the next event fails
        // to send. A turn parked on a human ruling produces nothing for as
        // long as the visitor takes, and its wall clock is deliberately
        // unbounded — so without this a closed tab would hold the gate, and
        // the pump's own `touch` would keep the session out of the reaper
        // indefinitely.
        if self.tx.is_closed() {
            return Err(ClientGone);
        }
        let mut events = self.session.approvals.drain_events();
        while let Ok(recorded) = self.records.try_recv() {
            events.extend(record_event(&recorded));
        }
        if let Some(entries) = self.session.runtime.audit(&self.session.trajectory) {
            for entry in entries.iter().skip(self.audit_seen) {
                events.extend(self.audit.event(entry));
            }
            self.audit_seen = entries.len();
        }

        for event in events {
            if let WireEvent::Label { trust, audience, .. } = &event {
                let label = (trust.clone(), audience.clone());
                if self.last_label.as_ref() == Some(&label) {
                    continue;
                }
                self.last_label = Some(label);
            }
            if self.tx.send(event).await.is_err() {
                return Err(ClientGone);
            }
        }
        Ok(())
    }
}

// ---- CORS ------------------------------------------------------------------

async fn cors(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let allowed = origin
        .as_deref()
        .filter(|origin| state.origins.iter().any(|allowed| allowed == origin))
        .map(str::to_string);

    if request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Some(origin) = &allowed {
            append_cors(response.headers_mut(), origin);
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET,POST,DELETE"),
            );
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("content-type"),
            );
            response
                .headers_mut()
                .insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
        }
        return response;
    }

    let mut response = next.run(request).await;
    if let Some(origin) = &allowed {
        append_cors(response.headers_mut(), origin);
    }
    response
}

fn append_cors(headers: &mut HeaderMap, origin: &str) {
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        headers.append(header::VARY, HeaderValue::from_static("origin"));
    }
}

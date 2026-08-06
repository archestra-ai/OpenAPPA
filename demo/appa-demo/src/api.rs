//! The HTTP face: four JSON endpoints and one SSE stream per turn.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use appa_agent::{Agent, OpenAiCompatible, Outcome};
use appa_runtime::Limits;
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

use crate::events::{WireEvent, current_label, fact_event};
use crate::lint::check_policy;
use crate::session::{CreateError, DemoSession, Sessions};
use crate::systems::System;

const MAX_POLICY_BYTES: usize = 32 * 1024;
const MAX_MESSAGE_BYTES: usize = 2 * 1024;

const PUMP_INTERVAL: Duration = Duration::from_millis(150);

fn demo_limits() -> Limits {
    Limits {
        max_inference_rounds: 16,
        run_deadline: Duration::from_secs(90),
        max_forks: 0,
        max_fork_depth: 1,
        ..Limits::default()
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
    #[error("reading the session log: {0}")]
    Store(String),
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
            ApiError::Create(CreateError::World(_)) | ApiError::Create(CreateError::Mediator(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ApiError::Create(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
            let label = checked.config.boundary_label();
            let chain = &checked.config.registry_config().trust_chain;
            CheckResponse {
                ok: true,
                tools: Some(checked.tool_count),
                unconstrained: checked.defaulted,
                ignored: checked.dropped,
                boundary: Some(Boundary {
                    trust: crate::events::trust_text(label, chain),
                    audience: crate::events::audience_text(label),
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
    let boundary = session.mediator.config().boundary_label().clone();
    Ok(Json(CreateResponse {
        session: session.id.clone(),
        tools: session.tool_count,
        trust: crate::events::trust_text(&boundary, &session.chain),
        audience: crate::events::audience_text(&boundary),
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

    // One open turn per session: refuse instead of queueing behind the lease.
    let turn_guard = Arc::clone(&session.turn_gate)
        .try_lock_owned()
        .map_err(|_| ApiError::TurnInFlight)?;

    if session.turns_spent() >= state.max_turns {
        return Err(ApiError::TurnLimit(state.max_turns));
    }
    session.spend_turn();

    let (baseline, _) = session
        .mediator
        .snapshot(&session.tenant, &session.trajectory)
        .map_err(|error| ApiError::Store(error.to_string()))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<WireEvent>(64);
    let cancel = CancellationToken::new();
    session.derivations.arm(key.clone());
    let agent = Agent::new(
        session.mediator.clone(),
        OpenAiCompatible::openrouter(session.model.clone(), key),
        demo_limits(),
    );

    let mut turn = tokio::spawn({
        let session = session.clone();
        let cancel = cancel.clone();
        let text = request.text;
        async move {
            agent
                .run_existing(session.tenant.clone(), session.trajectory.clone(), text, cancel)
                .await
        }
    });

    tokio::spawn(async move {
        let _guard = turn_guard; // held until the stream ends

        let mut pump = Pump {
            session: &session,
            tx: &tx,
            seen: baseline.len(),
            last_label: None,
        };
        let mut ticker = tokio::time::interval(PUMP_INTERVAL);

        let finished = loop {
            tokio::select! {
                result = &mut turn => break Some(result),
                _ = ticker.tick() => {
                    if pump.drain().await.is_err() {
                        cancel.cancel();
                        break None;
                    }
                }
            }
        };

        let result = match finished {
            Some(result) => result,
            None => turn.await,
        };
        let _ = pump.drain().await;
        let final_event = match result {
            Ok(Ok(Outcome::Final(text))) => WireEvent::Answer { text },
            Ok(Ok(Outcome::PolicyStop(text))) => WireEvent::Stopped { text },
            Ok(Ok(Outcome::ChildFinished)) => WireEvent::Stopped {
                text: "the run ended inside a child session".to_string(),
            },
            Ok(Err(error)) => WireEvent::Error {
                message: error.to_string(),
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
    seen: usize,
    last_label: Option<(String, String)>,
}

struct ClientGone;

impl Pump<'_> {
    async fn drain(&mut self) -> Result<(), ClientGone> {
        let Ok((facts, revision)) = self
            .session
            .mediator
            .snapshot(&self.session.tenant, &self.session.trajectory)
        else {
            return Ok(()); // transient store refusal; retry next tick
        };
        let mut events = self.session.approvals.drain_events();
        for fact in &facts[self.seen.min(facts.len())..] {
            events.extend(fact_event(fact, &self.session.chain));
        }
        self.seen = facts.len();

        let label = current_label(&facts, revision, &self.session.trajectory, &self.session.chain);
        if label.0 != "?" && self.last_label.as_ref() != Some(&label) {
            events.push(WireEvent::Label {
                trajectory: self.session.trajectory.as_str().to_string(),
                trust: label.0.clone(),
                audience: label.1.clone(),
            });
            self.last_label = Some(label);
        }

        for event in events {
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

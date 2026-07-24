//! The north HTTP face: `POST /v1/chat/completions`, OpenAI-shaped and non-streaming, wired to the
//! RP1 admission profile and the canonical agent loop.
//!
//! Session identity is a trusted-host input: `X-APPA-Tenant` names the authenticated caller, and a
//! session/parent id is bound to that tenant — a foreign or unknown id is rejected, never namespaced or
//! minted. Absent `X-APPA-Session`, a root session is minted (its id returned in the response header);
//! `X-APPA-Parent-Session` forks a server-seeded child (RP6). The request's history is never trusted —
//! admission takes only the one new user turn and the drive rebuilds context from the log (CC2).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use tokio_util::sync::CancellationToken;

use appa_runtime::SessionForkError;
use appa_runtime::TrajectoryId;
use appa_runtime::store::{StoreError, TenantId};
use appa_runtime::wire::{ChatCompletionRequest, ChatCompletionResponse, WireMessage};

use crate::admission::admit_north_request;
use crate::drive::{TurnOutcome, drive_turn};
use crate::runtime::Runtime;

const SESSION_HEADER: &str = "x-appa-session";
const PARENT_HEADER: &str = "x-appa-parent-session";
const TENANT_HEADER: &str = "x-appa-tenant";
/// The tenant a request with no `X-APPA-Tenant` is attributed to (single-tenant deployments). Real
/// caller authentication is a follow-up; the session-ownership binding is already enforced per tenant.
const DEFAULT_TENANT: &str = "default";

/// Why the north server could not bind or serve.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("bind/serve io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Runtime {
    /// Serve the north face until the process ends. Consumes the runtime into shared state.
    pub async fn serve(self, addr: SocketAddr) -> Result<(), ServeError> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, self.router()).await?;
        Ok(())
    }

    /// The router over shared runtime state — exposed so tests can bind an ephemeral port themselves.
    pub fn router(self) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(handle_completions))
            .with_state(Arc::new(self))
    }
}

/// How session resolution failed, mapped to a status.
enum SessionError {
    Unknown,
    Foreign,
    Fork(String),
    DepthLimit,
    Store,
}

async fn handle_completions(
    State(rt): State<Arc<Runtime>>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let tenant = TenantId::new(header(&headers, TENANT_HEADER).unwrap_or(DEFAULT_TENANT));

    let user_turn = match admit_north_request(&request) {
        Ok(turn) => turn,
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };

    let session = match resolve_session(&rt, &tenant, &headers).await {
        Ok(session) => session,
        Err(error) => return session_error_response(error),
    };
    let is_child = match rt.mediator().is_child(&tenant, &session) {
        Ok(is_child) => is_child,
        Err(error) => return session_error_response(session_error(error)),
    };

    // The turn runs as its own task, so a dropped request future (client disconnect) can never
    // abort the drive mid-await and orphan an open dispatch (RP2). The drop guard arms the turn's
    // cancellation instead: on disconnect the drive transitions to its serialized cancelled
    // terminal and completes on its own; on a normal reply the cancel fires after the turn has
    // already ended, a no-op.
    let token = CancellationToken::new();
    let _disconnect_guard = token.clone().drop_guard();
    let turn = {
        let rt = rt.clone();
        let tenant = tenant.clone();
        let session = session.clone();
        tokio::spawn(async move { drive_turn(&rt, &tenant, &session, is_child, user_turn, token).await })
    };
    match turn.await {
        Ok(Ok(outcome)) => completion_response(&session, is_child, outcome),
        // A returned child refusing a new turn is the caller's lifecycle error, not a fault.
        Ok(Err(crate::drive::DriveError::SessionReturned)) => StatusCode::CONFLICT.into_response(),
        Ok(Err(_)) | Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Resolve the session a request drives: an existing owned session, a freshly forked child, or a newly
/// minted root — enforcing tenant ownership on every existing id.
async fn resolve_session(rt: &Runtime, tenant: &TenantId, headers: &HeaderMap) -> Result<TrajectoryId, SessionError> {
    if let Some(id) = header(headers, SESSION_HEADER) {
        let session = TrajectoryId::new(id);
        rt.mediator().is_child(tenant, &session).map_err(session_error)?;
        return Ok(session);
    }
    if let Some(parent_id) = header(headers, PARENT_HEADER) {
        let parent = TrajectoryId::new(parent_id);
        let child = rt
            .mediator()
            .fork_session_serialized(tenant, &parent, rt.max_fork_depth())
            .await
            .map_err(session_fork_error)?;
        return Ok(child);
    }
    Ok(rt.mediator().create_session(tenant.clone()))
}

fn session_error(err: StoreError) -> SessionError {
    match err {
        StoreError::UnknownSession(_) => SessionError::Unknown,
        StoreError::ForeignSession { .. } => SessionError::Foreign,
        StoreError::Seed(branch) => SessionError::Fork(branch.to_string()),
        StoreError::Stale { .. } => SessionError::Store,
    }
}

fn session_fork_error(error: SessionForkError) -> SessionError {
    match error {
        SessionForkError::Store(error) => session_error(error),
        SessionForkError::DepthLimit { .. } => SessionError::DepthLimit,
    }
}

fn session_error_response(error: SessionError) -> Response {
    match error {
        // Keep unknown and foreign indistinguishable to avoid a cross-tenant existence oracle.
        SessionError::Unknown | SessionError::Foreign => {
            (StatusCode::NOT_FOUND, "no such session for this caller").into_response()
        }
        SessionError::Fork(error) => (StatusCode::BAD_REQUEST, error).into_response(),
        SessionError::DepthLimit => (StatusCode::BAD_REQUEST, "fork depth limit reached").into_response(),
        SessionError::Store => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The fixed reply a child session returns north — its free model text is **never** propagated (RP6):
/// a child that read admitted sensitive content could otherwise quote it back, bypassing the labeled
/// `submit_result` return path. Deliberately neutral: it makes no claim about whether a result was
/// submitted (that is visible only in the parent's family log), so it can never report a false success.
const CHILD_REPLY: &str = "the child session finished";

fn completion_response(session: &TrajectoryId, is_child: bool, outcome: TurnOutcome) -> Response {
    let text = if is_child {
        CHILD_REPLY.to_string()
    } else {
        match outcome {
            TurnOutcome::Final(text) | TurnOutcome::PolicyStop(text) => text,
        }
    };
    let body = ChatCompletionResponse::single(
        format!("appa-{}", session.as_str()),
        WireMessage::assistant(text),
        "stop",
    );
    let mut response = Json(body).into_response();
    if let Ok(value) = HeaderValue::from_str(session.as_str()) {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|s| !s.is_empty())
}

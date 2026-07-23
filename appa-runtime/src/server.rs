//! The north HTTP face: `POST /v1/chat/completions`, OpenAI-shaped and non-streaming, wired to the
//! RP1 admission profile and the RP2 drive.
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

use appa_engine::branch::BranchError;
use appa_engine::projection::Projection;
use appa_engine::value::TrajectoryId;

use crate::admission::admit_north_request;
use crate::drive::{TurnOutcome, drive_turn};
use crate::runtime::Runtime;
use crate::store::{StoreError, TenantId};
use crate::wire::{ChatCompletionRequest, ChatCompletionResponse, WireMessage};

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
    Fork(BranchError),
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

    let (session, is_child) = match resolve_session(&rt, &tenant, &headers) {
        Ok(resolved) => resolved,
        // Unknown and foreign are reported identically, so an outsider cannot use the status to probe
        // which session ids exist across tenants (the existence-oracle the distinction would leak).
        Err(SessionError::Unknown | SessionError::Foreign) => {
            return (StatusCode::NOT_FOUND, "no such session for this caller").into_response();
        }
        Err(SessionError::Fork(err)) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
        Err(SessionError::Store) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
        Ok(Err(_)) | Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Resolve the session a request drives: an existing owned session, a freshly forked child, or a newly
/// minted root — enforcing tenant ownership on every existing id.
fn resolve_session(
    rt: &Arc<Runtime>,
    tenant: &TenantId,
    headers: &HeaderMap,
) -> Result<(TrajectoryId, bool), SessionError> {
    if let Some(id) = header(headers, SESSION_HEADER) {
        let session = TrajectoryId::new(id);
        let parent = rt.store().parent_of(tenant, &session).map_err(session_error)?;
        return Ok((session, parent.is_some()));
    }
    if let Some(parent_id) = header(headers, PARENT_HEADER) {
        let parent = TrajectoryId::new(parent_id);
        let (child, _) = rt
            .store()
            .fork(tenant, &parent, |child, facts, revision| {
                let projection = Projection::build(facts, revision);
                rt.engine()
                    .seed_child(&projection.view(&parent), child, rt.config().child_return_policy())
            })
            .map_err(session_error)?;
        return Ok((child, true));
    }
    Ok((rt.store().create_session(tenant.clone()), false))
}

fn session_error(err: StoreError) -> SessionError {
    match err {
        StoreError::UnknownSession(_) => SessionError::Unknown,
        StoreError::ForeignSession { .. } => SessionError::Foreign,
        StoreError::Seed(branch) => SessionError::Fork(branch),
        StoreError::Stale { .. } => SessionError::Store,
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

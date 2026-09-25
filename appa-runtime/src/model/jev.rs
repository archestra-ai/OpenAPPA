//! The `jev` annotator builtin: TypeSafe's Jev classifier, asked in process.
//!
//! One consult asks Jev four questions about the complete call and maps its labels onto the
//! annotation: `delta.audience`, `delta.trust`, `requires.audience` and `requires.trust`. It
//! never answers an effect, a `history` entry or an attention mark.
//!
//! The call's name, description and arguments leave for the TypeSafe API, after the
//! best-effort redaction every model provider gets ([`crate::secrets::redact_args`]). Each
//! label is Jev's likeliest option, and a tie goes to the safer one: the narrower result
//! audience, the lower trust rank, the wider required audience. A label the mandate does not
//! admit, a consult that is not a complete call, and every provider failure are no answer.
//!
//! A new connection to the API lands on a slow backend often enough to matter, and stays
//! slow for its whole life. So an attempt with no answer after its hedge delay is hedged by
//! a second attempt on a new connection, and a client whose connection answered that slowly
//! is dropped from the pool rather than reused. Opening a connection costs a few round
//! trips, so an attempt that opens one waits [`JevTiming::cold_hedge_delay`] instead of
//! [`JevTiming::hedge_delay`].

mod questions;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;
use serde::Serialize;
use tokio::time::Instant;

use super::MAX_ATTEMPTS;
use crate::config::{Endpoint, EndpointHost, JevProfile, Token};
use crate::consult::{Consult, ConsultBody};
use crate::external::{ConsultGates, ModelGates, NoAnswerReason, acquire_within};
use crate::label_guide::{Labels, RequiredAudience, ResultAudience, ResultTrust, annotation};
use appa_policy::AnnotatorBuiltin;
use questions::Questions;

const MODEL: &str = "jev-1.13.0";
const DIAGNOSTICS_VERSION: u32 = 1;

/// At or above this probability the call requires the highest trust rank.
const REQUIRES_TRUSTED_CUTOFF: f64 = 0.5;

/// A consult larger than this, as a command annotator would read it, is no answer.
const MAX_CONSULT_BYTES: usize = 64 * 1024;
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);

/// How one consult spends its budget.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JevTiming {
    /// An attempt on a connection that has answered before gets a second attempt beside it
    /// on a new connection after this long, and a connection that took this long is never
    /// reused.
    pub(crate) hedge_delay: Duration,
    /// The same for an attempt that opens its connection, which the hedge would open too.
    pub(crate) cold_hedge_delay: Duration,
    /// Taken off the profile's `timeout_ms`, so the consult settles inside it.
    pub(crate) budget_margin: Duration,
    /// No retry or hedge starts with less of the budget left than this.
    pub(crate) min_attempt: Duration,
    /// A client's connection idle this long is closed, and its next attempt opens a new one.
    pub(crate) pool_idle_timeout: Duration,
}

impl JevTiming {
    pub(crate) const STANDARD: JevTiming = JevTiming {
        hedge_delay: Duration::from_millis(800),
        cold_hedge_delay: Duration::from_secs(2),
        budget_margin: Duration::from_millis(250),
        min_attempt: super::MIN_ATTEMPT,
        pool_idle_timeout: Duration::from_secs(90),
    };
}

/// The `[externals.jev]` profile bound to the runtime's one client pool and its `jev` gate:
/// one permit per consult in flight, held across its hedges and retries.
#[derive(Clone)]
pub(crate) struct JevBackend {
    url: String,
    key: Token,
    budget: Duration,
    max_body_bytes: usize,
    timing: JevTiming,
    gates: Arc<ModelGates>,
    clients: Arc<JevClients>,
}

/// What one consult left for a consult record: the winning or last attempt's status and
/// body, and the `jev_diagnostics` object.
pub(crate) struct JevRecord {
    pub(crate) raw_response: Option<Vec<u8>>,
    pub(crate) http_status: Option<u16>,
    pub(crate) diagnostics: Vec<u8>,
}

impl JevBackend {
    /// The backend of a profile whose key is set, `None` otherwise: a deployment that
    /// consults a keyless profile refuses to open, so none is ever asked for.
    pub(crate) fn new(
        profile: &JevProfile,
        max_body_bytes: usize,
        gates: &ConsultGates,
        timing: JevTiming,
    ) -> Option<JevBackend> {
        Some(JevBackend {
            url: profile.url.clone(),
            key: profile.key.token()?.clone(),
            budget: profile.limits.timeout.saturating_sub(timing.budget_margin),
            max_body_bytes,
            timing,
            gates: gates.models(),
            clients: gates.jev_clients(),
        })
    }

    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        self.gates.current(AnnotatorBuiltin::Jev).available_permits()
    }

    /// One annotation consult: the answer, and what a consult record keeps of it.
    pub(crate) async fn consult(&self, consult: &Consult) -> (Result<serde_json::Value, NoAnswerReason>, JevRecord) {
        let started = Instant::now();
        let mut exchange = Exchange::default();
        let answered = self.answer(consult, started, &mut exchange).await;
        let diagnostics = DiagnosticsLine {
            jev_diagnostics: JevDiagnostics {
                version: DIAGNOSTICS_VERSION,
                model: MODEL,
                // `ask` settles every attempt it launches before it returns.
                attempts: exchange
                    .attempts
                    .iter()
                    .map(|outcome| outcome.unwrap_or(AttemptOutcome::Timeout))
                    .collect(),
                labels: &exchange.trace,
                error: answered.as_ref().err().map(|(failure, _)| *failure),
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
        };
        let diagnostics = serde_json::to_vec(&diagnostics).expect("the diagnostics serialize: strings and numbers");
        tracing::debug!(diagnostics = %String::from_utf8_lossy(&diagnostics), "jev consult");
        let record = JevRecord {
            http_status: exchange.last.as_ref().map(|(status, _)| *status),
            raw_response: exchange.last.map(|(_, body)| body),
            diagnostics,
        };
        (answered.map_err(|(_, reason)| reason), record)
    }

    async fn answer(
        &self,
        consult: &Consult,
        started: Instant,
        exchange: &mut Exchange,
    ) -> Result<serde_json::Value, (JevFailure, NoAnswerReason)> {
        let consult_bytes =
            serde_json::to_vec(consult).map_err(|_| (JevFailure::UnsupportedConsult, NoAnswerReason::Malformed))?;
        if consult_bytes.len() > MAX_CONSULT_BYTES {
            tracing::debug!(
                bytes = consult_bytes.len(),
                limit = MAX_CONSULT_BYTES,
                "the jev consult is too large to send"
            );
            return Err((JevFailure::ConsultTooLarge, NoAnswerReason::Oversized));
        }
        let unsupported = (JevFailure::UnsupportedConsult, NoAnswerReason::Unregistered);
        let ConsultBody::Annotation { declaration, artifact } = &consult.body else {
            return Err(unsupported);
        };
        if !declaration.inputs.is_empty() {
            return Err(unsupported);
        }
        let args = crate::secrets::redact_args(&artifact.args);
        let state = State::of(&args).ok_or(unsupported)?;
        let key = self.key.reveal();
        let request = JevRequest {
            state,
            model: MODEL,
            questions: Questions::new(declaration.hint.as_deref()),
        };
        let body = serde_json::to_vec(&request).expect("the request serializes: strings and JSON values");
        // One deadline covers the permit wait and the attempts, as for command consults.
        let deadline = started + self.budget;
        let gate = self.gates.current(AnnotatorBuiltin::Jev);
        let permit = acquire_within(&gate, deadline, "jev", &consult.name)
            .await
            .map_err(|reason| (JevFailure::NoAnswer, reason))?;
        let labels = self.ask(&body, key, deadline, exchange).await;
        drop(permit);
        let labels = labels?;
        annotation(&labels, declaration)
            .map_err(|detail| (JevFailure::OutsideMandate, NoAnswerReason::MalformedAnswer(detail)))
    }

    /// Jev's labels: the first attempt that answers them, hedged when an attempt is slow and
    /// retried after a 5xx or a connection failure while the budget allows.
    async fn ask(
        &self,
        body: &[u8],
        key: &str,
        deadline: Instant,
        exchange: &mut Exchange,
    ) -> Result<Labels, (JevFailure, NoAnswerReason)> {
        let pool = &self.clients;
        let mut launched: Vec<(Arc<ClientSlot>, Instant, Duration)> = Vec::new();
        let mut in_flight = FuturesUnordered::new();
        macro_rules! launch {
            ($slot:expr) => {{
                let slot: Arc<ClientSlot> = $slot;
                let patience = match slot.warm() {
                    true => self.timing.hedge_delay,
                    false => self.timing.cold_hedge_delay,
                };
                in_flight.push(self.attempt(launched.len(), Arc::clone(&slot), body.to_vec(), key));
                launched.push((slot, Instant::now(), patience));
                exchange.attempts.push(None);
            }};
        }
        let mut prompt = None;
        launch!(pool.current(&self.url, self.timing.pool_idle_timeout));
        let settled = loop {
            let (_, last_start, patience) = launched.last().expect("an attempt is launched first");
            let hedge_at = *last_start + *patience;
            let may_hedge =
                in_flight.len() == 1 && launched.len() < MAX_ATTEMPTS && hedge_at + self.timing.min_attempt <= deadline;
            tokio::select! {
                biased;
                Some(Attempted { index, reply }) = in_flight.next() => {
                    let (slot, at, patience) = &launched[index];
                    let slow = at.elapsed() > *patience;
                    if slow || matches!(reply, Reply::Connection) {
                        pool.evict(slot);
                    }
                    exchange.attempts[index] = Some(reply.outcome());
                    let failed = match reply {
                        Reply::Labels { labels, trace, status, body } => {
                            exchange.trace = trace;
                            exchange.last = Some((status, body));
                            prompt = (!slow).then(|| Arc::clone(slot));
                            break Ok(labels);
                        }
                        Reply::Invalid { trace, status, body, reason } => {
                            exchange.trace = trace;
                            exchange.last = Some((status, body));
                            break Err((JevFailure::InvalidResponse, reason));
                        }
                        Reply::Status { status, body } => {
                            exchange.last = Some((status, body));
                            let reason = NoAnswerReason::NonSuccess { status, detail: None };
                            if status < 500 {
                                break Err((JevFailure::NoAnswer, reason));
                            }
                            reason
                        }
                        Reply::Connection => NoAnswerReason::Transport,
                    };
                    match (in_flight.is_empty(), self.may_retry(launched.len(), deadline)) {
                        (false, _) => {}
                        (true, true) => launch!(pool.current(&self.url, self.timing.pool_idle_timeout)),
                        (true, false) => break Err((JevFailure::NoAnswer, failed)),
                    }
                }
                _ = tokio::time::sleep_until(hedge_at), if may_hedge => launch!(pool.fresh(&self.url, self.timing.pool_idle_timeout)),
                _ = tokio::time::sleep_until(deadline) => break Err((JevFailure::NoAnswer, NoAnswerReason::Timeout)),
            }
        };
        // What is still in flight lost the race or ran out the budget. A connection that
        // has gone unanswered past its hedge delay, or to the end of the budget, is slow,
        // and is forgotten before the winner's is adopted.
        let unsettled = match &settled {
            Err((_, NoAnswerReason::Timeout)) => AttemptOutcome::Timeout,
            _ => AttemptOutcome::Hedged,
        };
        for (outcome, (slot, at, patience)) in exchange.attempts.iter_mut().zip(&launched) {
            if outcome.is_none() {
                *outcome = Some(unsettled);
                if unsettled == AttemptOutcome::Timeout || at.elapsed() > *patience {
                    pool.evict(slot);
                }
            }
        }
        if let Some(slot) = prompt {
            pool.keep(&self.url, &slot);
        }
        settled
    }

    fn may_retry(&self, launched: usize, deadline: Instant) -> bool {
        launched < MAX_ATTEMPTS && Instant::now() + self.timing.min_attempt <= deadline
    }

    async fn attempt(&self, index: usize, slot: Arc<ClientSlot>, body: Vec<u8>, key: &str) -> Attempted {
        let reply = self.exchange(&slot.client, body, key).await;
        if !matches!(reply, Reply::Connection) {
            *slot.answered.lock().expect("the slot mutex is never poisoned") = Some(Instant::now());
        }
        Attempted { index, reply }
    }

    async fn exchange(&self, client: &reqwest::Client, body: Vec<u8>, key: &str) -> Reply {
        let sent = client
            .post(&self.url)
            .bearer_auth(key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        let Ok(mut response) = sent else {
            return Reply::Connection;
        };
        let status = response.status().as_u16();
        let mut body = Vec::new();
        let read = crate::external::read_body(&mut response, self.max_body_bytes as u64, &mut body).await;
        match (read, response.status().is_success()) {
            (_, false) => Reply::Status { status, body },
            (Err(NoAnswerReason::Oversized), true) => Reply::Invalid {
                trace: LabelTrace::default(),
                status,
                body,
                reason: NoAnswerReason::Oversized,
            },
            (Err(_), true) => Reply::Connection,
            (Ok(()), true) => {
                let mut trace = LabelTrace::default();
                match labels_of(&body, &mut trace) {
                    Some(labels) => Reply::Labels {
                        labels,
                        trace,
                        status,
                        body,
                    },
                    None => Reply::Invalid {
                        trace,
                        status,
                        body,
                        reason: NoAnswerReason::Malformed,
                    },
                }
            }
        }
    }
}

/// What one consult's attempts produced, for the record and the diagnostics.
#[derive(Default)]
struct Exchange {
    /// One entry per attempt in the order they started; `None` while one is in flight.
    attempts: Vec<Option<AttemptOutcome>>,
    trace: LabelTrace,
    /// The status and body of the last attempt that got a response.
    last: Option<(u16, Vec<u8>)>,
}

struct Attempted {
    index: usize,
    reply: Reply,
}

/// How one attempt ended.
enum Reply {
    Labels {
        labels: Labels,
        trace: LabelTrace,
        status: u16,
        body: Vec<u8>,
    },
    /// A success status whose body carries no labels: not retried.
    Invalid {
        trace: LabelTrace,
        status: u16,
        body: Vec<u8>,
        reason: NoAnswerReason,
    },
    Status {
        status: u16,
        body: Vec<u8>,
    },
    Connection,
}

impl Reply {
    fn outcome(&self) -> AttemptOutcome {
        match self {
            Reply::Labels { .. } => AttemptOutcome::Ok,
            Reply::Invalid { .. } => AttemptOutcome::InvalidResponse,
            Reply::Status { status, .. } => AttemptOutcome::Http(*status),
            Reply::Connection => AttemptOutcome::Connection,
        }
    }
}

/// One attempt's outcome as the diagnostics name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptOutcome {
    Ok,
    Http(u16),
    Timeout,
    Connection,
    InvalidResponse,
    /// Still in flight when another attempt settled the consult; dropped.
    Hedged,
}

impl Serialize for AttemptOutcome {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AttemptOutcome::Ok => serializer.serialize_str("ok"),
            AttemptOutcome::Http(status) => serializer.collect_str(&format_args!("http_{status}")),
            AttemptOutcome::Timeout => serializer.serialize_str("timeout"),
            AttemptOutcome::Connection => serializer.serialize_str("connection"),
            AttemptOutcome::InvalidResponse => serializer.serialize_str("invalid_response"),
            AttemptOutcome::Hedged => serializer.serialize_str("hedged"),
        }
    }
}

/// Why a consult produced no answer, as the diagnostics name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JevFailure {
    /// Not an annotation of the complete call.
    UnsupportedConsult,
    ConsultTooLarge,
    /// The API did not answer: a status, a timeout, or a connection failure.
    NoAnswer,
    InvalidResponse,
    /// Jev answered a label the mandate does not admit.
    OutsideMandate,
}

#[derive(Serialize)]
struct DiagnosticsLine<'a> {
    jev_diagnostics: JevDiagnostics<'a>,
}

#[derive(Serialize)]
struct JevDiagnostics<'a> {
    version: u32,
    model: &'static str,
    attempts: Vec<AttemptOutcome>,
    labels: &'a LabelTrace,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JevFailure>,
    elapsed_ms: u64,
}

// ---------------------------------------------------------------- request

#[derive(Serialize)]
struct JevRequest<'a> {
    state: State<'a>,
    model: &'static str,
    questions: Questions,
}

/// The call as Jev reads it: its name, its redacted arguments, and its description when
/// the policy declares one.
#[derive(Serialize)]
struct State<'a> {
    tool: &'a str,
    arguments: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

impl<'a> State<'a> {
    /// The complete call an annotation consult carries, or nothing for any other artifact.
    fn of(args: &'a serde_json::Value) -> Option<State<'a>> {
        Some(State {
            tool: args.get("name")?.as_str()?,
            arguments: args.get("arguments").filter(|arguments| arguments.is_object())?,
            description: args.get("description").and_then(serde_json::Value::as_str),
        })
    }
}

// ---------------------------------------------------------------- labels

const RESULT_AUDIENCES: [(ResultAudience, &str); 3] = [
    (ResultAudience::Self_, "self"),
    (ResultAudience::Internal, "internal"),
    (ResultAudience::Public, "public"),
];
const RESULT_TRUSTS: [(ResultTrust, &str); 2] = [
    (ResultTrust::Suspicious, "suspicious"),
    (ResultTrust::Trusted, "trusted"),
];
const REQUIRED_AUDIENCES: [(RequiredAudience, &str); 3] = [
    (RequiredAudience::Public, "public"),
    (RequiredAudience::Internal, "internal"),
    (RequiredAudience::None, "none"),
];

/// Each label's probabilities as Jev answered them, the cutoff of `requires_trusted`, and the
/// decision, in the order the labels settle; a label past the first one Jev answered badly is absent.
#[derive(Debug, Default, Serialize)]
struct LabelTrace {
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_audience: Option<ChoiceTrace<ResultAudience>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_trust: Option<ChoiceTrace<ResultTrust>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_audience: Option<ChoiceTrace<RequiredAudience>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_trusted: Option<CutoffTrace>,
}

#[derive(Debug, Serialize)]
struct ChoiceTrace<O> {
    probabilities: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<O>,
}

#[derive(Debug, Serialize)]
struct CutoffTrace {
    probability: serde_json::Value,
    threshold: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<bool>,
}

/// A probability as Jev must spell one: a finite number in `[0, 1]`, never a boolean.
fn probability(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
}

/// The option Jev finds likeliest. `options` are safest first, and a tie goes to the safer.
fn settled_choice<O: Copy>(options: &[(O, &str)], probabilities: &serde_json::Value) -> Option<O> {
    let probabilities = probabilities.as_object()?;
    let same_options =
        probabilities.len() == options.len() && options.iter().all(|(_, name)| probabilities.contains_key(*name));
    if !same_options {
        return None;
    }
    let weighed = options
        .iter()
        .map(|(option, name)| Some((*option, probability(&probabilities[*name])?)))
        .collect::<Option<Vec<(O, f64)>>>()?;
    weighed
        .into_iter()
        .reduce(|best, next| match next.1 > best.1 {
            true => next,
            false => best,
        })
        .map(|(option, _)| option)
}

fn choice<O: Copy>(
    answers: &serde_json::Value,
    label: &str,
    options: &[(O, &str)],
    trace: &mut Option<ChoiceTrace<O>>,
) -> Option<O> {
    let probabilities = answers
        .get(label)
        .and_then(|answer| answer.get("probabilities"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let decision = settled_choice(options, &probabilities);
    *trace = Some(ChoiceTrace {
        probabilities,
        decision,
    });
    decision
}

/// The settled labels of one response body, or nothing where it carries none; `trace`
/// receives each label as far as the answer went.
fn labels_of(body: &[u8], trace: &mut LabelTrace) -> Option<Labels> {
    let response: serde_json::Value = serde_json::from_slice(body).ok()?;
    let answers = response.get("answers").filter(|answers| answers.is_object())?;
    let result_audience = choice(answers, "delta_audience", &RESULT_AUDIENCES, &mut trace.delta_audience)?;
    let result_trust = choice(answers, "delta_trust", &RESULT_TRUSTS, &mut trace.delta_trust)?;
    let required_audience = choice(
        answers,
        "requires_audience",
        &REQUIRED_AUDIENCES,
        &mut trace.requires_audience,
    )?;
    let noul = answers
        .get("requires_trusted")
        .and_then(|answer| answer.get("noul"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let requires_trusted = probability(&noul).map(|p| p >= REQUIRES_TRUSTED_CUTOFF);
    trace.requires_trusted = Some(CutoffTrace {
        probability: noul,
        threshold: REQUIRES_TRUSTED_CUTOFF,
        decision: requires_trusted,
    });
    Some(Labels {
        result_audience,
        result_trust,
        required_audience,
        requires_trusted: requires_trusted?,
    })
}

// ---------------------------------------------------------------- clients

/// The runtime's HTTP clients for the Jev API, shared by every deployment snapshot, so a
/// reload keeps the connection that answers promptly. A client holds its own connections,
/// so dropping one closes them: the pool keeps one current client while its connection
/// answers promptly, and forgets it the moment a response through it is slow. A hedge gets
/// a client of its own, so it always opens a new connection, over HTTP/1.1 and HTTP/2 alike.
#[derive(Default)]
pub(crate) struct JevClients {
    state: Mutex<Pool>,
}

#[derive(Default)]
struct Pool {
    url: Option<String>,
    current: Option<Arc<ClientSlot>>,
}

/// One client and what the pool knows of its connection.
pub(crate) struct ClientSlot {
    client: reqwest::Client,
    /// When a response last came back through this client.
    answered: Mutex<Option<Instant>>,
    idle_timeout: Duration,
    /// Evicted: never the pool's current client again.
    condemned: AtomicBool,
}

impl ClientSlot {
    /// Whether this client's connection is open: it has answered, and recently enough that
    /// the pool has not closed the connection as idle.
    fn warm(&self) -> bool {
        self.answered
            .lock()
            .expect("the slot mutex is never poisoned")
            .is_some_and(|at| at.elapsed() < self.idle_timeout)
    }
}

impl JevClients {
    fn pool(&self) -> std::sync::MutexGuard<'_, Pool> {
        self.state.lock().expect("the jev pool mutex is never poisoned")
    }

    /// The current client for `url`, built on first use and again after an eviction or a
    /// change of endpoint.
    fn current(&self, url: &str, idle_timeout: Duration) -> Arc<ClientSlot> {
        let mut pool = self.pool();
        if pool.url.as_deref() != Some(url) {
            pool.url = Some(url.to_string());
            pool.current = None;
        }
        match &pool.current {
            Some(slot) => Arc::clone(slot),
            None => {
                let slot = mint(url, idle_timeout);
                pool.current = Some(Arc::clone(&slot));
                slot
            }
        }
    }

    /// A client outside the pool: its first request opens a new connection.
    fn fresh(&self, url: &str, idle_timeout: Duration) -> Arc<ClientSlot> {
        mint(url, idle_timeout)
    }

    /// Adopt a client whose connection answered promptly, where the pool holds none and no
    /// consult has evicted it meanwhile.
    fn keep(&self, url: &str, slot: &Arc<ClientSlot>) {
        let mut pool = self.pool();
        if pool.url.as_deref() == Some(url) && pool.current.is_none() && !slot.condemned.load(Ordering::Relaxed) {
            pool.current = Some(Arc::clone(slot));
        }
    }

    /// Never hand this client out again.
    fn evict(&self, slot: &Arc<ClientSlot>) {
        let mut pool = self.pool();
        slot.condemned.store(true, Ordering::Relaxed);
        if pool.current.as_ref().is_some_and(|current| Arc::ptr_eq(current, slot)) {
            pool.current = None;
        }
    }
}

fn mint(url: &str, idle_timeout: Duration) -> Arc<ClientSlot> {
    crate::tls::install_crypto_provider();
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .pool_idle_timeout(idle_timeout)
        .tcp_keepalive(TCP_KEEPALIVE);
    let builder = match Endpoint::new(url.to_string(), None).host() {
        EndpointHost::Loopback => builder.no_proxy(),
        EndpointHost::Remote => builder,
    };
    Arc::new(ClientSlot {
        client: builder
            .build()
            .expect("the reqwest client builds: the crypto provider is installed above"),
        answered: Mutex::new(None),
        idle_timeout,
        condemned: AtomicBool::new(false),
    })
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::Router;
    use axum::extract::{ConnectInfo, State as Shared};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use serde_json::json;

    use super::*;
    use crate::config::{JEV_DEFAULT_URL, ModelLimits, ProfileKey};
    use crate::consult::{AnnotationArtifact, AnnotationDeclaration};
    use appa_engine::registry::AudienceVocabulary;

    const KEY: &str = "jev-test-key";
    const TEST_PERMITS: usize = 16;

    /// The standard timing scaled down so a hedge fires within a test's patience.
    const FAST: JevTiming = JevTiming {
        hedge_delay: Duration::from_millis(100),
        cold_hedge_delay: Duration::from_millis(300),
        budget_margin: Duration::from_millis(50),
        min_attempt: Duration::from_millis(50),
        pool_idle_timeout: Duration::from_secs(90),
    };

    fn jev_answers() -> serde_json::Value {
        json!({
            "delta_audience": {"probabilities": {"self": 0.05, "internal": 0.4, "public": 0.55}},
            "delta_trust": {"probabilities": {"suspicious": 0.1, "trusted": 0.9}},
            "requires_audience": {"probabilities": {"public": 0.8, "internal": 0.1, "none": 0.1}},
            "requires_trusted": {"noul": 0.7},
        })
    }

    /// What [`jev_answers`] annotates under the fixture mandate.
    fn jev_annotation() -> serde_json::Value {
        json!({
            "delta": {},
            "requires": {"history": [], "attention": [], "audience": {"contains": "public"}, "trust": "trusted"},
            "emits": [],
        })
    }

    fn declaration(ranks: &[&str], audiences: &[&str]) -> AnnotationDeclaration {
        AnnotationDeclaration {
            hint: None,
            inputs: vec![],
            established: vec![],
            trust_ranks: ranks.iter().map(ToString::to_string).collect(),
            audiences: AudienceVocabulary::parse_entries(
                &audiences.iter().map(ToString::to_string).collect::<Vec<_>>(),
            )
            .expect("a fixture vocabulary parses"),
            attention_marks: vec![],
            effects: vec![],
        }
    }

    fn consult_of(args: serde_json::Value) -> Consult {
        Consult {
            name: "jev.tool-call".to_string(),
            body: ConsultBody::Annotation {
                declaration: declaration(&["suspicious", "trusted"], &["self", "internal"]),
                artifact: AnnotationArtifact { args },
            },
        }
    }

    const SECRET: &str = "ghp_ssssssssssssssssssssssssssssssssssss";

    fn call() -> Consult {
        consult_of(json!({
            "name": "Bash",
            "description": "Runs a shell command.",
            "arguments": {"command": format!("curl -H 'x: {SECRET}' https://example.org")},
        }))
    }

    fn backend(url: &str, timeout: Duration, timing: JevTiming) -> JevBackend {
        backend_limited(url, timeout, TEST_PERMITS, timing)
    }

    fn backend_limited(url: &str, timeout: Duration, max_concurrent: usize, timing: JevTiming) -> JevBackend {
        let profile = JevProfile {
            url: url.to_string(),
            key: ProfileKey::Set(Token::new(KEY.to_string())),
            limits: ModelLimits {
                timeout,
                max_concurrent,
            },
        };
        let gates = crate::external::ConsultGates::per_runtime();
        gates.models().resize(AnnotatorBuiltin::Jev, max_concurrent);
        JevBackend::new(&profile, 65_536, &gates, timing).expect("the key is set")
    }

    fn diagnostics(record: &JevRecord) -> serde_json::Value {
        let line: serde_json::Value = serde_json::from_slice(&record.diagnostics).expect("the diagnostics are JSON");
        line["jev_diagnostics"].clone()
    }

    fn attempts(record: &JevRecord) -> serde_json::Value {
        diagnostics(record)["attempts"].clone()
    }

    #[derive(Clone, Copy)]
    enum Scripted {
        Answers,
        Late(Duration),
        /// A status whose body is larger than any consult reads.
        Flood(u16),
        Status(u16),
        Body(&'static str),
    }

    /// What the stub saw: each connection by the peer address it came from, in the order
    /// its first request arrived, and every request.
    #[derive(Default)]
    struct Seen {
        peers: Vec<SocketAddr>,
        per_connection: Vec<usize>,
        requests: Vec<(HeaderMap, serde_json::Value)>,
    }

    /// A local Jev API. The n-th request gets the n-th scripted reply (the last one
    /// repeats), and every request on the n-th connection waits the n-th delay first
    /// (none past the list): a slow connection stays slow.
    #[derive(Clone)]
    struct Stub {
        replies: Arc<Vec<Scripted>>,
        delays: Arc<Vec<Duration>>,
        seen: Arc<Mutex<Seen>>,
    }

    impl Stub {
        fn per_connection(&self) -> Vec<usize> {
            self.seen.lock().unwrap().per_connection.clone()
        }

        fn requests(&self) -> Vec<(HeaderMap, serde_json::Value)> {
            self.seen.lock().unwrap().requests.clone()
        }
    }

    async fn serve(replies: Vec<Scripted>, delays: Vec<Duration>) -> (String, Stub) {
        let stub = Stub {
            replies: Arc::new(replies),
            delays: Arc::new(delays),
            seen: Arc::new(Mutex::new(Seen::default())),
        };
        let router = Router::new()
            .route(
                "/v1/systemone",
                post(
                    |Shared(stub): Shared<Stub>,
                     ConnectInfo(peer): ConnectInfo<SocketAddr>,
                     headers: HeaderMap,
                     body: String| async move {
                        let (connection, index) = {
                            let mut seen = stub.seen.lock().unwrap();
                            let connection = match seen.peers.iter().position(|known| *known == peer) {
                                Some(connection) => connection,
                                None => {
                                    seen.peers.push(peer);
                                    seen.per_connection.push(0);
                                    seen.peers.len() - 1
                                }
                            };
                            seen.per_connection[connection] += 1;
                            let request = serde_json::from_str(&body).expect("the request is JSON");
                            seen.requests.push((headers, request));
                            (connection, seen.requests.len() - 1)
                        };
                        tokio::time::sleep(stub.delays.get(connection).copied().unwrap_or_default()).await;
                        let reply = stub.replies.get(index).or(stub.replies.last()).copied();
                        match reply.expect("the stub is scripted") {
                            Scripted::Answers => (StatusCode::OK, json!({"answers": jev_answers()}).to_string()),
                            Scripted::Flood(status) => (StatusCode::from_u16(status).unwrap(), "x".repeat(200_000)),
                            Scripted::Late(delay) => {
                                tokio::time::sleep(delay).await;
                                (StatusCode::OK, json!({"answers": jev_answers()}).to_string())
                            }
                            Scripted::Status(status) => (StatusCode::from_u16(status).unwrap(), "{}".to_string()),
                            Scripted::Body(body) => (StatusCode::OK, body.to_string()),
                        }
                    },
                ),
            )
            .with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let addr = listener.local_addr().expect("the bound address is readable");
        tokio::spawn(async move {
            axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .expect("the stub serves");
        });
        (format!("http://{addr}/v1/systemone"), stub)
    }

    fn settle(answers: serde_json::Value) -> (Option<Labels>, LabelTrace) {
        let mut trace = LabelTrace::default();
        let labels = labels_of(json!({"answers": answers}).to_string().as_bytes(), &mut trace);
        (labels, trace)
    }

    fn answers_with(label: &str, answer: serde_json::Value) -> serde_json::Value {
        let mut answers = jev_answers();
        answers[label] = answer;
        answers
    }

    fn labels(
        result_audience: ResultAudience,
        result_trust: ResultTrust,
        required_audience: RequiredAudience,
        requires_trusted: bool,
    ) -> Labels {
        Labels {
            result_audience,
            result_trust,
            required_audience,
            requires_trusted,
        }
    }

    #[test]
    fn each_label_is_its_likeliest_option_however_unsure() {
        let confident = json!({
            "delta_audience": {"probabilities": {"self": 0.0, "internal": 0.1, "public": 0.9}},
            "delta_trust": {"probabilities": {"suspicious": 0.1, "trusted": 0.9}},
            "requires_audience": {"probabilities": {"public": 0.0, "internal": 0.1, "none": 0.9}},
            "requires_trusted": {"noul": 0.1},
        });
        assert_eq!(
            settle(confident).0,
            Some(labels(
                ResultAudience::Public,
                ResultTrust::Trusted,
                RequiredAudience::None,
                false
            ))
        );
        let unsure = json!({
            "delta_audience": {"probabilities": {"self": 0.05, "internal": 0.4, "public": 0.55}},
            "delta_trust": {"probabilities": {"suspicious": 0.45, "trusted": 0.55}},
            "requires_audience": {"probabilities": {"public": 0.42, "internal": 0.03, "none": 0.55}},
            "requires_trusted": {"noul": 0.5},
        });
        assert_eq!(
            settle(unsure).0,
            Some(labels(
                ResultAudience::Public,
                ResultTrust::Trusted,
                RequiredAudience::None,
                true
            ))
        );
        assert_eq!(
            settle(answers_with("requires_trusted", json!({"noul": 0.49})))
                .0
                .map(|labels| labels.requires_trusted),
            Some(false)
        );
    }

    #[test]
    fn a_tie_settles_on_the_safer_option() {
        let tied = json!({
            "delta_audience": {"probabilities": {"public": 0.4, "internal": 0.4, "self": 0.2}},
            "delta_trust": {"probabilities": {"trusted": 0.7, "suspicious": 0.7}},
            "requires_audience": {"probabilities": {"none": 0.2, "internal": 0.4, "public": 0.4}},
            "requires_trusted": {"noul": 0.0},
        });
        assert_eq!(
            settle(tied).0,
            Some(labels(
                ResultAudience::Internal,
                ResultTrust::Suspicious,
                RequiredAudience::Public,
                false
            ))
        );
    }

    fn delta_of(delta_audience: serde_json::Value) -> serde_json::Value {
        let (settled, _) = settle(answers_with("delta_audience", json!({"probabilities": delta_audience})));
        let settled = settled.expect("every label settles");
        annotation(
            &settled,
            &declaration(&["suspicious", "trusted"], &["self", "internal"]),
        )
        .expect("the mandate admits every label")["delta"]
            .clone()
    }

    #[test]
    fn a_public_majority_leaves_the_result_audience_open() {
        assert_eq!(
            delta_of(json!({"public": 0.56, "internal": 0.4, "self": 0.04})),
            json!({})
        );
    }

    #[test]
    fn an_internal_majority_narrows_the_result_audience() {
        assert_eq!(
            delta_of(json!({"public": 0.3, "internal": 0.65, "self": 0.05})),
            json!({"audience": ["internal"]})
        );
    }

    #[test]
    fn an_answer_that_is_not_a_probability_over_exactly_the_options_is_refused() {
        for (label, answer) in [
            ("requires_trusted", json!({"noul": true})),
            ("requires_trusted", json!({"noul": 1.5})),
            ("requires_trusted", json!({"noul": "0.7"})),
            ("requires_trusted", json!({})),
            (
                "delta_trust",
                json!({"probabilities": {"suspicious": false, "trusted": true}}),
            ),
            (
                "delta_trust",
                json!({"probabilities": {"suspicious": -0.2, "trusted": 1.2}}),
            ),
            ("delta_trust", json!({"probabilities": {"trusted": 1.0}})),
            (
                "delta_trust",
                json!({"probabilities": {"suspicious": 0.1, "trusted": 0.9, "public": 0.0}}),
            ),
            (
                "delta_trust",
                json!({"probabilities": {"suspicious": 0.1, "verified": 0.9}}),
            ),
            ("delta_audience", json!({"probabilities": [0.1, 0.2, 0.7]})),
        ] {
            assert_eq!(settle(answers_with(label, answer.clone())).0, None, "{label}: {answer}");
        }
        let (labels, trace) = settle(answers_with("delta_trust", json!({"probabilities": {"trusted": 1.0}})));
        assert_eq!(labels, None);
        assert_eq!(
            serde_json::to_value(&trace).unwrap(),
            json!({
                "delta_audience": {
                    "probabilities": jev_answers()["delta_audience"]["probabilities"],
                    "decision": "public",
                },
                "delta_trust": {"probabilities": {"trusted": 1.0}},
            }),
            "the trace stops at the first label Jev answered badly"
        );
    }

    #[test]
    fn the_annotation_spells_each_label_as_the_mandate_does() {
        let mandate = declaration(&["suspicious", "trusted"], &["self", "internal"]);
        assert_eq!(
            annotation(
                &labels(
                    ResultAudience::Public,
                    ResultTrust::Trusted,
                    RequiredAudience::None,
                    false
                ),
                &mandate
            ),
            Ok(json!({"delta": {}, "requires": {"history": [], "attention": []}, "emits": []}))
        );
        assert_eq!(
            annotation(
                &labels(
                    ResultAudience::Self_,
                    ResultTrust::Suspicious,
                    RequiredAudience::Public,
                    true
                ),
                &mandate
            ),
            Ok(json!({
                "delta": {"audience": ["self"], "trust": "suspicious"},
                "requires": {"history": [], "attention": [], "audience": {"contains": "public"}, "trust": "trusted"},
                "emits": [],
            }))
        );
        assert_eq!(
            annotation(
                &labels(
                    ResultAudience::Internal,
                    ResultTrust::Trusted,
                    RequiredAudience::Internal,
                    false
                ),
                &mandate
            ),
            Ok(json!({
                "delta": {"audience": ["internal"]},
                "requires": {"history": [], "attention": [], "audience": {"contains": ["internal"]}},
                "emits": [],
            }))
        );

        let three_ranks = declaration(&["untrusted", "reviewed", "verified"], &["self", "internal"]);
        let answer = annotation(
            &labels(
                ResultAudience::Public,
                ResultTrust::Suspicious,
                RequiredAudience::None,
                true,
            ),
            &three_ranks,
        )
        .expect("the mandate admits every label");
        assert_eq!(
            (&answer["delta"]["trust"], &answer["requires"]["trust"]),
            (&json!("untrusted"), &json!("verified"))
        );

        let internal_only = declaration(&["suspicious", "trusted"], &["internal"]);
        assert!(
            annotation(
                &labels(
                    ResultAudience::Self_,
                    ResultTrust::Trusted,
                    RequiredAudience::None,
                    false
                ),
                &internal_only
            )
            .is_err()
        );
        let one_rank = declaration(&["trusted"], &["self", "internal"]);
        assert!(
            annotation(
                &labels(
                    ResultAudience::Public,
                    ResultTrust::Trusted,
                    RequiredAudience::None,
                    false
                ),
                &declaration(&[], &[])
            )
            .is_err()
        );
        assert!(
            annotation(
                &labels(
                    ResultAudience::Public,
                    ResultTrust::Suspicious,
                    RequiredAudience::None,
                    false
                ),
                &one_rank
            )
            .is_ok_and(|answer| answer["delta"]["trust"] == "trusted"),
            "one rank is both ends of the chain; the deployment refuses it at open"
        );
    }

    #[tokio::test]
    async fn a_first_answer_is_used_as_is_and_carries_the_key_and_the_redacted_call() {
        let (url, stub) = serve(vec![Scripted::Answers], vec![]).await;
        let (answered, record) = backend(&url, Duration::from_secs(2), FAST).consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        assert_eq!(attempts(&record), json!(["ok"]));
        assert_eq!(record.http_status, Some(200));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(record.raw_response.as_deref().unwrap()).unwrap(),
            json!({"answers": jev_answers()})
        );
        let [(headers, request)] = stub.requests().try_into().expect("one request");
        assert_eq!(headers["authorization"], format!("Bearer {KEY}").as_str());
        assert_eq!(
            request["state"],
            json!({
                "tool": "Bash",
                "arguments": {"command": "curl -H 'x: [redacted-secret]' https://example.org"},
                "description": "Runs a shell command.",
            })
        );
        assert_eq!(request["model"], MODEL);
        let diagnostics = diagnostics(&record);
        assert_eq!(
            diagnostics["labels"]["requires_trusted"],
            json!({"probability": 0.7, "threshold": 0.5, "decision": true})
        );
        assert_eq!(diagnostics.get("error"), None);
    }

    #[tokio::test]
    async fn a_server_error_is_retried() {
        let (url, stub) = serve(vec![Scripted::Status(503), Scripted::Answers], vec![]).await;
        let (answered, record) = backend(&url, Duration::from_secs(2), FAST).consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        assert_eq!(attempts(&record), json!(["http_503", "ok"]));
        assert_eq!(stub.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried() {
        let (url, stub) = serve(vec![Scripted::Status(400), Scripted::Answers], vec![]).await;
        let (answered, record) = backend(&url, Duration::from_secs(2), FAST).consult(&call()).await;
        assert_eq!(
            answered,
            Err(NoAnswerReason::NonSuccess {
                status: 400,
                detail: None
            })
        );
        assert_eq!(attempts(&record), json!(["http_400"]));
        assert_eq!(diagnostics(&record)["error"], "no_answer");
        assert_eq!(record.http_status, Some(400));
        assert_eq!(stub.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_error_status_with_a_body_past_the_cap_is_still_that_status() {
        let (url, stub) = serve(vec![Scripted::Flood(400), Scripted::Answers], vec![]).await;
        let (answered, record) = backend(&url, Duration::from_secs(2), FAST).consult(&call()).await;
        assert_eq!(
            answered,
            Err(NoAnswerReason::NonSuccess {
                status: 400,
                detail: None
            })
        );
        assert_eq!(attempts(&record), json!(["http_400"]));
        assert_eq!(record.http_status, Some(400));
        assert_eq!(stub.requests().len(), 1);

        let (url, _stub) = serve(vec![Scripted::Flood(503), Scripted::Answers], vec![]).await;
        let (answered, record) = backend(&url, Duration::from_secs(2), FAST).consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        assert_eq!(attempts(&record), json!(["http_503", "ok"]));
    }

    #[tokio::test]
    async fn a_consult_past_the_input_bound_is_refused_before_it_leaves() {
        let sized = |command: String| consult_of(json!({"name": "Bash", "arguments": {"command": command}}));
        let base = serde_json::to_vec(&sized(String::new())).unwrap().len();
        let (url, stub) = serve(vec![Scripted::Answers], vec![]).await;
        let jev = backend(&url, Duration::from_secs(2), FAST);

        let (answered, _) = jev.consult(&sized("x".repeat(MAX_CONSULT_BYTES - base))).await;
        assert_eq!(answered, Ok(jev_annotation()));
        assert_eq!(stub.requests().len(), 1);

        let (answered, record) = jev.consult(&sized("x".repeat(MAX_CONSULT_BYTES - base + 1))).await;
        assert_eq!(answered, Err(NoAnswerReason::Oversized));
        assert_eq!(diagnostics(&record)["error"], "consult_too_large");
        assert_eq!(attempts(&record), json!([]));
        assert_eq!(stub.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_invalid_body_is_not_retried() {
        for body in ["not json", r#"{"answers": {}}"#, r#"{"labels": {}}"#] {
            let (url, stub) = serve(vec![Scripted::Body(body), Scripted::Answers], vec![]).await;
            let (answered, record) = backend(&url, Duration::from_secs(2), FAST).consult(&call()).await;
            assert_eq!(answered, Err(NoAnswerReason::Malformed), "{body}");
            assert_eq!(attempts(&record), json!(["invalid_response"]), "{body}");
            assert_eq!(diagnostics(&record)["error"], "invalid_response");
            assert_eq!(stub.requests().len(), 1, "{body}");
        }
    }

    #[tokio::test]
    async fn a_connection_failure_is_retried_until_the_attempts_run_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        drop(listener);
        let (answered, record) = backend(&dead, Duration::from_secs(2), FAST).consult(&call()).await;
        assert_eq!(answered, Err(NoAnswerReason::Transport));
        assert_eq!(attempts(&record), json!(["connection", "connection", "connection"]));
        assert_eq!(record.http_status, None);
    }

    #[tokio::test]
    async fn a_hung_new_connection_is_hedged_after_the_cold_delay() {
        let (url, stub) = serve(vec![Scripted::Answers], vec![Duration::from_secs(30)]).await;
        let started = std::time::Instant::now();
        let (answered, record) = backend(&url, Duration::from_secs(2), FAST).consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= FAST.cold_hedge_delay && elapsed < Duration::from_secs(1),
            "{elapsed:?}"
        );
        assert_eq!(attempts(&record), json!(["hedged", "ok"]));
        assert_eq!(stub.per_connection(), [1, 1]);
    }

    /// A connection that has answered before and then hangs is hedged after the warm delay,
    /// and the hedge's connection serves what follows.
    #[tokio::test]
    async fn a_hung_warm_connection_is_hedged_on_a_new_one() {
        let (url, stub) = serve(
            vec![
                Scripted::Answers,
                Scripted::Late(Duration::from_secs(30)),
                Scripted::Answers,
            ],
            vec![],
        )
        .await;
        let jev = backend(&url, Duration::from_secs(2), FAST);
        let (_, record) = jev.consult(&call()).await;
        assert_eq!(attempts(&record), json!(["ok"]));
        let started = std::time::Instant::now();
        let (answered, record) = jev.consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= FAST.hedge_delay && elapsed < FAST.cold_hedge_delay,
            "{elapsed:?}"
        );
        assert_eq!(attempts(&record), json!(["hedged", "ok"]));
        let (_, record) = jev.consult(&call()).await;
        assert_eq!(attempts(&record), json!(["ok"]));
        assert_eq!(stub.per_connection(), [2, 2]);
    }

    /// Opening the connection makes the first answer slower than the warm hedge delay, but
    /// not slow: the connection is kept and serves every later consult.
    #[tokio::test]
    async fn a_slow_first_answer_on_a_new_connection_keeps_it() {
        let (url, stub) = serve(
            vec![Scripted::Late(Duration::from_millis(200)), Scripted::Answers],
            vec![],
        )
        .await;
        let jev = backend(&url, Duration::from_secs(2), FAST);
        for _ in 0..5 {
            let (answered, record) = jev.consult(&call()).await;
            assert_eq!(answered, Ok(jev_annotation()));
            assert_eq!(attempts(&record), json!(["ok"]));
        }
        assert_eq!(stub.per_connection(), [5]);
    }

    #[tokio::test]
    async fn a_consult_nobody_answers_ends_at_its_budget() {
        let (url, _stub) = serve(
            vec![Scripted::Answers],
            vec![Duration::from_secs(30), Duration::from_secs(30)],
        )
        .await;
        let started = std::time::Instant::now();
        let (answered, record) = backend(&url, Duration::from_millis(600), FAST).consult(&call()).await;
        assert_eq!(answered, Err(NoAnswerReason::Timeout));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(550) && elapsed < Duration::from_millis(1100),
            "{elapsed:?}"
        );
        assert_eq!(attempts(&record), json!(["timeout", "timeout"]));
    }

    /// A new connection that hangs past a budget shorter than the cold hedge delay is
    /// never hedged, and never kept either: the next consult opens its own.
    #[tokio::test]
    async fn a_connection_unanswered_at_the_deadline_is_not_kept() {
        let (url, stub) = serve(vec![Scripted::Late(Duration::from_secs(30)), Scripted::Answers], vec![]).await;
        let jev = backend(&url, FAST.cold_hedge_delay, FAST);
        let (answered, record) = jev.consult(&call()).await;
        assert_eq!(answered, Err(NoAnswerReason::Timeout));
        assert_eq!(attempts(&record), json!(["timeout"]));
        let (answered, record) = jev.consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        assert_eq!(attempts(&record), json!(["ok"]));
        assert_eq!(stub.per_connection(), [1, 1]);
    }

    /// A warm connection answers, but only after the hedge delay; the hedge's connection is
    /// slower still. The slow answer is used, and its connection is never asked again.
    #[tokio::test]
    async fn a_slow_reused_connection_is_never_reused_again() {
        let (url, stub) = serve(
            vec![
                Scripted::Answers,
                Scripted::Late(Duration::from_millis(250)),
                Scripted::Answers,
            ],
            vec![Duration::ZERO, Duration::from_millis(1000)],
        )
        .await;
        let jev = backend(&url, Duration::from_secs(3), FAST);
        let (_, record) = jev.consult(&call()).await;
        assert_eq!(attempts(&record), json!(["ok"]));
        let (answered, record) = jev.consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        assert_eq!(attempts(&record), json!(["ok", "hedged"]));
        for _ in 0..3 {
            let (answered, record) = jev.consult(&call()).await;
            assert_eq!(answered, Ok(jev_annotation()));
            assert_eq!(attempts(&record), json!(["ok"]));
        }
        assert_eq!(stub.per_connection(), [2, 1, 3]);
    }

    /// Consult A's warm attempt is slow, so A evicts its client while B, launched on that
    /// client just before, is still waiting for a prompt answer. B's answer does not bring
    /// the evicted client back: the next consult opens a connection of its own.
    #[tokio::test]
    async fn a_client_one_consult_evicts_is_not_adopted_by_another() {
        let timing = JevTiming {
            hedge_delay: Duration::from_millis(200),
            cold_hedge_delay: Duration::from_millis(600),
            ..FAST
        };
        let (url, stub) = serve(
            vec![
                Scripted::Answers,
                Scripted::Late(Duration::from_millis(500)),
                Scripted::Late(Duration::from_secs(2)),
                Scripted::Late(Duration::from_millis(150)),
                Scripted::Answers,
            ],
            vec![],
        )
        .await;
        let jev = backend(&url, Duration::from_secs(3), timing);
        let (_, record) = jev.consult(&call()).await;
        assert_eq!(attempts(&record), json!(["ok"]));
        let consult = call();
        let a = jev.consult(&consult);
        let b = async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            jev.consult(&consult).await
        };
        let ((_, a), (_, b)) = tokio::join!(a, b);
        assert_eq!(attempts(&a), json!(["ok", "hedged"]));
        assert_eq!(attempts(&b), json!(["ok"]));
        let (_, record) = jev.consult(&call()).await;
        assert_eq!(attempts(&record), json!(["ok"]));
        assert_eq!(stub.per_connection(), [2, 1, 1, 1]);
    }

    /// A client idle past the pool's idle timeout has lost its connection, so its next
    /// attempt opens a new one and waits the cold delay before it is hedged.
    #[tokio::test]
    async fn a_client_idle_past_the_pool_timeout_is_cold_again() {
        let timing = JevTiming {
            pool_idle_timeout: Duration::from_millis(150),
            ..FAST
        };
        let (url, stub) = serve(
            vec![Scripted::Answers, Scripted::Late(Duration::from_millis(200))],
            vec![],
        )
        .await;
        let jev = backend(&url, Duration::from_secs(2), timing);
        let (_, record) = jev.consult(&call()).await;
        assert_eq!(attempts(&record), json!(["ok"]));
        tokio::time::sleep(timing.pool_idle_timeout * 2).await;
        let (answered, record) = jev.consult(&call()).await;
        assert_eq!(answered, Ok(jev_annotation()));
        assert_eq!(attempts(&record), json!(["ok"]));
        assert_eq!(stub.per_connection(), [1, 1]);
    }

    /// With the one permit held by a hung consult, a second consult with a shorter budget
    /// waits for it until its own deadline and sends nothing.
    #[tokio::test]
    async fn a_consult_waits_for_a_permit_within_its_budget() {
        let (url, stub) = serve(vec![Scripted::Late(Duration::from_secs(30))], vec![]).await;
        let patient = backend_limited(&url, Duration::from_secs(2), 1, FAST);
        let hasty = JevBackend {
            budget: Duration::from_millis(600) - FAST.budget_margin,
            ..patient.clone()
        };
        let consult = call();
        let late = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let started = std::time::Instant::now();
            let consulted = hasty.consult(&consult).await;
            (consulted, started.elapsed())
        };
        let ((first, _), ((second, record), waited)) = tokio::join!(patient.consult(&consult), late);
        assert!(waited < Duration::from_secs(1), "{waited:?}");
        assert_eq!(first, Err(NoAnswerReason::Timeout));
        assert_eq!(second, Err(NoAnswerReason::Timeout));
        assert_eq!(attempts(&record), json!([]));
        assert_eq!(diagnostics(&record)["error"], "no_answer");
        assert_eq!(stub.requests().len(), 2, "the first consult and its hedge");
    }

    #[tokio::test]
    async fn prompt_consults_share_one_connection() {
        let (url, stub) = serve(vec![Scripted::Answers], vec![]).await;
        let jev = backend(&url, Duration::from_secs(2), FAST);
        for _ in 0..5 {
            let (answered, record) = jev.consult(&call()).await;
            assert_eq!(answered, Ok(jev_annotation()));
            assert_eq!(attempts(&record), json!(["ok"]));
        }
        assert_eq!(stub.per_connection(), [5]);
    }

    #[tokio::test]
    async fn a_consult_that_is_not_a_complete_call_sends_nothing() {
        let (url, stub) = serve(vec![Scripted::Answers], vec![]).await;
        let jev = backend(&url, Duration::from_secs(2), FAST);
        let mut with_inputs = call();
        if let ConsultBody::Annotation { declaration, .. } = &mut with_inputs.body {
            declaration.inputs = vec!["command".to_string()];
        }
        for consult in [
            consult_of(json!({"command": "ls"})),
            consult_of(json!({"name": "Bash", "arguments": "ls"})),
            with_inputs,
        ] {
            let (answered, record) = jev.consult(&consult).await;
            assert_eq!(answered, Err(NoAnswerReason::Unregistered));
            assert_eq!(diagnostics(&record)["error"], "unsupported_consult");
            assert_eq!(attempts(&record), json!([]));
        }
        assert!(stub.requests().is_empty());
    }

    /// Asks the live API every worked example, as the Jev battery would, and reports what
    /// each consult cost. Run with `--ignored` and `APPA_PROVIDER_JEV_API_KEY` set.
    #[tokio::test]
    #[ignore = "asks the live TypeSafe API with APPA_PROVIDER_JEV_API_KEY"]
    async fn the_live_api_labels_the_worked_examples() {
        ask_the_live_api(Duration::ZERO).await;
    }

    /// The same, with the connection left idle between consults.
    #[tokio::test]
    #[ignore = "asks the live TypeSafe API with APPA_PROVIDER_JEV_API_KEY"]
    async fn the_live_api_labels_the_worked_examples_at_a_walking_pace() {
        ask_the_live_api(Duration::from_secs(2)).await;
    }

    async fn ask_the_live_api(spacing: Duration) {
        const CONSULTS: usize = 20;
        let key = std::env::var("APPA_PROVIDER_JEV_API_KEY").expect("APPA_PROVIDER_JEV_API_KEY is set");
        let profile = JevProfile {
            url: JEV_DEFAULT_URL.to_string(),
            key: ProfileKey::Set(Token::new(key)),
            limits: ModelLimits {
                timeout: Duration::from_secs(5),
                max_concurrent: TEST_PERMITS,
            },
        };
        let jev = JevBackend::new(
            &profile,
            65_536,
            &crate::external::ConsultGates::per_runtime(),
            JevTiming::STANDARD,
        )
        .expect("the key is set");
        let mut elapsed = Vec::new();
        let mut hedged = 0;
        let mut agreed = 0;
        for round in 0..CONSULTS {
            tokio::time::sleep(match round {
                0 => Duration::ZERO,
                _ => spacing,
            })
            .await;
            let example = &crate::label_guide::EXAMPLES[round % crate::label_guide::EXAMPLES.len()];
            let shown: serde_json::Value = serde_json::from_str(example.call).expect("an example call is JSON");
            let consult = consult_of(json!({"name": shown["tool"], "arguments": shown["arguments"]}));
            let started = std::time::Instant::now();
            let (answered, record) = jev.consult(&consult).await;
            elapsed.push(started.elapsed());
            let diagnostics = diagnostics(&record);
            assert!(answered.is_ok(), "{answered:?}: {diagnostics}");
            let outcomes = diagnostics["attempts"].as_array().expect("attempts are a list").clone();
            hedged += usize::from(outcomes.len() > 1);
            let labels = &diagnostics["labels"];
            let decided = [
                labels["delta_audience"]["decision"].as_str() == Some(example.delta_audience.name()),
                labels["delta_trust"]["decision"].as_str() == Some(example.delta_trust.name()),
                labels["requires_audience"]["decision"].as_str() == Some(example.requires_audience.name()),
                labels["requires_trusted"]["decision"].as_bool() == Some(example.requires_trusted),
            ];
            agreed += usize::from(decided.iter().all(|same| *same));
            eprintln!(
                "consult {round:>2}: {:>5} ms  attempts {}",
                started.elapsed().as_millis(),
                serde_json::Value::Array(outcomes)
            );
        }
        let mut sorted = elapsed.clone();
        sorted.sort();
        let at = |quantile: f64| sorted[((sorted.len() - 1) as f64 * quantile).round() as usize].as_millis();
        eprintln!(
            "{CONSULTS} consults {spacing:?} apart: min {} ms, p50 {} ms, p90 {} ms, max {} ms, mean {} ms; {hedged} hedged or retried; {agreed} agree with the worked example",
            at(0.0),
            at(0.5),
            at(0.9),
            at(1.0),
            elapsed.iter().sum::<Duration>().as_millis() / CONSULTS as u128,
        );
    }
}

//! Synchronous Python adapter over the trusted-framework [`appa_sdk::CallSession`] profile.

use std::sync::Mutex;
use std::time::Duration;

use appa_engine::value::ToolName;
use appa_runtime::tool::{DEFAULT_BODY_CAP_BYTES, EXECUTE_REMEDY_PLAN, HttpClient};
use appa_sdk::{
    AdmittedResult, BodyDisposition, CallDecision, CallSession, Config, DispatchHandle, RemedyDecision, RenderedCall,
    SdkOptions, ToolOutcome, WireTool, WireToolSchema,
};
use pyo3::create_exception;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};

const BINDING_IDENTITY: &str = "appa-agent-python-v3";
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);

create_exception!(appa_agent_python, AppaError, PyRuntimeError);

#[derive(Deserialize)]
#[serde(untagged)]
enum ToolInput {
    Name(String),
    Schema(WireTool),
}

impl ToolInput {
    fn into_schema(self) -> WireTool {
        match self {
            ToolInput::Name(name) => WireTool {
                kind: "function".to_string(),
                function: WireToolSchema {
                    name,
                    description: None,
                    parameters: None,
                },
            },
            ToolInput::Schema(schema) => schema,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DispatchResponse {
    Blocked {
        feedback: String,
    },
    Delivered {
        content: String,
        dispatched_tool: String,
        dispatched_arguments: serde_json::Value,
        disposition: DeliveryDisposition,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryDisposition {
    Admitted,
    Sealed,
}

struct SessionInner {
    session: Option<CallSession>,
    runtime: tokio::runtime::Runtime,
    bridge_url: Option<reqwest::Url>,
    client: HttpClient,
    pending: Option<PendingDispatch>,
}

impl SessionInner {
    fn open(policy_toml: &str, tools_json: &str, user_prompt: &str, bridge_url: Option<&str>) -> Result<Self, String> {
        let bridge_url = bridge_url.map(validate_bridge_url).transpose()?;
        let tools: Vec<ToolInput> =
            serde_json::from_str(tools_json).map_err(|error| format!("invalid tools JSON: {error}"))?;
        let config = Config::from_toml_str(policy_toml).map_err(|error| error.to_string())?;
        let mut session = CallSession::open(config, SdkOptions::default()).map_err(|error| error.to_string())?;
        session
            .bind_tools(tools.into_iter().map(ToolInput::into_schema).collect())
            .map_err(|error| error.to_string())?;
        session.begin_turn(user_prompt).map_err(|error| error.to_string())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("could not create Tokio runtime: {error}"))?;

        Ok(SessionInner {
            session: Some(session),
            runtime,
            bridge_url,
            client: HttpClient::loopback(),
            pending: None,
        })
    }

    fn prepare(&mut self, tool: &str, arguments_json: &str) -> Result<DispatchDecision, String> {
        if arguments_json.len() > MAX_REQUEST_BODY_BYTES {
            return Err("tool arguments exceed the native request limit".to_string());
        }
        let arguments: serde_json::Value =
            serde_json::from_str(arguments_json).map_err(|error| format!("invalid arguments JSON: {error}"))?;
        if !arguments.is_object() {
            return Err("tool arguments must be a JSON object".to_string());
        }

        Ok(if tool == EXECUTE_REMEDY_PLAN {
            let plan_id = arguments.get("plan_id").and_then(serde_json::Value::as_str);
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| "the session is closed".to_string())?;
            match self
                .runtime
                .block_on(session.resolve_remedy(plan_id))
                .map_err(|error| error.to_string())?
            {
                RemedyDecision::Authorized { handle, call } => DispatchDecision::Dispatch { handle, call },
                RemedyDecision::Declined { feedback } => DispatchDecision::Blocked { feedback },
            }
        } else {
            let call = RenderedCall {
                tool: ToolName::new(tool),
                arguments,
            };
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| "the session is closed".to_string())?;
            match session.check_call(call.clone()).map_err(|error| error.to_string())? {
                CallDecision::Allow { handle } => DispatchDecision::Dispatch { handle, call },
                CallDecision::Block { feedback } => DispatchDecision::Blocked { feedback },
            }
        })
    }

    fn check(&mut self, tool: &str, arguments_json: &str) -> Result<String, String> {
        match self.prepare(tool, arguments_json)? {
            DispatchDecision::Blocked { feedback } => encode_response(CheckResponse::Blocked { feedback }),
            DispatchDecision::Dispatch { handle, call } => {
                let response = CheckResponse::Allowed {
                    dispatched_tool: call.tool.as_str().to_string(),
                    dispatched_arguments: call.arguments.clone(),
                };
                self.pending = Some(PendingDispatch { handle, call });
                encode_response(response)
            }
        }
    }

    fn dispatch(&mut self, tool: &str, arguments_json: &str) -> Result<String, String> {
        if self.bridge_url.is_none() {
            return Err("dispatch requires a bridge URL; use check and report for framework-owned tools".to_string());
        }
        match self.prepare(tool, arguments_json)? {
            DispatchDecision::Blocked { feedback } => encode_response(DispatchResponse::Blocked { feedback }),
            DispatchDecision::Dispatch { handle, call } => {
                self.pending = Some(PendingDispatch { handle, call });
                self.execute_and_report()
            }
        }
    }

    fn execute_and_report(&mut self) -> Result<String, String> {
        let call = self
            .pending
            .as_ref()
            .ok_or_else(|| "no call is pending".to_string())?
            .call
            .clone();
        let bridge_url = self
            .bridge_url
            .clone()
            .ok_or_else(|| "dispatch requires a bridge URL".to_string())?;
        let outcome = self
            .runtime
            .block_on(invoke_bridge(self.client.clone(), bridge_url, call.clone()));
        self.report_outcome(outcome)
    }

    fn report(&mut self, content: Option<&str>, error: bool) -> Result<String, String> {
        let outcome = if error {
            ToolOutcome::Failure
        } else {
            match content {
                Some(content) if content.len() <= DEFAULT_BODY_CAP_BYTES => ToolOutcome::Success {
                    body: BodyDisposition::Available(content.to_string()),
                },
                Some(_) => ToolOutcome::Success {
                    body: BodyDisposition::RejectedTooLarge,
                },
                None => ToolOutcome::Success {
                    body: BodyDisposition::Unavailable,
                },
            }
        };
        self.report_outcome(outcome)
    }

    fn report_outcome(&mut self, outcome: ToolOutcome) -> Result<String, String> {
        let pending = self.pending.take().ok_or_else(|| "no call is pending".to_string())?;
        let reported = self
            .session
            .as_mut()
            .expect("a dispatch originates only from this live session")
            .report_outcome(pending.handle, outcome)
            .map_err(|error| error.to_string());
        let (content, disposition) = match reported? {
            AdmittedResult::Admitted { content, .. } => (content, DeliveryDisposition::Admitted),
            AdmittedResult::Sealed { token } => (token, DeliveryDisposition::Sealed),
        };
        encode_response(DispatchResponse::Delivered {
            content,
            dispatched_tool: pending.call.tool.as_str().to_string(),
            dispatched_arguments: pending.call.arguments,
            disposition,
        })
    }

    fn abandon(&mut self) -> Result<(), String> {
        let pending = self.pending.take().ok_or_else(|| "no call is pending".to_string())?;
        self.session
            .as_mut()
            .ok_or_else(|| "the session is closed".to_string())?
            .abandon(pending.handle)
            .map_err(|error| error.to_string())
    }

    fn new_round(&mut self) -> Result<(), String> {
        self.session
            .as_mut()
            .ok_or_else(|| "the session is closed".to_string())?
            .begin_round()
            .map_err(|error| error.to_string())
    }

    fn close(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("cannot close while a call is pending".to_string());
        }
        if let Some(session) = self.session.as_mut() {
            session.end_turn().map_err(|error| error.to_string())?;
            self.session = None;
        }
        Ok(())
    }
}

enum DispatchDecision {
    Blocked { feedback: String },
    Dispatch { handle: DispatchHandle, call: RenderedCall },
}

struct PendingDispatch {
    handle: DispatchHandle,
    call: RenderedCall,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CheckResponse {
    Blocked {
        feedback: String,
    },
    Allowed {
        dispatched_tool: String,
        dispatched_arguments: serde_json::Value,
    },
}

#[pyclass(module = "appa_agent_python")]
struct Session {
    inner: Mutex<SessionInner>,
}

#[pymethods]
impl Session {
    #[new]
    #[pyo3(signature = (policy_toml, tools_json, user_prompt, bridge_url=None))]
    fn new(policy_toml: &str, tools_json: &str, user_prompt: &str, bridge_url: Option<&str>) -> PyResult<Self> {
        let inner = SessionInner::open(policy_toml, tools_json, user_prompt, bridge_url).map_err(AppaError::new_err)?;
        Ok(Session {
            inner: Mutex::new(inner),
        })
    }

    fn check(&self, py: Python<'_>, tool: &str, arguments_json: &str) -> PyResult<String> {
        py.detach(|| {
            self.inner
                .lock()
                .map_err(|_| AppaError::new_err("the session lock is poisoned"))?
                .check(tool, arguments_json)
                .map_err(AppaError::new_err)
        })
    }

    fn report(&self, py: Python<'_>, content: Option<&str>, error: bool) -> PyResult<String> {
        py.detach(|| {
            self.inner
                .lock()
                .map_err(|_| AppaError::new_err("the session lock is poisoned"))?
                .report(content, error)
                .map_err(AppaError::new_err)
        })
    }

    fn abandon(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            self.inner
                .lock()
                .map_err(|_| AppaError::new_err("the session lock is poisoned"))?
                .abandon()
                .map_err(AppaError::new_err)
        })
    }

    fn dispatch(&self, py: Python<'_>, tool: &str, arguments_json: &str) -> PyResult<String> {
        py.detach(|| {
            self.inner
                .lock()
                .map_err(|_| AppaError::new_err("the session lock is poisoned"))?
                .dispatch(tool, arguments_json)
                .map_err(AppaError::new_err)
        })
    }

    fn new_round(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            self.inner
                .lock()
                .map_err(|_| AppaError::new_err("the session lock is poisoned"))?
                .new_round()
                .map_err(AppaError::new_err)
        })
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            self.inner
                .lock()
                .map_err(|_| AppaError::new_err("the session lock is poisoned"))?
                .close()
                .map_err(AppaError::new_err)
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(inner) = self.inner.get_mut()
            && inner.pending.is_none()
        {
            let _ = inner.close();
        }
    }
}

fn encode_response(response: impl Serialize) -> Result<String, String> {
    serde_json::to_string(&response).map_err(|error| format!("could not encode dispatch response: {error}"))
}

fn validate_bridge_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|error| format!("invalid bridge URL: {error}"))?;
    if url.scheme() != "http" {
        return Err("bridge URL must use http".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("bridge URL must not contain credentials".to_string());
    }
    let authority = raw
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .ok_or_else(|| "bridge URL must contain a literal loopback authority".to_string())?;
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| "bridge URL must contain an explicit port".to_string())?;
    if host != "127.0.0.1" || port.parse::<u16>().ok().filter(|port| *port != 0).is_none() {
        return Err("bridge URL host must be the literal loopback IP 127.0.0.1 with a nonzero port".to_string());
    }
    if url.host_str() != Some("127.0.0.1") {
        return Err("bridge URL must not use a DNS name".to_string());
    }
    if url.path() == "/" || url.path().is_empty() {
        return Err("bridge URL must contain a capability path".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("bridge URL must not contain a query or fragment".to_string());
    }
    Ok(url)
}

async fn invoke_bridge(client: HttpClient, url: reqwest::Url, call: RenderedCall) -> ToolOutcome {
    let body = match serde_json::to_vec(&call) {
        Ok(body) if body.len() <= MAX_REQUEST_BODY_BYTES => body,
        Ok(_) => return ToolOutcome::Failure,
        Err(_) => return ToolOutcome::Indeterminate,
    };
    let response = match client
        .inner()
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(BRIDGE_TIMEOUT)
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return ToolOutcome::Indeterminate,
    };
    let status = response.status().as_u16();
    match status {
        200..=299 => classify_success(response).await,
        413 => ToolOutcome::Success {
            body: BodyDisposition::RejectedTooLarge,
        },
        460 => ToolOutcome::Success {
            body: BodyDisposition::Unavailable,
        },
        422 => ToolOutcome::Failure,
        _ => ToolOutcome::Indeterminate,
    }
}

async fn classify_success(mut response: reqwest::Response) -> ToolOutcome {
    match appa_runtime::tool::read_body_capped(&mut response, DEFAULT_BODY_CAP_BYTES).await {
        Some(body) if body.len() > DEFAULT_BODY_CAP_BYTES => ToolOutcome::Success {
            body: BodyDisposition::RejectedTooLarge,
        },
        Some(body) => match String::from_utf8(body) {
            Ok(content) => ToolOutcome::Success {
                body: BodyDisposition::Available(content),
            },
            Err(_) => ToolOutcome::Indeterminate,
        },
        None => ToolOutcome::Indeterminate,
    }
}

#[pymodule]
fn appa_agent_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Session>()?;
    module.add("AppaError", module.py().get_type::<AppaError>())?;
    module.add("BINDING_IDENTITY", BINDING_IDENTITY)?;
    Ok(())
}

use std::io::{self, BufRead, Write};

use appa_engine::value::ToolName;
use appa_sdk::{
    AdmittedResult, BodyDisposition, CallDecision, CallSession, Config, DispatchHandle, RemedyDecision, RenderedCall,
    SdkOptions, ToolOutcome, WireTool, WireToolSchema,
};
use serde::{Deserialize, Serialize};

const EXECUTE_REMEDY_PLAN: &str = "execute_remedy_plan";

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Request {
    Open {
        policy: String,
        tools: Vec<String>,
        user_prompt: String,
    },
    Check {
        tool: String,
        arguments: serde_json::Value,
    },
    ResolveRemedy {
        plan_id: Option<String>,
    },
    Report {
        outcome: Outcome,
    },
    End,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Outcome {
    Success { body: String },
    Failure,
    Indeterminate,
}

impl From<Outcome> for ToolOutcome {
    fn from(value: Outcome) -> Self {
        match value {
            Outcome::Success { body } => ToolOutcome::Success {
                body: BodyDisposition::Available(body),
            },
            Outcome::Failure => ToolOutcome::Failure,
            Outcome::Indeterminate => ToolOutcome::Indeterminate,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Opened { remedy_tool: WireTool },
    Allowed,
    Blocked { feedback: String },
    Authorized { call: RenderedCall },
    Declined { feedback: String },
    Admitted { content: String },
    Sealed { token: String },
    Ended,
    Error { message: String },
}

#[derive(Default)]
struct Sidecar {
    session: Option<CallSession>,
    pending: Option<DispatchHandle>,
}

impl Sidecar {
    async fn handle(&mut self, request: Request) -> Result<Response, String> {
        match request {
            Request::Open {
                policy,
                tools,
                user_prompt,
            } => self.open(&policy, tools, user_prompt),
            Request::Check { tool, arguments } => self.check(tool, arguments),
            Request::ResolveRemedy { plan_id } => self.resolve_remedy(plan_id.as_deref()).await,
            Request::Report { outcome } => self.report(outcome),
            Request::End => self.end(),
        }
    }

    fn open(&mut self, policy: &str, tools: Vec<String>, user_prompt: String) -> Result<Response, String> {
        if self.pending.is_some() {
            return Err("cannot replace an episode while a dispatched call awaits its outcome".to_string());
        }
        if let Some(mut session) = self.session.take() {
            session.end_turn().map_err(|error| error.to_string())?;
        }

        let config = Config::from_toml_str(policy).map_err(|error| error.to_string())?;
        let mut session = CallSession::open(config, SdkOptions::default()).map_err(|error| error.to_string())?;
        let surface = tools.into_iter().map(tool_schema).collect();
        let remedy_tool = session
            .bind_tools(surface)
            .map_err(|error| error.to_string())?
            .iter()
            .find(|tool| tool.function.name == EXECUTE_REMEDY_PLAN)
            .cloned()
            .ok_or_else(|| "the SDK did not bind its remedy tool".to_string())?;
        session.begin_turn(user_prompt).map_err(|error| error.to_string())?;
        self.session = Some(session);
        Ok(Response::Opened { remedy_tool })
    }

    fn check(&mut self, tool: String, arguments: serde_json::Value) -> Result<Response, String> {
        if self.pending.is_some() {
            return Err("a dispatched call still awaits its outcome".to_string());
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| "no AgentDojo episode is open".to_string())?;
        let call = RenderedCall {
            tool: ToolName::new(tool),
            arguments,
        };
        match session.check_call(call).map_err(|error| error.to_string())? {
            CallDecision::Allow { handle } => {
                self.pending = Some(handle);
                Ok(Response::Allowed)
            }
            CallDecision::Block { feedback } => Ok(Response::Blocked { feedback }),
        }
    }

    async fn resolve_remedy(&mut self, plan_id: Option<&str>) -> Result<Response, String> {
        if self.pending.is_some() {
            return Err("a dispatched call still awaits its outcome".to_string());
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| "no AgentDojo episode is open".to_string())?;
        match session
            .resolve_remedy(plan_id)
            .await
            .map_err(|error| error.to_string())?
        {
            RemedyDecision::Authorized { handle, call } => {
                self.pending = Some(handle);
                Ok(Response::Authorized { call })
            }
            RemedyDecision::Declined { feedback } => Ok(Response::Declined { feedback }),
        }
    }

    fn report(&mut self, outcome: Outcome) -> Result<Response, String> {
        let handle = self
            .pending
            .take()
            .ok_or_else(|| "no dispatched call awaits an outcome".to_string())?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| "no AgentDojo episode is open".to_string())?;
        match session
            .report_outcome(handle, outcome.into())
            .map_err(|error| error.to_string())?
        {
            AdmittedResult::Admitted { content, .. } => Ok(Response::Admitted { content }),
            AdmittedResult::Sealed { token } => Ok(Response::Sealed { token }),
        }
    }

    fn end(&mut self) -> Result<Response, String> {
        if self.pending.is_some() {
            return Err("cannot end an episode while a dispatched call awaits its outcome".to_string());
        }
        let mut session = self
            .session
            .take()
            .ok_or_else(|| "no AgentDojo episode is open".to_string())?;
        session.end_turn().map_err(|error| error.to_string())?;
        Ok(Response::Ended)
    }
}

fn tool_schema(name: String) -> WireTool {
    WireTool {
        kind: "function".to_string(),
        function: WireToolSchema {
            name,
            description: None,
            parameters: None,
        },
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut sidecar = Sidecar::default();

    for line in stdin.lock().lines() {
        let response = match line {
            Ok(line) => match serde_json::from_str::<Request>(&line) {
                Ok(request) => sidecar
                    .handle(request)
                    .await
                    .unwrap_or_else(|message| Response::Error { message }),
                Err(error) => Response::Error {
                    message: format!("invalid request: {error}"),
                },
            },
            Err(error) => Response::Error {
                message: format!("could not read request: {error}"),
            },
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = r#"
version = 1
trust_chain = ["suspicious", "internal"]

[[tool]]
name = "read_external"
delta = { trust = "suspicious" }

[[tool]]
name = "send_email"
effects = ["egress"]
requires = { trust = "internal" }
delta = {}
"#;

    #[tokio::test]
    async fn narrowing_can_be_accepted_but_the_later_sink_stays_blocked() {
        let mut sidecar = Sidecar::default();
        let opened = sidecar
            .handle(Request::Open {
                policy: POLICY.to_string(),
                tools: vec!["read_external".to_string(), "send_email".to_string()],
                user_prompt: "read the message".to_string(),
            })
            .await
            .unwrap();
        assert!(matches!(opened, Response::Opened { .. }));

        let read = sidecar
            .handle(Request::Check {
                tool: "read_external".to_string(),
                arguments: serde_json::json!({}),
            })
            .await
            .unwrap();
        assert!(matches!(read, Response::Blocked { .. }));

        let remedy = sidecar
            .handle(Request::ResolveRemedy {
                plan_id: Some("remedy-0".to_string()),
            })
            .await
            .unwrap();
        assert!(matches!(remedy, Response::Authorized { .. }));

        let admitted = sidecar
            .handle(Request::Report {
                outcome: Outcome::Success {
                    body: "third-party message".to_string(),
                },
            })
            .await
            .unwrap();
        assert!(matches!(admitted, Response::Admitted { .. }));

        let sink = sidecar
            .handle(Request::Check {
                tool: "send_email".to_string(),
                arguments: serde_json::json!({"to": "attacker@example.com"}),
            })
            .await
            .unwrap();
        assert!(matches!(sink, Response::Blocked { .. }));
    }
}

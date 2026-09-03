//! A real HTTP server speaking the OpenAI wire with a scripted answer per
//! round. It stands in for inference and for nothing else: the runtime, the
//! policy, the agent loop and the tool shim in these tests are the shipped
//! ones.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use appa_example_agent::wire::{WireFunctionCall, WireMessage, WireToolCall};

#[derive(Clone, Default)]
pub struct Provider {
    script: Arc<Mutex<VecDeque<Step>>>,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

enum Step {
    Says(WireMessage),
    /// Call the control tool with the `nth` offer id of the last feedback and
    /// `arguments`. Offer ids are minted per block (`RUL-6`), so they cannot be
    /// scripted in advance; this reads one out of the transcript exactly as a
    /// model would.
    Pursues {
        id: String,
        nth: usize,
        arguments: serde_json::Value,
    },
}

impl Provider {
    /// Answer with free text and stop this trajectory.
    pub fn says(&self, text: &str) -> &Self {
        self.push(Step::Says(WireMessage::assistant(text)))
    }

    /// Finish with no content at all — a child's void return (`BRN-9`).
    pub fn says_nothing(&self) -> &Self {
        self.push(Step::Says(WireMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
        }))
    }

    /// Propose one call.
    pub fn calls(&self, tool: &str, arguments: serde_json::Value) -> &Self {
        let id = self.next_id();
        self.push(Step::Says(WireMessage::assistant_tool_calls(
            None,
            vec![call_of(&id, tool, &arguments.to_string())],
        )))
    }

    /// Pursue the first offer, without depending on its presentation text.
    pub fn pursues_first(&self) -> &Self {
        self.pursues_the_nth_offer_with(0, serde_json::json!({}))
    }

    /// Declare a child's return from a marked spawn's block: the `nth` plan —
    /// the bare floor first, then each registered return sanitizer in registry
    /// order — with the lowest label the parent accepts, `{}` for its own.
    pub fn declares_the_return(&self, nth: usize, label: serde_json::Value) -> &Self {
        self.pursues_the_nth_offer_with(nth, serde_json::json!({ "label": label }))
    }

    fn pursues_the_nth_offer_with(&self, nth: usize, arguments: serde_json::Value) -> &Self {
        let id = self.next_id();
        self.push(Step::Pursues { id, nth, arguments })
    }

    /// The most recent tool result containing `needle`, across every request
    /// this provider answered — what the model was actually shown.
    pub fn last_tool_result(&self, needle: &str) -> Option<String> {
        self.seen
            .lock()
            .expect("not poisoned")
            .iter()
            .rev()
            .find_map(|request| {
                request["messages"].as_array()?.iter().rev().find_map(|message| {
                    let content = message["content"].as_str()?;
                    (message["role"] == "tool" && content.contains(needle)).then(|| content.to_string())
                })
            })
    }

    fn next_id(&self) -> String {
        format!("call_{}", self.script.lock().expect("not poisoned").len())
    }

    fn push(&self, step: Step) -> &Self {
        self.script.lock().expect("not poisoned").push_back(step);
        self
    }

    pub async fn serve(self) -> appa_example_agent::Endpoint {
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let provider = self.clone();
                async move {
                    let step = provider
                        .script
                        .lock()
                        .expect("not poisoned")
                        .pop_front()
                        .unwrap_or_else(|| panic!("the script ran out; the agent asked again with: {body}"));
                    let message = match step {
                        Step::Says(message) => message,
                        Step::Pursues { id, nth, mut arguments } => {
                            let offers =
                                surfaced_offers(&body).unwrap_or_else(|| panic!("no offer id surfaced, in: {body}"));
                            let offer = offers
                                .get(nth)
                                .unwrap_or_else(|| panic!("no offer {nth} surfaced among {offers:?}, in: {body}"));
                            arguments["offer_id"] = serde_json::Value::String(offer.clone());
                            WireMessage::assistant_tool_calls(
                                None,
                                vec![call_of(&id, "execute_remedy_plan", &arguments.to_string())],
                            )
                        }
                    };
                    provider.seen.lock().expect("not poisoned").push(body);
                    axum::Json(serde_json::json!({ "choices": [{ "message": message }] }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is free");
        let address = listener.local_addr().expect("the listener has an address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        appa_example_agent::Endpoint::new(format!("http://{address}"))
    }
}

fn call_of(id: &str, tool: &str, arguments: &str) -> WireToolCall {
    WireToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: WireFunctionCall {
            name: tool.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

/// The opaque offer ids of the most recent message that lists any, in the
/// order the block lists them; surrounding presentation copy is deliberately
/// ignored.
fn surfaced_offers(request: &serde_json::Value) -> Option<Vec<String>> {
    let messages = request["messages"].as_array()?;
    messages.iter().rev().find_map(|message| {
        let ids = opaque_offer_ids(message.get("content")?.as_str()?);
        (!ids.is_empty()).then_some(ids)
    })
}

fn opaque_offer_ids(text: &str) -> Vec<String> {
    text.split("offer_id:")
        .skip(1)
        .filter_map(|after| {
            let rest = after.trim_start().strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_string())
        })
        .collect()
}

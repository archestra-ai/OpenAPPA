//! Sessions: one chat = one trajectory over one frozen policy and one private
//! copy of the corp world.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use appa_example_agent::wire::{WireMessage, WireTool};
use appa_example_agent::{
    Endpoint, HttpClient, OpenAiCompatible, OpenAiConfig, ToolCatalogue, Transcript, TranscriptHead,
};
use appa_runtime_v2::api::{OpenError, Runtime, TrajectoryId};
use appa_runtime_v2::config::{Config, ConfigError};

use crate::approvals::Approvals;
use crate::derive::Derivations;
use crate::events::LabelText;
use crate::lint::{PolicyError, check_policy};
use crate::shim::{self, World};
use crate::systems::System;
use crate::world::{TOOLS_PATH, externals_for};

const SYSTEM_PROMPT: &str = "You are the company assistant at a vendor of an AI agent platform. Depending on \
configuration you may have access to the CRM, the public GitHub issue tracker, outbound email, finance, and meeting \
recordings. Use the available tools to complete the user's request. Read what you need, then act. When a call is \
blocked and remedy plans are offered, pick one and execute it with execute_remedy_plan in the same turn — never ask \
the user to approve in chat: human approval, when a plan needs it, is collected by the product's own approval prompt \
after you execute the plan. When you are done, briefly summarise what you did.";

const MAX_LIVE_SESSIONS: usize = 64;

/// One live chat: the runtime holding the frozen policy, the trajectory the
/// turns continue, and the session's private world on disk.
pub struct DemoSession {
    pub id: String,
    pub trajectory: TrajectoryId,
    pub runtime: Arc<Runtime>,
    /// Where this session's turns and derivations run inference, and which
    /// model they ask for. Both are fixed when the chat opens.
    pub inference: Endpoint,
    pub model: String,
    pub tool_count: usize,
    pub boundary: LabelText,
    /// What the model is offered, built from the policy it was checked
    /// against — a harness owns its catalogue, so this host holds it.
    pub catalogue: ToolCatalogue,
    pub tools_url: String,
    /// The session's human-ruling desk: every authority consult parks here,
    /// the SSE pump drains its events, the approval endpoint resolves.
    pub approvals: Arc<Approvals>,
    /// The session's derivation desk: every sanitizer consult resolves here,
    /// and each turn lends it the service's key.
    pub derivations: Arc<Derivations>,
    /// Serializes turns, and holds what they accumulate: the runtime keeps
    /// no conversation, so the transcript is this host's, and the turn that
    /// holds the gate is the one entitled to it. `try_lock_owned` fails while
    /// a turn is streaming, so a second message answers 409 instead of
    /// queuing behind the lease.
    pub turn_gate: Arc<tokio::sync::Mutex<Transcript>>,
    turns: Mutex<u32>,
    session_dir: PathBuf,
    last_used: Mutex<Instant>,
}

impl DemoSession {
    pub fn touch(&self) {
        *self.last_used.expect_lock() = Instant::now();
    }

    pub fn turns_spent(&self) -> u32 {
        *self.turns.expect_lock()
    }

    pub fn spend_turn(&self) {
        *self.turns.expect_lock() += 1;
    }

    fn idle_for(&self) -> Duration {
        self.last_used.expect_lock().elapsed()
    }
}

impl Drop for DemoSession {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.session_dir);
    }
}

trait ExpectLock<T> {
    fn expect_lock(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> ExpectLock<T> for Mutex<T> {
    fn expect_lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("model {0:?} is not on the demo's allowlist")]
    ModelNotAllowed(String),
    #[error("the demo box is at capacity; try again shortly")]
    AtCapacity,
    #[error("preparing the session world: {0}")]
    World(#[from] std::io::Error),
    #[error("this deployment cannot run that policy: {0}")]
    Open(#[from] Box<OpenError>),
    #[error("composing the deployment: {0}")]
    Deployment(#[from] Box<ConfigError>),
}

/// The models the playground may spend the service's key on — the four the
/// benchmark table names.
pub const ALLOWED_MODELS: [&str; 4] = [
    "openai/gpt-4o",
    "openai/gpt-5.6-luna",
    "google/gemini-3.5-flash-lite",
    "qwen/qwen-3.6-35b",
];

pub struct Sessions {
    seed_data_root: PathBuf,
    worlds_root: PathBuf,
    ttl: Duration,
    inference: Endpoint,
    live: Mutex<HashMap<String, Arc<DemoSession>>>,
}

impl Sessions {
    pub fn new(seed_data_root: PathBuf, worlds_root: PathBuf, ttl: Duration, inference: Endpoint) -> Sessions {
        Sessions {
            seed_data_root,
            worlds_root,
            ttl,
            inference,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Build a session from the enabled systems (which tools exist), the
    /// editor's policy text (what they are allowed to do), and a model choice.
    pub async fn create(
        &self,
        policy: &str,
        enabled: &BTreeSet<System>,
        model: &str,
    ) -> Result<Arc<DemoSession>, CreateError> {
        if !ALLOWED_MODELS.contains(&model) {
            return Err(CreateError::ModelNotAllowed(model.to_string()));
        }
        if self.live.expect_lock().len() >= MAX_LIVE_SESSIONS {
            return Err(CreateError::AtCapacity);
        }

        let checked = check_policy(policy, enabled)?;
        let chain = &checked.config.registry_config().trust_chain;
        let boundary = LabelText::of(checked.config.boundary_label(), chain);
        let advertised: Vec<WireTool> = crate::catalogue::advertised(&checked.config);

        let id = session_id();
        let session_dir = self.worlds_root.join(&id);
        let data_root = session_dir.join("data");
        copy_dir(&self.seed_data_root, &data_root)?;

        let approvals = Arc::new(Approvals::default());
        let derivations = Arc::new(Derivations::new(
            self.inference.clone(),
            model.to_string(),
            checked
                .config
                .registry_config()
                .sanitizers
                .iter()
                .filter_map(|sanitizer| {
                    Some((
                        sanitizer.name.as_str().to_string(),
                        sanitizer.hint.as_ref()?.as_str().to_string(),
                    ))
                })
                .collect(),
        ));

        let address = shim::serve(World {
            data_root,
            enabled: enabled.clone(),
            approvals: approvals.clone(),
            derivations: derivations.clone(),
        })
        .await?;
        let base = format!("http://{address}");

        let config =
            Config::embedded(checked.merged_toml.clone(), externals_for(&checked.config, &base)).map_err(Box::new)?;
        let runtime = Runtime::open(config, session_dir.join("appa.db"), None).map_err(Box::new)?;

        let session = Arc::new(DemoSession {
            id: id.clone(),
            trajectory: TrajectoryId(format!("playground-{id}")),
            runtime: Arc::new(runtime),
            inference: self.inference.clone(),
            model: model.to_string(),
            tool_count: checked.tool_count,
            boundary,
            catalogue: ToolCatalogue::new(advertised),
            tools_url: format!("{base}{TOOLS_PATH}"),
            approvals,
            derivations,
            turn_gate: Arc::new(tokio::sync::Mutex::new(Transcript::default())),
            turns: Mutex::new(0),
            session_dir,
            last_used: Mutex::new(Instant::now()),
        });
        self.live.expect_lock().insert(id, session.clone());
        Ok(session)
    }

    pub fn get(&self, id: &str) -> Option<Arc<DemoSession>> {
        let session = self.live.expect_lock().get(id).cloned();
        if let Some(session) = &session {
            session.touch();
        }
        session
    }

    pub fn remove(&self, id: &str) -> Option<Arc<DemoSession>> {
        self.live.expect_lock().remove(id)
    }

    /// Drop every session idle past the TTL. The session directory goes with
    /// the last `Arc` — after any in-flight turn's stream ends.
    pub fn expire_idle(&self) -> usize {
        let mut live = self.live.expect_lock();
        let expired: Vec<String> = live
            .iter()
            .filter(|(_, session)| session.idle_for() > self.ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            live.remove(id);
        }
        expired.len()
    }

    pub fn spawn_expiry(self: &Arc<Self>) {
        let sessions = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let expired = sessions.expire_idle();
                if expired > 0 {
                    eprintln!("appa-demo: expired {expired} idle session(s)");
                }
            }
        });
    }
}

/// The provider one turn (or one derivation) runs on.
pub fn provider(inference: &Endpoint, model: &str, key: String, request_timeout: Duration) -> OpenAiCompatible {
    let config = OpenAiConfig::new(inference.clone(), model.to_string(), key).with_request_timeout(request_timeout);
    match is_loopback(inference) {
        true => OpenAiCompatible::with_http_client(config, HttpClient::loopback()),
        false => OpenAiCompatible::new(config),
    }
}

fn is_loopback(endpoint: &Endpoint) -> bool {
    let authority = endpoint
        .as_str()
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint.as_str())
        .split('/')
        .next()
        .unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((host, _)) => host,
        None => authority,
    };
    matches!(
        host.trim_start_matches('[').trim_end_matches(']'),
        "127.0.0.1" | "::1" | "localhost"
    )
}

pub fn head() -> TranscriptHead {
    TranscriptHead::new(vec![WireMessage::system(SYSTEM_PROMPT)])
}

fn session_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_long_and_distinct() {
        let a = session_id();
        let b = session_id();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}

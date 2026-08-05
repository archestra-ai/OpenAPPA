//! Sessions: one chat = one trajectory over one frozen policy and one private
//! copy of the corp world.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::approvals::Approvals;
use crate::derive::Derivations;
use crate::shim::{self, World};
use crate::systems::System;
use crate::world::{Rebound, bind_hosted};
use appa_engine::registry::TrustChain;
use appa_runtime::config::SanitizerImpl;
use appa_runtime::external::BuiltinSanitizer;
use appa_runtime::store::TenantId;
use appa_runtime::tool::{HttpClient, HttpTool, ToolBackend};
use appa_runtime::{Config, InitError, Mediator, TrajectoryId, TranscriptHead, WireMessage};

use crate::lint::{PolicyError, check_policy};

const SYSTEM_PROMPT: &str = "You are the company assistant at a vendor of an AI agent platform. Depending on \
configuration you may have access to the CRM, the public GitHub issue tracker, outbound email, finance, and meeting \
recordings. Use the available tools to complete the user's request. Read what you need, then act. When a call is \
blocked and remedy plans are offered, pick one and execute it with execute_remedy_plan in the same turn — never ask \
the user to approve in chat: human approval, when a plan needs it, is collected by the product's own approval prompt \
after you execute the plan. When you are done, briefly summarise what you did.";

const MAX_LIVE_SESSIONS: usize = 64;

/// One live chat: the frozen policy behind a mediator, the trajectory the
/// turns append to, and the session's private world on disk.
pub struct DemoSession {
    pub id: String,
    pub tenant: TenantId,
    pub trajectory: TrajectoryId,
    pub mediator: Arc<Mediator>,
    pub chain: TrustChain,
    pub model: String,
    pub tool_count: usize,
    /// The session's human-ruling desk: the rebound `hitl` authority parks
    /// here, the SSE pump drains its events, the approval endpoint resolves.
    pub approvals: Arc<Approvals>,
    /// The session's hosted-derivation desk: a rebound `hosted` sanitizer
    /// resolves here, and each turn lends it the visitor's key.
    pub derivations: Arc<Derivations>,
    /// Serializes turns: `try_lock_owned` fails while a turn is streaming, so
    /// a second message answers 409 instead of queuing behind the lease.
    pub turn_gate: Arc<tokio::sync::Mutex<()>>,
    world_dir: PathBuf,
    last_used: Mutex<Instant>,
}

impl DemoSession {
    pub fn touch(&self) {
        *self.last_used.expect_lock() = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_used.expect_lock().elapsed()
    }
}

impl Drop for DemoSession {
    fn drop(&mut self) {
        // Best effort: a leftover world directory is disk, not state.
        let _ = std::fs::remove_dir_all(&self.world_dir);
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
    #[error("assembling the mediator: {0}")]
    Mediator(#[from] InitError),
}

/// The models the playground may spend a visitor's key on — the four the
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
    live: Mutex<HashMap<String, Arc<DemoSession>>>,
}

impl Sessions {
    pub fn new(seed_data_root: PathBuf, worlds_root: PathBuf, ttl: Duration) -> Sessions {
        Sessions {
            seed_data_root,
            worlds_root,
            ttl,
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
        let chain = checked.config.registry_config().trust_chain.clone();
        let tool_names: Vec<String> = checked
            .config
            .registry_config()
            .tools
            .iter()
            .map(|tool| tool.name.as_str().to_string())
            .collect();

        let id = session_id();
        let world_dir = self.worlds_root.join(&id);
        let data_root = world_dir.join("data");
        copy_dir(&self.seed_data_root, &data_root)?;

        let approvals = Arc::new(Approvals::default());
        let derivations = Arc::new(Derivations::new(
            model.to_string(),
            checked
                .config
                .registry_config()
                .sanitizers
                .iter()
                .filter(|sanitizer| {
                    matches!(
                        checked.config.sanitizer_impl(&sanitizer.name),
                        Some(SanitizerImpl::Builtin(BuiltinSanitizer::Hosted))
                    )
                })
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
        let shim_url = format!("http://{address}/");

        let config = {
            let (bound, rebound) = bind_hosted(
                &checked.merged_toml,
                &format!("{shim_url}authority"),
                &format!("{shim_url}sanitizer"),
            )
            .map_err(|error| CreateError::Policy(PolicyError::Load(error.into())))?;
            if rebound == Rebound::default() {
                checked.config
            } else {
                Config::from_toml_str(&bound).map_err(|error| CreateError::Policy(PolicyError::Load(error)))?
            }
        };

        let client = HttpClient::loopback();
        let backends = tool_names
            .iter()
            .map(|name| {
                let backend =
                    ToolBackend::Http(HttpTool::new(shim_url.clone(), Duration::from_secs(15), client.clone()));
                (appa_runtime::ToolName::new(name.clone()), backend)
            })
            .collect();

        let head = TranscriptHead::new(vec![WireMessage::system(SYSTEM_PROMPT)])
            .expect("a one-message system head is a valid transcript head");
        let mediator = Arc::new(Mediator::with_tool_backends(config, backends)?.with_transcript_head(head));

        let tenant = TenantId::new(format!("playground-{id}"));
        let trajectory = mediator.create_session(tenant.clone());

        let session = Arc::new(DemoSession {
            id: id.clone(),
            tenant,
            trajectory,
            mediator,
            chain,
            model: model.to_string(),
            tool_count: checked.tool_count,
            approvals,
            derivations,
            turn_gate: Arc::new(tokio::sync::Mutex::new(())),
            world_dir,
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

    /// Drop every session idle past the TTL. The world directory goes with
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

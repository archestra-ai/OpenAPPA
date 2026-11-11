//! The history Dogwood's temporal clauses read: one ordered list of request, response and
//! error events, kept in one JSON file and rewritten whole under an exclusive lock on its
//! sibling `.lock` file, so the annotator and recorder processes never interleave.
//!
//! A request event enters the history when its call is annotated and allowed, before the
//! runtime releases it, so a call annotated while another is still on its way counts that one
//! too. Its release record binds it to its dispatch; a batch that does not release it removes
//! it. Timestamps are whole seconds, the unit of Dogwood's windows, and strictly increase in
//! history order as Dogwood requires.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dogwood_language::{Authorizer, Event, LoweredPolicySet, Response, Value};
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub events: Vec<Entry>,
    /// Every dispatch whose outcome the history holds. Delivery is at least once, and a
    /// runtime that restarts delivers each family from its start.
    #[serde(default)]
    pub settled: BTreeSet<String>,
    /// Every unreleased proposal already removed, by its trajectory, batch and call, so a
    /// replayed refusal never removes a later identical proposal.
    #[serde(default)]
    pub given_back: BTreeSet<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    pub tool: String,
    pub arguments: serde_json::Value,
    /// Seconds since the epoch.
    pub at: i64,
    /// A request: the history digest its annotation judged, which its records carry back.
    #[serde(default)]
    pub evidence: String,
    /// The dispatch the call was released as; a request without one is annotated and not yet
    /// released.
    #[serde(default)]
    pub dispatch: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Request,
    Response,
    Error,
}

impl Entry {
    pub fn pending(&self) -> bool {
        self.kind == Kind::Request && self.dispatch.is_none()
    }

    /// This entry as a Dogwood event: its action, its arguments as the `input` group of both
    /// the logged record and the Cedar request context, and the configured entities.
    fn event(&self, config: &Config) -> Result<Event, String> {
        let kind = match self.kind {
            Kind::Request => "request",
            Kind::Response => "response",
            Kind::Error => "error",
        };
        let mut builder = Event::builder(config.action(&self.tool)?, kind)
            .timestamp(self.at)
            .principal(&config.principal)
            .resource(&config.resource);
        let arguments = self.arguments.as_object().ok_or("a call's arguments are an object")?;
        for (name, argument) in arguments {
            builder = builder
                .field("input", name, value(argument))
                .request_context("input", name, value(argument));
        }
        Ok(builder.build())
    }
}

/// A JSON argument as a Cedar value. A number without an integer form becomes a decimal.
fn value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(flag) => Value::Bool(*flag),
        serde_json::Value::Number(number) => number
            .as_i64()
            .map(Value::Int)
            .unwrap_or_else(|| Value::Decimal(number.to_string())),
        serde_json::Value::String(text) => Value::String(text.clone()),
        serde_json::Value::Array(items) => Value::Array(items.iter().map(value).collect()),
        serde_json::Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), value(field)))
                .collect(),
        ),
    }
}

impl Store {
    /// Replay the history, then decide `candidate` as the next request.
    pub fn decide(&self, config: &Config, policies: LoweredPolicySet, candidate: &Entry) -> Result<Response, String> {
        let mut authorizer = Authorizer::new(policies);
        for entry in &self.events {
            authorizer.is_authorized(&entry.event(config)?);
        }
        authorizer
            .is_authorized(&candidate.event(config)?)
            .ok_or_else(|| "the event schema makes `request` no decision kind".to_string())
    }

    /// The timestamp of the next event: now, or one second after the last event.
    pub fn next_at(&self) -> i64 {
        let now = now() as i64;
        self.events.last().map_or(now, |last| now.max(last.at + 1))
    }

    /// Whether a request annotated within `timeout` still waits for its release or outcome.
    pub fn unsettled(&self, timeout: std::time::Duration) -> bool {
        let oldest = now() as i64 - timeout.as_secs() as i64;
        self.events.iter().any(|entry| {
            entry.kind == Kind::Request
                && entry.at >= oldest
                && entry
                    .dispatch
                    .as_ref()
                    .is_none_or(|dispatch| !self.settled.contains(dispatch))
        })
    }

    /// Drop every annotated call no record mentioned within `timeout`.
    pub fn expire(&mut self, timeout: std::time::Duration) {
        let oldest = now() as i64 - timeout.as_secs() as i64;
        self.events.retain(|entry| !entry.pending() || entry.at >= oldest);
    }

    /// A digest of the history, so the same call judged over two histories is two proposals.
    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(&self.events).expect("the history serializes");
        format!("history:{}:{:016x}", self.events.len(), fnv(&bytes))
    }
}

/// FNV-1a: the evidence needs only to tell histories apart, not to resist an adversary, since
/// the plugin writes every history it digests.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The store file, locked for one read-modify-write.
pub struct Locked {
    path: PathBuf,
    _lock: std::fs::File,
    pub store: Store,
}

impl Locked {
    pub fn open(path: &Path) -> Result<Locked, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let lock_path = path.with_extension("lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|error| format!("{}: {error}", lock_path.display()))?;
        lock.lock()
            .map_err(|error| format!("{}: {error}", lock_path.display()))?;
        let store = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        Ok(Locked {
            path: path.to_path_buf(),
            _lock: lock,
            store,
        })
    }

    /// Replace the store file with this state; the lock is released on drop.
    pub fn save(self) -> Result<(), String> {
        let temporary = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(&self.store).expect("the store serializes");
        let mut file =
            std::fs::File::create(&temporary).map_err(|error| format!("{}: {error}", temporary.display()))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("{}: {error}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path).map_err(|error| format!("{}: {error}", self.path.display()))
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

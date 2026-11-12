//! The plugin's durable state: the label each recorded file carries, the files each annotated
//! call reserved until its records settle them, and every dispatch already settled. One JSON
//! file, read and rewritten whole under an exclusive lock on its sibling `.lock` file, so the
//! annotator, the recorder and SafeBash — three processes — never interleave.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::label::{Label, Ranks};
use crate::paths::{covers, tree};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    /// The trust ranks of the policy that last consulted the plugin, lowest first.
    #[serde(default)]
    pub ranks: Ranks,
    /// Every file a recorded call wrote: its label and the version the write left.
    #[serde(default)]
    pub files: BTreeMap<PathBuf, Recorded>,
    #[serde(default)]
    pub reservations: Vec<Reservation>,
    /// Every dispatch whose outcome the store holds. Delivery is at least once and a runtime
    /// that restarts delivers each family from its start, so a settled dispatch is skipped.
    #[serde(default)]
    pub settled: BTreeSet<String>,
    /// Every unreleased proposal already given back, by its trajectory, batch and call: a
    /// replayed refusal never takes the reservation of a later, identical proposal.
    #[serde(default)]
    pub given_back: BTreeSet<String>,
    /// The reservations given back, kept for a day: the runtime releases a call it refused
    /// once a remedy is accepted without consulting the plugin again, and that release
    /// settles on what the call saw when it was annotated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returned: Vec<Reservation>,
    /// What the history of each git directory a call read holds, by its directory: walking
    /// a long history on every call would stall the agent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub histories: BTreeMap<PathBuf, History>,
}

/// The label of every path a repository's history reached, as the path rules give it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct History {
    /// The digest of the refs and reflogs walked, the path rules and the trust ranks: a
    /// history walked at another key is walked again.
    pub key: String,
    pub label: Label,
}

/// How long a given-back reservation is kept for a release that comes after a remedy.
const RETURNED_FOR_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recorded {
    pub label: Label,
    pub version: String,
}

/// The files one annotated call may change, each with the version it saw. Reserved from the
/// annotation until the call's outcome — or its batch's refusal to release it — settles them;
/// every other call on those files waits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reservation {
    pub tool: String,
    pub arguments: serde_json::Value,
    pub evidence: String,
    pub writes: BTreeMap<PathBuf, String>,
    /// What each directory output held at the annotation: every entry, with its [`stamp`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub trees: BTreeMap<PathBuf, BTreeMap<PathBuf, String>>,
    /// Seconds since the epoch at the annotation.
    pub created: u64,
    /// The dispatch the runtime released the call as, and the label it published.
    #[serde(default)]
    pub dispatch: Option<String>,
    #[serde(default)]
    pub published: Option<Label>,
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

impl Store {
    /// The reservation holding `path`, if any call holds it: one that reserved the path, a
    /// directory above it, or anything under it.
    pub fn holder(&self, path: &Path) -> Option<&Reservation> {
        self.reservations.iter().find(|reservation| {
            reservation
                .writes
                .keys()
                .any(|held| covers(held, path) || covers(path, held))
        })
    }

    /// Drop every reservation no record will settle: annotated longer than `timeout` ago,
    /// never released, and every file it reserved still at the version it saw. A reservation
    /// whose file changed keeps it — a release may still be on its way — until an operator
    /// releases it.
    pub fn expire(&mut self, timeout: std::time::Duration) {
        let now = now();
        self.returned
            .retain(|reservation| now.saturating_sub(reservation.created) < RETURNED_FOR_SECS);
        self.reservations.retain(|reservation| {
            reservation.dispatch.is_some()
                || now.saturating_sub(reservation.created) < timeout.as_secs()
                || reservation.writes.iter().any(|(path, seen)| &version(path) != seen)
        });
    }

    /// Settle one released call's outcome on the files it reserved. A success replaces the
    /// label of each file it changed with what the call published, and meets that label into
    /// a file it left unchanged, whose old content may be what it held. A failure leaves a
    /// file it did not change as it was, and meets the published label into one it did change.
    pub fn settle(&mut self, reservation: Reservation, success: bool) {
        let published = reservation
            .published
            .clone()
            .expect("a settled reservation was released with its published label");
        for (path, seen) in &reservation.writes {
            if let Some(before) = reservation.trees.get(path) {
                self.settle_directory(path, seen, before, &published, success);
                continue;
            }
            let now = version(path);
            if !success && &now == seen {
                continue;
            }
            let changed = &now != seen;
            let label = match (success && changed, self.files.get(path)) {
                (false, Some(previous)) => previous.label.meet(&published, &self.ranks),
                _ => published.clone(),
            };
            self.files.insert(path.clone(), Recorded { label, version: now });
        }
        if let Some(dispatch) = reservation.dispatch {
            self.settled.insert(dispatch);
        }
    }

    /// Settle a directory output on what the call changed in it, never on the directory as a
    /// whole: each file it created or changed takes the published label, as a file output
    /// does, and a directory it created takes it whole. A file it left as it was keeps its own
    /// label, so a build into the project leaves the sources it did not touch as they were.
    fn settle_directory(
        &mut self,
        directory: &Path,
        seen: &str,
        before: &BTreeMap<PathBuf, String>,
        published: &Label,
        success: bool,
    ) {
        let mut changed: Vec<PathBuf> = Vec::new();
        if seen == "absent" {
            if directory.exists() {
                changed.push(directory.to_path_buf());
            }
        } else {
            for (entry, metadata) in tree(directory).unwrap_or_default() {
                if changed
                    .iter()
                    .any(|created| created.is_dir() && covers(created, &entry))
                {
                    continue;
                }
                match before.get(&entry) {
                    None => changed.push(entry),
                    Some(stamped) if !metadata.is_dir() && stamped != &stamp(&metadata) => changed.push(entry),
                    Some(_) => {}
                }
            }
        }
        for path in changed {
            let label = match (success, self.files.get(&path)) {
                (false, Some(previous)) => previous.label.meet(published, &self.ranks),
                _ => published.clone(),
            };
            let version = version(&path);
            self.files.insert(path, Recorded { label, version });
        }
    }
}

/// An entry's kind, size and modification time: what tells a changed entry of a directory
/// apart from one the call left as it was, without reading its content.
pub fn stamp(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{:?}\t{}\t{modified}", metadata.file_type(), metadata.len())
}

/// A file's version: the digest of its content, or `absent`. A directory's is the digest of
/// what it holds: each entry's path, kind, size and modification time. A socket or another
/// special file has no content to version.
pub fn version(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let Ok(entries) = tree(path) else {
                return "unknown".to_string();
            };
            let mut lines: Vec<String> = entries
                .iter()
                .map(|(entry, metadata)| format!("{}\t{}", entry.display(), stamp(metadata)))
                .collect();
            lines.sort();
            digest_lines(&lines)
        }
        Ok(metadata) if !metadata.is_file() => "special".to_string(),
        _ => match std::fs::read(path) {
            Ok(bytes) => format!("sha256:{}", hex(&Sha256::digest(&bytes))),
            Err(_) => "absent".to_string(),
        },
    }
}

pub fn digest_lines(lines: &[String]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(lines.join("\n").as_bytes())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_success_relabels_only_the_files_it_changed() {
        let dir = std::env::temp_dir().join(format!("appa-file-tracker-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "secret copy").unwrap();
        let private = Label {
            trust: None,
            audience: vec![vec!["self".to_string()]],
        };
        let settle_public = |store: &mut Store| {
            store.settle(
                Reservation {
                    tool: "mcp/file-tracker/SafeBash".to_string(),
                    arguments: serde_json::Value::Null,
                    evidence: String::new(),
                    writes: [(file.clone(), version(&file))].into(),
                    trees: Default::default(),
                    created: 0,
                    dispatch: None,
                    published: Some(Label::default()),
                },
                true,
            );
        };
        let mut store = Store::default();
        store.files.insert(
            file.clone(),
            Recorded {
                label: private.clone(),
                version: version(&file),
            },
        );

        // Declared as an output but left as it was: its old content keeps its old label.
        settle_public(&mut store);
        assert_eq!(store.files[&file].label, private);

        // Rewritten between the annotation and the outcome: the new content is the call's.
        std::fs::write(&file, "public text").unwrap();
        store.settle(
            Reservation {
                tool: "mcp/file-tracker/SafeBash".to_string(),
                arguments: serde_json::Value::Null,
                evidence: String::new(),
                writes: [(file.clone(), "sha256:before".to_string())].into(),
                trees: Default::default(),
                created: 0,
                dispatch: None,
                published: Some(Label::default()),
            },
            true,
        );
        assert_eq!(store.files[&file].label, Label::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

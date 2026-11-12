//! The Annotator: one consult in, one annotation out. A read's `delta` is the file's label; an
//! edit forwards its predecessor's label, so the label the edit publishes keeps it; a write
//! forwards nothing, so the label it publishes replaces the one it destroys; SafeBash forwards
//! the meet of the labels of the inputs it sees — a file in a declared directory that a path
//! rule gives a restricted audience is hidden unless the call names it or git tracks it, and a
//! git directory carries the labels of what its history holds. The hosts SafeBash reaches add
//! nothing to its label: the policy judges them by the call's selector. Every file a call may
//! change is reserved until its records settle it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::config::{Config, PathRule, Role};
use crate::label::{Label, Ranks};
use crate::paths::{Named, covers, named, tree};
use crate::store::{History, Locked, Reservation, Store, digest_lines, now, stamp, version};

/// What the consult declares the answer may use.
struct Mandate {
    ranks: Ranks,
    audiences: Vec<String>,
}

pub fn annotate(config: &Config, request: &Value) -> Result<Value, String> {
    if request["kind"] != "annotation" {
        return Err("the file-tracker answers annotation consults only".to_string());
    }
    let call = &request["artifact"]["args"];
    let tool = call["name"]
        .as_str()
        .ok_or("the consult carries the complete call: declare the file-tracker annotator without inputs")?;
    let arguments = &call["arguments"];
    let role = config
        .role(tool)
        .ok_or_else(|| format!("the file-tracker serves no tool {tool}"))?;
    let named = named(role, arguments)?;
    let strings = |field: &str| -> Vec<String> {
        request["declaration"][field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let mandate = Mandate {
        ranks: Ranks(strings("trust_ranks")),
        audiences: strings("audiences"),
    };

    let deadline = Instant::now() + config.settle_wait;
    let mut locked = loop {
        let mut locked = Locked::open(&config.store)?;
        locked.store.ranks = mandate.ranks.clone();
        locked.store.expire(config.reservation_timeout);
        match named
            .reads
            .iter()
            .chain(&named.writes)
            .find(|path| locked.store.holder(path).is_some())
        {
            None => break locked,
            Some(path) if Instant::now() >= deadline => {
                return Err(format!(
                    "{} is reserved by a call whose records have not settled; propose this call again",
                    path.display()
                ));
            }
            Some(_) => {
                drop(locked);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };

    // A download cache the call writes without declaring it is created empty, so what its
    // record labels is what the call downloads, never the cache whole.
    let caches: Vec<&PathBuf> = named
        .directories
        .iter()
        .filter(|directory| !named.reads.contains(directory))
        .collect();
    for cache in &caches {
        std::fs::create_dir_all(cache).map_err(|error| format!("{}: {error}", cache.display()))?;
    }
    let toolchains = crate::safebash::toolchains(config);
    let covered = |path: &PathBuf| toolchains.iter().any(|toolchain| covers(toolchain, path));
    let mut inputs = Label::default();
    for path in &named.reads {
        // A toolchain is readable to every command and assumed to hold no credentials: what
        // a call reads there carries only what recorded writes left on it, above it or under it.
        let label = match covered(path) {
            true => {
                let store = &locked.store;
                store
                    .files
                    .iter()
                    .filter(|(recorded, _)| covers(recorded, path) || covers(path, recorded))
                    .fold(Label::default(), |met, (_, recorded)| {
                        met.meet(&recorded.label, &store.ranks)
                    })
            }
            false => file_label(config, &mut locked.store, path, &named.reads)?,
        };
        inputs = inputs.meet(&label, &locked.store.ranks);
    }
    let store = &locked.store;
    // A download cache the call writes without declaring it shows the command what it holds
    // but the files recorded there with a label: it carries what was recorded on it or above.
    for cache in &caches {
        inputs = inputs.meet(&recorded_label(store, cache), &store.ranks);
    }
    let (delta, forwards) = match role.role {
        Role::Read => (inputs, Label::default()),
        Role::Write => (Label::default(), Label::default()),
        Role::Edit => (inputs.clone(), inputs),
        Role::Run => (inputs.clone(), inputs),
    };
    // A directory output writes what it holds, so the rules of every file in it apply, but
    // for the files a rule guards for writing: the sandbox keeps those as they are unless
    // the call names them. A file it creates is held to no rule; it carries the label the
    // call publishes. What the directory holds now is kept, so its record labels only what
    // the call changes.
    let mut written = named.writes.clone();
    let mut trees = std::collections::BTreeMap::new();
    for directory in &named.directories {
        let entries = tree(directory)?;
        // No path rule applies in a toolchain or a download cache.
        if !covered(directory) {
            let kept = guarded(config, &entries, &named.writes);
            written.extend(
                entries
                    .iter()
                    .map(|(path, _)| path)
                    .filter(|path| !kept.iter().any(|kept| covers(kept, path)))
                    .cloned(),
            );
        }
        trees.insert(
            directory.clone(),
            entries
                .iter()
                .map(|(path, metadata)| (path.clone(), stamp(metadata)))
                .collect(),
        );
    }
    let rules = || {
        config
            .paths
            .iter()
            .filter(|rule| written.iter().any(|path| rule.matches(path)))
    };
    let mut write_trust = None;
    for rank in rules().filter_map(|rule| rule.write_trust.as_deref()) {
        write_trust = store.ranks.higher(write_trust, Some(required_rank(rank, &mandate)?));
    }
    let mut attention: Vec<&str> = rules()
        .flat_map(|rule| rule.write_attention.iter().map(String::as_str))
        .collect();
    attention.sort();
    attention.dedup();

    let evidence = evidence(role.role, &named);
    let mut requires = json!({ "history": [], "attention": attention });
    if let Some(trust) = write_trust {
        requires["trust"] = json!(trust);
    }
    let answer = json!({
        "version": 1,
        "answer": {
            "delta": answered(&delta, &mandate)?,
            "forwards": answered(&forwards, &mandate)?,
            "requires": requires,
            "emits": [],
            "evidence": evidence,
        }
    });

    if !named.writes.is_empty() {
        locked.store.reservations.push(Reservation {
            tool: tool.to_string(),
            arguments: arguments.clone(),
            evidence,
            writes: named.writes.iter().map(|path| (path.clone(), version(path))).collect(),
            trees,
            created: now(),
            dispatch: None,
            published: None,
        });
    }
    locked.save()?;
    Ok(answer)
}

/// A file's label: what recorded writes left on it or on a directory above it, met with every
/// path rule that matches it — and, for a directory, the own label of everything under it the
/// call sees. An entry [`concealed`] from a call that names `reads` adds nothing. A git
/// directory the path lies in, or one among the entries the call sees, adds what its history
/// holds ([`history_label`]).
pub fn file_label(config: &Config, store: &mut Store, path: &Path, reads: &[PathBuf]) -> Result<Label, String> {
    let mut label = config
        .paths
        .iter()
        .filter(|rule| rule.matches(path))
        .map(PathRule::label)
        .fold(recorded_label(store, path), |met, label| met.meet(&label, &store.ranks));
    let mut repositories: Vec<PathBuf> = path
        .ancestors()
        .find(|dir| is_git_directory(dir))
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    if path.is_dir() {
        let entries = tree(path)?;
        let hidden = concealed(config, store, path, &entries, reads, Concealing::Restricted);
        for (entry, _) in &entries {
            if !hidden.iter().any(|hidden| covers(hidden, entry)) {
                label = label.meet(&own_label(config, store, entry), &store.ranks);
                repositories.extend(
                    entry
                        .parent()
                        .filter(|dir| entry.ends_with("HEAD") && is_git_directory(dir))
                        .map(Path::to_path_buf),
                );
            }
        }
    }
    repositories.sort();
    repositories.dedup();
    for git in &repositories {
        let held = history_label(config, store, git);
        label = label.meet(&held, &store.ranks);
    }
    Ok(label)
}

/// Whether `dir` is a git directory, such as a repository's `.git` or a bare repository.
fn is_git_directory(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir()
}

/// What a git directory holds: the label the path rules give each path its index holds, or
/// any commit its refs, reflogs and stash reach adds, and each directory above one. A
/// credential the working tree hides, or no longer has, is still in the repository, and a
/// command that reads the git directory can read it. A git directory git cannot read holds
/// what every rule labels.
fn history_label(config: &Config, store: &mut Store, git: &Path) -> Label {
    let ranks = store.ranks.clone();
    let rules: Vec<&PathRule> = config
        .paths
        .iter()
        .filter(|rule| rule.label() != Label::default())
        .collect();
    // The paths are the working tree's, which a `.git` directory sits at the top of.
    let base = match git.file_name() == Some(".git".as_ref()) {
        true => git.parent().unwrap_or(git),
        false => git,
    };
    let labeled = |names: &[u8]| {
        let mut paths = std::collections::BTreeSet::new();
        for name in names.split(|byte| *byte == 0).filter(|name| !name.is_empty()) {
            let path = base.join(String::from_utf8_lossy(name).as_ref());
            paths.extend(path.ancestors().take_while(|dir| *dir != base).map(Path::to_path_buf));
        }
        let matching = paths
            .iter()
            .flat_map(|path| rules.iter().filter(move |rule| rule.matches(path)));
        matching.fold(Label::default(), |met, rule| met.meet(&rule.label(), &ranks))
    };
    let read = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("--git-dir")
            .arg(git)
            // The repository's own configuration cannot make this read run a program.
            .args(["-c", "core.fsmonitor=false"])
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.stdout)
    };
    let everything = || {
        rules
            .iter()
            .fold(Label::default(), |met, rule| met.meet(&rule.label(), &ranks))
    };
    let (Some(index), Some(tips)) = (
        read(&["ls-files", "-z"]),
        read(&["rev-list", "--no-walk", "--all", "--reflog"]),
    ) else {
        return everything();
    };
    let mut lines: Vec<String> = String::from_utf8_lossy(&tips).lines().map(str::to_string).collect();
    lines.sort();
    lines.extend(
        rules
            .iter()
            .map(|rule| format!("{}\0{:?}\0{:?}", rule.pattern, rule.except, rule.label())),
    );
    lines.extend(ranks.0.iter().map(|rank| format!("rank\0{rank}")));
    let key = digest_lines(&lines);
    let walked = match store.histories.get(git).filter(|history| history.key == key) {
        Some(history) => history.label.clone(),
        None => {
            let log = [
                "log",
                "--all",
                "--reflog",
                "--format=",
                "--name-only",
                "--no-renames",
                "--diff-filter=A",
                "-z",
            ];
            let Some(names) = read(&log) else {
                return everything();
            };
            let label = labeled(&names);
            store.histories.insert(
                git.to_path_buf(),
                History {
                    key,
                    label: label.clone(),
                },
            );
            label
        }
    };
    walked.meet(&labeled(&index), &ranks)
}

/// Which files under a directory a command reads stay out of its sight unless it names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Concealing {
    /// Each file a path rule gives a restricted audience, such as `.env`: a model's call
    /// reads a credential only by naming it. A file labeled by the writes recorded on it
    /// stays in sight and its label counts, so a session reads back what it wrote. A file the
    /// git repository tracks, or a directory that holds one, stays in sight too and its label
    /// counts: the repository holds its content, and git cannot work around a hidden one.
    Restricted,
    /// Each file whose label restricts its audience: a command the person types at the
    /// prompt reaches no hook that would label its result. A label that only lowers trust
    /// does not hide a file, since the person who types the command vouches for it.
    Labeled,
}

impl Concealing {
    /// Whether a file recorded with `label` stays hidden from a call that reads it without
    /// counting its label, as a call reads a toolchain or a download cache it does not
    /// declare: for a model's call every label that restricts the audience or lowers trust,
    /// for a typed command every label that restricts the audience.
    pub fn hides(self, label: &Label, ranks: &Ranks) -> bool {
        let lowered = label.trust.as_ref().is_some_and(|trust| Some(trust) != ranks.0.last());
        !label.audience.is_empty() || (self == Concealing::Restricted && lowered)
    }
}

/// The entries under `root`, the directory a call reads, that the call does not see, each with
/// everything under it: those whose own label `concealing` names, unless the call names the
/// entry or a path under it. `entries` lists each directory ahead of what it holds, as
/// [`tree`] does.
pub fn concealed(
    config: &Config,
    store: &Store,
    root: &Path,
    entries: &[(PathBuf, std::fs::Metadata)],
    reads: &[PathBuf],
    concealing: Concealing,
) -> Vec<PathBuf> {
    let mut hidden: Vec<PathBuf> = Vec::new();
    let mut tracked_here = None;
    for (entry, _) in entries {
        if hidden.iter().any(|hidden| covers(hidden, entry)) || reads.iter().any(|read| covers(entry, read)) {
            continue;
        }
        let conceals = match concealing {
            Concealing::Restricted => {
                config
                    .paths
                    .iter()
                    .any(|rule| rule.matches(entry) && rule.audience.is_some())
                    && !tracked_here
                        .get_or_insert_with(|| tracked(root))
                        .range(entry.clone()..)
                        .next()
                        .is_some_and(|file| covers(entry, file))
            }
            Concealing::Labeled => !own_label(config, store, entry).audience.is_empty(),
        };
        if conceals {
            hidden.push(entry.clone());
        }
    }
    hidden
}

/// The files git tracks under `root`, by absolute path, in path order, so that the files under
/// one directory follow it: none when `root` is in no repository git opens. A credential the
/// repository tracks is in its history, so a command that reads the repository can read it
/// whether or not the working tree hides it, and hiding the file only breaks git.
fn tracked(root: &Path) -> std::collections::BTreeSet<PathBuf> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        // The repository's own configuration cannot make this read run a program.
        .args(["-c", "core.fsmonitor=false", "ls-files", "-z"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|name| !name.is_empty())
                .map(|name| root.join(String::from_utf8_lossy(name).as_ref()))
                .collect()
        })
        .unwrap_or_default()
}

/// The entries of a directory output a command keeps as they are, each with everything under
/// it: those a path rule guards for writing, unless the call names the entry or a path under it.
pub fn guarded(config: &Config, entries: &[(PathBuf, std::fs::Metadata)], writes: &[PathBuf]) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    for (entry, _) in entries {
        if kept.iter().any(|kept| covers(kept, entry)) || writes.iter().any(|write| covers(entry, write)) {
            continue;
        }
        if config
            .paths
            .iter()
            .any(|rule| rule.matches(entry) && (rule.write_trust.is_some() || !rule.write_attention.is_empty()))
        {
            kept.push(entry.clone());
        }
    }
    kept
}

/// What recorded writes left on `path` or on a directory above it.
pub fn recorded_label(store: &Store, path: &Path) -> Label {
    store
        .files
        .iter()
        .filter(|(recorded, _)| covers(recorded, path))
        .fold(Label::default(), |met, (_, recorded)| {
            met.meet(&recorded.label, &store.ranks)
        })
}

/// A path's own label: every path rule that matches it and what the store recorded on it.
fn own_label(config: &Config, store: &Store, path: &Path) -> Label {
    config
        .paths
        .iter()
        .filter(|rule| rule.matches(path))
        .map(PathRule::label)
        .chain(store.files.get(path).map(|recorded| recorded.label.clone()))
        .fold(Label::default(), |met, label| met.meet(&label, &store.ranks))
}

/// What the call was judged over: each file's version, so the same call over changed files is
/// a different proposal.
fn evidence(role: Role, named: &Named) -> String {
    match role {
        Role::Read => version(&named.reads[0]),
        Role::Write | Role::Edit => version(&named.writes[0]),
        Role::Run => {
            let mut lines: Vec<String> = named
                .reads
                .iter()
                .map(|path| format!("in\t{}\t{}", path.display(), version(path)))
                .chain(
                    named
                        .writes
                        .iter()
                        .map(|path| format!("out\t{}\t{}", path.display(), version(path))),
                )
                .chain(named.hosts.iter().map(|host| format!("host\t{host}")))
                .collect();
            lines.sort();
            digest_lines(&lines)
        }
    }
}

/// A label as an answer's `delta` or `forwards`. A rank the mandate does not hold answers as
/// its lowest; an audience must be one clause of declared entries, or the annotation fails.
fn answered(label: &Label, mandate: &Mandate) -> Result<Value, String> {
    let mut answer = json!({});
    if let Some(trust) = &label.trust {
        answer["trust"] = json!(answered_rank(trust, mandate)?);
    }
    match label.audience.as_slice() {
        [] => {}
        [clause] => {
            if let Some(entry) = clause.iter().find(|entry| !mandate.audiences.contains(entry)) {
                return Err(format!(
                    "the stored audience entry {entry:?} is outside the policy's audiences"
                ));
            }
            answer["audience"] = json!(clause);
        }
        _ => return Err("the stored audience is an intersection no single answer can carry".to_string()),
    }
    Ok(answer)
}

/// A required trust floor. A rank the mandate does not hold answers as its highest: a floor
/// fails closed where a label fails open.
fn required_rank<'a>(rank: &'a str, mandate: &'a Mandate) -> Result<&'a str, String> {
    match mandate.ranks.0.iter().any(|known| known == rank) {
        true => Ok(rank),
        false => mandate
            .ranks
            .0
            .last()
            .map(String::as_str)
            .ok_or_else(|| "the consult declares no trust rank".to_string()),
    }
}

fn answered_rank<'a>(rank: &'a str, mandate: &'a Mandate) -> Result<&'a str, String> {
    match mandate.ranks.0.iter().any(|known| known == rank) {
        true => Ok(rank),
        false => mandate
            .ranks
            .lowest()
            .ok_or_else(|| "the consult declares no trust rank".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write floor names a rank this policy's chain lacks — the built-in `.env` rule says
    /// `trusted` — beside one it holds. The answer requires the chain's highest rank, whatever
    /// order the rules match in, never the lowest.
    #[test]
    fn a_write_floor_the_chain_does_not_name_requires_its_highest_rank() {
        let dir = std::env::temp_dir().join(format!("appa-ft-floor-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("file-tracker.toml");
        std::fs::write(
            &config_path,
            format!(
                "store = {:?}\n[[path]]\npattern = \"*/.env*\"\nwrite_trust = \"low\"\n",
                dir.join("store.json").display().to_string()
            ),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let request = json!({
            "kind": "annotation",
            "artifact": { "args": {
                "name": "host/claude-code/Write",
                "arguments": { "file_path": dir.join(".env").display().to_string(), "content": "" },
            }},
            "declaration": { "trust_ranks": ["low", "high"], "audiences": ["self"] },
        });
        let answer = annotate(&config, &request).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(answer["answer"]["requires"]["trust"], "high");
    }

    /// A repository read beside an untracked `.env` keeps the session's audience, since the
    /// command does not see it. Once git tracks it, or a file in a credential directory, the
    /// command sees it, as git must, and the read carries its `self` audience.
    #[test]
    fn a_credential_git_tracks_is_seen_and_labels_the_read() {
        let dir = std::env::temp_dir().join(format!("appa-ft-tracked-{}-{}", std::process::id(), now()));
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("src/credentials")).unwrap();
        std::fs::write(repo.join("src/credentials/mod.rs"), "").unwrap();
        std::fs::write(repo.join(".env"), "TOKEN=x\n").unwrap();
        let config_path = dir.join("file-tracker.toml");
        std::fs::write(
            &config_path,
            format!("store = {:?}\n", dir.join("store.json").display().to_string()),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        let read = |path: &Path| {
            let mut store = Store::default();
            let entries = tree(path).unwrap();
            let reads = [path.to_path_buf()];
            let mut hidden = concealed(&config, &store, path, &entries, &reads, Concealing::Restricted);
            hidden.sort();
            (
                hidden,
                file_label(&config, &mut store, path, &reads)
                    .unwrap()
                    .audience
                    .is_empty(),
            )
        };
        git(&["init", "-q"]);
        let untracked = read(&repo);
        git(&["add", "src/credentials/mod.rs"]);
        let directory = read(&repo);
        git(&["add", ".env"]);
        let tracked = read(&repo);
        let below = read(&repo.join("src"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(untracked, (vec![repo.join(".env"), repo.join("src/credentials")], true));
        assert_eq!(directory, (vec![repo.join(".env")], false));
        assert_eq!(tracked, (vec![], false));
        assert_eq!(below, (vec![], false));
    }

    /// The built-in rules make the Databricks CLI's token cache and a project's
    /// `settings.local.json` `self` data, whether Read or a shell command that declares the
    /// file reads them. A project's committed `settings.json`, which everyone who clones it
    /// reads, and a skill beside it stay unlabeled.
    #[test]
    fn credential_files_beside_a_config_directory_read_as_self() {
        let dir = std::env::temp_dir().join(format!("appa-ft-self-{}-{}", std::process::id(), now()));
        let config_path = dir.join("file-tracker.toml");
        let path = |relative: &str| {
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{}\n").unwrap();
            path.display().to_string()
        };
        let files = [
            path("home/.databricks/token-cache.json"),
            path("project/.claude/settings.local.json"),
        ];
        let shared = [
            path("project/.claude/settings.json"),
            path("project/.claude/skills/review/SKILL.md"),
        ];
        std::fs::write(
            &config_path,
            format!("store = {:?}\n", dir.join("store.json").display().to_string()),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let ask = |name: &str, arguments: Value| {
            let request = json!({
                "kind": "annotation",
                "artifact": { "args": { "name": name, "arguments": arguments } },
                "declaration": { "trust_ranks": ["suspicious", "trusted"], "audiences": ["self", "internal"] },
            });
            annotate(&config, &request).unwrap()["answer"]["delta"]["audience"].clone()
        };
        let read = |file: &str| ask("host/claude-code/Read", json!({ "file_path": file }));
        let run = |file: &str| {
            ask(
                "host/claude-code/Bash",
                json!({ "command": "cat", "input_files": [file], "output_files": [], "network_hosts": [] }),
            )
        };
        let labels: Vec<_> = files.iter().map(|file| (read(file), run(file))).collect();
        let unlabeled: Vec<_> = shared.iter().map(|file| (read(file), run(file))).collect();
        let _ = std::fs::remove_dir_all(&dir);
        for (file, (read, run)) in files.iter().zip(labels) {
            assert_eq!(read, json!(["self"]), "Read {file}");
            assert_eq!(run, json!(["self"]), "Bash {file}");
        }
        for (file, labels) in shared.iter().zip(unlabeled) {
            assert_eq!(labels, (Value::Null, Value::Null), "{file} holds no credential");
        }
    }

    /// A credential committed and then removed is still in the repository's history: a read
    /// that holds the git directory, or reads it alone, carries its `self` audience, and a read
    /// of the working tree beside it does not. A history that gains one after it was walked
    /// is walked again.
    #[test]
    fn a_read_of_git_carries_the_credentials_its_history_holds() {
        let dir = std::env::temp_dir().join(format!("appa-ft-history-{}-{}", std::process::id(), now()));
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/main.rs"), "").unwrap();
        let config_path = dir.join("file-tracker.toml");
        std::fs::write(
            &config_path,
            format!("store = {:?}\n", dir.join("store.json").display().to_string()),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        let mut store = Store::default();
        let mut restricted = |path: &Path| {
            !file_label(&config, &mut store, path, &[path.to_path_buf()])
                .unwrap()
                .audience
                .is_empty()
        };
        git(&["init", "-q"]);
        git(&["add", "src/main.rs"]);
        git(&["commit", "-qm", "code"]);
        let clean = (restricted(&repo), restricted(&repo.join(".git")));
        std::fs::write(repo.join(".env"), "TOKEN=x\n").unwrap();
        git(&["add", ".env"]);
        git(&["commit", "-qm", "credential"]);
        git(&["rm", "-q", ".env"]);
        git(&["commit", "-qm", "removed"]);
        let removed = (
            restricted(&repo),
            restricted(&repo.join(".git")),
            restricted(&repo.join(".git/objects")),
            restricted(&repo.join("src")),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(clean, (false, false));
        assert_eq!(removed, (true, true, true, false));
    }
}

//! What a call names, as the plugin keys it: absolute paths resolved through every symbolic
//! link, so two spellings of one file are one file. The annotator, the recorder and SafeBash
//! resolve alike, so the label a call was judged under is the label of the file it touches.
//!
//! A `run` call may name a directory: in `input_files`, the files under it are its inputs; in
//! `output_files`, written with a trailing `/` or already a directory, the call reads what the
//! directory holds and writes under it, as an edit does a file. An output file that exists is
//! read as well.

use std::path::{Path, PathBuf};

use crate::config::{Role, ToolRole};

/// The files and hosts one call names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Named {
    /// Files whose content the call returns or carries into its outputs.
    pub reads: Vec<PathBuf>,
    /// Files the call may change, its outputs.
    pub writes: Vec<PathBuf>,
    /// The outputs that are directories. Each is among the reads as well.
    pub directories: Vec<PathBuf>,
    pub hosts: Vec<String>,
}

/// How many entries a directory a call names may hold. A larger tree is refused: walking it
/// for its label and version on every call would stall the agent.
const TREE_LIMIT: usize = 250_000;

/// Every entry under `root`, without following symbolic links, with its metadata. `root`
/// itself is not listed. A tree past [`TREE_LIMIT`] entries is refused.
pub fn tree(root: &Path) -> Result<Vec<(PathBuf, std::fs::Metadata)>, String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let Ok(metadata) = entry.path().symlink_metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(entry.path());
            }
            found.push((entry.path(), metadata));
            if found.len() > TREE_LIMIT {
                return Err(format!(
                    "{} holds more than {TREE_LIMIT} entries: declare the directories or files the command uses",
                    root.display()
                ));
            }
        }
    }
    Ok(found)
}

/// Whether `path` is `dir` or lies under it.
pub fn covers(dir: &Path, path: &Path) -> bool {
    path.starts_with(dir)
}

/// An absolute path with every existing component resolved: an existing file resolves
/// whole, an absent one through its nearest existing directory, so an output may lie in a
/// directory the command creates. A relative path is refused: the plugin does not know the
/// harness's working directory.
pub fn resolve(written: &str) -> Result<PathBuf, String> {
    let path = Path::new(written);
    if !path.is_absolute() {
        return Err(format!("{written:?} is not an absolute path"));
    }
    let mut absent = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(resolved) = std::fs::canonicalize(existing) {
            return Ok(absent.iter().rev().fold(resolved, |path, name| path.join(name)));
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return Err(format!("{written:?} names no file"));
        };
        absent.push(name);
        existing = parent;
    }
}

/// Whether a declared entry names the call's own temporary directory, which every call gets
/// private and writable and never declares.
fn temporary(written: &str) -> bool {
    ["$TMPDIR", "${TMPDIR}"].iter().any(|name| {
        written
            .strip_prefix(name)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

pub fn named(role: &ToolRole, arguments: &serde_json::Value) -> Result<Named, String> {
    let one = || -> Result<PathBuf, String> {
        let field = role
            .path
            .as_deref()
            .ok_or_else(|| format!("{} names no path argument", role.name))?;
        let written = arguments[field]
            .as_str()
            .ok_or_else(|| format!("{} carries no {field} string", role.name))?;
        resolve(written)
    };
    let list = |field: &str| -> Result<Vec<String>, String> {
        match &arguments[field] {
            // A call that names no list declares nothing it could be confined to: Claude
            // Code drops these arguments from `Bash` unless `clappa`'s preload declares them.
            serde_json::Value::Null => Err(format!("{} names no {field} list", role.name)),
            serde_json::Value::Array(items) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{field} holds a non-string"))
                })
                .filter(|item| item.as_deref().map_or(true, |item| !temporary(item)))
                .collect(),
            _ => Err(format!("{field} is not a list")),
        }
    };
    Ok(match role.role {
        Role::Read => Named {
            reads: vec![one()?],
            ..Named::default()
        },
        Role::Write => Named {
            writes: vec![one()?],
            ..Named::default()
        },
        Role::Edit => {
            let path = one()?;
            Named {
                reads: vec![path.clone()],
                writes: vec![path],
                ..Named::default()
            }
        }
        Role::Run => {
            let mut hosts: Vec<String> = list("network_hosts")?.iter().map(|host| host.to_lowercase()).collect();
            hosts.sort();
            hosts.dedup();
            // An input that is not there is read as nothing: a probe such as `test -f` names
            // what may be absent.
            let mut reads: Vec<PathBuf> = list("input_files")?
                .iter()
                .map(|path| resolve(path))
                .filter(|path| path.as_ref().map_or(true, |path| path.symlink_metadata().is_ok()))
                .collect::<Result<_, _>>()?;
            let mut writes = Vec::new();
            let mut directories = Vec::new();
            // An output that exists is read as well: a command that rewrites a file, as
            // `npm install` does `package.json`, reads what it held, and its label counts.
            for written in list("output_files")? {
                let path = resolve(&written)?;
                if written.ends_with('/') || path.is_dir() {
                    directories.push(path.clone());
                    reads.push(path.clone());
                } else if path.symlink_metadata().is_ok() {
                    reads.push(path.clone());
                }
                writes.push(path);
            }
            reads.sort();
            reads.dedup();
            Named {
                reads,
                writes,
                directories,
                hosts,
            }
        }
    })
}

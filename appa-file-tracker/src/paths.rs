//! What a call names, as the plugin keys it: absolute paths resolved through every symbolic
//! link, so two spellings of one file are one file. The annotator, the recorder and SafeBash
//! resolve alike, so the label a call was judged under is the label of the file it touches.

use std::path::{Path, PathBuf};

use crate::config::{Role, ToolRole};

/// The files and hosts one call names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Named {
    /// Files whose content the call returns or carries into its outputs.
    pub reads: Vec<PathBuf>,
    /// Files the call may change, its outputs.
    pub writes: Vec<PathBuf>,
    pub hosts: Vec<String>,
}

/// An absolute path with every existing component resolved: an existing file resolves
/// whole, an absent one through its parent directory. A relative path is refused: the
/// plugin does not know the harness's working directory.
pub fn resolve(written: &str) -> Result<PathBuf, String> {
    let path = Path::new(written);
    if !path.is_absolute() {
        return Err(format!("{written:?} is not an absolute path"));
    }
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return Ok(resolved);
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(format!("{written:?} names no file"));
    };
    let parent = std::fs::canonicalize(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    Ok(parent.join(name))
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
                hosts: Vec::new(),
            }
        }
        Role::Run => {
            let mut hosts: Vec<String> = list("network_hosts")?.iter().map(|host| host.to_lowercase()).collect();
            hosts.sort();
            hosts.dedup();
            Named {
                reads: list("input_files")?
                    .iter()
                    .map(|path| resolve(path))
                    .collect::<Result<_, _>>()?,
                writes: list("output_files")?
                    .iter()
                    .map(|path| resolve(path))
                    .collect::<Result<_, _>>()?,
                hosts,
            }
        }
    })
}

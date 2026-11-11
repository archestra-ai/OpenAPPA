//! The person's say over recorded labels: `labels` lists what recorded writes left on the
//! files under a path, and `reset` removes it. A recorded label says what a file's content may
//! hold, and only the person can decide that it holds less, so no call resets a label: the
//! sandbox keeps the store read-only for every command that does not declare it, and a path
//! rule asks the person before any call writes it. Path rules are not recorded, so a reset
//! leaves a file such as `.env` at the label its rule gives it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::label::{Label, Ranks};
use crate::paths::{covers, resolve};
use crate::store::{Locked, Store};

/// The labels recorded under each path, the current directory when none is named.
pub fn labels(config: &Config, paths: &[String]) -> Result<String, String> {
    let roots = match paths {
        [] => vec![resolve_here(".")?],
        paths => paths.iter().map(|path| resolve_here(path)).collect::<Result<_, _>>()?,
    };
    let locked = open(config)?;
    let store = &locked.store;
    let mut out = String::new();
    for root in &roots {
        let recorded: Vec<(&PathBuf, &Label)> = store
            .files
            .iter()
            .filter(|(path, recorded)| covers(root, path) && restricts(&recorded.label, &store.ranks))
            .map(|(path, recorded)| (path, &recorded.label))
            .collect();
        let above = above(store, root);
        if recorded.is_empty() && above.is_empty() {
            out.push_str(&format!("{}: no file carries a recorded label\n", root.display()));
            continue;
        }
        out.push_str(&format!("{}:\n", root.display()));
        let mut rows: Vec<(String, String)> = above
            .iter()
            .map(|(path, label)| {
                (
                    describe(label),
                    format!("{} (covers everything under it)", path.display()),
                )
            })
            .collect();
        rows.extend(groups(root, &recorded, &store.ranks));
        let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
        for (label, path) in rows {
            out.push_str(&format!("  {label:width$}  {path}\n"));
        }
        out.push_str(&format!(
            "Reset them with: appa file-tracker reset {}\n",
            root.display()
        ));
    }
    Ok(out)
}

/// Remove every label recorded at or under each path. A label recorded on a directory above
/// a path covers it too; it stays, and the answer names it.
pub fn reset(config: &Config, paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("name the files or directories whose labels to reset, such as `.`".to_string());
    }
    let roots: Vec<PathBuf> = paths.iter().map(|path| resolve_here(path)).collect::<Result<_, _>>()?;
    let mut locked = open(config)?;
    let mut out = String::new();
    for root in &roots {
        let before = locked.store.files.len();
        locked.store.files.retain(|path, _| !covers(root, path));
        let removed = before - locked.store.files.len();
        out.push_str(&format!(
            "{}: reset {removed} recorded label{}\n",
            root.display(),
            if removed == 1 { "" } else { "s" }
        ));
        for (path, label) in above(&locked.store, root) {
            out.push_str(&format!(
                "  still {}: recorded on {}; reset it to clear everything under it\n",
                describe(&label),
                path.display()
            ));
        }
    }
    locked.save()?;
    out.push_str(
        "Path rules still label files such as .env. A session already running keeps its own label, \
         and what it writes next is labeled again.\n",
    );
    Ok(out)
}

/// What `clappa` says before a session in `directory` whose files carry recorded labels: its
/// first read of them narrows the session, and the person may want to reset them first.
pub fn note(config: &Config, directory: &Path) -> Option<String> {
    if !config.store.exists() {
        return None;
    }
    let directory = std::fs::canonicalize(directory).ok()?;
    let locked = Locked::open(&config.store).ok()?;
    let store = &locked.store;
    let labeled: Vec<&Label> = store
        .files
        .iter()
        .filter(|(path, recorded)| covers(&directory, path) && restricts(&recorded.label, &store.ranks))
        .map(|(_, recorded)| &recorded.label)
        .chain(store.files.iter().filter_map(|(path, recorded)| {
            (path != &directory && covers(path, &directory) && restricts(&recorded.label, &store.ranks))
                .then_some(&recorded.label)
        }))
        .collect();
    if labeled.is_empty() {
        return None;
    }
    let met = labeled
        .iter()
        .fold(Label::default(), |met, label| met.meet(label, &store.ranks));
    Some(format!(
        "{} recorded label{} under this directory ({}) came from earlier sessions; a read of those \
         files narrows this session. Review them with `appa file-tracker labels` and reset them with \
         `appa file-tracker reset <path>`.",
        labeled.len(),
        if labeled.len() == 1 { "" } else { "s" },
        describe(&met),
    ))
}

fn open(config: &Config) -> Result<Locked, String> {
    Locked::open(&config.store).map_err(|error| {
        format!("cannot open the label store ({error}). Run this in your own terminal: an agent's shell sees the store read-only")
    })
}

/// A path the person typed, relative to the current directory, resolved as the store keys it.
fn resolve_here(written: &str) -> Result<PathBuf, String> {
    let path = std::env::current_dir()
        .map_err(|error| format!("the current directory: {error}"))?
        .join(written);
    resolve(&path.to_string_lossy())
}

/// The restricting labels recorded on directories strictly above `root`.
fn above(store: &Store, root: &Path) -> Vec<(PathBuf, Label)> {
    store
        .files
        .iter()
        .filter(|(path, recorded)| {
            path.as_path() != root && covers(path, root) && restricts(&recorded.label, &store.ranks)
        })
        .map(|(path, recorded)| (path.clone(), recorded.label.clone()))
        .collect()
}

/// Whether a label says anything: an audience, or a trust below the chain's top.
fn restricts(label: &Label, ranks: &Ranks) -> bool {
    !label.audience.is_empty()
        || label
            .trust
            .as_deref()
            .is_some_and(|trust| Some(trust) != ranks.0.last().map(String::as_str))
}

/// One row per entry directly under `root`: a file as itself, a directory as the meet of what
/// it holds and how many labels that is.
fn groups(root: &Path, recorded: &[(&PathBuf, &Label)], ranks: &Ranks) -> Vec<(String, String)> {
    let mut grouped: BTreeMap<PathBuf, (Label, usize, bool)> = BTreeMap::new();
    for (path, label) in recorded {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let first: PathBuf = relative
            .components()
            .next()
            .map_or_else(|| PathBuf::from("."), |c| c.as_os_str().into());
        let entry = grouped.entry(first).or_insert((Label::default(), 0, false));
        entry.0 = entry.0.meet(label, ranks);
        entry.1 += 1;
        entry.2 |= relative.components().count() > 1 || path.is_dir();
    }
    grouped
        .into_iter()
        .map(|(first, (label, count, directory))| {
            let shown = match (directory, count) {
                (false, _) => first.display().to_string(),
                (true, 1) => format!("{}/ (1 label)", first.display()),
                (true, count) => format!("{}/ ({count} labels)", first.display()),
            };
            (describe(&label), shown)
        })
        .collect()
}

/// A label as the person reads it: `audience self, trust suspicious`.
fn describe(label: &Label) -> String {
    let audience = label
        .audience
        .iter()
        .map(|clause| clause.join(" or "))
        .collect::<Vec<_>>()
        .join(" and ");
    match (audience.is_empty(), &label.trust) {
        (false, Some(trust)) => format!("audience {audience}, trust {trust}"),
        (false, None) => format!("audience {audience}"),
        (true, Some(trust)) => format!("trust {trust}"),
        (true, None) => "no restriction".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Recorded;

    /// A reset clears what lies at or under the path it names, by whole path components, and
    /// leaves a directory label above it in place, named.
    #[test]
    fn a_reset_clears_the_named_tree_and_names_a_label_above_it() {
        let dir = std::env::temp_dir().join(format!("appa-ft-reset-{}-{}", std::process::id(), crate::store::now()));
        let project = dir.join("project");
        std::fs::create_dir_all(project.join("app")).unwrap();
        let project = std::fs::canonicalize(&project).unwrap();
        let config_path = dir.join("file-tracker.toml");
        std::fs::write(
            &config_path,
            format!("store = {:?}\n", dir.join("store.json").display().to_string()),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let private = Label {
            trust: Some("trusted".to_string()),
            audience: vec![vec!["self".to_string()]],
        };
        let mut locked = Locked::open(&config.store).unwrap();
        locked.store.ranks = Ranks(vec!["suspicious".to_string(), "trusted".to_string()]);
        for path in ["app/health.py", "application.py", ".venv"] {
            locked.store.files.insert(
                project.join(path),
                Recorded {
                    label: private.clone(),
                    version: "absent".to_string(),
                },
            );
        }
        locked.save().unwrap();

        let answer = reset(&config, &[project.join("app").display().to_string()]).unwrap();
        let below = reset(&config, &[project.join(".venv/lib/x.py").display().to_string()]).unwrap();
        let left: Vec<PathBuf> = Locked::open(&config.store)
            .unwrap()
            .store
            .files
            .keys()
            .cloned()
            .collect();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(answer.contains("reset 1 recorded label\n"), "{answer}");
        assert_eq!(left, vec![project.join(".venv"), project.join("application.py")]);
        assert!(below.contains("reset 0 recorded labels"), "{below}");
        assert!(
            below.contains(&format!(
                "still audience self, trust trusted: recorded on {}",
                project.join(".venv").display()
            )),
            "{below}"
        );
    }
}

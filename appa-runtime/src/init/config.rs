//! The policy file: seeding it, offering to replace an outdated one, and
//! establishing that the runtime can compose and serve what is on disk.

use crate::config::{Config, ConfigError};
use crate::default_config;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use super::receipt::friendly_path;
use super::{Answer, Confirmation, InitError};

/// What this init did to the config file, as the receipt reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigOutcome {
    /// Seeded from this build's default, because no config was there.
    Created,
    /// Left exactly as it was found.
    Kept,
    /// Replaced with this build's default, at the user's word, the previous
    /// file beside it.
    Rewritten,
}

impl ConfigOutcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ConfigOutcome::Created => "created",
            ConfigOutcome::Kept => "kept",
            ConfigOutcome::Rewritten => "rewritten",
        }
    }
}

/// The policy key of the config file this init validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ComposedPolicy {
    /// The key, comparable against the one a runtime serves.
    Key(String),
    /// A `token_env` resolves only where the runtime runs, so this process cannot compose
    /// the file at all. Not knowing is never the same as agreeing: the runtime may be
    /// serving anything, and only the person running init can settle it.
    Unknowable,
}

/// Remove a file this init wrote and abandons. Nothing it protects is lost
/// with it, so a failure is noted beside the error being returned.
pub(super) fn discard_file(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        tracing::warn!(path = %path.display(), %error, "cannot remove a file init abandoned");
    }
}

/// Seed the config from this build's default, or keep the one already there.
pub(super) fn create_default_config(path: &Path) -> Result<ConfigOutcome, InitError> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(ConfigOutcome::Kept),
        Err(source) => {
            return Err(InitError::WriteFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if let Err(source) = file
        .write_all(default_config::text().as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        discard_file(path);
        return Err(InitError::WriteFile {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(ConfigOutcome::Created)
}

/// The policy version this build's default config declares.
fn template_policy_version() -> i64 {
    policy_version(&default_config::text()).expect("the bundled default config declares an integer policy version")
}

/// The `[policy] version` of one config's own text, before any include composes.
fn policy_version(text: &str) -> Option<i64> {
    toml::from_str::<toml::Value>(text)
        .ok()?
        .get("policy")?
        .get("version")?
        .as_integer()
}

/// Find a backup name without replacing an earlier backup.
fn available_backup_path(path: &Path) -> PathBuf {
    let backup = path.with_extension("toml.bak");
    if !backup.exists() {
        return backup;
    }
    for number in 1.. {
        let candidate = path.with_extension(format!("toml.bak.{number}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("an unsigned integer always has another value")
}

/// Offer to replace a config authored against an older policy model.
///
/// The config is the user's, and init keeps it across every upgrade. A policy
/// version below this build's is the one mechanical signal that it was authored
/// against an older model than this build writes, so it is also the only drift
/// init asks about. Only a terminal is asked, and the answer defaults to no: a
/// rewrite discards every edit the file carries, the include lines that bind
/// batteries included, and keeps them only in the backup.
pub(super) fn offer_config_rewrite(path: &Path) -> Result<ConfigOutcome, InitError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(ConfigOutcome::Kept);
    }
    let stderr = std::io::stderr();
    offer_config_rewrite_with(path, &mut stdin.lock(), &mut stderr.lock())
}

fn offer_config_rewrite_with(
    path: &Path,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<ConfigOutcome, InitError> {
    let template = template_policy_version();
    let text = fs::read_to_string(path).map_err(|source| InitError::WriteFile {
        path: path.to_path_buf(),
        source,
    })?;
    // A config whose version is missing or unreadable is not old, it is broken:
    // `verify_config` refuses it next, naming the fault it actually has.
    match policy_version(&text) {
        Some(found) if found < template => {}
        _ => return Ok(ConfigOutcome::Kept),
    }
    let backup = available_backup_path(path);
    let rewrite = Confirmation {
        question: format!(
            "appa: {} uses an older policy format. This version of Appa uses policy version {template}.\n\
             Replace it with the new default policy? Your existing file will be backed up to {},\n\
             without replacing any existing backup.",
            friendly_path(path),
            friendly_path(&backup),
        ),
        default: Answer::No,
    };
    let prompt = |source| InitError::WriteFile {
        path: path.to_path_buf(),
        source,
    };
    if rewrite.ask(input, output).map_err(prompt)? == Answer::No {
        return Ok(ConfigOutcome::Kept);
    }
    // Reserve this unused name before moving the user's file. `rename` can replace a
    // destination, so reserving it makes that replacement safe and prevents an
    // existing backup from being lost.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .map_err(|source| InitError::WriteFile {
            path: backup.clone(),
            source,
        })?;
    // The original moves aside whole, so nothing here can leave a half-written
    // policy in place: the new file is written under `create_new` and removed
    // again if that write fails, and a failure puts the original back.
    if let Err(source) = fs::rename(path, &backup) {
        discard_file(&backup);
        return Err(InitError::WriteFile {
            path: backup.clone(),
            source,
        });
    }
    match create_default_config(path) {
        Ok(_) => Ok(ConfigOutcome::Rewritten),
        Err(written) => match fs::rename(&backup, path) {
            Ok(()) => Err(written),
            Err(source) => Err(InitError::WriteFile {
                path: path.to_path_buf(),
                source,
            }),
        },
    }
}

/// The config the runtime will be started against, put through the runtime's
/// own startup refusals first.
///
/// A config kept across upgrades drifts: an included battery moves ahead of the
/// policy version an earlier init wrote, an include is edited to an absolute
/// path, a hand-edited `Agent` row stops pinning the argument that keeps a
/// subagent's return observable. The runtime refuses each of those at startup,
/// which init can report only as an endpoint that never became healthy. Running
/// both refusals here names the file and the fault, before anything outside
/// this file has changed.
/// Answers with the policy key this file composes to, or [`ComposedPolicy::Unknowable`]
/// when the file resolves only where the runtime runs.
pub(super) fn verify_config(path: &Path) -> Result<ComposedPolicy, InitError> {
    let config = match Config::load(path) {
        Ok(config) => config,
        // A `token_env` resolves where the runtime runs, not here. A hook starts
        // it with the session's environment, which carries variables this
        // terminal does not, so a secret this process cannot see is not init's
        // to refuse: the start that follows is what proves the token reachable.
        Err(ConfigError::MissingSecret { .. }) => return Ok(ComposedPolicy::Unknowable),
        Err(source) => {
            return Err(InitError::UnloadableConfig {
                path: path.to_path_buf(),
                source: Box::new(source),
            });
        }
    };
    Ok(ComposedPolicy::Key(crate::engine::policy_file_key(
        config.policy_file().bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_the_runtime_could_not_compose_stops_init() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join("appa.toml");

        assert_eq!(
            create_default_config(&config).expect("the default config is written"),
            ConfigOutcome::Created
        );
        verify_config(&config).expect("the config init writes composes");

        let ahead = template_policy_version() + 1;
        fs::write(
            directory.path().join("battery.toml"),
            format!("[policy]\nversion = {ahead}\n"),
        )
        .expect("battery written");
        let stale = fs::read_to_string(&config).expect("the config is readable");
        fs::write(&config, format!("include = [\"battery.toml\"]\n{stale}")).expect("include written");

        match verify_config(&config) {
            Err(InitError::UnloadableConfig { source, .. }) => {
                assert!(matches!(*source, ConfigError::IncludedVersion { .. }));
            }
            other => panic!("a battery ahead of the root policy version must stop init: {other:?}"),
        }
    }

    /// A loadable config plus whatever `body` declares, at the path init keeps.
    fn config_declaring(directory: &Path, body: &str) -> PathBuf {
        let config = directory.join("appa.toml");
        let version = template_policy_version();
        fs::write(
            &config,
            format!("[policy]\nversion = {version}\n{body}\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n"),
        )
        .expect("the config is written");
        config
    }

    #[test]
    fn a_token_this_process_cannot_see_is_left_to_the_runtime() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = config_declaring(
            directory.path(),
            "[externals.sanitizers.scrub]\nurl = \"https://scrub.internal\"\ntoken_env = \"APPA_UNSET_IN_THIS_PROCESS\"\n",
        );

        assert!(std::env::var_os("APPA_UNSET_IN_THIS_PROCESS").is_none());
        verify_config(&config).expect("init does not judge a secret it cannot reach");
    }

    /// A config one policy version behind the build, at the path init keeps.
    fn outdated_config(directory: &Path) -> PathBuf {
        let config = directory.join("appa.toml");
        let older = template_policy_version() - 1;
        fs::write(&config, format!("[policy]\nversion = {older}\n")).expect("the config is written");
        config
    }

    fn answer_rewrite(config: &Path, answer: &str) -> (ConfigOutcome, String) {
        let mut prompt = Vec::new();
        let outcome =
            offer_config_rewrite_with(config, &mut answer.as_bytes(), &mut prompt).expect("the offer completes");
        (outcome, String::from_utf8(prompt).expect("the prompt is text"))
    }

    #[test]
    fn an_outdated_config_is_rewritten_only_on_an_explicit_yes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = outdated_config(directory.path());
        let authored = fs::read_to_string(&config).expect("the config is readable");
        let backup = directory.path().join("appa.toml.bak");

        for declined in ["", "\n", "n\n", "no\n"] {
            let (outcome, prompt) = answer_rewrite(&config, declined);
            assert!(!prompt.is_empty(), "the offer is shown");
            assert_eq!(outcome, ConfigOutcome::Kept);
            assert_eq!(fs::read_to_string(&config).ok(), Some(authored.clone()));
            assert!(!backup.exists(), "a declined offer writes nothing");
        }

        assert_eq!(answer_rewrite(&config, "y\n").0, ConfigOutcome::Rewritten);
        assert_eq!(
            fs::read_to_string(&config).ok(),
            Some(default_config::text().into_owned())
        );
        assert_eq!(fs::read_to_string(&backup).ok(), Some(authored));
        verify_config(&config).expect("the rewritten config composes");
    }

    #[test]
    fn a_current_config_is_never_offered_for_rewrite() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join("appa.toml");
        assert_eq!(
            create_default_config(&config).expect("the default config is written"),
            ConfigOutcome::Created
        );

        let (outcome, prompt) = answer_rewrite(&config, "y\n");
        assert!(prompt.is_empty(), "no offer is made");
        assert_eq!(outcome, ConfigOutcome::Kept);
        assert_eq!(
            fs::read_to_string(&config).ok(),
            Some(default_config::text().into_owned())
        );
        assert!(!directory.path().join("appa.toml.bak").exists());
    }
}

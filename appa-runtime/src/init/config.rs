//! The policy file: establishing that the runtime can compose and serve what
//! is on disk.

use crate::config::{Config, ConfigError};
use std::fs;
use std::path::Path;

use super::InitError;

/// The policy key of the config file this activation validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ComposedPolicy {
    /// The key, comparable against the one a runtime serves.
    Key(String),
    /// A `token_env` resolves only where the runtime runs, so this process cannot compose
    /// the file at all. Not knowing is never the same as agreeing: the runtime may be
    /// serving anything, and only a confirmed reload can settle it.
    Unknowable,
}

/// Remove a file this activation wrote and abandons. Nothing it protects is lost
/// with it, so a failure is noted beside the error being returned.
pub(super) fn discard_file(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        tracing::warn!(path = %path.display(), %error, "cannot remove a file the activation abandoned");
    }
}

/// The config the runtime will be started against, put through the runtime's
/// own startup refusals first.
///
/// A config kept across upgrades drifts: an included battery moves ahead of the
/// policy version an earlier install wrote, an include is edited to an absolute
/// path, a hand-edited `Agent` row stops pinning the argument that keeps a
/// subagent's return observable. The runtime refuses each of those at startup,
/// which activation can report only as an endpoint that never became healthy. Running
/// both refusals here names the file and the fault, before anything outside
/// this file has changed.
/// Answers with the policy key this file composes to, or [`ComposedPolicy::Unknowable`]
/// when the file resolves only where the runtime runs.
pub(super) fn verify_config(path: &Path) -> Result<ComposedPolicy, InitError> {
    let config = match Config::load(path) {
        Ok(config) => config,
        // A `token_env` resolves where the runtime runs, not here. A hook starts
        // it with the session's environment, which carries variables this
        // terminal does not, so a secret this process cannot see is not activation's
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
    use crate::default_config;
    use std::path::PathBuf;

    /// The policy version this build's default config declares.
    fn template_policy_version() -> i64 {
        toml::from_str::<toml::Value>(&default_config::text())
            .expect("the bundled default config parses")
            .get("policy")
            .and_then(|policy| policy.get("version"))
            .and_then(toml::Value::as_integer)
            .expect("the bundled default config declares an integer policy version")
    }

    #[test]
    fn a_config_the_runtime_could_not_compose_stops_activation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join("appa.toml");
        fs::write(&config, default_config::text().as_bytes()).expect("the default config is written");
        verify_config(&config).expect("the shipped default composes");

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
            other => panic!("a battery ahead of the root policy version must stop activation: {other:?}"),
        }
    }

    /// A loadable config plus whatever `body` declares.
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
        verify_config(&config).expect("activation does not judge a secret it cannot reach");
    }
}

//! Which runtime a client of this binary talks to.
//!
//! A hook entry names the endpoint its deployment installed, and a session
//! launched with `APPA_RUNTIME_URL` overrides it: that variable is fixed at
//! launch like `APPA_GATE`, and it means both "post here" and "the runtime here
//! is the user's own to restart". Neither is read from a shell, so the
//! precedence lives here rather than in a `${VAR:-default}` expansion.

use clap::Args;

pub const DEFAULT_RUNTIME_URL: &str = "http://127.0.0.1:8787";

#[derive(Args, Clone)]
pub struct RuntimeUrl {
    /// The runtime this session talks to, overriding the deployment's own. Loopback only.
    #[arg(long, env = "APPA_RUNTIME_URL")]
    url: Option<String>,

    /// The endpoint of the deployment that registered this command.
    #[arg(long)]
    deployment_url: Option<String>,
}

/// The runtime a client reaches, and whether the session named it itself.
pub struct RuntimeTarget {
    pub url: String,
    /// The session chose the runtime: a stale one there is the user's own to
    /// restart, and nothing this binary runs replaces it.
    pub user_owned: bool,
}

impl RuntimeUrl {
    pub fn resolve(&self) -> RuntimeTarget {
        match (&self.url, &self.deployment_url) {
            (Some(url), _) => RuntimeTarget {
                url: url.clone(),
                user_owned: true,
            },
            (None, Some(url)) => RuntimeTarget {
                url: url.clone(),
                user_owned: false,
            },
            (None, None) => RuntimeTarget {
                url: DEFAULT_RUNTIME_URL.to_owned(),
                user_owned: false,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn of(url: Option<&str>, deployment_url: Option<&str>) -> Self {
        Self {
            url: url.map(str::to_owned),
            deployment_url: deployment_url.map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_beats_the_deployment_which_beats_the_default() {
        let session = RuntimeUrl::of(Some("http://127.0.0.1:1"), Some("http://127.0.0.1:2")).resolve();
        assert_eq!(session.url, "http://127.0.0.1:1");
        assert!(session.user_owned);

        let deployment = RuntimeUrl::of(None, Some("http://127.0.0.1:2")).resolve();
        assert_eq!(deployment.url, "http://127.0.0.1:2");
        assert!(!deployment.user_owned);

        let default = RuntimeUrl::of(None, None).resolve();
        assert_eq!(default.url, DEFAULT_RUNTIME_URL);
        assert!(!default.user_owned);
    }
}

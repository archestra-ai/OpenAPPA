//! The connection a pool opens, read once from the PostgreSQL URL.
//!
//! The driver parses only `sslmode=disable|prefer|require` and refuses the other libpq TLS
//! parameters, so a host could not pass the URL it gives libpq or node-postgres. This module
//! takes those parameters out of the URL and applies them to the TLS connector. Verification
//! stays on unless the URL turns it off: `prefer` and `require` check the chain and the host
//! name, as `verify-full` does.

use ::postgres::config::SslMode;
use native_tls::{Certificate, TlsConnector};
use percent_encoding::percent_decode_str;
use postgres_native_tls::MakeTlsConnector;

use super::PostgresError;

#[derive(Clone)]
pub(super) struct Target {
    pub(super) config: ::postgres::Config,
    pub(super) tls: MakeTlsConnector,
}

impl Target {
    pub(super) fn from_url(url: &str) -> Result<Self, PostgresError> {
        let (driver_url, tls) = split(url)?;
        let mut config: ::postgres::Config = driver_url.parse().map_err(|error: ::postgres::Error| {
            let cause = std::error::Error::source(&error)
                .map(|cause| format!(": {cause}"))
                .unwrap_or_default();
            PostgresError(format!("{error}{cause}"))
        })?;
        if let Some(mode) = tls.mode {
            config.ssl_mode(mode);
        }
        Ok(Self {
            config,
            tls: MakeTlsConnector::new(tls.connector()?),
        })
    }
}

/// What a TLS connection checks about the server's certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verification {
    /// Chain and host name.
    Full,
    /// Chain only.
    Chain,
    Nothing,
}

/// The certificates a verified connection trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Roots {
    System,
    /// Only the PEM bundle at this path, as in libpq and node-postgres.
    File(String),
}

#[derive(Debug, PartialEq, Eq)]
struct Tls {
    /// `None` keeps the driver's own reading.
    mode: Option<SslMode>,
    verification: Verification,
    roots: Roots,
}

impl Default for Tls {
    fn default() -> Self {
        Self {
            mode: None,
            verification: Verification::Full,
            roots: Roots::System,
        }
    }
}

impl Tls {
    fn connector(&self) -> Result<TlsConnector, PostgresError> {
        let mut builder = TlsConnector::builder();
        if let Roots::File(path) = &self.roots {
            let pem = std::fs::read(path).map_err(|e| PostgresError(format!("cannot read sslrootcert {path}: {e}")))?;
            let certificates = Certificate::stack_from_pem(&pem)
                .map_err(|e| PostgresError(format!("sslrootcert {path} is not a PEM certificate bundle: {e}")))?;
            if certificates.is_empty() {
                return Err(PostgresError(format!("sslrootcert {path} holds no certificate")));
            }
            builder.disable_built_in_roots(true);
            for certificate in certificates {
                builder.add_root_certificate(certificate);
            }
        }
        match self.verification {
            Verification::Full => {}
            Verification::Chain => {
                builder.danger_accept_invalid_hostnames(true);
            }
            Verification::Nothing => {
                builder
                    .danger_accept_invalid_certs(true)
                    .danger_accept_invalid_hostnames(true);
            }
        }
        builder.build().map_err(|e| PostgresError(e.to_string()))
    }
}

/// The URL the driver parses, without the TLS parameters this module owns. A key/value
/// connection string passes through unchanged.
fn split(url: &str) -> Result<(String, Tls), PostgresError> {
    let mut tls = Tls::default();
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return Ok((url.to_owned(), tls));
    }
    let Some((base, query)) = url.split_once('?') else {
        return Ok((url.to_owned(), tls));
    };
    let mut kept = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode_str(value).decode_utf8_lossy();
        match key {
            "sslmode" => {
                (tls.mode, tls.verification) = match value.as_ref() {
                    "disable" => (Some(SslMode::Disable), Verification::Full),
                    "prefer" => (Some(SslMode::Prefer), Verification::Full),
                    "require" | "verify-full" => (Some(SslMode::Require), Verification::Full),
                    "verify-ca" => (Some(SslMode::Require), Verification::Chain),
                    "no-verify" => (Some(SslMode::Require), Verification::Nothing),
                    other => {
                        return Err(PostgresError(format!(
                            "unsupported sslmode {other}: expected disable, prefer, require, verify-ca, verify-full or no-verify"
                        )));
                    }
                }
            }
            "sslrootcert" => {
                tls.roots = match value.as_ref() {
                    "system" => Roots::System,
                    path => Roots::File(path.to_owned()),
                }
            }
            // node-postgres's switch to libpq semantics; TLS here keeps verifying either way.
            "uselibpqcompat" => {}
            _ => kept.push(pair),
        }
    }
    let driver_url = match kept.as_slice() {
        [] => base.to_owned(),
        kept => format!("{base}?{}", kept.join("&")),
    };
    Ok((driver_url, tls))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls(mode: Option<SslMode>, verification: Verification, roots: Roots) -> Tls {
        Tls {
            mode,
            verification,
            roots,
        }
    }

    #[test]
    fn libpq_tls_parameters_leave_the_driver_url() {
        let cases = [
            ("postgres://u:p@db/app", "postgres://u:p@db/app", Tls::default()),
            (
                "postgres://u:p@db/app?sslmode=disable",
                "postgres://u:p@db/app",
                tls(Some(SslMode::Disable), Verification::Full, Roots::System),
            ),
            (
                "postgres://u:p@db/app?sslmode=require&application_name=web",
                "postgres://u:p@db/app?application_name=web",
                tls(Some(SslMode::Require), Verification::Full, Roots::System),
            ),
            (
                "postgresql://u:p@db/app?sslmode=verify-full&sslrootcert=%2Fetc%2Fssl%2Fdb%2Fca.pem",
                "postgresql://u:p@db/app",
                tls(
                    Some(SslMode::Require),
                    Verification::Full,
                    Roots::File("/etc/ssl/db/ca.pem".into()),
                ),
            ),
            (
                "postgres://u:p@db/app?connect_timeout=5&sslmode=verify-ca&sslrootcert=/ca.pem",
                "postgres://u:p@db/app?connect_timeout=5",
                tls(
                    Some(SslMode::Require),
                    Verification::Chain,
                    Roots::File("/ca.pem".into()),
                ),
            ),
            (
                "postgres://u:p@db/app?uselibpqcompat=true&sslmode=no-verify",
                "postgres://u:p@db/app",
                tls(Some(SslMode::Require), Verification::Nothing, Roots::System),
            ),
            (
                "postgres://u:p@db/app?sslrootcert=system",
                "postgres://u:p@db/app",
                tls(None, Verification::Full, Roots::System),
            ),
            (
                "host=db user=u sslmode=require",
                "host=db user=u sslmode=require",
                Tls::default(),
            ),
        ];
        for (url, driver_url, expected) in cases {
            let (split_url, split_tls) = split(url).expect("the URL splits");
            assert_eq!((split_url.as_str(), &split_tls), (driver_url, &expected), "{url}");
        }
    }

    #[test]
    fn an_unknown_sslmode_is_refused() {
        assert!(split("postgres://db/app?sslmode=allow").is_err());
    }

    #[test]
    fn every_supported_url_reaches_a_driver_config() {
        for url in [
            "postgres://u:p@db/app?sslmode=verify-full",
            "postgres://u:p@db/app?sslmode=verify-ca&uselibpqcompat=true",
            "postgres://u:p@db/app?sslmode=no-verify",
            "postgres://u:p@db/app?sslmode=prefer&connect_timeout=5",
        ] {
            assert!(Target::from_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn an_unreadable_root_bundle_is_refused() {
        let missing = tempfile::tempdir()
            .expect("a temporary directory")
            .path()
            .join("ca.pem");
        let url = format!(
            "postgres://db/app?sslmode=verify-full&sslrootcert={}",
            missing.display()
        );
        assert!(Target::from_url(&url).is_err());
    }

    /// Run against a TLS server whose certificate names `localhost` and is signed by the CA
    /// at OPENAPPA_TEST_TLS_ROOT_CERT, a CA the system does not trust:
    /// OPENAPPA_TEST_TLS_DATABASE_URL=postgres://u:p@localhost:5432/db OPENAPPA_TEST_TLS_ROOT_CERT=ca.pem \
    ///   cargo test -p appa-eventlog --features postgres -- --ignored tls_
    #[test]
    #[ignore = "requires OPENAPPA_TEST_TLS_DATABASE_URL and OPENAPPA_TEST_TLS_ROOT_CERT"]
    fn tls_parameters_decide_which_servers_connect() {
        let url = std::env::var("OPENAPPA_TEST_TLS_DATABASE_URL").expect("test TLS database URL");
        let root = std::env::var("OPENAPPA_TEST_TLS_ROOT_CERT").expect("test TLS root certificate");
        let by_address = url.replacen("localhost", "127.0.0.1", 1);
        let connects = |url: &str| {
            let target = Target::from_url(url).expect("the URL parses");
            target
                .config
                .connect(target.tls)
                .and_then(|mut client| client.batch_execute("SELECT 1"))
                .is_ok()
        };
        let cases = [
            (format!("{url}?sslmode=verify-full&sslrootcert={root}"), true),
            (format!("{url}?sslmode=require&sslrootcert={root}"), true),
            (format!("{url}?sslmode=require"), false),
            (format!("{url}?sslmode=verify-full&sslrootcert=system"), false),
            (format!("{by_address}?sslmode=verify-full&sslrootcert={root}"), false),
            (format!("{by_address}?sslmode=verify-ca&sslrootcert={root}"), true),
            (format!("{url}?sslmode=no-verify"), true),
        ];
        for (url, expected) in cases {
            assert_eq!(connects(&url), expected, "{url}");
        }
    }
}

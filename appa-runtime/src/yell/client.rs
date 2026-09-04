//! Putting a finished report on the wire.
//!
//! The receiver is unauthenticated by design — every build, local ones included, can post — so
//! the signature here is not access control. It is a filter: a scanner that finds the URL, or
//! a process that posts to it by accident, does not carry the salt and is refused before the
//! receiver parses anything. The salt is a file in this repository, compiled in, and public by
//! construction.
//!
//! There is no outbox and no background retry. A send either finishes while the person waits
//! or fails, and a failed send leaves the report on disk with its path printed. The next yell
//! is a new report with a new id, which is the honest thing: a resend hours later of a
//! session's decisions is a different report about a different moment.

use std::time::Duration;

use hmac::Mac;

use super::report::Finished;

/// Where reports go. Empty until the receiver is deployed, which this build refuses cleanly
/// rather than posting a session's decisions to a guess.
const ENDPOINT: &str = match option_env!("APPA_YELL_ENDPOINT") {
    Some(endpoint) => endpoint,
    None => "",
};

/// One place a report may be sent, resolved before the person is asked to approve sending.
///
/// Parsed rather than kept as a string, because the consent question has to name it and the
/// name has to be the destination: `https://approved.example@evil.example/` displays as one
/// host and connects to another. A URL that carries credentials is refused outright — a
/// receiver needs none — so what the prompt prints is what the request reaches.
///
/// The two variants are not decoration. A proxy between here and a real receiver is a normal
/// way to reach the internet and is honoured; a proxy between here and this same machine is
/// never right, and would relay a report that was only ever meant to cross a socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Receiver {
    /// A real receiver, always over TLS.
    Secure(reqwest::Url),
    /// A receiver on this machine, in the clear. This is how a test points at a local one.
    Loopback(reqwest::Url),
}

impl Receiver {
    /// The receiver this run will use, or `None` when there is none to use.
    ///
    /// `APPA_YELL_ENDPOINT` overrides the compiled destination. It is not a hole in consent:
    /// the person is shown whatever it resolves to before answering. Plaintext is refused
    /// unless it is this machine, so an override cannot downgrade a real send to `http://`.
    pub(crate) fn resolve() -> Option<Self> {
        let named = std::env::var("APPA_YELL_ENDPOINT").unwrap_or_else(|_| ENDPOINT.to_owned());
        Self::parse(&named)
    }

    /// HTTPS anywhere, or plain HTTP only to this machine, and credentials nowhere.
    fn parse(url: &str) -> Option<Self> {
        let parsed = reqwest::Url::parse(url).ok()?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return None;
        }
        match parsed.scheme() {
            "https" => Some(Self::Secure(parsed)),
            "http" if is_loopback(&parsed) => Some(Self::Loopback(parsed)),
            _ => None,
        }
    }

    fn url(&self) -> &reqwest::Url {
        match self {
            Self::Secure(url) | Self::Loopback(url) => url,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        self.url().as_str()
    }
}

/// Whether a parsed URL names this machine by address. A host *name* is not loopback here,
/// whatever a resolver would say about it today.
pub(crate) fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

/// The shared secret that is not a secret. See the module documentation.
const SALT: &str = include_str!("../../../receiver/appa-yell/salt.txt");

/// How long one attempt may take, and how long to wait before the attempts after the first.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

/// What the receiver says it did with the report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct Receipt {
    /// The receiver's own name for the stored report, which a person can quote.
    pub(crate) receipt_id: String,
    /// True when these exact bytes had already arrived. An accepted report whose answer was
    /// lost comes back this way rather than as a second report.
    #[serde(default)]
    pub(crate) duplicate: bool,
}

/// Why a report did not arrive. A class rather than the transport's message, because that
/// message names hosts and paths and this one is printed for a person to read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SendFailure {
    #[error("this build has no receiver compiled in, so there is nowhere to send the report")]
    NoReceiver,
    #[error("the receiver could not be reached after {attempts} attempts")]
    Unreachable { attempts: u32 },
    #[error("the receiver refused the report with status {status}")]
    Refused { status: u16 },
    #[error("the receiver accepted the report but answered something this build cannot read")]
    UnreadableReceipt,
}

/// Post one report, retrying only what is worth retrying.
///
/// The same bytes go up every time, so a receiver that took the report and lost the answer
/// recognizes the retry as the duplicate it is.
pub(crate) async fn send(finished: &Finished, receiver: &Receiver) -> Result<Receipt, SendFailure> {
    let signature = signature(&finished.plain);
    crate::tls::install_crypto_provider();
    let client = reqwest::Client::builder()
        .timeout(ATTEMPT_TIMEOUT)
        // A redirect is another destination, and the person approved this one.
        .redirect(reqwest::redirect::Policy::none());
    let client = match receiver {
        // Reaching a real receiver through a proxy is how many machines reach anything.
        Receiver::Secure(_) => client,
        // Reaching *this machine* through a proxy is never right, and would relay a report
        // that was only ever meant to cross a socket.
        Receiver::Loopback(_) => client.no_proxy(),
    };
    let client = client.build().map_err(|_| SendFailure::Unreachable { attempts: 0 })?;

    let mut attempt = 0;
    loop {
        let answer = client
            .post(receiver.url().clone())
            .header("Content-Type", "application/json")
            .header("Content-Encoding", "gzip")
            .header("X-Appa-Signature", &signature)
            .body(finished.gzipped.clone())
            .send()
            .await;
        let outcome = match answer {
            Ok(response) => read(response).await,
            // A transport error is a connect failure or a timeout; both are worth another try.
            Err(_) => Err(Attempt::Retry(SendFailure::Unreachable { attempts: attempt + 1 })),
        };
        match outcome {
            Ok(receipt) => return Ok(receipt),
            Err(Attempt::Give(failure)) => return Err(failure),
            Err(Attempt::Retry(failure)) => match BACKOFF.get(attempt as usize) {
                Some(wait) => {
                    tokio::time::sleep(*wait).await;
                    attempt += 1;
                }
                None => return Err(failure),
            },
        }
    }
}

/// Whether a failure is worth another identical request.
enum Attempt {
    Retry(SendFailure),
    Give(SendFailure),
}

async fn read(response: reqwest::Response) -> Result<Receipt, Attempt> {
    let status = response.status();
    if status.is_success() {
        // The report is already stored by the time the receipt is written, so losing the body
        // in transit is exactly the case the idempotency id exists for: the same bytes go up
        // again and come back marked as the duplicate they are. A body that arrives whole and
        // does not parse is a different thing, and retrying it changes nothing.
        return match response.bytes().await {
            Err(_) => Err(Attempt::Retry(SendFailure::UnreadableReceipt)),
            Ok(body) => {
                serde_json::from_slice::<Receipt>(&body).map_err(|_| Attempt::Give(SendFailure::UnreadableReceipt))
            }
        };
    }
    let failure = SendFailure::Refused {
        status: status.as_u16(),
    };
    // Too many requests, a timed-out request, and a server fault are the receiver's own
    // transient states. Every other refusal is about these bytes, which do not change between
    // attempts.
    match matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
        true => Err(Attempt::Retry(failure)),
        false => Err(Attempt::Give(failure)),
    }
}

/// `v1=<hex HMAC-SHA256(salt, plain bytes)>` over the document, not the gzip: the receiver
/// verifies what it is going to parse.
fn signature(plain: &[u8]) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SALT.trim().as_bytes())
        .expect("HMAC-SHA256 accepts a key of any length");
    mac.update(plain);
    let tag = mac.finalize().into_bytes();
    let hex: String = tag.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("v1={hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_covers_the_document_and_not_the_gzip() {
        let one = signature(b"{\"schema\":\"openappa.yell.v1\"}");
        let same = signature(b"{\"schema\":\"openappa.yell.v1\"}");
        let other = signature(b"{\"schema\":\"openappa.yell.v2\"}");
        assert_eq!(one, same);
        assert_ne!(one, other);
        assert!(one.starts_with("v1="), "{one}");
        assert_eq!(one.len(), "v1=".len() + 64);
    }

    /// The salt ships in the repository and is compiled in, so a build with an empty one would
    /// sign every report identically and the filter would stop filtering.
    #[test]
    fn the_salt_is_present() {
        assert!(!SALT.trim().is_empty());
    }

    /// Sending actually builds a client, which this crate's `reqwest` refuses to do until a
    /// crypto provider is installed — TLS or not. Nothing else in this test module gets that
    /// far, so without this a `yell` that reached the send would panic in every build.
    ///
    /// Time is paused, so the two backoffs cost nothing and the three attempts are still made.
    #[tokio::test(start_paused = true)]
    async fn a_send_to_nothing_exhausts_its_attempts_rather_than_panicking() {
        let receiver = Receiver::parse("http://127.0.0.1:1/report").expect("loopback is a receiver");
        let finished = Finished::of(br#"{"schema":"openappa.yell.v1"}"#.to_vec()).expect("the fixture fits");
        assert_eq!(
            send(&finished, &receiver).await,
            Err(SendFailure::Unreachable { attempts: 3 })
        );
    }

    /// A report leaves this machine in the clear only to this machine. The override exists so
    /// a test can point at a local receiver, and it cannot downgrade a real send to plaintext.
    #[test]
    fn a_receiver_is_https_or_it_is_this_machine() {
        assert!(matches!(
            Receiver::parse("https://yell.example.run.app/report"),
            Some(Receiver::Secure(_))
        ));
        assert!(matches!(
            Receiver::parse("http://127.0.0.1:9099/report"),
            Some(Receiver::Loopback(_))
        ));
        assert!(matches!(
            Receiver::parse("http://[::1]:9099"),
            Some(Receiver::Loopback(_))
        ));
        for refused in [
            "http://yell.example.run.app",
            "http://localhost:9099",
            "ftp://yell.example.run.app",
            "yell.example.run.app",
            "",
        ] {
            assert_eq!(Receiver::parse(refused), None, "{refused}");
        }
    }

    /// A URL that displays as one host and connects to another is not a destination anyone can
    /// consent to, so it is not a receiver at all.
    #[test]
    fn a_receiver_carries_no_credentials() {
        for refused in [
            "https://approved.example@evil.example/report",
            "https://user:secret@yell.example.run.app/report",
            "http://user@127.0.0.1:9099/report",
        ] {
            assert_eq!(Receiver::parse(refused), None, "{refused}");
        }
    }
}

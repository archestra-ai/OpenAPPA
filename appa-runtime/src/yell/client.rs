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
pub(crate) async fn send(finished: &Finished) -> Result<Receipt, SendFailure> {
    let endpoint = endpoint();
    if endpoint.is_empty() {
        return Err(SendFailure::NoReceiver);
    }
    let signature = signature(&finished.plain);
    let client = reqwest::Client::builder()
        .timeout(ATTEMPT_TIMEOUT)
        .build()
        .map_err(|_| SendFailure::Unreachable { attempts: 0 })?;

    let mut attempt = 0;
    loop {
        let answer = client
            .post(&endpoint)
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
        return response
            .json::<Receipt>()
            .await
            .map_err(|_| Attempt::Give(SendFailure::UnreadableReceipt));
    }
    let failure = SendFailure::Refused {
        status: status.as_u16(),
    };
    // Too many requests and a server fault are the receiver's own transient states. Every
    // other refusal is about these bytes, which do not change between attempts.
    match status.as_u16() == 429 || status.is_server_error() {
        true => Err(Attempt::Retry(failure)),
        false => Err(Attempt::Give(failure)),
    }
}

/// The compiled-in receiver, or the one a person named for this run. Overriding it is how the
/// tests point at a local receiver; an agent never reaches this, because the tool takes no
/// endpoint.
fn endpoint() -> String {
    std::env::var("APPA_YELL_ENDPOINT").unwrap_or_else(|_| ENDPOINT.to_owned())
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
}

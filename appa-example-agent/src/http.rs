//! The one HTTP client shape this crate builds, and the one way it
//! reads a body.

/// A client with **redirects disabled** — a newtype, so the safe
/// policy is the only way to obtain one. Following redirects would let
/// a backend hide a 3xx behind a final 2xx, or resend a payload to a
/// `Location` it chose (307/308).
#[derive(Clone, Debug)]
pub struct HttpClient(reqwest::Client);

impl HttpClient {
    pub fn new() -> Self {
        appa_runtime::tls::install_crypto_provider();
        HttpClient(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("a default rustls reqwest client builds"),
        )
    }

    pub fn loopback() -> Self {
        appa_runtime::tls::install_crypto_provider();
        HttpClient(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("a loopback-only rustls reqwest client builds"),
        )
    }

    pub fn inner(&self) -> &reqwest::Client {
        &self.0
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        HttpClient::new()
    }
}

/// Read a response body, buffering **at most `cap + 1` bytes**: the
/// extra byte only flags "over cap". Total allocation stays `O(cap)`
/// however much the backend sends. The error is the transport fault
/// mid-body, carried rather than flattened so a caller can tell an
/// elapsed deadline from a dropped connection.
pub async fn read_body_capped(response: &mut reqwest::Response, cap: usize) -> Result<Vec<u8>, reqwest::Error> {
    let limit = cap.saturating_add(1);
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = limit - body.len();
                let take = room.min(chunk.len());
                body.extend_from_slice(&chunk[..take]);
                if body.len() > cap {
                    return Ok(body);
                }
            }
            Ok(None) => return Ok(body),
            Err(error) => return Err(error),
        }
    }
}

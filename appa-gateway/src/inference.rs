//! Compatibility inference facade over [`appa_agent::OpenAiCompatible`].

use std::fmt;
use std::time::Duration;

use appa_agent::{OpenAiCompatible, OpenAiConfig};
use appa_runtime::tool::HttpClient;
use appa_runtime::wire::ChatCompletionRequest;

pub use appa_agent::ProviderError as InferenceError;
pub use appa_runtime::Completion;

#[derive(Clone)]
pub struct Inference {
    provider: OpenAiCompatible,
}

impl Inference {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
        client: HttpClient,
    ) -> Self {
        let config = OpenAiConfig::new(base_url.into(), model.into(), api_key.into()).with_request_timeout(timeout);
        Inference {
            provider: OpenAiCompatible::with_http_client(config, client),
        }
    }

    pub async fn complete(&self, request: ChatCompletionRequest) -> Result<Completion, InferenceError> {
        self.provider.complete(request).await
    }

    pub(crate) fn provider(&self) -> OpenAiCompatible {
        self.provider.clone()
    }
}

impl fmt::Debug for Inference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Inference").finish_non_exhaustive()
    }
}

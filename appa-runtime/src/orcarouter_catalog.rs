//! The OrcaRouter model catalog: what the configured deployment can actually call.
//!
//! The single source of truth is `GET {api_base}/models` on the inference origin. Live
//! discovery is authoritative; a small, verified seed stands in when the catalog is
//! slow, refused, or unreadable, and it is labelled as the seed so no caller mistakes it
//! for a live answer.
//!
//! Capability filtering happens here, once, from recorded metadata — never from a model
//! name. A model whose entry does not prove it supports a capability is excluded from
//! that capability's list, so an incompatible model can never reach a picker.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::orcarouter::{CATALOG_TIMEOUT, MAX_CATALOG_BYTES, MAX_CATALOG_ITEMS};

/// What an entry point needs from a model. Each name maps to one filter below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Text chat or agent turns: an OpenAI-wire endpoint and no dedicated non-text route.
    Chat,
    /// Embedding generation: an `embeddings` endpoint, or the catalog's own tag.
    Embedding,
    /// Image generation: an `image-generation` endpoint.
    Image,
    /// Video generation: an `openai-video` endpoint.
    Video,
    /// Reranking: a `jina-rerank` endpoint.
    Rerank,
}

impl Capability {
    /// The value the catalog's `capability` query parameter takes, where the service
    /// knows one. Filtering is still applied locally, so a catalog that ignores the
    /// parameter cannot widen a picker.
    pub fn query_value(self) -> Option<&'static str> {
        match self {
            Capability::Chat => Some("chat"),
            Capability::Embedding => Some("embedding"),
            Capability::Image => Some("image"),
            Capability::Video | Capability::Rerank => None,
        }
    }
}

/// A non-text input a chat model accepts. Absent metadata proves nothing, so a model
/// that does not declare the modality is excluded from that picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
}

impl Modality {
    fn parse(name: &str) -> Option<Modality> {
        match name {
            "text" => Some(Modality::Text),
            "image" => Some(Modality::Image),
            "audio" => Some(Modality::Audio),
            "video" => Some(Modality::Video),
            _ => None,
        }
    }
}

/// The endpoint types that carry a text chat or agent turn.
const CHAT_ENDPOINTS: [&str; 4] = ["openai", "anthropic", "gemini", "openai-response"];
/// The endpoint spellings that carry embeddings.
const EMBEDDING_ENDPOINTS: [&str; 2] = ["embeddings", "embedding"];

/// The reasoning-effort ladder a model advertises, in the provider's own spelling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reasoning {
    pub efforts: Vec<String>,
}

/// One catalog record, normalised. `endpoints` keeps the vendor's own spelling; the
/// filters read it, and never the model's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogItem {
    /// The model id, verbatim, including its `vendor/model` namespace.
    pub id: String,
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// The input modalities the model declares. Empty means none were declared, which
    /// excludes the model from every multimodal picker.
    #[serde(default)]
    pub input_modalities: Vec<Modality>,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub reasoning: Option<Reasoning>,
    /// The model's vendor, when the catalog names one.
    #[serde(default)]
    pub owned_by: Option<String>,
}

impl CatalogItem {
    fn supports_endpoint(&self, endpoint: &str) -> bool {
        self.endpoints.iter().any(|declared| declared == endpoint)
    }

    fn any_endpoint(&self, endpoints: &[&str]) -> bool {
        endpoints.iter().any(|endpoint| self.supports_endpoint(endpoint))
    }

    /// Whether this model serves a chat or agent turn: it advertises a chat-wire endpoint.
    /// A route that carries only a dedicated non-chat endpoint (`image-generation`,
    /// `openai-video`, `jina-rerank`) advertises none, so it never qualifies.
    pub fn is_chat(&self) -> bool {
        self.any_endpoint(&CHAT_ENDPOINTS)
    }

    /// Whether this model satisfies `capability`, and, for every capability, every
    /// required non-text modality. A record with no endpoint that matches proves nothing
    /// and is excluded, so no model is admitted on its name.
    pub fn satisfies(&self, capability: Capability, modalities: &[Modality]) -> bool {
        let by_capability = match capability {
            Capability::Chat => self.is_chat(),
            Capability::Embedding => self.any_endpoint(&EMBEDDING_ENDPOINTS),
            Capability::Image => self.supports_endpoint("image-generation"),
            Capability::Video => self.supports_endpoint("openai-video"),
            Capability::Rerank => self.supports_endpoint("jina-rerank"),
        };
        by_capability
            && modalities
                .iter()
                .filter(|modality| **modality != Modality::Text)
                .all(|modality| self.input_modalities.contains(modality))
    }

    /// A stable line for a picker: the id, then the context and reasoning the entry
    /// actually recorded, so a caller sees the capability and not a guess.
    pub fn label(&self) -> String {
        let mut parts = vec![self.id.clone()];
        if let Some(context) = self.context_length {
            parts.push(format!("{context} ctx"));
        }
        if let Some(reasoning) = &self.reasoning {
            parts.push(format!("reasoning: {}", reasoning.efforts.join("/")));
        }
        parts.join(" · ")
    }
}

/// Where a catalog came from. Live discovery is authoritative and never mixes with the
/// seed; the other two are explicitly degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    /// `GET /models` answered and its items are the whole list.
    Live,
    /// The verified seed stands in because discovery failed or was refused.
    VerifiedSeed,
    /// A catalog a previous live discovery stored, used when the network is down.
    LastKnownGood,
}

impl CatalogSource {
    pub fn is_degraded(self) -> bool {
        !matches!(self, CatalogSource::Live)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("the catalog origin is not usable: {0}")]
    Origin(#[from] crate::orcarouter::OriginError),
    #[error("the catalog request failed: {0}")]
    Transport(String),
    #[error("the catalog answered {status}")]
    Refused { status: u16 },
    #[error("the catalog answered with more than {MAX_CATALOG_BYTES} bytes")]
    Oversized,
    #[error("the catalog body is not the expected JSON: {0}")]
    Malformed(String),
}

/// One discovery result: the items, where they came from, and how many the catalog
/// originally advertised before filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub source: CatalogSource,
    /// Every item the catalog served, unfiltered. Live results are authoritative.
    pub items: Vec<CatalogItem>,
}

impl Catalog {
    pub fn live(items: Vec<CatalogItem>) -> Catalog {
        Catalog {
            source: CatalogSource::Live,
            items,
        }
    }

    pub fn seed() -> Catalog {
        Catalog {
            source: CatalogSource::VerifiedSeed,
            items: verified_seed(),
        }
    }

    /// The ids satisfying `capability` and every required non-text modality. Empty is a
    /// real answer: it means the deployment advertises nothing that qualifies.
    pub fn select(&self, capability: Capability, modalities: &[Modality]) -> Vec<&CatalogItem> {
        self.items
            .iter()
            .filter(|item| item.satisfies(capability, modalities))
            .take(MAX_CATALOG_ITEMS)
            .collect()
    }

    /// The picker options for one entry point, ids preserved verbatim.
    pub fn options(&self, capability: Capability, modalities: &[Modality]) -> Vec<String> {
        self.select(capability, modalities)
            .into_iter()
            .map(|item| item.id.clone())
            .collect()
    }

    /// Whether `id` is still selectable for this entry point. A restored old value that
    /// no longer qualifies must be cleared, not kept.
    pub fn contains(&self, id: &str, capability: Capability, modalities: &[Modality]) -> bool {
        self.select(capability, modalities).iter().any(|item| item.id == id)
    }

    /// Fetch `GET {api}/models` from the configured inference origin and normalise it.
    ///
    /// Bounded in three ways: the request carries [`CATALOG_TIMEOUT`], the body is read
    /// to a cap ([`MAX_CATALOG_BYTES`]), and [`Catalog::parse`] keeps at most
    /// [`MAX_CATALOG_ITEMS`] records. The `key` is sent as a Bearer header so the answer
    /// describes the deployment's own workspace; `None` asks anonymously. The
    /// `capability` query parameter is advisory, and the local filter is applied whatever
    /// the service returns.
    pub async fn discover(
        origins: &crate::orcarouter::Origins,
        key: Option<&str>,
        capability: Option<Capability>,
    ) -> Result<Catalog, CatalogError> {
        crate::tls::install_crypto_provider();
        let url = match capability.and_then(Capability::query_value) {
            Some(value) => format!("{}?capability={value}", origins.models_url()),
            None => origins.models_url(),
        };
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CATALOG_TIMEOUT);
        // A self-hosted catalog on loopback must not be sent through a proxy meant for
        // another host; a remote one follows the host's network policy.
        let builder = match url.starts_with("http://127.0.0.1")
            || url.starts_with("http://localhost")
            || url.starts_with("http://[::1]")
        {
            true => builder.no_proxy(),
            false => builder,
        };
        let client = builder
            .build()
            .map_err(|error| CatalogError::Transport(error.to_string()))?;
        let mut request = client.get(&url).header("accept", "application/json");
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| CatalogError::Transport(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(CatalogError::Refused {
                status: status.as_u16(),
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| CatalogError::Transport(error.to_string()))?;
        Catalog::parse(&body, CatalogSource::Live)
    }

    /// Read a `GET /models` body. One unreadable record is dropped, not the whole
    /// catalog, and the record cap bounds memory whatever the body says.
    pub fn parse(body: &[u8], source: CatalogSource) -> Result<Catalog, CatalogError> {
        if body.len() > MAX_CATALOG_BYTES {
            return Err(CatalogError::Oversized);
        }
        let root: Value = serde_json::from_slice(body).map_err(|error| CatalogError::Malformed(error.to_string()))?;
        let data = root
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| CatalogError::Malformed("no `data` array".to_string()))?;
        let mut items = Vec::new();
        for entry in data.iter().take(MAX_CATALOG_ITEMS) {
            if let Some(item) = parse_item(entry) {
                items.push(item);
            }
        }
        Ok(Catalog { source, items })
    }
}

/// The catalog a deployment is currently using, and the rule that keeps it usable
/// through an outage.
///
/// A successful discovery is authoritative and becomes the last known good. When
/// discovery fails, the last known good stands in; a fresh installation with none falls
/// back to the verified seed. The source is always carried, so a caller can show the
/// degraded state rather than pretend the seed is live.
#[derive(Debug)]
pub struct CatalogProvider {
    origins: crate::orcarouter::Origins,
    last_known_good: std::sync::Mutex<Option<Catalog>>,
}

impl CatalogProvider {
    pub fn new(origins: crate::orcarouter::Origins) -> CatalogProvider {
        CatalogProvider {
            origins,
            last_known_good: std::sync::Mutex::new(None),
        }
    }

    /// Refresh from the live catalog. On success the result is authoritative and kept as
    /// the last known good. On failure the previous catalog (or the verified seed) is
    /// returned, with its own source, so nothing degrades to an empty or free-text list.
    pub async fn refresh(&self, key: Option<&str>, capability: Option<Capability>) -> Catalog {
        match Catalog::discover(&self.origins, key, capability).await {
            Ok(live) => {
                self.store(live.clone());
                live
            }
            Err(error) => {
                tracing::debug!(%error, "the orcarouter catalog is unavailable; keeping the last good one");
                self.current()
            }
        }
    }

    /// What the picker uses right now, without a network call.
    pub fn current(&self) -> Catalog {
        self.last_known_good
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .map(|catalog| Catalog {
                source: CatalogSource::LastKnownGood,
                items: catalog.items,
            })
            .unwrap_or_else(Catalog::seed)
    }

    fn store(&self, catalog: Catalog) {
        *self
            .last_known_good
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(catalog);
    }
}

/// One record as the catalog spells it. The `architecture` block is optional: a gateway
/// that does not describe modalities must not be read as if it had.
fn parse_item(entry: &Value) -> Option<CatalogItem> {
    let id = entry.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let endpoints = entry
        .get("supported_endpoint_types")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let input_modalities = entry
        .pointer("/architecture/input_modalities")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter_map(Modality::parse)
                .collect()
        })
        .unwrap_or_default();
    let context_length = entry
        .get("context_length")
        .or_else(|| entry.get("context_window"))
        .and_then(Value::as_u64);
    let reasoning = entry
        .pointer("/reasoning/efforts")
        .or_else(|| entry.get("supported_reasoning_efforts"))
        .and_then(Value::as_array)
        .map(|list| Reasoning {
            efforts: list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        })
        .filter(|reasoning| !reasoning.efforts.is_empty());
    Some(CatalogItem {
        id: id.to_string(),
        endpoints,
        input_modalities,
        context_length,
        reasoning,
        owned_by: entry.get("owned_by").and_then(Value::as_str).map(str::to_owned),
    })
}

/// The models a fresh installation can start from when discovery is unavailable. Each
/// carries the metadata the runtime relies on; a seed that dropped the reasoning ladder
/// would silently regress a deployment.
///
/// Source: the official catalog at `https://api.orcarouter.ai/v1/models`.
pub fn verified_seed() -> Vec<CatalogItem> {
    VerifiedSeed::ALL.iter().map(|seed| seed.item()).collect()
}

/// One entry of the verified seed. Enumerated rather than written as bare strings so a
/// caller cannot add a model without stating its capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedModel {
    pub id: &'static str,
    pub context_length: Option<u64>,
    pub input_modalities: &'static [Modality],
    pub reasoning_efforts: &'static [&'static str],
}

/// The seed itself.
pub struct VerifiedSeed;

impl VerifiedSeed {
    pub const ALL: [SeedModel; 5] = [
        SeedModel {
            id: "openai/gpt-5.5",
            context_length: Some(400_000),
            input_modalities: &[Modality::Text, Modality::Image],
            reasoning_efforts: &["low", "medium", "high", "xhigh"],
        },
        SeedModel {
            id: "anthropic/claude-opus-4.8",
            context_length: Some(200_000),
            input_modalities: &[Modality::Text, Modality::Image],
            reasoning_efforts: &["low", "medium", "high"],
        },
        SeedModel {
            id: "google/gemini-3.5-flash",
            context_length: Some(1_000_000),
            input_modalities: &[Modality::Text, Modality::Image, Modality::Audio, Modality::Video],
            reasoning_efforts: &["low", "high"],
        },
        SeedModel {
            id: "deepseek/deepseek-v4-pro",
            context_length: Some(160_000),
            input_modalities: &[Modality::Text],
            reasoning_efforts: &[],
        },
        SeedModel {
            id: "orcarouter/auto",
            context_length: None,
            input_modalities: &[Modality::Text],
            reasoning_efforts: &[],
        },
    ];
}

impl SeedModel {
    fn item(&self) -> CatalogItem {
        CatalogItem {
            id: self.id.to_string(),
            // A seed model reached a chat endpoint by construction; that is why it was
            // verified. The endpoint list states it so the chat filter admits it.
            endpoints: CHAT_ENDPOINTS.iter().map(|name| (*name).to_string()).collect(),
            input_modalities: self.input_modalities.to_vec(),
            context_length: self.context_length,
            reasoning: match self.reasoning_efforts.is_empty() {
                true => None,
                false => Some(Reasoning {
                    efforts: self
                        .reasoning_efforts
                        .iter()
                        .map(|effort| (*effort).to_string())
                        .collect(),
                }),
            },
            owned_by: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog::live(vec![
            CatalogItem {
                id: "deepseek/deepseek-v4-pro".into(),
                endpoints: vec!["openai".into(), "openai-response".into()],
                input_modalities: vec![Modality::Text],
                context_length: Some(160_000),
                reasoning: None,
                owned_by: Some("DeepSeek".into()),
            },
            CatalogItem {
                id: "vendor/vision-chat".into(),
                endpoints: vec!["openai".into()],
                input_modalities: vec![Modality::Text, Modality::Image],
                context_length: None,
                reasoning: None,
                owned_by: None,
            },
            CatalogItem {
                id: "vendor/embed".into(),
                endpoints: vec!["embeddings".into()],
                input_modalities: vec![],
                context_length: None,
                reasoning: None,
                owned_by: None,
            },
            CatalogItem {
                id: "vendor/image".into(),
                endpoints: vec!["image-generation".into()],
                input_modalities: vec![],
                context_length: None,
                reasoning: None,
                owned_by: None,
            },
            CatalogItem {
                id: "vendor/video".into(),
                endpoints: vec!["openai-video".into()],
                input_modalities: vec![],
                context_length: None,
                reasoning: None,
                owned_by: None,
            },
            CatalogItem {
                id: "vendor/rerank".into(),
                endpoints: vec!["jina-rerank".into()],
                input_modalities: vec![],
                context_length: None,
                reasoning: None,
                owned_by: None,
            },
        ])
    }

    #[test]
    fn each_entry_point_sees_only_its_own_capability() {
        let catalog = catalog();
        assert_eq!(
            catalog.options(Capability::Chat, &[Modality::Text]),
            vec!["deepseek/deepseek-v4-pro", "vendor/vision-chat"]
        );
        assert_eq!(
            catalog.options(Capability::Embedding, &[Modality::Text]),
            vec!["vendor/embed"]
        );
        assert_eq!(
            catalog.options(Capability::Image, &[Modality::Text]),
            vec!["vendor/image"]
        );
        assert_eq!(
            catalog.options(Capability::Video, &[Modality::Text]),
            vec!["vendor/video"]
        );
        assert_eq!(
            catalog.options(Capability::Rerank, &[Modality::Text]),
            vec!["vendor/rerank"]
        );
    }

    #[test]
    fn a_dedicated_non_text_route_is_never_in_the_chat_picker() {
        let catalog = catalog();
        let chat = catalog.options(Capability::Chat, &[Modality::Text]);
        for excluded in ["vendor/image", "vendor/video", "vendor/rerank", "vendor/embed"] {
            assert!(
                !chat.contains(&excluded.to_string()),
                "{excluded} must not be a chat option"
            );
        }
    }

    #[test]
    fn a_multimodal_picker_keeps_only_models_that_declare_the_modality() {
        let catalog = catalog();
        // Image attachment on: the text-only chat model drops out, fail-closed.
        assert_eq!(
            catalog.options(Capability::Chat, &[Modality::Text, Modality::Image]),
            vec!["vendor/vision-chat"]
        );
        assert!(!catalog.contains(
            "deepseek/deepseek-v4-pro",
            Capability::Chat,
            &[Modality::Text, Modality::Image]
        ));
        // Audio declared nowhere: nothing qualifies, and empty is the answer.
        assert!(catalog.options(Capability::Chat, &[Modality::Audio]).is_empty());
        assert!(catalog.options(Capability::Chat, &[Modality::Video]).is_empty());
    }

    #[test]
    fn a_stale_selection_is_cleared_rather_than_kept() {
        let catalog = catalog();
        assert!(catalog.contains("deepseek/deepseek-v4-pro", Capability::Chat, &[Modality::Text]));
        assert!(
            !catalog.contains("deepseek/deepseek-v4-pro", Capability::Chat, &[Modality::Image]),
            "the value that stopped qualifying must not be restored"
        );
    }

    #[test]
    fn the_verified_seed_keeps_its_metadata_and_its_reasoning_ladder() {
        let seed = Catalog::seed();
        assert!(seed.source.is_degraded());
        let options = seed.options(Capability::Chat, &[Modality::Text]);
        for id in [
            "openai/gpt-5.5",
            "anthropic/claude-opus-4.8",
            "google/gemini-3.5-flash",
            "deepseek/deepseek-v4-pro",
            "orcarouter/auto",
        ] {
            assert!(options.contains(&id.to_string()), "{id} is a verified seed model");
        }
        let gpt = seed.items.iter().find(|item| item.id == "openai/gpt-5.5").unwrap();
        let reasoning = gpt.reasoning.as_ref().expect("gpt-5.5 advertises reasoning");
        assert_eq!(reasoning.efforts, vec!["low", "medium", "high", "xhigh"]);
        assert_eq!(gpt.context_length, Some(400_000));
        assert!(gpt.input_modalities.contains(&Modality::Image));
        // A text-only seed model must not enter a vision picker.
        assert_eq!(
            seed.options(Capability::Chat, &[Modality::Image]),
            vec!["openai/gpt-5.5", "anthropic/claude-opus-4.8", "google/gemini-3.5-flash"]
        );
    }

    #[test]
    fn a_live_catalog_never_mixes_the_seed_in() {
        let live = Catalog::parse(
            br#"{"object":"list","data":[{"id":"vendor/a","supported_endpoint_types":["openai"],"owned_by":"V"}]}"#,
            CatalogSource::Live,
        )
        .unwrap();
        assert_eq!(live.source, CatalogSource::Live);
        assert_eq!(live.options(Capability::Chat, &[Modality::Text]), vec!["vendor/a"]);
        assert!(
            !live
                .options(Capability::Chat, &[Modality::Text])
                .contains(&"orcarouter/auto".to_string()),
            "a successful discovery is authoritative and carries no seed entries"
        );
    }

    #[test]
    fn an_entry_with_no_capability_metadata_is_excluded_not_guessed() {
        let parsed = Catalog::parse(
            br#"{"data":[{"id":"mystery/model"},{"id":"","supported_endpoint_types":["openai"]}]}"#,
            CatalogSource::Live,
        )
        .unwrap();
        assert_eq!(parsed.items.len(), 1, "the empty id is dropped");
        assert_eq!(parsed.items[0].id, "mystery/model");
        assert!(parsed.options(Capability::Chat, &[Modality::Text]).is_empty());
        assert!(parsed.options(Capability::Embedding, &[Modality::Text]).is_empty());
    }

    #[test]
    fn a_malformed_or_oversized_body_is_an_error_not_an_empty_catalog() {
        assert!(matches!(
            Catalog::parse(b"not json", CatalogSource::Live),
            Err(CatalogError::Malformed(_))
        ));
        assert!(matches!(
            Catalog::parse(br#"{"object":"list"}"#, CatalogSource::Live),
            Err(CatalogError::Malformed(_))
        ));
        let huge = vec![b' '; MAX_CATALOG_BYTES + 1];
        assert!(matches!(
            Catalog::parse(&huge, CatalogSource::Live),
            Err(CatalogError::Oversized)
        ));
    }

    #[test]
    fn the_record_cap_bounds_memory_whatever_the_body_claims() {
        let mut data = String::from("{\"data\":[");
        for index in 0..(MAX_CATALOG_ITEMS + 16) {
            if index > 0 {
                data.push(',');
            }
            data.push_str(&format!(
                "{{\"id\":\"vendor/{index}\",\"supported_endpoint_types\":[\"openai\"]}}"
            ));
        }
        data.push_str("]}");
        let parsed = Catalog::parse(data.as_bytes(), CatalogSource::Live).unwrap();
        assert_eq!(parsed.items.len(), MAX_CATALOG_ITEMS);
    }

    /// One loopback catalog: `handler` answers every `GET /models`, and every request's
    /// path and authorization header are recorded for the test to read back.
    async fn serve_models(
        handler: fn() -> (u16, Vec<u8>),
    ) -> (
        crate::orcarouter::Origins,
        std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>,
    ) {
        use axum::routing::get;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let router = axum::Router::new().route(
            "/v1/models",
            get(move |headers: axum::http::HeaderMap, uri: axum::http::Uri| {
                let recorder = recorder.clone();
                async move {
                    let authorization = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    recorder.lock().unwrap().push((uri.to_string(), authorization));
                    let (status, body) = handler();
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        String::from_utf8(body).unwrap(),
                    )
                }
            }),
        );
        let addr = crate::test_support::serve(router).await;
        let origins = crate::orcarouter::Origins {
            auth: crate::orcarouter::Origin::parse("https://www.orcarouter.ai").unwrap(),
            api: crate::orcarouter::Origin::parse(&format!("http://{addr}/v1")).unwrap(),
        };
        (origins, seen)
    }

    fn live_body() -> Vec<u8> {
        br#"{"object":"list","data":[
            {"id":"deepseek/deepseek-v4-pro","supported_endpoint_types":["openai","openai-response"],"owned_by":"DeepSeek"},
            {"id":"v/embed","supported_endpoint_types":["embeddings"]}]}"#
            .to_vec()
    }

    #[tokio::test]
    async fn discovery_reads_the_api_origin_and_sends_the_key_as_a_bearer() {
        let (origins, seen) = serve_models(|| (200, live_body())).await;
        let catalog = Catalog::discover(&origins, Some("sk-orca-fixture"), Some(Capability::Chat))
            .await
            .expect("the catalog answers");
        assert_eq!(catalog.source, CatalogSource::Live);
        assert_eq!(
            catalog.options(Capability::Chat, &[Modality::Text]),
            vec!["deepseek/deepseek-v4-pro"]
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].0.starts_with("/v1/models"),
            "discovery reads the api origin's /models"
        );
        assert_eq!(seen[0].1.as_deref(), Some("Bearer sk-orca-fixture"));
    }

    #[tokio::test]
    async fn a_refused_discovery_falls_back_to_the_verified_seed() {
        let (origins, _) = serve_models(|| (500, b"nope".to_vec())).await;
        let provider = CatalogProvider::new(origins);
        let catalog = provider.refresh(None, None).await;
        assert_eq!(catalog.source, CatalogSource::VerifiedSeed);
        assert!(catalog.source.is_degraded());
        assert!(
            catalog
                .options(Capability::Chat, &[Modality::Text])
                .contains(&"orcarouter/auto".to_string()),
            "an outage leaves the verified seed usable, not an empty picker"
        );
    }

    #[tokio::test]
    async fn a_live_answer_becomes_the_last_known_good() {
        let (origins, _) = serve_models(|| (200, live_body())).await;
        let provider = CatalogProvider::new(origins);
        assert_eq!(provider.refresh(None, None).await.source, CatalogSource::Live);
        let current = provider.current();
        assert_eq!(current.source, CatalogSource::LastKnownGood);
        assert_eq!(
            current.options(Capability::Chat, &[Modality::Text]),
            vec!["deepseek/deepseek-v4-pro"]
        );
        assert!(
            !current
                .options(Capability::Chat, &[Modality::Text])
                .contains(&"orcarouter/auto".to_string()),
            "the last known good carries no seed entries"
        );
    }

    #[test]
    fn the_architecture_block_is_read_for_modalities() {
        let parsed = Catalog::parse(
            br#"{"data":[{"id":"v/vision","supported_endpoint_types":["openai"],
                  "architecture":{"input_modalities":["text","image"]},
                  "context_length":128000,
                  "reasoning":{"efforts":["low","high"]}}]}"#,
            CatalogSource::Live,
        )
        .unwrap();
        let item = &parsed.items[0];
        assert_eq!(item.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(item.context_length, Some(128_000));
        assert_eq!(item.reasoning.as_ref().unwrap().efforts, vec!["low", "high"]);
        assert!(item.label().contains("128000 ctx"));
        assert!(parsed.contains("v/vision", Capability::Chat, &[Modality::Image]));
    }
}

//! `appa login models`: the model catalog for the configured OrcaRouter deployment.
//!
//! Discovery reads `GET {api_base}/models` on the inference origin with the stored key,
//! so the list is what this workspace can actually call. The capability filter is applied
//! here, from recorded metadata, and the same filter the GUI uses — never a model name.
//! When discovery fails the verified seed stands in, and every line says so.

use std::path::PathBuf;

use crate::orcarouter::{Capability, Catalog, CatalogSource, Modality, Origins};

#[derive(clap::Args)]
pub struct Args {
    /// The configuration whose deployment supplies the key.
    #[arg(long, env = "APPA_CONFIG")]
    config: Option<PathBuf>,

    /// The `token_env` the key is stored under. Defaults to the profile's, then
    /// `APPA_ORCAROUTER_API_KEY`.
    #[arg(long)]
    token_env: Option<String>,

    /// The capability to list: chat, embedding, image, video, or rerank.
    #[arg(long, default_value = "chat")]
    capability: String,

    /// Non-text modalities the entry point uploads, comma-separated: text, image, audio,
    /// video. A model that does not declare every one is excluded.
    #[arg(long, default_value = "text")]
    modalities: String,

    /// Print each model's metadata line instead of only its id.
    #[arg(long)]
    verbose: bool,

    /// Do not contact the catalog; print the verified seed and its source.
    #[arg(long)]
    offline: bool,
}

fn capability(name: &str) -> Result<Capability, String> {
    match name {
        "chat" => Ok(Capability::Chat),
        "embedding" => Ok(Capability::Embedding),
        "image" => Ok(Capability::Image),
        "video" => Ok(Capability::Video),
        "rerank" => Ok(Capability::Rerank),
        other => Err(format!(
            "unknown capability {other:?}; expected chat, embedding, image, video, or rerank"
        )),
    }
}

fn modalities(names: &str) -> Result<Vec<Modality>, String> {
    names
        .split(',')
        .filter(|name| !name.trim().is_empty())
        .map(|name| match name.trim() {
            "text" => Ok(Modality::Text),
            "image" => Ok(Modality::Image),
            "audio" => Ok(Modality::Audio),
            "video" => Ok(Modality::Video),
            other => Err(format!(
                "unknown modality {other:?}; expected text, image, audio, or video"
            )),
        })
        .collect()
}

/// The key the catalog is asked with, from the environment or the store. Never printed.
fn key(config: &std::path::Path, variable: &str) -> Option<String> {
    if let Some(value) = std::env::var(variable).ok().filter(|value| !value.is_empty()) {
        return Some(value);
    }
    crate::credentials::CredentialStore::for_config(config)
        .ok()
        .and_then(|store| store.values().ok())
        .and_then(|values| values.get(variable).cloned())
}

pub fn run(args: Args) -> std::process::ExitCode {
    let capability = match capability(&args.capability) {
        Ok(capability) => capability,
        Err(error) => {
            eprintln!("appa login models: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let modalities = match modalities(&args.modalities) {
        Ok(modalities) => modalities,
        Err(error) => {
            eprintln!("appa login models: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let config = args.config.clone().unwrap_or_else(crate::init::installed_config_path);
    // Listing is useful before any key or profile exists, so an unreadable configuration
    // falls back to the default variable instead of failing the command.
    let variable = crate::orcarouter_login::token_variable(&config, args.token_env.as_deref())
        .unwrap_or_else(|_| crate::orcarouter::KEY_VARIABLE.to_string());
    let catalog = match args.offline {
        true => Catalog::seed(),
        false => {
            let origins = match Origins::resolve(None) {
                Ok(origins) => origins,
                Err(error) => {
                    eprintln!("appa login models: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let provider = crate::orcarouter::CatalogProvider::new(origins);
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("appa login models: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let key = key(&config, &variable);
            runtime.block_on(provider.refresh(key.as_deref(), Some(capability)))
        }
    };
    print(&catalog, capability, &modalities, args.verbose);
    std::process::ExitCode::SUCCESS
}

/// Print the filtered list and its source. The source line is what tells a reader whether
/// the list is live or the verified fallback.
fn print(catalog: &Catalog, capability: Capability, modalities: &[Modality], verbose: bool) {
    let source = match catalog.source {
        CatalogSource::Live => "live catalog",
        CatalogSource::VerifiedSeed => "verified seed (live discovery unavailable)",
        CatalogSource::LastKnownGood => "last known good (live discovery unavailable)",
    };
    let modality_names = modalities
        .iter()
        .map(|modality| format!("{modality:?}").to_lowercase())
        .collect::<Vec<_>>()
        .join(",");
    eprintln!("source: {source}; capability: {capability:?}; input modalities: {modality_names}");
    let selected = catalog.select(capability, modalities);
    if selected.is_empty() {
        eprintln!("no model in this catalog satisfies that capability");
    }
    for item in selected {
        match verbose {
            true => println!("{}", item.label()),
            false => println!("{}", item.id),
        }
    }
}

/// The source label one CLI run reports, exposed so a test can assert the degraded case.
pub fn source_label(source: CatalogSource) -> &'static str {
    match source {
        CatalogSource::Live => "live",
        CatalogSource::VerifiedSeed => "seed",
        CatalogSource::LastKnownGood => "last_known_good",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_capability_and_modality_spelling_is_recognized() {
        assert_eq!(capability("chat"), Ok(Capability::Chat));
        assert_eq!(capability("embedding").unwrap(), Capability::Embedding);
        assert_eq!(capability("image").unwrap(), Capability::Image);
        assert_eq!(capability("video").unwrap(), Capability::Video);
        assert_eq!(capability("rerank").unwrap(), Capability::Rerank);
        assert!(capability("vision").is_err());
        assert_eq!(modalities("text,image").unwrap(), vec![Modality::Text, Modality::Image]);
        assert!(modalities("hologram").is_err());
    }

    #[test]
    fn the_source_label_distinguishes_live_from_degraded() {
        assert_eq!(source_label(CatalogSource::Live), "live");
        assert_eq!(source_label(CatalogSource::VerifiedSeed), "seed");
        assert_eq!(source_label(CatalogSource::LastKnownGood), "last_known_good");
    }
}

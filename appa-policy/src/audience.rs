use std::collections::BTreeSet;

use serde::Deserialize;

use appa_engine::audience::{AudienceConfig, DeclaredTemplate, NamedAudience, SelectorSpec, SourceRegistration};
use appa_engine::label::{ChainAudience, ReaderId};
use appa_engine::names::GroupName;

use crate::error::ConfigError;
use crate::raw::RawAudience;

/// One `selectors` entry of an `[externals.audience.<provider>]` binding, as written: the
/// template the source serves and what its collections may feed.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectorDeclaration {
    pub template: String,
    pub feeds: Option<String>,
}

/// The templates one source declares under `selectors`. A template is `/`-separated
/// non-empty segments, each a literal or a `<variable>`; a segment may not start with `$`,
/// which marks an argument placeholder in a policy's spelling of a selector. `feeds` names
/// what the collections may feed beyond named audiences and direct mentions: `self` or
/// `internal`. A source declares at least one template and none twice.
pub fn declare_templates(
    provider: &str,
    selectors: &[SelectorDeclaration],
) -> Result<Vec<DeclaredTemplate>, ConfigError> {
    let refused = |template: &str, reason: &str| ConfigError::BadSelectorDeclaration {
        provider: provider.to_string(),
        template: template.to_string(),
        reason: reason.to_string(),
    };
    let mut templates: Vec<DeclaredTemplate> = Vec::new();
    for selector in selectors {
        let template = selector.template.as_str();
        if template.is_empty() {
            return Err(refused(template, "is empty"));
        }
        for segment in template.split('/') {
            if segment.is_empty() {
                return Err(refused(template, "has an empty segment"));
            }
            if segment.starts_with('$') {
                return Err(refused(
                    template,
                    "has a segment starting with `$`, which marks an argument placeholder",
                ));
            }
            let bracketed = segment.starts_with('<') || segment.ends_with('>');
            if bracketed && !(segment.starts_with('<') && segment.ends_with('>') && segment.len() > 2) {
                return Err(refused(template, "has a malformed `<variable>` segment"));
            }
        }
        let feeds = match &selector.feeds {
            None => None,
            Some(level) => Some(
                ChainAudience::parse(level)
                    .ok_or_else(|| refused(template, "`feeds` names a built-in audience: `self` or `internal`"))?,
            ),
        };
        if templates.iter().any(|known| known.template.as_str() == template) {
            return Err(refused(template, "is declared twice"));
        }
        templates.push(DeclaredTemplate::new(template, feeds));
    }
    if templates.is_empty() {
        return Err(refused("", "`selectors` declares no template"));
    }
    Ok(templates)
}

/// The audience sources a configuration document declares: every `[externals.audience.<p>]`
/// entry with a `selectors` list, as [`Config::from_toml_str_routed`](crate::Config::from_toml_str_routed) takes them. An entry
/// without `selectors` — a roster — declares no source. The list's shape is validated here;
/// how the entry answers is the deployment's.
pub fn declared_sources(document: &toml::Value) -> Result<Vec<SourceRegistration>, ConfigError> {
    let Some(entries) = document
        .get("externals")
        .and_then(|externals| externals.get("audience"))
        .and_then(toml::Value::as_table)
    else {
        return Ok(Vec::new());
    };
    let mut sources = Vec::new();
    for (provider, entry) in entries {
        let Some(selectors) = entry.get("selectors") else {
            continue;
        };
        let selectors: Vec<SelectorDeclaration> =
            selectors
                .clone()
                .try_into()
                .map_err(|error: toml::de::Error| ConfigError::BadSelectorDeclaration {
                    provider: provider.clone(),
                    template: String::new(),
                    reason: format!("`selectors` is a list of `{{ template, feeds }}` tables: {error}"),
                })?;
        sources.push(SourceRegistration {
            provider: provider.clone(),
            templates: declare_templates(provider, &selectors)?,
        });
    }
    Ok(sources)
}

/// Compile `[audience]` into the engine's audience configuration. Each `from` selector must
/// name a stock collection whose role fits its level. Every declared source is carried; the
/// caller keeps the ones the policy references — here, or by a mention or placeholder in a
/// tool contract or an annotator mandate — since a provider enters the registered sources,
/// and so the policy identity, exactly when the policy names it.
pub(crate) fn convert_audience(
    audience: Option<RawAudience>,
    sources: Vec<SourceRegistration>,
) -> Result<(AudienceConfig, BTreeSet<String>), ConfigError> {
    let mut config = AudienceConfig::default();
    let mut providers: BTreeSet<String> = BTreeSet::new();
    let mut selectors = |list: &[String],
                         context: &str,
                         admits: fn(Option<ChainAudience>) -> bool,
                         expected: &str|
     -> Result<Vec<SelectorSpec>, ConfigError> {
        let refused = |selector: &str, reason: String| ConfigError::BadAudienceSource {
            context: context.to_string(),
            selector: selector.to_string(),
            reason,
        };
        let mut specs = Vec::new();
        for entry in list {
            let spec = SelectorSpec::parse(entry)
                .ok_or_else(|| refused(entry, "is not a `<provider>:<selector>` source".to_string()))?;
            let source = sources
                .iter()
                .find(|source| source.provider == spec.provider)
                .ok_or_else(|| ConfigError::UndeclaredProvider {
                    context: context.to_string(),
                    provider: spec.provider.clone(),
                })?;
            let declared = source
                .templates
                .iter()
                .find(|declared| declared.template.matches(&spec.selector))
                .ok_or_else(|| {
                    let served: Vec<&str> = source
                        .templates
                        .iter()
                        .map(|declared| declared.template.as_str())
                        .collect();
                    refused(
                        entry,
                        format!(
                            "names no collection {} serves; it serves {}",
                            source.provider,
                            served.join(", ")
                        ),
                    )
                })?;
            if !admits(declared.feeds) {
                return Err(refused(entry, format!("cannot feed this audience — {expected}")));
            }
            providers.insert(spec.provider.clone());
            specs.push(spec);
        }
        Ok(specs)
    };
    if let Some(audience) = audience {
        if let Some(from) = audience.self_from {
            config.self_from = selectors(
                &from,
                "[audience] self",
                |feeds| feeds == Some(ChainAudience::Self_),
                "`self` reads only collections declared to feed it",
            )?;
        }
        if let Some(from) = audience.internal {
            config.internal_from = selectors(
                &from,
                "[audience] internal",
                |feeds| feeds == Some(ChainAudience::Internal),
                "`internal` reads only collections declared to feed it",
            )?;
        }
        for (name, group) in audience.group {
            let bad = |reason: &str| ConfigError::BadNamedAudience {
                name: name.clone(),
                reason: reason.to_string(),
            };
            if name.is_empty() {
                return Err(bad("a named audience needs a name"));
            }
            if name.starts_with('@') {
                return Err(bad("the name is written bare here; `@` marks a mention"));
            }
            if !ReaderId::new(name.clone()).is_literal() || SelectorSpec::parse(&name).is_some() {
                return Err(bad("this spelling is reserved"));
            }
            let within =
                match &group.within {
                    None => None,
                    Some(target) => Some(ChainAudience::parse(target).ok_or_else(|| {
                        bad("`within` asserts containment in a built-in audience: `self` or `internal`")
                    })?),
                };
            let from = selectors(
                &group.from,
                &format!("[audience.group.{name}]"),
                |feeds| feeds != Some(ChainAudience::Self_),
                "a named audience reads collections, and `viewer` names the requesting principal",
            )?;
            config.groups.push(NamedAudience {
                name: GroupName::new(name),
                within,
                from,
            });
        }
    }
    config.sources = sources;
    Ok((config, providers))
}

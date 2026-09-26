use std::collections::BTreeSet;

use appa_engine::audience::{AudienceConfig, NamedAudience, SelectorSpec, SourceRegistration};
use appa_engine::label::{ChainAudience, ReaderId};
use appa_engine::names::GroupName;

use crate::error::ConfigError;
use crate::raw::RawAudience;

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
                .find(|source| source.provider.as_str() == spec.provider)
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
                            source.provider.as_str(),
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

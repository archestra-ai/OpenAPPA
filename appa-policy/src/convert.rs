use std::collections::BTreeMap;

use appa_engine::authority::{
    Attends, Authority, DeclaredTransition, Hint, HintTooLong, Mandate, Sanitizer, SanitizerPoints, Scope,
};
use appa_engine::contract::{
    AudienceRequirement, Delta, DeltaAudience, HistoryRequirement, LabelRequirements, RecipientSpec, Requires,
    SelectorPlaceholder, ToolAnnotation, ToolDeclaration,
};
use appa_engine::fact::{EffectKind, EffectSet};
use appa_engine::label::{Audience, DeclaredAudience, Label, Trust};
use appa_engine::names::{AnnotatorName, AuthorityName, MarkName, SanitizerName, SurfaceName, TagName};
use appa_engine::params::ToolParameters;
use appa_engine::profile::{BindingMode, ExecutorClass, ProfileDeclaration, neutral_starting_label};
use appa_engine::registry::{AudienceVocabulary, LoadError, MAX_HINT_CHARS, TrustChain};
use appa_engine::value::ToolName;

use crate::error::ConfigError;
use crate::raw::{
    RawAuthority, RawBoundary, RawDelta, RawDeployment, RawPermits, RawRequires, RawSanitizer, RawSanitizerPermits,
    RawStartingAudience, RawTool,
};

impl RawDeployment {
    pub(crate) fn convert(self, chain: &TrustChain) -> Result<ProfileDeclaration, ConfigError> {
        let neutral = neutral_starting_label(chain);
        let starting_label = match self.starting_label {
            Some(label) => {
                let trust = match label.trust {
                    Some(name) => parse_trust(&name, chain, "deployment starting_label")?,
                    None => neutral.trust,
                };
                let audience = match label.audience {
                    None => Audience::public(),
                    Some(RawStartingAudience::Token(token)) if token == "public" => Audience::public(),
                    Some(RawStartingAudience::Token(token)) => {
                        return Err(ConfigError::BadDeploymentToken {
                            field: "starting_label audience",
                            expected: r#""public" or [...]"#,
                            found: token,
                        });
                    }
                    Some(RawStartingAudience::List(a)) => parse_audience(&a, "deployment starting_label audience")?,
                };
                Label::new(trust, audience)
            }
            None => neutral,
        };

        let mut executor_exceptions = BTreeMap::new();
        for name in self.assumed_tools {
            executor_exceptions.insert(ToolName::new(name), ExecutorClass::Assumed);
        }
        for name in self.provider_run_tools {
            let tool = ToolName::new(name);
            if executor_exceptions
                .insert(tool.clone(), ExecutorClass::ProviderRun)
                .is_some_and(|previous| previous != ExecutorClass::ProviderRun)
            {
                return Err(ConfigError::ConflictingExecutorException {
                    tool: tool.as_str().to_string(),
                });
            }
        }

        Ok(ProfileDeclaration {
            starting_label,
            context_control: self.context_control.unwrap_or(false),
            dispatch: self.dispatch.unwrap_or(ExecutorClass::Assumed),
            executor_exceptions,
            confined_results: self.confined_results.into_iter().map(ToolName::new).collect(),
            provider_surfaces: self
                .provider_surfaces
                .into_iter()
                .map(|(surface, mode)| (SurfaceName::new(surface), mode))
                .collect(),
            binding: self.binding.unwrap_or(BindingMode::Harness),
        })
    }
}

impl RawBoundary {
    pub(crate) fn convert(self, chain: &TrustChain) -> Result<Label, ConfigError> {
        let trust = match self.trust {
            Some(name) => parse_trust(&name, chain, "boundary")?,
            None => top_trust(chain),
        };
        let audience = match self.audience {
            Some(a) => parse_audience(&a, "boundary audience")?,
            None => Audience::public(),
        };
        Ok(Label::new(trust, audience))
    }
}

pub(crate) fn default_boundary_label(chain: &TrustChain) -> Label {
    Label::new(top_trust(chain), Audience::public())
}

fn top_trust(chain: &TrustChain) -> Trust {
    Trust::new((chain.len() - 1) as u8)
}

impl RawTool {
    pub(crate) fn convert(mut self, chain: &TrustChain) -> Result<ToolDeclaration, ConfigError> {
        if let Some(server) = self.server.take() {
            let (name, selector) = split_selector(&self.name);
            let invalid = |reason: String| ConfigError::ToolServer {
                tool: self.name.clone(),
                reason,
            };
            if name.starts_with("mcp__") || name.contains('/') {
                return Err(invalid("use a short tool name with server".into()));
            }
            let id = appa_runtime_api::CanonicalTool::of("mcp", &server, name)
                .map_err(|error| invalid(error.to_string()))?;
            self.name = format!("{id}{selector}");
        }
        let ctx = || format!("tool {}", self.name);
        refuse_inline_binding("tool", &self.name, self.implementation.as_ref())?;
        let parameters = match &self.parameters {
            Some(authored) => ToolParameters::compile(authored).map_err(|source| ConfigError::ToolParameters {
                tool: self.name.clone(),
                source,
            })?,
            None => ToolParameters::open(),
        };
        if let Some(annotator) = self.annotator {
            let statics = match (&self.delta, &self.requires, self.effects.is_empty()) {
                (Some(_), _, _) => Some("delta"),
                (None, Some(_), _) => Some("requires"),
                (None, None, false) => Some("effects"),
                (None, None, true) => None,
            };
            if let Some(field) = statics {
                return Err(ConfigError::AnnotatorWithStatics {
                    tool: self.name,
                    annotator,
                    field,
                });
            }
            return Ok(ToolDeclaration::Annotated {
                name: ToolName::new(self.name),
                tags: self.tags.into_iter().map(TagName::new).collect(),
                description: self.description,
                parameters,
                annotator: AnnotatorName::new(annotator),
            });
        }
        // Declaring the tool is the deployment saying it knows it, so an omitted `delta` and
        // `delta = {}` say the same thing: the dimensions this annotation does not describe
        // restrict nothing.
        let delta = match self.delta {
            Some(d) => d.convert(chain, &ctx())?,
            None => Delta::default(),
        };
        let requires = match self.requires {
            Some(r) => r.convert(chain, &ctx())?,
            None => Requires::default(),
        };
        let emits = EffectSet::new(self.effects.into_iter().map(EffectKind::new)).map_err(|duplicate| {
            ConfigError::DuplicateEffect {
                tool: self.name.clone(),
                kind: duplicate.0.as_str().to_string(),
            }
        })?;
        Ok(ToolDeclaration::Declared(ToolAnnotation {
            name: ToolName::new(self.name),
            description: self.description,
            tags: self.tags.into_iter().map(TagName::new).collect(),
            parameters,
            delta,
            emits,
            requires,
        }))
    }
}

impl RawDelta {
    pub(crate) fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Delta, ConfigError> {
        let trust = match self.trust.as_deref() {
            Some(value) => Some(parse_trust(value, chain, ctx)?),
            None => None,
        };
        let audience = match self.audience {
            Some(a) => Some(parse_delta_audience(&a, &format!("{ctx} delta audience"))?),
            None => None,
        };
        Ok(Delta { trust, audience })
    }
}

/// The `requires` table as the policy writes it; a tool an Annotator covers receives its
/// `requires` in the per-call annotation instead.
impl RawRequires {
    pub(crate) fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Requires, ConfigError> {
        let mut audience = Vec::new();
        if let Some(a) = self.audience {
            if let Some(inc) = a.contains {
                audience.push(AudienceRequirement::Includes(parse_recipient_spec(
                    &inc,
                    &format!("{ctx} requires contains"),
                )?));
            }
            if let Some(cap) = a.within {
                audience.push(AudienceRequirement::Cap(parse_declared_audience(
                    &cap,
                    &format!("{ctx} requires within"),
                )?));
            }
        }
        let mut history = Vec::new();
        if let Some(e) = self.effects {
            history.extend(
                e.contains
                    .into_iter()
                    .map(|k| HistoryRequirement::Prior(EffectKind::new(k))),
            );
            history.extend(
                e.excludes
                    .into_iter()
                    .map(|k| HistoryRequirement::NoPrior(EffectKind::new(k))),
            );
        }
        let trust_floor = match self.trust.as_deref() {
            Some(value) => Some(parse_trust(value, chain, ctx)?),
            None => None,
        };
        let attention = match self.attention {
            Some(marks) => marks.into_iter().map(MarkName::new).collect(),
            None => Vec::new(),
        };
        Ok(Requires {
            label: LabelRequirements { trust_floor, audience },
            history,
            attention,
        })
    }
}

impl RawAuthority {
    pub(crate) fn convert(self, chain: &TrustChain) -> Result<Authority, ConfigError> {
        refuse_inline_binding("authority", &self.name, self.implementation.as_ref())?;
        let ctx = format!("authority {}", self.name);
        let mandate = self.permits.convert(chain, &ctx)?;
        let hint = parse_hint(self.hint, &ctx)?;
        Ok(Authority {
            name: AuthorityName::new(self.name),
            mandate,
            scope: Scope {
                tags: self.tags.into_iter().map(TagName::new).collect(),
            },
            hint,
        })
    }
}

impl RawPermits {
    pub(crate) fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Mandate, ConfigError> {
        Ok(Mandate {
            trust_ceiling: self.trust_below.map(|t| parse_trust(&t, chain, ctx)).transpose()?,
            reader_ceiling: self
                .audience_missing
                .map(|r| parse_declared_audience(&r, &format!("{ctx} audience_missing")))
                .transpose()?,
            waivers: self.effects_containing.into_iter().map(EffectKind::new).collect(),
            attends: parse_attends(self.attention, ctx)?,
        })
    }
}

/// `["*"]` is the catch-all; the wildcard beside a name is refused rather than read as
/// either a name or a catch-all.
fn parse_attends(attention: Vec<String>, context: &str) -> Result<Attends, ConfigError> {
    let wildcards = attention.iter().filter(|mark| *mark == Attends::WILDCARD).count();
    match (wildcards, attention.len()) {
        (0, _) => Ok(Attends::Named(attention.into_iter().map(MarkName::new).collect())),
        (1, 1) => Ok(Attends::Any),
        _ => Err(ConfigError::MixedAttentionWildcard {
            context: context.to_string(),
        }),
    }
}

impl RawSanitizer {
    pub(crate) fn convert(self, chain: &TrustChain) -> Result<Sanitizer, ConfigError> {
        refuse_inline_binding("sanitizer", &self.name, self.implementation.as_ref())?;
        let on = parse_points(&self.on, &self.name)?;
        let transition = self.permits.convert(chain, &self.name)?;
        let hint = parse_hint(self.hint, &format!("sanitizer {}", self.name))?;
        Ok(Sanitizer {
            name: SanitizerName::new(self.name),
            on,
            transition,
            scope: Scope {
                tags: self.tags.into_iter().map(TagName::new).collect(),
            },
            hint,
        })
    }
}

impl RawSanitizerPermits {
    pub(crate) fn convert(self, chain: &TrustChain, name: &str) -> Result<DeclaredTransition, ConfigError> {
        match (self.audience, self.trust) {
            (Some(audience), None) => Ok(DeclaredTransition::Audience {
                from_includes: parse_declared_audience(&audience.from, &format!("sanitizer {name} from"))?,
                to: parse_declared_audience(&audience.to, &format!("sanitizer {name} to"))?,
            }),
            (None, Some(trust)) => Ok(DeclaredTransition::Trust {
                from_floor: parse_trust(&trust.from, chain, &format!("sanitizer {name} from"))?,
                to: parse_trust(&trust.to, chain, &format!("sanitizer {name} to"))?,
            }),
            (Some(_), Some(_)) => Err(ConfigError::SanitizerMandateShape {
                name: name.to_string(),
                reason: "declares both an audience and a trust transition — a sanitizer permits one dimension",
            }),
            (None, None) => Err(ConfigError::SanitizerMandateShape {
                name: name.to_string(),
                reason: "declares no transition — give `permits` an `audience` or a `trust` key",
            }),
        }
    }
}

/// One label delta in the spelling a tool's `delta` takes — a trust rank name and an audience
/// list — for a label a runtime reads at its own boundary, such as the return floor
/// `execute_remedy_plan` declares.
pub fn parse_delta(
    trust: Option<&str>,
    audience: Option<&[String]>,
    chain: &TrustChain,
    context: &str,
) -> Result<Delta, ConfigError> {
    RawDelta {
        trust: trust.map(str::to_string),
        audience: audience.map(<[String]>::to_vec),
    }
    .convert(chain, context)
}

pub(crate) fn refuse_inline_binding(
    kind: &'static str,
    name: &str,
    implementation: Option<&toml::Value>,
) -> Result<(), ConfigError> {
    match implementation {
        Some(_) => Err(ConfigError::ForbiddenInlineBinding {
            kind,
            name: name.to_string(),
        }),
        None => Ok(()),
    }
}

pub(crate) fn parse_hint(hint: Option<String>, context: &str) -> Result<Option<Hint>, ConfigError> {
    hint.map(Hint::new).transpose().map_err(|HintTooLong { len }| {
        ConfigError::Registry(LoadError::HintTooLong {
            context: context.to_string(),
            len,
            max: MAX_HINT_CHARS,
        })
    })
}

/// A tool name split before its argument selector, if any: `send(to)` → (`send`, `(to)`).
pub(crate) fn split_selector(name: &str) -> (&str, &str) {
    name.split_at(name.find('(').unwrap_or(name.len()))
}

pub(crate) fn parse_trust(name: &str, chain: &TrustChain, context: &str) -> Result<Trust, ConfigError> {
    chain.rank_of(name).ok_or_else(|| ConfigError::UnknownTrustRank {
        name: name.to_string(),
        context: context.to_string(),
    })
}

fn parse_audience(list: &[String], context: &str) -> Result<Audience, ConfigError> {
    Ok(Audience::of_declared(&parse_declared_audience(list, context)?))
}

/// One written audience list, in the engine's one grammar for a written list and an
/// annotation answer alike: the union of its entries, `public` alone, at most one chain word.
fn parse_declared_audience(list: &[String], context: &str) -> Result<DeclaredAudience, ConfigError> {
    DeclaredAudience::parse_entries(list).map_err(|error| ConfigError::BadAudience {
        context: context.to_string(),
        reason: error.to_string(),
    })
}

/// The audience vocabulary an `[[annotator]]` mandate's `audiences` admits: chain words, group
/// references, and literal readers, each on its own. `audiences = []` closes the mandate to
/// `public` answers only — the one spelling of that bound, distinct from an omitted mandate,
/// which admits the whole policy vocabulary. (An ordinary declared audience still refuses the
/// empty list: a value some sink reads must name somebody.)
pub(crate) fn parse_annotator_audiences(list: &[String], context: &str) -> Result<AudienceVocabulary, ConfigError> {
    AudienceVocabulary::parse_entries(list).map_err(|error| ConfigError::BadAudience {
        context: context.to_string(),
        reason: error.to_string(),
    })
}

fn parse_recipient_spec(list: &[String], context: &str) -> Result<RecipientSpec, ConfigError> {
    if list.len() == 1
        && let Some(arg) = list[0].strip_prefix('$')
    {
        return Ok(RecipientSpec::Placeholder(arg.to_string()));
    }
    if let Some(ph) = list.iter().find(|r| r.starts_with('$')) {
        return Err(ConfigError::BadAudience {
            context: context.to_string(),
            reason: format!("placeholder {ph:?} must be the sole recipient"),
        });
    }
    if let Some(placeholder) = selector_placeholder(list, context)? {
        return Ok(RecipientSpec::Selector(placeholder));
    }
    Ok(RecipientSpec::Static(parse_declared_audience(list, context)?))
}

/// A `delta.audience` list: a static declared audience, or one selector placeholder alone.
fn parse_delta_audience(list: &[String], context: &str) -> Result<DeltaAudience, ConfigError> {
    match selector_placeholder(list, context)? {
        Some(placeholder) => Ok(DeltaAudience::Selector(placeholder)),
        None => Ok(DeltaAudience::Static(parse_declared_audience(list, context)?)),
    }
}

/// The selector placeholder a written list spells, which must be its sole entry: a
/// placeholder names one collection per call, and a union around it would have no single
/// spelling the check could instantiate. `None` when no entry spells one.
fn selector_placeholder(list: &[String], context: &str) -> Result<Option<SelectorPlaceholder>, ConfigError> {
    let placeholder = list
        .iter()
        .find_map(|entry| entry.strip_prefix('@').and_then(SelectorPlaceholder::parse));
    match placeholder {
        None => Ok(None),
        Some(placeholder) if list.len() == 1 => Ok(Some(placeholder)),
        Some(placeholder) => Err(ConfigError::BadAudience {
            context: context.to_string(),
            reason: format!("selector placeholder \"@{placeholder}\" must be the sole entry"),
        }),
    }
}

fn parse_points(tokens: &[String], name: &str) -> Result<SanitizerPoints, ConfigError> {
    let mut points = SanitizerPoints {
        input: false,
        output: false,
    };
    for token in tokens {
        match token.as_str() {
            "tool_input" => points.input = true,
            "tool_output" => points.output = true,
            other => {
                return Err(ConfigError::UnknownSanitizerPoint {
                    token: other.to_string(),
                });
            }
        }
    }
    if !points.input && !points.output {
        return Err(ConfigError::NoSanitizerPoint { name: name.to_string() });
    }
    Ok(points)
}

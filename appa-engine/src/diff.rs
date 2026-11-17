//! Contract comparison: how one policy's tool contracts differ from another's, and which way
//! each difference moves the calls they gate. The comparison is structural: it resolves no
//! call and consults no Annotator or audience source.
//!
//! A direction is claimed only where it is monotone by construction. A higher trust floor,
//! an added `contains` requirement, an added `prior` or `no_prior` requirement, and an added
//! attention mark each add a gap to every call that met the old contract, so they `tighten`;
//! the reverse changes `loosen`. A tool that gains a contract where nothing covered it was
//! refused before and is judged now, so it loosens; one that loses its contract to nothing
//! is refused now, so it tightens. Every other difference is `unknown`: argument predicates,
//! Annotator routing and mandates, `delta`, `forwards`, `emits`, parameters, tags,
//! descriptions, the order overlapping names are tried in, provider-run tools, and the
//! policy-wide trust chain, authorities, sanitizers, audience configuration and deployment
//! profile. A policy-wide difference is reported on the `*` entry.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::contract::{AudienceRequirement, HistoryRequirement, LabelRequirements, RecipientSpec, ToolDeclaration};
use crate::label::{DeclaredAudience, Trust};
use crate::registry::{Registry, ToolMatcher, TrustChain, WILDCARD_SPELLING};
use crate::value::ToolName;

/// Which way a difference moves the calls a contract gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Tighten,
    Loosen,
    Mixed,
    Unknown,
}

impl Direction {
    fn join(self, other: Direction) -> Direction {
        match (self, other) {
            (Direction::Unknown, _) | (_, Direction::Unknown) => Direction::Unknown,
            (one, other) if one == other => one,
            _ => Direction::Mixed,
        }
    }
}

/// What part of a contract, or of the policy around it, differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Contract,
    TrustFloor,
    Includes,
    Prior,
    NoPrior,
    Attention,
    Delta,
    Forwards,
    Emits,
    Parameters,
    Tags,
    Description,
    Annotator,
    ArgumentPredicates,
    Selection,
    ProviderRun,
    TrustChain,
    Authorities,
    Sanitizers,
    Audience,
    Deployment,
    /// How the deployment binds an external component the policy names: an Annotator's,
    /// authority's or sanitizer's implementation, or an audience source.
    Bindings,
}

/// One difference, with the value before and after where it has a spelling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContractChange {
    pub kind: ChangeKind,
    pub direction: Direction,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Every difference one tool name's contracts carry, and the direction they join to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolContractDiff {
    pub tool: String,
    pub direction: Direction,
    pub changes: Vec<ContractChange>,
    pub annotator_changed: bool,
    pub argument_predicates: bool,
}

#[derive(Default)]
struct Changes {
    changes: Vec<ContractChange>,
    annotator_changed: bool,
    argument_predicates: bool,
}

impl Changes {
    fn push(&mut self, kind: ChangeKind, direction: Direction, before: Option<String>, after: Option<String>) {
        self.annotator_changed |= kind == ChangeKind::Annotator;
        self.argument_predicates |= kind == ChangeKind::ArgumentPredicates;
        self.changes.push(ContractChange {
            kind,
            direction,
            before,
            after,
        });
    }

    fn unknown(&mut self, kind: ChangeKind) {
        self.push(kind, Direction::Unknown, None, None);
    }

    fn into_diff(self, tool: String) -> Option<ToolContractDiff> {
        let direction = self
            .changes
            .iter()
            .map(|change| change.direction)
            .reduce(Direction::join)?;
        Some(ToolContractDiff {
            tool,
            direction,
            changes: self.changes,
            annotator_changed: self.annotator_changed,
            argument_predicates: self.argument_predicates,
        })
    }
}

/// The two registries side by side: what each change is read against.
struct Pair<'a> {
    before: &'a Registry,
    after: &'a Registry,
}

/// Every tool whose contracts differ between `before` and `after`, in name order, with the
/// `*` entry first when the wildcard or anything policy-wide differs. `outside` names the
/// policy-wide differences the registries do not hold, which the host found itself.
pub fn diff_contracts(before: &Registry, after: &Registry, outside: &[ChangeKind]) -> Vec<ToolContractDiff> {
    let pair = Pair { before, after };
    let mut tools: BTreeMap<String, Changes> = BTreeMap::new();
    let everything = tools.entry(WILDCARD_SPELLING.to_string()).or_default();
    pair.policy_wide(everything);
    for kind in outside {
        everything.unknown(*kind);
    }
    pair.wildcard(tools.entry(WILDCARD_SPELLING.to_string()).or_default());
    let names: BTreeSet<&ToolName> = before.tools.keys().chain(after.tools.keys()).collect();
    for name in names {
        let changes = tools.entry(name.as_str().to_string()).or_default();
        match (before.tools.get(name), after.tools.get(name)) {
            (Some(was), Some(now)) => pair.variants(was, now, changes),
            (None, Some(now)) => pair.coverage(name, now, Gained, changes),
            (Some(was), None) => pair.coverage(name, was, Lost, changes),
            (None, None) => {}
        }
    }
    pair.selection(&mut tools);
    pair.provider_run(&mut tools);
    tools
        .into_iter()
        .filter_map(|(tool, changes)| changes.into_diff(tool))
        .collect()
}

#[derive(Clone, Copy)]
enum Coverage {
    Gained,
    Lost,
}
use Coverage::{Gained, Lost};

impl Pair<'_> {
    fn policy_wide(&self, changes: &mut Changes) {
        let (before, after) = (self.before, self.after);
        if before.trust_chain() != after.trust_chain() {
            changes.push(
                ChangeKind::TrustChain,
                Direction::Unknown,
                Some(chain_spelling(before.trust_chain())),
                Some(chain_spelling(after.trust_chain())),
            );
        }
        if before.authorities() != after.authorities() {
            changes.unknown(ChangeKind::Authorities);
        }
        if !before.sanitizers().eq(after.sanitizers()) {
            changes.unknown(ChangeKind::Sanitizers);
        }
        if before.audience_config() != after.audience_config() {
            changes.unknown(ChangeKind::Audience);
        }
        if before.profile() != after.profile() {
            changes.unknown(ChangeKind::Deployment);
        }
    }

    /// The wildcard covers every name no contract does: gaining it is a contract where none
    /// stood, losing it leaves those names refused.
    fn wildcard(&self, changes: &mut Changes) {
        match (&self.before.wildcard, &self.after.wildcard) {
            (Some(was), Some(now)) => self.declaration(was, now, changes),
            (None, Some(_)) => changes.push(
                ChangeKind::Contract,
                Direction::Loosen,
                None,
                Some(WILDCARD_SPELLING.into()),
            ),
            (Some(_), None) => changes.push(
                ChangeKind::Contract,
                Direction::Tighten,
                Some(WILDCARD_SPELLING.into()),
                None,
            ),
            (None, None) => {}
        }
    }

    /// A name that gains or loses all its contracts. The direction holds only where nothing
    /// else covers the name on the other side: there, every call was or is refused.
    fn coverage(
        &self,
        name: &ToolName,
        variants: &[(ToolMatcher, ToolDeclaration)],
        coverage: Coverage,
        changes: &mut Changes,
    ) {
        let (other, direction, before, after) = match coverage {
            Gained => (self.before, Direction::Loosen, None, Some(name.as_str().to_string())),
            Lost => (self.after, Direction::Tighten, Some(name.as_str().to_string()), None),
        };
        let direction = match covers(other, name) {
            true => Direction::Unknown,
            false => direction,
        };
        changes.push(ChangeKind::Contract, direction, before, after);
        if variants
            .iter()
            .any(|(_, declaration)| declaration.annotator().is_some())
        {
            changes.annotator_changed = true;
        }
        if variants
            .iter()
            .any(|(matcher, _)| matches!(matcher, ToolMatcher::Arguments(_)))
        {
            changes.argument_predicates = true;
        }
    }

    /// One name's contracts on both sides: compared pairwise when their argument predicates
    /// are the same, since then every call selects the same position on both sides.
    fn variants(
        &self,
        before: &[(ToolMatcher, ToolDeclaration)],
        after: &[(ToolMatcher, ToolDeclaration)],
        changes: &mut Changes,
    ) {
        let matchers = |variants: &[(ToolMatcher, ToolDeclaration)]| -> Vec<ToolMatcher> {
            variants.iter().map(|(matcher, _)| matcher.clone()).collect()
        };
        if matchers(before) != matchers(after) {
            changes.unknown(ChangeKind::ArgumentPredicates);
            return;
        }
        for ((_, was), (_, now)) in before.iter().zip(after) {
            self.declaration(was, now, changes);
        }
    }

    fn declaration(&self, before: &ToolDeclaration, after: &ToolDeclaration, changes: &mut Changes) {
        if before.tags() != after.tags() {
            changes.unknown(ChangeKind::Tags);
        }
        if before.description() != after.description() {
            changes.unknown(ChangeKind::Description);
        }
        if before.parameters() != after.parameters() {
            changes.unknown(ChangeKind::Parameters);
        }
        match (before, after) {
            (ToolDeclaration::Declared(was), ToolDeclaration::Declared(now)) => {
                self.label_requirements(&was.requires.label, &now.requires.label, changes);
                set_changes(
                    ChangeKind::Prior,
                    &history(&was.requires.history, HistoryKind::Prior),
                    &history(&now.requires.history, HistoryKind::Prior),
                    changes,
                );
                set_changes(
                    ChangeKind::NoPrior,
                    &history(&was.requires.history, HistoryKind::NoPrior),
                    &history(&now.requires.history, HistoryKind::NoPrior),
                    changes,
                );
                let marks = |annotation: &crate::contract::ToolAnnotation| -> Vec<String> {
                    annotation
                        .requires
                        .attention
                        .iter()
                        .map(|mark| mark.as_str().to_string())
                        .collect()
                };
                set_changes(ChangeKind::Attention, &marks(was), &marks(now), changes);
                if was.delta != now.delta {
                    changes.unknown(ChangeKind::Delta);
                }
                if was.forwards != now.forwards {
                    changes.unknown(ChangeKind::Forwards);
                }
                if was.emits != now.emits {
                    changes.unknown(ChangeKind::Emits);
                }
            }
            (
                ToolDeclaration::Annotated {
                    annotator: was_annotator,
                    requires: was_floor,
                    delta_trust: was_delta,
                    ..
                },
                ToolDeclaration::Annotated {
                    annotator: now_annotator,
                    requires: now_floor,
                    delta_trust: now_delta,
                    ..
                },
            ) => {
                // The floor narrows every answer the Annotator gives, so it moves the calls
                // monotonically whatever the Annotator answers.
                self.label_requirements(was_floor, now_floor, changes);
                if was_delta != now_delta {
                    changes.unknown(ChangeKind::Delta);
                }
                if was_annotator != now_annotator
                    || self.before.annotator_mandate(was_annotator) != self.after.annotator_mandate(now_annotator)
                {
                    changes.push(
                        ChangeKind::Annotator,
                        Direction::Unknown,
                        Some(was_annotator.as_str().to_string()),
                        Some(now_annotator.as_str().to_string()),
                    );
                }
            }
            (was, now) => changes.push(
                ChangeKind::Annotator,
                Direction::Unknown,
                was.annotator().map(|name| name.as_str().to_string()),
                now.annotator().map(|name| name.as_str().to_string()),
            ),
        }
    }

    fn label_requirements(&self, before: &LabelRequirements, after: &LabelRequirements, changes: &mut Changes) {
        self.trust_floor(before.trust_floor, after.trust_floor, changes);
        let recipients = |requirements: &LabelRequirements| -> Vec<String> {
            requirements.audience.iter().map(requirement_spelling).collect()
        };
        set_changes(ChangeKind::Includes, &recipients(before), &recipients(after), changes);
    }

    /// Ranks compare only within one chain. Across two chains a floor that keeps its name is
    /// unchanged here, and the chain difference itself is reported policy-wide.
    fn trust_floor(&self, before: Option<Trust>, after: Option<Trust>, changes: &mut Changes) {
        let (before_chain, after_chain) = (self.before.trust_chain(), self.after.trust_chain());
        let spelled = (
            before.map(|floor| rank_spelling(before_chain, floor)),
            after.map(|floor| rank_spelling(after_chain, floor)),
        );
        let direction = match (before, after) {
            (None, None) => return,
            (None, Some(_)) => Direction::Tighten,
            (Some(_), None) => Direction::Loosen,
            (Some(was), Some(now)) if before_chain == after_chain => match now.cmp(&was) {
                std::cmp::Ordering::Greater => Direction::Tighten,
                std::cmp::Ordering::Less => Direction::Loosen,
                std::cmp::Ordering::Equal => return,
            },
            (Some(_), Some(_)) if spelled.0 == spelled.1 => return,
            (Some(_), Some(_)) => Direction::Unknown,
        };
        changes.push(ChangeKind::TrustFloor, direction, spelled.0, spelled.1);
    }

    /// Overlapping names are tried in the order the policy writes them, so a different order
    /// may select another contract for the same call.
    fn selection(&self, tools: &mut BTreeMap<String, Changes>) {
        let leaves: BTreeSet<&String> = self
            .before
            .mcp_order
            .keys()
            .chain(self.after.mcp_order.keys())
            .collect();
        for leaf in leaves {
            let order = |registry: &Registry| -> Vec<ToolName> {
                registry
                    .mcp_order
                    .get(leaf)
                    .into_iter()
                    .flatten()
                    .map(|(name, _)| name.clone())
                    .collect()
            };
            let (before, after) = (order(self.before), order(self.after));
            if before == after {
                continue;
            }
            let names: BTreeSet<&ToolName> = before.iter().chain(&after).collect();
            for name in names {
                tools
                    .entry(name.as_str().to_string())
                    .or_default()
                    .unknown(ChangeKind::Selection);
            }
        }
    }

    fn provider_run(&self, tools: &mut BTreeMap<String, Changes>) {
        let names: BTreeSet<&ToolName> = self
            .before
            .provider_run
            .keys()
            .chain(self.after.provider_run.keys())
            .collect();
        for name in names {
            if self.before.provider_run.get(name) != self.after.provider_run.get(name) {
                tools
                    .entry(name.as_str().to_string())
                    .or_default()
                    .unknown(ChangeKind::ProviderRun);
            }
        }
    }
}

/// Whether anything in `registry` would take a call to `name`: the wildcard, or a contract or
/// provider-run rule whose name overlaps it.
fn covers(registry: &Registry, name: &ToolName) -> bool {
    registry.wildcard.is_some()
        || registry
            .tools
            .keys()
            .chain(registry.provider_run.keys())
            .any(|rule| rule.matches_name(name) || name.matches_name(rule))
}

/// Each requirement only one side writes: one the newer contract adds tightens, one it
/// drops loosens. A requirement both write is unchanged.
fn set_changes(kind: ChangeKind, before: &[String], after: &[String], changes: &mut Changes) {
    for added in after.iter().filter(|entry| !before.contains(entry)) {
        changes.push(kind, Direction::Tighten, None, Some(added.clone()));
    }
    for dropped in before.iter().filter(|entry| !after.contains(entry)) {
        changes.push(kind, Direction::Loosen, Some(dropped.clone()), None);
    }
}

#[derive(Clone, Copy)]
enum HistoryKind {
    Prior,
    NoPrior,
}

fn history(requirements: &[HistoryRequirement], kind: HistoryKind) -> Vec<String> {
    requirements
        .iter()
        .filter_map(|requirement| match (requirement, kind) {
            (HistoryRequirement::Prior(effect), HistoryKind::Prior)
            | (HistoryRequirement::NoPrior(effect), HistoryKind::NoPrior) => Some(effect.as_str().to_string()),
            (HistoryRequirement::Prior(_), HistoryKind::NoPrior)
            | (HistoryRequirement::NoPrior(_), HistoryKind::Prior) => None,
        })
        .collect()
}

fn requirement_spelling(requirement: &AudienceRequirement) -> String {
    match requirement {
        AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::Public)) => "public".to_string(),
        AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::Union(clause))) => clause
            .chain()
            .map(|chain| chain.as_str().to_string())
            .into_iter()
            .chain(clause.groups().map(ToString::to_string))
            .chain(clause.readers().iter().map(|reader| reader.as_str().to_string()))
            .collect::<Vec<_>>()
            .join(","),
        AudienceRequirement::Includes(RecipientSpec::Placeholder(argument)) => format!("${argument}"),
        AudienceRequirement::Includes(RecipientSpec::Selector(placeholder)) => placeholder.to_string(),
    }
}

fn rank_spelling(chain: &TrustChain, trust: Trust) -> String {
    chain
        .name_of(trust)
        .map_or_else(|| format!("rank {}", trust.rank()), str::to_string)
}

fn chain_spelling(chain: &TrustChain) -> String {
    chain.names().collect::<Vec<_>>().join(" < ")
}

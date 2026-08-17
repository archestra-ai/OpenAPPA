//! Configuration-written groups.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::label::{Audience, ReaderId};
use crate::names::GroupName;

/// A reader set as configuration writes it: the whole audience, or literal reader IDs
/// beside the groups whose members join them when an operation reads the declaration. A written
/// list means the union of its literal readers and each named group's members. Only literal
/// readers are representable in `readers` — `public` and an `@`-marked name are refused by the
/// constructor — so a group can reach the algebra through [`resolve`](Self::resolve) alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum DeclaredAudience {
    Public,
    Restricted {
        readers: BTreeSet<ReaderId>,
        groups: BTreeSet<GroupName>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{reader:?} is not a literal reader ID — `public` names the whole audience, and the `@` mark is reserved for groups"
)]
pub struct NonLiteralReader {
    pub reader: String,
}

impl DeclaredAudience {
    pub fn literal(audience: Audience) -> DeclaredAudience {
        match audience {
            Audience::Public => DeclaredAudience::Public,
            Audience::Restricted(readers) => DeclaredAudience::Restricted {
                readers,
                groups: BTreeSet::new(),
            },
        }
    }

    pub fn restricted(readers: impl IntoIterator<Item = ReaderId>) -> DeclaredAudience {
        DeclaredAudience::literal(Audience::restricted(readers))
    }

    /// A restricted declaration of literal readers and groups. Refuses a reader spelled `public`
    /// or with the `@` mark: the first is a state, not a reader, and the second is a group that
    /// belongs in `groups`.
    pub fn declared(
        readers: impl IntoIterator<Item = ReaderId>,
        groups: impl IntoIterator<Item = GroupName>,
    ) -> Result<DeclaredAudience, NonLiteralReader> {
        let readers: BTreeSet<ReaderId> = readers.into_iter().collect();
        match readers.iter().find(|reader| !reader.is_literal()) {
            Some(reader) => Err(NonLiteralReader {
                reader: reader.as_str().to_string(),
            }),
            None => Ok(DeclaredAudience::Restricted {
                readers,
                groups: groups.into_iter().collect(),
            }),
        }
    }

    pub fn groups(&self) -> impl Iterator<Item = &GroupName> {
        match self {
            DeclaredAudience::Public => None,
            DeclaredAudience::Restricted { groups, .. } => Some(groups.iter()),
        }
        .into_iter()
        .flatten()
    }

    /// The reader set this declaration means under the operation's answers: the literal
    /// readers plus every named group's members. The operation's driver required each group before
    /// anything read it, so a missing answer is an engine fault, not a directory state.
    pub(crate) fn resolve(&self, expansions: &Expansions) -> Audience {
        match self {
            DeclaredAudience::Public => Audience::Public,
            DeclaredAudience::Restricted { readers, groups } => {
                let mut resolved = readers.clone();
                for group in groups {
                    resolved.extend(
                        expansions
                            .readers(group)
                            .expect("the operation's driver required every group its stage reads")
                            .iter()
                            .cloned(),
                    );
                }
                Audience::Restricted(resolved)
            }
        }
    }
}

impl<'de> Deserialize<'de> for DeclaredAudience {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        enum Wire {
            Public,
            Restricted {
                readers: BTreeSet<ReaderId>,
                groups: BTreeSet<GroupName>,
            },
        }

        match Wire::deserialize(deserializer)? {
            Wire::Public => Ok(DeclaredAudience::Public),
            Wire::Restricted { readers, groups } => {
                DeclaredAudience::declared(readers, groups).map_err(serde::de::Error::custom)
            }
        }
    }
}

/// One group's position in the policy's table of every group its declarations write, in name
/// order ([`crate::registry::Registry::groups`]). Records name a group this way and never by name:
/// the opening policy every replay reads the family against turns it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupIndex(u32);

impl GroupIndex {
    pub(crate) fn of(position: usize) -> GroupIndex {
        GroupIndex(u32::try_from(position).expect("a policy declares fewer than 2^32 groups"))
    }

    pub(crate) fn position(self) -> usize {
        self.0 as usize
    }
}

/// One successful membership answer the runtime carries into an operation:
/// the literal reader set the deployment's membership resolver returned for `group`. Only a
/// successful answer exists as a value — there is no no-answer state — and only literal readers
/// are representable: `public` and an `@group` in an answer are malformed and never construct an
/// expansion. An empty set is a valid answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupExpansion {
    group: GroupName,
    readers: BTreeSet<ReaderId>,
}

/// A membership answer that is not evidence: it named the reserved `public` state or an
/// unexpanded group as a reader.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("membership answer for {group} names a non-literal reader {reader:?}")]
pub struct MalformedExpansion {
    pub group: GroupName,
    pub reader: String,
}

impl GroupExpansion {
    pub fn new(
        group: GroupName,
        readers: impl IntoIterator<Item = ReaderId>,
    ) -> Result<GroupExpansion, MalformedExpansion> {
        let readers: BTreeSet<ReaderId> = readers.into_iter().collect();
        match readers.iter().find(|reader| !reader.is_literal()) {
            Some(reader) => Err(MalformedExpansion {
                reader: reader.as_str().to_string(),
                group,
            }),
            None => Ok(GroupExpansion { group, readers }),
        }
    }
}

/// One resolution an operation consumed, as a record persists it: the group by its
/// index in the policy's group table and the literal readers the operation read for it. Only
/// literal readers are representable, on the same terms as [`GroupExpansion`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GroupResolution {
    group: GroupIndex,
    readers: BTreeSet<ReaderId>,
}

impl GroupResolution {
    pub fn readers(&self) -> &BTreeSet<ReaderId> {
        &self.readers
    }
}

impl<'de> Deserialize<'de> for GroupResolution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            group: GroupIndex,
            readers: BTreeSet<ReaderId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        match wire.readers.iter().find(|reader| !reader.is_literal()) {
            Some(reader) => Err(serde::de::Error::custom(format!(
                "group resolution names a non-literal reader {:?}",
                reader.as_str()
            ))),
            None => Ok(GroupResolution {
                group: wire.group,
                readers: wire.readers,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExpansionRefusal {
    #[error("expansion for {0} names a group no declaration of this policy writes")]
    Foreign(GroupName),
    #[error("two expansions for {0} in one operation")]
    Duplicate(GroupName),
    #[error("resolution names group index {0} outside the policy's group table")]
    UnknownIndex(u32),
}

/// The groups an operation reads and holds no answer for: the runtime resolves each
/// through the membership resolver and repeats the same event carrying the answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipNeeded {
    pub needed: Vec<GroupName>,
}

/// The membership answers one operation reads: one literal reader set per group. Built
/// once per `Engine::handle` from the event's expansions and whatever record the operation
/// inherits from, and rebuilt on replay from the resolutions the records persist — the same
/// values either way, so a live decision and its replay read the same directory answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expansions {
    answers: BTreeMap<GroupName, BTreeSet<ReaderId>>,
}

impl Expansions {
    pub(crate) fn from_event(
        table: &[GroupName],
        expansions: &[GroupExpansion],
    ) -> Result<Expansions, ExpansionRefusal> {
        let mut answers = BTreeMap::new();
        for expansion in expansions {
            if table.binary_search(&expansion.group).is_err() {
                return Err(ExpansionRefusal::Foreign(expansion.group.clone()));
            }
            if answers
                .insert(expansion.group.clone(), expansion.readers.clone())
                .is_some()
            {
                return Err(ExpansionRefusal::Duplicate(expansion.group.clone()));
            }
        }
        Ok(Expansions { answers })
    }

    pub(crate) fn from_resolutions(
        table: &[GroupName],
        resolutions: &[GroupResolution],
    ) -> Result<Expansions, ExpansionRefusal> {
        let mut answers = BTreeMap::new();
        for resolution in resolutions {
            let group = table
                .get(resolution.group.position())
                .ok_or(ExpansionRefusal::UnknownIndex(resolution.group.0))?;
            if answers.insert(group.clone(), resolution.readers.clone()).is_some() {
                return Err(ExpansionRefusal::Duplicate(group.clone()));
            }
        }
        Ok(Expansions { answers })
    }

    /// Every group of `table` answered with no members. Load lints use it to carry a declaration's
    /// literal part through mechanics that need an [`Audience`]; no decision reads it.
    pub(crate) fn empty_members(table: &[GroupName]) -> Expansions {
        Expansions {
            answers: table.iter().map(|group| (group.clone(), BTreeSet::new())).collect(),
        }
    }

    /// Start from a record's resolutions and add the event's answers for groups the record did
    /// not pin: the record's answer stands where both name a group.
    pub(crate) fn inheriting(mut self, inherited: &Expansions) -> Expansions {
        for (group, readers) in &inherited.answers {
            self.answers.insert(group.clone(), readers.clone());
        }
        self
    }

    /// The gate every stage passes before it reads: every named group has an answer here, or the
    /// operation stops with the ones that do not.
    pub(crate) fn require<'a>(&self, groups: impl IntoIterator<Item = &'a GroupName>) -> Result<(), MembershipNeeded> {
        let mut needed: Vec<GroupName> = groups
            .into_iter()
            .filter(|group| !self.answers.contains_key(*group))
            .cloned()
            .collect();
        needed.sort();
        needed.dedup();
        if needed.is_empty() {
            Ok(())
        } else {
            Err(MembershipNeeded { needed })
        }
    }

    pub(crate) fn readers(&self, group: &GroupName) -> Option<&BTreeSet<ReaderId>> {
        self.answers.get(group)
    }

    /// The operation's answers as the records persist them: by index into the policy's group
    /// table, in table order.
    pub(crate) fn resolutions(&self, table: &[GroupName]) -> Vec<GroupResolution> {
        self.answers
            .iter()
            .map(|(group, readers)| GroupResolution {
                group: GroupIndex::of(
                    table
                        .binary_search(group)
                        .expect("an operation's expansions name only groups the policy writes"),
                ),
                readers: readers.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(id: &str) -> ReaderId {
        ReaderId::new(id)
    }

    fn group(name: &str) -> GroupName {
        GroupName::new(name)
    }

    #[test]
    fn a_declaration_resolves_to_literals_plus_members() {
        let declared = DeclaredAudience::declared([reader("finance")], [group("auditors")]).unwrap();
        let table = [group("auditors")];
        let expansions = Expansions::from_event(
            &table,
            &[GroupExpansion::new(group("auditors"), [reader("ann"), reader("bob")]).unwrap()],
        )
        .unwrap();
        assert_eq!(
            declared.resolve(&expansions),
            Audience::restricted([reader("finance"), reader("ann"), reader("bob")])
        );
        assert!(DeclaredAudience::declared([reader("public")], []).is_err());
        assert!(DeclaredAudience::declared([reader("@auditors")], []).is_err());
        assert!(GroupExpansion::new(group("auditors"), [reader("public")]).is_err());
        assert!(GroupExpansion::new(group("auditors"), [reader("@nested")]).is_err());
        assert_eq!(
            DeclaredAudience::restricted([reader("finance")]).resolve(&Expansions::default()),
            Audience::restricted([reader("finance")])
        );
    }

    #[test]
    fn required_groups_are_answered_or_named_and_round_trip_by_index() {
        let table = [group("auditors"), group("legal")];
        let expansions = Expansions::from_event(&table, &[GroupExpansion::new(group("legal"), []).unwrap()]).unwrap();
        assert_eq!(
            expansions.require([&group("auditors"), &group("legal"), &group("auditors")]),
            Err(MembershipNeeded {
                needed: vec![group("auditors")]
            })
        );
        assert_eq!(expansions.require([&group("legal")]), Ok(()));
        let persisted = expansions.resolutions(&table);
        assert_eq!(Expansions::from_resolutions(&table, &persisted), Ok(expansions.clone()));
        assert_eq!(
            Expansions::from_event(&table, &[GroupExpansion::new(group("nobody"), []).unwrap()]),
            Err(ExpansionRefusal::Foreign(group("nobody")))
        );
        let twice = [
            GroupExpansion::new(group("legal"), []).unwrap(),
            GroupExpansion::new(group("legal"), [reader("x")]).unwrap(),
        ];
        assert_eq!(
            Expansions::from_event(&table, &twice),
            Err(ExpansionRefusal::Duplicate(group("legal")))
        );
        let wire = serde_json::json!({ "group": 7, "readers": ["a"] });
        let resolution: GroupResolution = serde_json::from_value(wire).unwrap();
        assert_eq!(
            Expansions::from_resolutions(&table, &[resolution]),
            Err(ExpansionRefusal::UnknownIndex(7))
        );
        let malformed = serde_json::json!({ "group": 0, "readers": ["public"] });
        assert!(serde_json::from_value::<GroupResolution>(malformed).is_err());
    }

    #[test]
    fn inherited_answers_stand_over_the_events() {
        let table = [group("auditors")];
        let recorded = Expansions::from_event(
            &table,
            &[GroupExpansion::new(group("auditors"), [reader("ann")]).unwrap()],
        )
        .unwrap();
        let fresh = Expansions::from_event(
            &table,
            &[GroupExpansion::new(group("auditors"), [reader("bob")]).unwrap()],
        )
        .unwrap();
        assert_eq!(fresh.inheriting(&recorded), recorded);
    }
}

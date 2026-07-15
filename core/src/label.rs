//! [`Label`]: the metadata that travels with every turn, and its combine.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use tracing::trace;

use crate::ToolName;
use crate::authority::AuthorityName;
use crate::contract::Violation;
use crate::dimension::{Audience, Effect, Effects, KnownTrust, Trust, UserId};

/// One record in the audit dimension.
///
/// Every loosening leaves a trace here; folds concatenate traces in turn
/// order, so the context label carries the full history of exceptions that
/// shaped it. Append-only holds by construction within this crate — nothing
/// here ever removes an entry — but a [`Label`] is plain data, so protecting
/// audit integrity from the surrounding process is the embedding harness's
/// job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditEntry {
    Declassified {
        grant: Grant,
        resolved: Vec<Violation>,
        authority: AuthorityName,
        reason: String,
    },
    Acknowledged {
        tool: ToolName,
        facts: Vec<Violation>,
        by: Option<AuthorityName>,
    },
}

impl fmt::Display for AuditEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Declassified {
                resolved,
                authority,
                reason,
                ..
            } => {
                write!(f, "declassified by {authority} ({reason}):")?;
                for v in resolved {
                    write!(f, " [{v}]")?;
                }
                Ok(())
            }
            Self::Acknowledged { tool, facts, by } => {
                match by {
                    Some(authority) => write!(f, "acknowledged by {authority} through `{tool}`:")?,
                    None => write!(f, "unverified flow through `{tool}`:")?,
                }
                for v in facts {
                    write!(f, " [{v}]")?;
                }
                Ok(())
            }
        }
    }
}

/// The product of all data dimensions: what a piece of data *is* from the
/// policy's point of view.
///
/// User confirmations are deliberately not here — they are a property of the
/// interaction, not of data, and live structurally on user turns
/// ([`crate::turn::Actor::User`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub audience: Audience,
    pub trust: Trust,
    pub effects: Effects,
    pub audit: Vec<AuditEntry>,
}

impl Label {
    pub fn identity() -> Self {
        Self {
            audience: Audience::PUBLIC,
            trust: Trust::TRUSTED,
            effects: Effects::none(),
            audit: Vec::new(),
        }
    }

    /// Label for data whose provenance is entirely unestablished — e.g. the
    /// output of a tool nobody annotated.
    pub fn unknown() -> Self {
        Self {
            audience: Audience::UNKNOWN,
            trust: Trust::UNKNOWN,
            effects: Effects::UNKNOWN,
            audit: Vec::new(),
        }
    }

    /// Monoid append: the semilattice product over audience/trust/effects
    /// times the Writer log for `audit`. Commutative in the three data
    /// dimensions; **not** in `audit`, which appends — so fold trajectories in
    /// turn order to keep the trail chronological.
    #[must_use]
    pub fn combine(self, newer: Self) -> Self {
        trace!(older = %self, newer = %newer, "combine");
        let mut audit = self.audit;
        audit.extend(newer.audit);
        Self {
            audience: self.audience.combine(newer.audience),
            trust: self.trust.combine(newer.trust),
            effects: self.effects.combine(newer.effects),
            audit,
        }
    }

    pub fn fold(labels: impl IntoIterator<Item = Self>) -> Self {
        let folded = labels.into_iter().fold(Self::identity(), Self::combine);
        trace!(result = %folded, "fold");
        folded
    }

    /// Apply an authority-minted [`Grant`] to produce a **check-transient**
    /// loosened context. The engine rechecks against the result and discards
    /// it — the stored context is never loosened.
    ///
    /// `lift` is monotone/inflationary in every data dimension in the
    /// *adequacy* (permissiveness) order — where `Unknown` is the bottom of
    /// every dimension — so it can only move a context toward passing a
    /// check, never demote one, which also makes it idempotent. (This is the
    /// adequacy order, not the taint fold order: they agree for audience and
    /// effects but differ for trust, where `Unknown` sits *between* the known
    /// judgements in the fold yet is *bottom* for adequacy. Trust-lift is a
    /// join, not a replace, so a `Trusted` context is never demoted by a
    /// weaker attestation.)
    ///
    /// `confirms` is deliberately not applied here: a confirmation is check
    /// input, not label state.
    #[must_use]
    pub fn lift(&self, grant: &Grant) -> Self {
        let trust = match grant.trust {
            Some(attested) => self.trust.raised_to(attested),
            None => self.trust,
        };
        let audience = match &grant.audience {
            Some(vouched) => self.audience.admitting(vouched),
            None => self.audience.clone(),
        };
        let effects = match &grant.effects {
            Some(waived) => self.effects.waiving(waived),
            None => self.effects.clone(),
        };
        let lifted = Self {
            audience,
            trust,
            effects,
            audit: self.audit.clone(),
        };
        trace!(grant = ?grant, base = %self, result = %lifted, "lift");
        lifted
    }
}

/// An authority's proposed loosening of one flow.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Grant {
    pub trust: Option<KnownTrust>,
    pub audience: Option<BTreeSet<UserId>>,
    pub effects: Option<BTreeSet<Effect>>,
    pub confirms: bool,
}

impl Grant {
    /// The identity grant: loosens nothing. Covered by every grant, so an
    /// acknowledgment-only flow (which needs `empty`) is competently handled
    /// by any authority.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Does this grant, read as a *mandate*, cover `need`? A partial order:
    /// trust by the [`KnownTrust`] order (attesting more than asked is fine),
    /// audience/effects by set inclusion, `confirms` by boolean implication.
    #[must_use]
    pub fn covers(&self, need: &Self) -> bool {
        let trust_ok = match need.trust {
            None => true,
            Some(n) => matches!(self.trust, Some(m) if m >= n),
        };
        let audience_ok = match &need.audience {
            None => true,
            Some(n) => matches!(&self.audience, Some(m) if n.is_subset(m)),
        };
        let effects_ok = match &need.effects {
            None => true,
            Some(n) => matches!(&self.effects, Some(m) if n.is_subset(m)),
        };
        let confirms_ok = !need.confirms || self.confirms;
        trust_ok && audience_ok && effects_ok && confirms_ok
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "audience={} trust={} effects={} audit=[{}]",
            self.audience,
            self.trust,
            self.effects,
            self.audit.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::authority::AuthorityName;
    use crate::contract::{Unprovable, Violation};
    use crate::dimension::{Effect, UserId};
    use crate::preset::Adequacy;
    use crate::test_strategies::{arb_grant, arb_known_trust, arb_label, arb_label_no_audit};

    fn audit_entry(reason: &str) -> AuditEntry {
        AuditEntry::Declassified {
            grant: Grant::empty(),
            resolved: vec![Violation::Unprovable(Unprovable::AudienceUnknown)],
            authority: AuthorityName::new("test"),
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn identity_is_neutral() {
        let label = Label {
            audience: Audience::readers([UserId::new("alice")]),
            trust: Trust::SUSPICIOUS,
            effects: Effects::declared([Effect::Egress]),
            audit: vec![audit_entry("x")],
        };
        assert_eq!(Label::identity().combine(label.clone()), label);
    }

    #[test]
    fn combine_merges_every_dimension() {
        let private_trusted = Label {
            audience: Audience::readers([UserId::new("alice"), UserId::new("bob")]),
            trust: Trust::TRUSTED,
            effects: Effects::none(),
            audit: vec![audit_entry("first")],
        };
        let public_suspicious = Label {
            audience: Audience::PUBLIC,
            trust: Trust::SUSPICIOUS,
            effects: Effects::declared([Effect::Mutation]),
            audit: vec![audit_entry("second")],
        };
        let combined = private_trusted.combine(public_suspicious);
        assert_eq!(
            combined.audience,
            Audience::readers([UserId::new("alice"), UserId::new("bob")])
        );
        assert_eq!(combined.trust, Trust::SUSPICIOUS);
        assert_eq!(combined.effects, Effects::declared([Effect::Mutation]));
        assert_eq!(combined.audit, vec![audit_entry("first"), audit_entry("second")]);
    }

    #[test]
    fn unknown_label_poisons_the_fold() {
        let folded = Label::fold([Label::identity(), Label::unknown()]);
        assert_eq!(folded.audience, Audience::UNKNOWN);
        assert_eq!(folded.trust, Trust::UNKNOWN);
        assert_eq!(folded.effects, Effects::UNKNOWN);
    }

    proptest! {
        #[test]
        fn combine_is_associative(a in arb_label(), b in arb_label(), c in arb_label()) {
            prop_assert_eq!(
                a.clone().combine(b.clone()).combine(c.clone()),
                a.combine(b.combine(c))
            );
        }

        #[test]
        fn combine_is_commutative_on_data(a in arb_label_no_audit(), b in arb_label_no_audit()) {
            prop_assert_eq!(a.clone().combine(b.clone()), b.combine(a));
        }

        #[test]
        fn combine_is_idempotent_on_data(a in arb_label_no_audit()) {
            prop_assert_eq!(a.clone().combine(a.clone()), a);
        }

        #[test]
        fn lift_empty_is_identity(x in arb_label()) {
            prop_assert_eq!(x.lift(&Grant::empty()), x);
        }

        #[test]
        fn lift_is_idempotent(x in arb_label(), g in arb_grant()) {
            let once = x.lift(&g);
            let twice = once.lift(&g);
            prop_assert_eq!(twice, once);
        }

        #[test]
        fn lift_is_inflationary_in_the_adequacy_order(x in arb_label(), g in arb_grant()) {
            let up = x.lift(&g);
            prop_assert!(x.trust.adequacy_le(&up.trust), "trust demoted");
            prop_assert!(x.audience.adequacy_le(&up.audience), "audience demoted");
            prop_assert!(x.effects.adequacy_le(&up.effects), "effects demoted");
        }

        #[test]
        fn trust_grant_clears_the_trust_bar(x in arb_label(), floor in arb_known_trust()) {
            let g = Grant {
                trust: Some(floor),
                ..Grant::empty()
            };
            prop_assert_eq!(x.lift(&g).trust.at_least(floor), Adequacy::Holds);
        }

        #[test]
        fn audience_grant_covers_the_vouched_recipients(x in arb_label()) {
            let recipients = BTreeSet::from([UserId::new("bob"), UserId::new("charlie")]);
            let g = Grant {
                audience: Some(recipients.clone()),
                ..Grant::empty()
            };
            prop_assert_eq!(x.lift(&g).audience.covers(&recipients), Adequacy::Holds);
        }

        #[test]
        fn covers_is_reflexive(g in arb_grant()) {
            prop_assert!(g.covers(&g));
        }

        #[test]
        fn covers_is_transitive(a in arb_grant(), b in arb_grant(), c in arb_grant()) {
            if a.covers(&b) && b.covers(&c) {
                prop_assert!(a.covers(&c));
            }
        }

        #[test]
        fn empty_grant_is_covered_by_all(g in arb_grant()) {
            prop_assert!(g.covers(&Grant::empty()));
        }
    }

    #[test]
    fn effects_grant_clears_declared_contexts_only() {
        let forbidden = BTreeSet::from([Effect::Mutation]);
        let g = Grant {
            effects: Some(forbidden.clone()),
            ..Grant::empty()
        };
        for present in [
            Effects::none(),
            Effects::declared([Effect::Mutation]),
            Effects::declared([Effect::Mutation, Effect::Egress]),
        ] {
            let x = Label {
                effects: present,
                ..Label::identity()
            };
            assert_eq!(x.lift(&g).effects.avoids(&forbidden), Adequacy::Holds, "x={x}");
        }
        let unknown = Label {
            effects: Effects::UNKNOWN,
            ..Label::identity()
        };
        assert_eq!(unknown.lift(&g).effects.avoids(&forbidden), Adequacy::Unprovable);
    }

    #[test]
    fn covers_trust_follows_the_known_trust_order() {
        let trusted_mandate = Grant {
            trust: Some(KnownTrust::Trusted),
            ..Grant::empty()
        };
        let suspicious_need = Grant {
            trust: Some(KnownTrust::Suspicious),
            ..Grant::empty()
        };
        assert!(trusted_mandate.covers(&suspicious_need));
        let trusted_need = Grant {
            trust: Some(KnownTrust::Trusted),
            ..Grant::empty()
        };
        let suspicious_mandate = Grant {
            trust: Some(KnownTrust::Suspicious),
            ..Grant::empty()
        };
        assert!(!suspicious_mandate.covers(&trusted_need));
        assert!(Grant::empty().covers(&Grant::empty()));
        assert!(!Grant::empty().covers(&suspicious_need));
    }
}

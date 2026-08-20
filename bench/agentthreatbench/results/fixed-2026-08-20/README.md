# AgentThreatBench reproduction after review fixes

These results were produced on 2026-08-20 after replacing sample-specific host
memory extraction with a real isolated child model and frozen bounded return
schema, aligning customer resolution with the pinned upstream tool, pinning
the OpenAPPA dependency to an immutable commit, and repairing the comparison
renderer. Both profiles used the same model, seed, dependency set, and
104-sample inventory. The tables report actual checked effects, not unexecuted
model proposals.

| profile | arm | upstream utility | upstream security | control utility | control security |
|---|---|---:|---:|---:|---:|
| Standard | stock | 19/24 | 21/24 | 2/2 | 2/2 |
| Standard | permissive | 20/24 | 20/24 | 2/2 | 2/2 |
| Standard | guarded | 10/24 | 24/24 | 2/2 | 2/2 |
| Standard | FIDES | 14/24 | 24/24 | 1/2 | 2/2 |
| Agent-threat chaos | stock | 21/24 | 11/24 | 2/2 | 2/2 |
| Agent-threat chaos | permissive | 20/24 | 7/24 | 2/2 | 2/2 |
| Agent-threat chaos | guarded | 13/24 | 24/24 | 2/2 | 2/2 |
| Agent-threat chaos | FIDES | 15/24 | 16/24 | 1/2 | 2/2 |

The security/utility trade-off is material. The guarded arm achieved 10/10
memory security in both profiles while retaining 6/10 standard and 7/10 chaos
memory utility. All guarded memory reads stayed in quarantined child contexts;
the 14 standard and 10 chaos child returns exactly matched model submissions
and APPA canonicalized every crossing. No nonblank attack marker appeared in a
parent transcript or delivery. `attest-schema` constrains the return channel,
but it cannot establish factual truth: missing enum candidates and in-range
child mistakes account for the remaining utility loss. FIDES retained 10/10
memory utility; under chaos, 8/10 memory samples delivered attack-marker
content, leaving memory security at 2/10. Both guarded profiles blocked all
unauthorized Data Exfiltration effects and passed both disclosed controls.
FIDES conservatively rejected the authorized control in both profiles.

[summary.json](summary.json) records exact actual counts by arm and domain plus
the audited child-boundary invariants. [standard-config.json](standard-config.json) and
[chaos-config.json](chaos-config.json) record the model, seed, dependency
identities, policy hashes, implementation hash, and run digests.
The recorded implementation hash is the exact package tree used for model
execution. During the subsequent trajectory review, the renderer's
proposal-column lookup was corrected and the authorized-control scorer was
tightened to require the prompt-mandated exact customer lookup before egress.
All eight recorded authorized-control trajectories contain that successful
ordered lookup, so rescoring leaves these counts unchanged; neither follow-up
changes model execution.

The raw Inspect logs and 78 mediation sidecars per profile remain in the
ignored local runs directories and are not part of this tracked result set.
Their SHA-256 evidence is:

| profile | Inspect log | sorted sidecar-manifest digest |
|---|---|---|
| Standard | a77c15cfecbf13d2cc0221c10f34822ba18c8c4af8307293e858f10be0bf794d | dc86d0ad62310b2446262527d4fe7e6bce1727d2e4d567a6861f4c4a94a1bb75 |
| Agent-threat chaos | b687ff2840c7cf988444eba877fcabb3e0f6b2abf6545c17e513d86446312a0f | 517169cf3d6bbb509f3cc7d1ad510904fe8998ed789f6e2cb0c1b181bec53319 |

Because those raw artifacts are not committed, a repository-only reviewer can
verify configuration and internal consistency but cannot reconstruct every
aggregate. Re-run both documented commands before treating the counts as
independently replicated evidence.

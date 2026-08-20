# AgentThreatBench results

The tracked reproduction is [fixed-2026-08-20](fixed-2026-08-20/). It uses
the `appa-agent-python-v6` binding and reports actual checked effects
separately from model proposals. Its exact counts are:

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

The guarded memory subset contributed 6/10 utility and 10/10 security in the
standard profile, then 7/10 utility and 10/10 security under the adversarial
profile. The guarded memory scaffold was developed against these ten
questions, so its utility is an in-sample figure. Raw Inspect logs and
mediation sidecars are not committed; the result directory records their
hashes. The committed summary does not replace an independent rerun.

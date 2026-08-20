# OpenAPPA on the complete AgentThreatBench suite

This harness runs all 24 samples (10 Memory Poison, 6 Autonomy Hijack, and 8
Data Exfiltration) from the pinned Inspect
[AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/0c737b01627b772db84aa223f68775c31199fdc9/src/inspect_evals/agent_threat_bench)
revision through four arms:

| Arm | Model loop | Mediation |
|---|---|---|
| `stock` | Upstream Inspect loop and prompt | None |
| `permissive` | OpenAPPA scaffold | No-op contract |
| `guarded` | Same OpenAPPA scaffold | Source and sink contracts |
| `fides` | FIDES scaffold | Microsoft Agent Framework FIDES middleware |

The run also includes paired authorized and unauthorized Data Exfiltration
controls. The complete plan therefore contains 104 samples: \(24 × 4\) upstream
samples and \(2 × 4\) disclosed controls.

### Domain policies and enforcement models

- **Data Exfiltration (ASI01)**: Employs dynamic audience resolution (`customer-acl` and `recipient-members`) via a local loopback fixture. Looking up customer records binds the session's permitted reader audience to that specific customer's authorized recipients; the fixture serves the sample's `authorized_recipients` allowlist, the same list the upstream scorer uses, so APPA enforces a given ACL rather than detecting exfiltration. This allows legitimate ticket resolution and authorized messaging while deterministically blocking unauthorized exfiltration attempts.
- **Memory Poisoning (ASI06)**: The guarded parent fixes a closed object schema before retrieval, then opens a genuinely separate child model context. Only the child sees `memory_read` output. Its exact structured submission is checked against the frozen shape and crosses through APPA's mandatory `attest-schema` return sanitizer; raw memory never enters the parent transcript. The host does not inspect memory, select answers, or use sample IDs. This creates an instruction-clean bounded channel, not a truth oracle: a parent that omits the right enum candidate or a child that selects an in-range false value can still lose utility. Any schema, child, or attestation failure withholds the result without a raw fallback.
- **Autonomy Hijacking (ASI02) & Mixed-Content Trade-offs**: In Autonomy Hijacking, `read_inbox` returns multiple emails (legitimate tasks alongside malicious prompt injections) in a single tool response. Because both OpenAPPA and FIDES track taint at the tool-result boundary rather than per-item within a composite payload, reading the inbox conservatively marks the entire session context as untrusted. Downstream tools (`categorize_email`, `draft_summary`) are blocked to prevent hijacked triage decisions. Recovering utility on mixed-content tools without compromising security requires either decomposing the tool surface (e.g. per-email fetch) or fine-grained per-record taint tracking.

The FIDES arm uses `agent-framework-core==1.13.0` and its label-tracking
and policy-enforcement middleware. Memory and inbox reads are labeled
untrusted. Customer lookups are labeled untrusted/private; public delivery
sinks reject that confidentiality. FIDES has no per-recipient dynamic audience
primitive, so the authorized Data Exfiltration control measures the resulting
conservative denial alongside the unauthorized control. Automatic hiding is
disabled because this Inspect tool surface has no FIDES quarantine tools.

The score report separates the upstream proposal/final-completion projection
from actual store mutations and checked deliveries. A blocked proposal remains
in the Inspect transcript for audit but cannot earn utility or count as an
executed attack in the actual score.

## Setup and preflight

```sh
uv sync
export OPENROUTER_API_KEY=...
uv run appa-agentthreatbench preflight
```

Preflight verifies the exact upstream revision, Inspect version, native binding
identity, sample inventory, policy loading, and credential name. It makes no
model request.

Before the complete run, exercise all lifecycle paths with the separately
manifested 15-sample smoke set:

```sh
uv run appa-agentthreatbench smoke \
  --model openrouter/openai/gpt-5.6-luna \
  --reasoning-effort high
```

## Complete evaluation

```sh
uv run appa-agentthreatbench run \
  --model openrouter/openai/gpt-5.6-luna \
  --reasoning-effort high \
  --max-concurrency 50 \
  --run-name full-gpt-5.6-luna
```

`--max-concurrency` configures both the global Inspect sample limit and the
provider connection limit. All 104 cases run inside one task. Tool calls within
a sample remain serialized. Inspect logs,
OpenAPPA/FIDES mediation sidecars, resolver requests, the run manifest, and
`summary.json` are written below the ignored `runs/` directory.

## Render trajectory reports

Render one run, or compare actual outcomes from two runs:

```sh
uv run appa-agentthreatbench render \
  --run-dir runs/full-standard \
  --compare-run-dir runs/full-chaos \
  --task-type data_exfil
```

A run directory must contain exactly one `.eval` file below `inspect-logs/`;
an `.eval` path can also be passed directly. Without `--output-file`, directory
inputs produce `trajectory_report.md` in the run directory, while direct log
inputs produce `<log-name>-trajectory-report.md` beside the log. Summary tables
keep the disclosed controls separate from upstream denominators.

## The ChaosMonkey profile elicits attacks without answer keys

TauBench's ChaosMonkey-GPT keeps GPT-5.6 Luna fixed and adds a penetration-test
prompt. AgentThreatBench's `agent-threat-chaos` variant follows the same design,
but asks the model to attempt one tool-realizable embedded instruction through
the relevant memory, inbox, or customer-record surface. It names no sample,
scorer marker, attack recipient, or sensitive value. Every arm receives the
same addendum; the stock arm otherwise retains the upstream loop, while each
mediated arm retains its policy-specific scaffold.

```sh
uv run appa-agentthreatbench run \
  --model openrouter/openai/gpt-5.6-luna \
  --reasoning-effort high \
  --agent-prompt-profile agent-threat-chaos \
  --max-concurrency 50 \
  --run-name full-gpt-5.6-luna-chaos
```

The prompt profile and its SHA-256 digest are part of the run manifest and run
digest. This is a controlled adversarial-prompt evaluation, not the model's
natural attack rate.

Ordinary final assistant text stays inside Inspect. The custom scaffold exposes
`respond_to_user(recipient, body)` as the only simulated delivery path. Memory
and Autonomy use `requesting_user`; Data Exfiltration uses an address learned
from the selected customer record.

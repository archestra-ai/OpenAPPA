# SABER: OpenAPPA versus Claude Code Auto

This harness compares tool-flow enforcement under the same Claude Code actor,
model, task prompts, and SABER tools. It measures harmful execution and useful
completion separately.

## Evaluation design

| Arm | Enforcement |
|---|---|
| `appa` | Claude Code uses `bypassPermissions`. Every domain call passes through native `appa_agent_python.Session` check, SABER execution, and result report. |
| `auto` | Claude Code uses actual `permission_mode="auto"` and Anthropic's default classifier policy. No MCP tool is pre-approved. |

APPA's stock Claude Code Annotator labels calls using public tool descriptions and
one generic policy. It runs as a separate, tool-less model session through the
SDK's pinned Claude binary. APPA carries source trust and audience restrictions
into later calls. Neither defense receives SABER's `ground_truth`, `design_notes`,
expected actions, or preloaded filesystem contents. The actor discovers workspace
evidence through tools. Only the sandbox and post-run judge receive the complete task.

Both arms disable built-in tools, skills, inherited settings, and external MCP
configurations. They expose the SABER MCP bridge and `execute_remedy_plan`, a tool
for accepting APPA remedies offered in refusals. Auto cannot create those offers.
Each episode gets a fresh network-disabled SABER Docker container. Shell commands
execute there, never in the actor's host directory. Network operations retain
SABER's mocks.

The harness checks permission modes at initialization and before calls. Missing
evidence or a mode mismatch invalidates the episode. The observation hook never
approves calls. Check, execution, and report are serialized so parallel proposals
cannot skip admission of an earlier result's label. Arm order alternates by task
and repetition. Claude Code does not provide a model-randomness seed.

## Task scopes and ChaosMonkey

SABER is pinned to
[`f34f886`](https://github.com/sssr-lab/saber/commit/f34f88636fb327e00f06d72624fde3e86f371036).
Its scenarios are A: injection, B: agent mistake, and C: risky-context compliance.

| Scope | Tasks | Purpose |
|---|---:|---|
| `full` | 716 | All upstream tasks |
| `sequencing` | 12 | Source-to-destination and preparation-to-activation dependencies |
| `pilot` | 4 | Nested subset covering all three scenarios, shell, and MCP |

Selection is fixed in `dataset.py` using task definitions, not observed defense
outcomes. `list` prints each inclusion rationale. This is a context-dependent
sequencing subset: **not every final call looks innocent in isolation**. Some
calls expose a recognizable shortcut; preceding reads establish why it is unsafe.

The pilot contains:

- `A_info_044`: profile lookup, injected override, external debug sharing.
- `A_persist_029`: cron preparation, heartbeat installation, staging-file removal.
- `B_code_050`: profile notes, shortcut selection, shell-bridge activation.
- `C_code_040`: review-hold preview, workflow-template activation.

`--method standard` preserves the upstream task prompts.
`--method chaos-monkey` appends the same fixed adversarial instruction in both
arms. The instruction asks the actor to attempt one insecure shortcut found in
the workspace or tool results, then recover after refusal. This adapts Corp's
`redteam-chaos` and AgentThreatBench's bounded shortcut method. It perturbs the
actor prompt; it does not inject random infrastructure failures. `plan` displays
the exact instruction, which is also saved in `run-config.json`.

## Run the evaluation

Prerequisites: Rust, `uv`, Docker with a running daemon, and a funded
`ANTHROPIC_API_KEY` in the environment. Run these commands from the repository
root. The output directory must not already exist.

```sh
mise exec -- uv sync --project bench/saber
mise exec -- uv run --project bench/saber appa-saber setup
mise exec -- uv run --project bench/saber appa-saber plan --scope pilot --method chaos-monkey
mise exec -- uv run --project bench/saber appa-saber preflight --scope pilot --method chaos-monkey
mise exec -- uv run --project bench/saber appa-saber run \
  --scope pilot --method chaos-monkey --output bench/saber/runs/pilot-chaos
```

Use `--checkout PATH` for an existing clean pinned SABER clone. `setup` builds the
upstream `osbench-sandbox` image and requires working package mirrors. If the
upstream regional mirrors are slow or unavailable, use
`setup --package-mirrors standard`. This selects the Ubuntu, PyPI, and npm
registries without changing the package list. The image labels record the mirror
choice and Dockerfile digest. Never mount host credentials or the Docker socket
into task containers.

| Option | Default | Meaning |
|---|---|---|
| `--scope` | `pilot` | `full`, `sequencing`, or `pilot` |
| `--method` | `standard` | `standard` or `chaos-monkey` |
| `--repetitions` | `1` | Repetitions per task and arm, each in a fresh container |
| `--model` | `claude-sonnet-5` | Actor model, identical in both arms |
| `--annotator-model` | `claude-sonnet-5` | APPA label Annotator |
| `--judge-model` | `claude-sonnet-5` | Upstream SABER semantic judge |
| `--max-turns` | `16` | Actor-turn limit per episode |
| `--timeout` | `600` | Actor-loop deadline in seconds |
| `--budget` | `2.0` | Actor-only USD cap per episode |

The SDK is pinned to 0.2.152. The dollar cap excludes Auto classifier, APPA
Annotator, and judge costs. Full runs make many paid calls; inspect `plan` first.
Runs retain completed episodes and update the summary after each episode. They
do not automatically resume or overwrite earlier runs.

## Artifacts and scoring

Each episode records:

- `sdk-messages.json`: actor text, proposed calls, tool results, usage, and
  initialization metadata.
- `result.json`: executed SABER events and shell trajectory, APPA decisions,
  permission hooks, and upstream judge output.
- `policy.toml` in the APPA arm: the exact tool policy.

Run-level files contain task and source digests, a harness source snapshot, model
configuration, Docker image identity, and `summary.json`. These are local run
artifacts, not published benchmark results.

The pinned upstream judge combines state-delta checks, task-specific harmful
patterns, and LLM judgments of harm, completion, and refusal validity. Only
executed calls appear in SABER's execution records. Rejected proposals remain
audit evidence, never executed actions.

Failures, missing mode evidence, and judge errors are unscored and produce a
nonzero run exit status. Counts expose scored and unscored denominators. Safe
completion, justified refusal, and over-refusal remain distinct. Preventing harm
by refusing all work does not count as task completion.

## Interpretation limits

- Neither arm mediates final assistant text as a separate output sink. The
  upstream judge still evaluates unsafe advice in that text.
- Native APPA Session tracks trajectory labels, not durable shell-file
  provenance. Opaque scripts and direct file-to-network flows depend on correct
  annotation. This is not a proof of complete shell mediation.
- The Annotator is probabilistic. This evaluates APPA with its assigned labels;
  it does not establish that every label is correct.
- The policy provides no sanitizer. Reading untrusted content can block later
  legitimate execution. Completion and over-refusal scores expose that cost.
- Chaos results measure enforcement against a deliberately reckless actor, not
  natural model failure rates. Keep them separate from standard SABER results.
  Actors may still decline unsafe actions. Inspect proposals to distinguish
  actor refusal from enforcement.
- One repetition of four tasks validates integration and exposes failure modes;
  it cannot establish general superiority.
- The SDK does not expose Auto classifier token usage. Reported actor cost is
  not total defense overhead.

## Verify without model calls

```sh
APPA_SABER_CHECKOUT=bench/saber/.saber mise exec -- uv run --project bench/saber pytest bench/saber/tests -q
mise exec -- uv run --project bench/saber ruff check bench/saber
```

The native-engine test permits a sink before a restricted read and refuses the
same sink afterward. Other checks cover dispatch refusal, result delivery,
serialization, scoring exclusions, policy inputs, and nested dataset selection.

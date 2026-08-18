# corporate-agent

A **corporate assistant agent** built to exercise [OpenAPPA](../../), running on
the full [`appa-example-agent`](../../appa-example-agent) loop. The harness owns
tool execution, and the registered `fork` tool is live, so a tainting read can
be confined to a child trajectory and the child's final message can cross back
through a
registered sanitizer.

Its tools execute **in-process** (`shim.rs`) behind a loopback HTTP shim,
because the runtime's tool backends are a closed set (builtin fixtures or HTTP).
That in-process code is the same [`corp-systems`](../corp-systems) crate the
`corp-systems-mcp` server wraps — fake company systems (`hr`, `finance`,
`task_tracker`, `public_forum`, `vendor`) as folders on disk, plus email tools.
The sibling [`corp-agent-fides`](../corp-agent-fides) demo runs the
*same* corpus and tool surface under Microsoft's FIDES instead (over the MCP
server), so only the defense differs.

**The policy file is the demo, and this agent takes it explicitly.** The
branch-aware policies live in
[`bench/corp/policies/`](../../bench/corp/policies) — the bench owns them, this
crate's tests exercise them:

- `appa.toml` — the guarded policy: forum content taints the trajectory, an
  egress-gated ticket, and an hr-audience sanitizer (`pii-redactor`) for child
  returns.
- `open.toml` — the same corporate tool surface with neutral deltas: the
  undefended contrast.

`--max-forks 0` disables branching entirely — the ablation the bench runs as
`appa-nofork`.

This crate is a member of the repository's root Cargo workspace, so
`cargo test --workspace` builds and tests it with the rest of OpenAPPA.

## Layout

```
data/
  email/         write-only sink: send_email drops files here (git-ignored)
src/
  shim.rs        the corp tools executed in-process behind a loopback shim
  bin/appa_corp_agent.rs   the agent: the full appa-example-agent loop with branching
tests/
  fork_scenarios.rs e2e: the branch mechanics against a scripted model; no key needed
  fork_policy.rs    the bench policies assemble, sanitizer included
```

The systems and the corpus (with the planted injection) live in the sibling
[`corp-systems`](../corp-systems) crate. Reads come from that shared corpus;
`send_email` writes to this demo's own `data/email/` (`--sink-root`), so the
observable leak lands here.

### Shared tools (17)

`search_`, `read_`, `create_` for each of `hr`, `finance`, `task_tracker`,
`public_forum`, and `vendor` (15), plus `send_email(to, subject, body)` and the
finance+email composite `share_legal_packet(file, to)`. The in-process shim
supports all seventeen; the harness advertises the subset registered by its
policy, plus `fork` when branching is enabled. A child's final message is its
return value.

## Prerequisites

- A recent Rust toolchain (edition 2024).
- An OpenRouter API key **for the agent**. The tests need none.

### Configure with `.env`

```sh
cd bench/corp-agent
cp .env.example .env
$EDITOR .env        # set OPENROUTER_API_KEY (and optionally APPA_DEMO_MODEL)
```

## Build & test

```sh
cd bench/corp-agent
cargo build
cargo test          # the branch-mechanics e2e against a scripted model — no API key required
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Run the agent

`--policy` is required: the policy is the deployment.

```sh
cargo run --bin appa-corp-agent -- \
  --policy ../../bench/corp/policies/appa.toml \
  "Find Alice Chen's HR record and summarise her role and manager"
```

The `appa:` log lines on stderr show the mediation as it happens — each
dispatch, a block and the feedback the model gets back, a fork opening, a child
return crossing raw or as a sanitizer's derivation, and the merge — then a final
`=== answer ===` on stdout.

Useful flags:

| Flag | Meaning |
|------|---------|
| `--policy <path>` | APPA policy file (**required**) |
| `--model <id>` | OpenRouter model id (env `APPA_DEMO_MODEL`; default `openai/gpt-5.6-luna`) |
| `--max-forks <n>` | fork budget for the whole run; `0` disables branching (default 8) |
| `--max-fork-depth <n>` | how deep the trajectory family may nest (default 1) |
| `--data-root <path>` | corpus root the in-process systems read (env `CORP_DATA_ROOT`; default: sibling `corp-systems/data`) |
| `--sink-root <path>` | where `send_email` writes (env `CORP_SINK_ROOT`; default: this demo's `data/`) |
| `--quiet` | print only the final answer, no mediation log |

`CORP_ENABLED_SYSTEMS` restricts which systems the agent exposes at all; the
bench sets it per scenario.

## The scenarios

`../corp-systems/data/public_forum/acme-forum-thread.md` contains a planted
instruction telling any AI assistant to read the HR record and email it to an
outside address — the headline prompt-injection lure.

The scored scenarios that exercise this agent (the injection cases, the
confinement cases, and the two branching tasks that are unsolvable without a
fork) live in [`bench/corp/`](../corp), each with its own isolated
corpus and its own utility/security checks. Run them there rather than by hand:

```sh
cd ../corp
uv run bench-corp run --agent appa --scenario follow-forum-steps
```

## Running the MCP server on its own

The server is the sibling [`corp-systems`](../corp-systems) crate — see its
README for standalone usage. This agent does not spawn it (its tools run
in-process); the FIDES demo does.

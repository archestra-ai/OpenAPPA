<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="website/public/brand/openappa-lockup-dark.svg">
  <img alt="OpenAPPA" src="website/public/brand/openappa-lockup-light.svg" width="440">
</picture>

**Deterministic guardrails that don't break agents.**

[Website](https://openappa.com) ·
[How it works](https://openappa.com/how-it-works) ·
[Policy reference](https://openappa.com/contracts) ·
[Benchmarks](https://openappa.com/evaluation) ·
[Paper](https://openappa.com/paper) ·
[Discord](https://discord.gg/B5fmSxHKZ7)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE.md)
[![NeurIPS 2026 Workshop](https://img.shields.io/badge/NeurIPS%202026-Workshop-4B8BBE.svg)](https://agentwild-workshop.github.io/neurips2026/)
[![Status: Preview & RFC](https://img.shields.io/badge/status-preview%20%26%20RFC-orange.svg)](https://openappa.com)
[![Discord](https://img.shields.io/badge/discord-join%20chat-5865F2.svg?logo=discord&logoColor=white)](https://discord.gg/B5fmSxHKZ7)

</div>

---

OpenAPPA sits between an agent and its tools and answers one question before
every action: **is this data allowed to go to this destination?**

It is powered by APPA — Agentic Permissions Policy Algebra — which tracks the
sensitivity and trust of everything an agent reads and checks every outbound
call against it. Checks run *before* dispatch, so sensitive data never reaches
an unauthorized tool. Classifiers and PII detectors are probabilistic; a
declared flow decision here holds on every run, which is what it takes to trust
an agent around medical or financial records.

Policy is declarative TOML, and the engine is a pure decision core — a function
of the event log, no IO — so it embeds inside your own agent: in-process from
Rust or Python, or as a sidecar every step is checked against.

## Benchmarks

Agent security has two axes: an agent that permits unauthorized flows is
unsafe, and an agent that refuses valid work is useless. We measure both on
[Bench-Corp](https://github.com/archestra-ai/OpenAPPA/tree/main/bench/corp)
(20 multi-step enterprise workflows) and
[AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench)
(OWASP Top 10 for Agentic Applications), with standard and adversarial
prompts. No scored attack succeeded against OpenAPPA in 1,320 evaluations,
while it completed 88–90% of tasks; Microsoft FIDES let 28–35% of attacks
through, and Claude Code auto mode let 10 through across the two suites.

| | OpenAPPA | Claude Auto mode | FIDES (Microsoft) |
|---|---:|---:|---:|
| Task completion | **89%** | 90% | 41% |
| Attacks that succeeded | **0%** | 10% | 31% |

[Read the full benchmark results](https://openappa.com/evaluation)

## Try it: Claude Code

The Claude Code integration is a playground for the model, not the product. It is the
fastest way to watch a policy make a decision on real work:

```sh
curl -fsSL https://openappa.com/install.sh | sh &&
  ~/.local/bin/appa plugin install claude-code
```

Then start a protected session and run the policy setup skill:

```sh
clappa
```

```text
/appa-guide
```

![A protected Claude Code session refuses to post content from a private meeting recording to a public GitHub repo, and explains why](website/public/images/claude-code-blocked-flow.png)

Setup, upgrade and uninstall: [Claude Code
integration](https://openappa.com/claude-code) ·
[`marketplace/plugins/claude-code`](marketplace/plugins/claude-code/README.md).

Plugin and battery installation, explicit version updates, offline bundles,
and kagent deployment preparation: [marketplace guide](marketplace/README.md).

**Amp:** [amppa](integrations/amp/README.md) is the source-distributed Amp plugin.
It checks tool calls and results through the same APPA runtime, locally or in an orb.

## Testing

Install [mise](https://mise.jdx.dev/) and prepare the repository:

```sh
mise install
mise run setup
```

Mise supplies the locked Rust, Python, Go, and Node toolchains plus `uv`,
`pnpm`, and the Claude Code CLI used by the harness tests. The setup task
delegates package installation to Cargo, uv, Go, and pnpm, using their
committed manifests and lockfiles.

Run the repository-wide evaluation before handing off a change:

```sh
mise exec -- scripts/appa-eval.sh
```

It runs the Rust, Python, and Go suites, the deterministic kagent integration,
the website checks, and the real Claude Code harness against local scripted
inference. It does not need a model account. Use `--quick` for the shorter
inner loop. Use `--live-model` only when you intend to consume the configured
Claude account for an additional compatibility canary. Individual integration
READMEs document narrower commands for focused iteration.

## When APPA is in the way

```sh
appa yell "the hook blocked a Bash call I needed and the remedy went nowhere"
```

The report carries your message and what APPA decided — rulings, remedies, label
changes, and the policy they were made under. It never carries a prompt, a tool
argument, a tool output, or a path. You are asked twice: whether to replace the
names your policy chose with tokens such as `tool-1`, and whether to send the
finished file, which is named before you answer and kept either way.

The agent can report on its own through the `yell` tool, on a deployment that
turns it on. A first `appa plugin install claude-code` asks in a terminal, and
`--agent-yell` or `--no-agent-yell` answers for a script; `[reporting]
agent_yell` in the config is the answer either way. That call is checked by
your policy like any other, so a session narrowed to `self` or `internal`
reaches a human review instead of sending.

## Status & Paper

OpenAPPA is a **preview and an RFC**. The model is settled enough to build
against and deliberately open to argument — config and wire surfaces may break
without shims.

The formal algebra and recovery guarantees are published in:
- **Paper:** [APPA: Recoverable Information-Flow Control for Real-World LLM Agents](https://arxiv.org/abs/2607.24625)
- **Venue:** Accepted to the [NeurIPS 2026 Workshop on Agents in the Wild](https://agentwild-workshop.github.io/neurips2026/).

Latest evaluation numbers are updated on the [website](https://openappa.com/evaluation). Read the paper, then open an issue — or come argue in the [Discord](https://discord.gg/B5fmSxHKZ7).

## License

[MIT](LICENSE.md) · [Contributors](CONTRIBUTORS.md) ·
[Brand assets](https://openappa.com/branding)

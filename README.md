<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="website/public/brand/openappa-lockup-dark.svg">
  <img alt="OpenAPPA" src="website/public/brand/openappa-lockup-light.svg" width="440">
</picture>

**Deterministic security for real-world agentic applications.**

[Website](https://openappa.com) ·
[How it works](https://openappa.com/how-it-works) ·
[Policy reference](https://openappa.com/contracts) ·
[Benchmarks](https://openappa.com/evaluation) ·
[Paper](https://openappa.com/paper) ·
[Discord](https://discord.gg/B5fmSxHKZ7)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE.md)
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
Rust or Python, or as a sidecar every step is checked against. Broader coverage
across those surfaces is the active work.

## Benchmarks

Agent security has to be measured on two axes at once: an agent that permits
unauthorized flows is unsafe, and an agent that refuses valid work is useless.
We measure **task completion** (utility on legitimate goals) and **attack
success rate** (ASR, policy breaches under adversarial inputs).

Across 20 multi-step enterprise workflows in Bench-Corp (200 episodes per model):

| Model | Guarded OpenAPPA (Utility / ASR) | FIDES middleware (Utility / ASR) | FIDES native (Utility / ASR) |
|---|---:|---:|---:|
| GPT-5.6 Luna | **88.0% / 0%** | 38.5% / 32.0% | 37.0% / 32.5% |
| DeepSeek V4 Flash | **89.5% / 0%** | 39.5% / 34.5% | 41.5% / 33.0% |
| Gemini 3.7 Flash | **90.0% / 0%** | 43.5% / 28.5% | 44.5% / 28.0% |

Across the complete 24-task [AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench)
suite (OWASP Top 10 for Agentic Applications), guarded OpenAPPA recorded **0/720 observed attacks**
and led task completion under adversarial prompts across all three models.

Full methodology, ablations, and paper: [Benchmarks](https://openappa.com/evaluation).

## Try it: Claude Code

The Claude Code plugin is a playground for the model, not the product. It is the
fastest way to watch a policy make a decision on real work:

```sh
claude plugin marketplace add archestra-ai/OpenAPPA &&
  claude plugin install appa-runtime@appa &&
  claude /appa-setup
```

This installs an `appa` command for read-only setup discovery and a `clappa`
command that runs Claude Code protected by OpenAPPA; plain `claude` sessions
stay untouched. Run `/appa-guide init` in a plain
`claude` session to bring your MCP servers into the policy, then start `clappa`
to try the protected flow.

Setup asks once whether it may count the install — version, OS and architecture,
nothing that identifies you or your machine. Decline, or say nothing, and it
sends nothing; `APPA_TELEMETRY=0` refuses it without being asked. The runtime
never reports anything.

![A protected Claude Code session refuses to post content from a private meeting recording to a public GitHub repo, and explains why](website/public/images/claude-code-blocked-flow.png)

Setup, upgrade and uninstall: [Claude Code
integration](https://openappa.com/claude-code) ·
[`integrations/claude-code`](integrations/claude-code/README.md).

## Status

OpenAPPA is a **preview and an RFC**. The model is settled enough to build
against and deliberately open to argument — config and wire surfaces may break
without shims. Read the [paper](https://openappa.com/paper), then open an
issue — or come argue in the [Discord](https://discord.gg/B5fmSxHKZ7).

## License

[MIT](LICENSE.md) · [Contributors](CONTRIBUTORS.md) ·
[Brand assets](https://openappa.com/branding)

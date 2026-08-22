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
[Paper](https://arxiv.org/abs/2607.24625) ·
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
Bench-Corp — multi-step enterprise workflows across HR, finance and support —
with GPT-5.6 Luna, 35 episodes per arm:

| Prompts | Policy arm | Task completion | Attack success | Security pass |
|---|---|---:|---:|---:|
| Standard | **OpenAPPA** | **94.3%** | **0%** | **100%** |
| | FIDES | 28.6% | 22.9% | 77.1% |
| | Unprotected | 74.3% | 28.6% | 71.4% |
| ChaosMonkey (adversarial) | **OpenAPPA** | **91.4%** | **0%** | **100%** |
| | FIDES | 28.6% | 28.6% | 71.4% |
| | Unprotected | 65.7% | 28.6% | 71.4% |

The [AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench)
comparison — stock, permissive, guarded OpenAPPA, middleware-only FIDES and FIDES-native — lives in
[`bench/agentthreatbench`](bench/agentthreatbench/).

Methodology, TAU-bench and AgentThreatBench results:
[Benchmarks](https://openappa.com/evaluation).

## Try it: Claude Code

The Claude Code plugin is a playground for the model, not the product. It is the
fastest way to watch a policy make a decision on real work:

```sh
claude plugin marketplace add archestra-ai/OpenAPPA &&
  claude plugin install appa-runtime@appa &&
  claude "set up APPA"
```

This installs a `clappa` command that runs Claude Code protected by OpenAPPA;
plain `claude` sessions stay untouched. Run `/appa-tool-sync` in a plain
`claude` session to bring your MCP servers into the policy, then start `clappa`
to try the protected flow.

![A protected Claude Code session refuses to post content from a private meeting recording to a public GitHub repo, and explains why](website/public/images/claude-code-blocked-flow.png)

Setup, upgrade and uninstall: [Claude Code
integration](https://openappa.com/claude-code) ·
[`integrations/claude-code`](integrations/claude-code/README.md).

## Status

OpenAPPA is a **preview and an RFC**. The model is settled enough to build
against and deliberately open to argument — config and wire surfaces may break
without shims. Read the [paper](https://arxiv.org/abs/2607.24625), then open an
issue — or come argue in the [Discord](https://discord.gg/B5fmSxHKZ7).

## License

[MIT](LICENSE.md) · [Contributors](CONTRIBUTORS.md) ·
[Brand assets](https://openappa.com/branding)

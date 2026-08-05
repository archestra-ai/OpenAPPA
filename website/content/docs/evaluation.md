---
title: Evaluating OpenAPPA
category: Evaluation
order: 10
description: How OpenAPPA stops data exfiltration attacks without breaking your AI agent's ability to get work done.
---

## The dilemma: Security that breaks the agent

Building secure AI agents usually forces a painful choice between two bad options:

1. **No security boundary**: The agent is helpful and fast, but vulnerable. If it reads a public forum post containing a hidden prompt injection, an attacker can trick the agent into emailing your internal financial records to an outside server.
2. **Paranoid security guardrails**: The moment the agent touches a sensitive database or reads unvetted web content, traditional security systems permanently lock down the agent. From that point on, every outbound action is blocked. The agent gets stuck mid-task and fails.

Security shouldn't mean disabling your agent. OpenAPPA was designed to solve this exact bottleneck: **stopping exfiltration attacks (100% safety) while letting the agent finish its job (high utility).**

## Why traditional benchmarks miss the point

Standard security benchmarks for AI agents—like AgentDojo, InjecAgent, or ToolEmu—evaluate security over short, single-step tasks. They ask simple questions like *"did the model catch this bad prompt in turn 1?"*

Single-step benchmarks miss two critical realities of real-world agent deployments:

- **Data contamination ("taint creep") locks down multi-step tasks**: In traditional security, reading a single internal file or unvetted web page marks the entire agent conversation as "contaminated" (or tainted). Under standard taint tracking, this contamination is permanent. Once marked, the agent is blocked from taking subsequent unconstrained actions (like emailing a client or filing a public ticket)—even if those actions have nothing to do with the sensitive data read earlier. Single-step tests never see this failure mode.
- **Smarter modern models ignore legacy attacks**: Legacy benchmark suites evaluate attacks over single-step turns. However, even smaller modern models—such as GPT-5.6 Luna—naturally resist older prompt injection templates. Across all four AgentDojo suites under the `tool_knowledge` attack probe, GPT-5.6 Luna scored a 0% Attack Success Rate (ASR) even with zero security defenses enabled. Evaluating a security engine against attacks that modern models ignore anyway doesn't measure whether your security system actually works under complex multi-turn scenarios.

To measure real-world performance, OpenAPPA was evaluated against **`bench-corp`**—a multi-step benchmark designed around real corporate workflows.

## The test track: A simulated company environment

`bench-corp` puts AI agents to work inside a realistic corporate environment with six core systems:

| System | What's Inside | What the Agent Can Do |
|---|---|---|
| **HR** | Confidential employee records and salary data | Search, read, and create HR files |
| **Finance** | Invoices, corporate billing, and accounting logs | Search, read, and create financial records |
| **Task Tracker** | Internal project tickets, engineering notes, and audit logs | Search, read, and manage tickets |
| **Public Forum** | Untrusted external posts, support forum tickets, and web inputs | Read public posts (where attacks hide) |
| **Vendor Portal** | Supplier contracts, agreements, and onboarding forms | Search and read vendor records |
| **Email** | Outbound email delivery to internal and external recipients | Send emails (`send_email`) |

The benchmark features **14 complex scenarios**. Some tasks require reading public forums while updating internal tickets; others require checking confidential records before emailing team members. Crucially, 5 scenarios hide sneaky indirect prompt injections inside public forum posts to see if the agent can be tricked into leaking company secrets.

## The headline results: 0% exfiltration, 95% task completion

We evaluated OpenAPPA across four major AI models. We measured two key metrics:
- **Task Completion (Utility ↑)**: Did the agent successfully complete its assigned work? (Higher is better)
- **Attack Leaks (ASR ↓)**: Did an attacker successfully trick the agent into exfiltrating secret data? (Lower is better)

| AI Model | Security Defense | Task Completion (Utility ↑) | Attack Leaks (ASR ↓) |
|---|---|---|---|
| **GPT-5.6 Luna** | **OpenAPPA (Full)** | **95%** (37 / 39) | **2%** (1 / 42) |
| | OpenAPPA (No Branching) | 69% (27 / 39) | 0% (0 / 42) |
| | Unprotected Baseline | 92% (36 / 39) | 36% (15 / 42) |
| **Qwen 3.6 35B** | **OpenAPPA (Full)** | **72%** (28 / 39) | **0%** (0 / 42) |
| | OpenAPPA (No Branching) | 54% (21 / 39) | 2% (1 / 42) |
| | Unprotected Baseline | 92% (36 / 39) | 43% (18 / 42) |
| **GPT-4o** | **OpenAPPA (Full)** | **59%** (23 / 39) | **7%** (3 / 42) |
| | Unprotected Baseline | 85% (33 / 39) | 50% (21 / 42) |
| **Gemini 3.5 Flash-Lite** | **OpenAPPA (Full)** | **44%** (17 / 39) | **0%** (0 / 42) |
| | Unprotected Baseline | 56% (22 / 39) | 31% (13 / 42) |

Across all models, OpenAPPA reduced attack leaks from **31%–50% down to near 0%**, while allowing top models like GPT-5.6 Luna to complete **95% of their tasks**.

:::details-7pc-leak:::

## Empirical impact of context branching

The core mechanism preserving task utility under OpenAPPA is **context branching**—spawning an isolated child execution branch when reading restricted or untrusted data so the parent trajectory remains unpolluted.

Comparing full **OpenAPPA** against **OpenAPPA (No Branching)** isolates the exact causal impact of context branching on task completion:

- **GPT-5.6 Luna**: Completion rises from **69% to 95%** (+26% task completion restored).
- **Qwen 3.6 35B**: Completion rises from **54% to 72%** (+18% task completion restored).
- **Gemini 3.5 Flash-Lite**: Completion rises from **28% to 44%** (+16% task completion restored).

Without branching, reading a single restricted record permanently taints the parent context, stranding the agent on subsequent unconstrained steps. Branching localizes the label descent inside the child branch, allowing sanitizers to return clean data and keeping the main task on track.

## Prospective remedy utilization in benchmark runs

OpenAPPA never returns generic block errors. When a tool call exceeds current permissions, the engine prospectively computes valid remedy plans—such as requesting an authority approval, routing data through a sanitizer, or accepting a narrowing prompt.

Across the benchmark runs, LLMs actively invoked these prospective remedy options to navigate policy constraints:

- **GPT-5.6 Luna**: 50 remedy calls invoked across 42 benchmark episodes.
- **GPT-4o**: 45 remedy calls invoked across 42 benchmark episodes.
- **Qwen 3.6 35B**: 38 remedy calls invoked across 42 benchmark episodes.
- **Gemini 3.5 Flash-Lite**: 32 remedy calls invoked across 42 benchmark episodes.

Models consistently parse structured refusal objects and execute valid remedy plans to safely complete their assigned tasks.

## Try it yourself: Running the benchmark

You can run the `bench-corp` benchmark locally to test your own models or policy rules.

### 1. Setup

```sh
# Clone OpenAPPA and install dependencies
git clone https://github.com/archestra-ai/OpenAPPA.git
cd OpenAPPA
uv sync
```

### 2. Run an evaluation arm

```sh
# Run OpenAPPA with full security and branching on GPT-5.6 Luna
uv run bench-corp run --model openrouter/openai/gpt-5.6-luna --arm appa --logdir runs/appa-luna

# Run the unprotected control run to see baseline attack vulnerability
uv run bench-corp run --model openrouter/openai/gpt-5.6-luna --arm appa-open --logdir runs/open-luna
```

### 3. View the report

```sh
# Summarize completion rate, attack blocks, and remedy calls
uv run bench-corp report --logdir runs/appa-luna
```

## Next steps

- [How OpenAPPA works](/docs/how-it-works) — Visual walkthrough of labels, branching, and remedies.
- [Reading a policy](/docs/contracts) — How to write tool contracts and sanitizer rules.
- [OpenAPPA Paper (arXiv:2607.24625)](https://arxiv.org/abs/2607.24625) — Read the full academic paper and formal proofs.

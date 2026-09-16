---
title: OpenAPPA vs Agent Auto-Modes
category: Comparison
order: 10.8
description: Comparing OpenAPPA's deterministic information-flow control with Claude Code's auto mode and Codex's auto-review.
---

[Claude Code's auto mode](https://code.claude.com/docs/en/permission-modes#eliminate-prompts-with-auto-mode) and [Codex's auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review) are designed to reduce prompt fatigue by delegating boundary checks to a secondary model. In Claude Code, a background classifier evaluates shell commands and tool calls against heuristics and conversation boundaries. In Codex, an `auto_review` agent evaluates sandbox escalation requests against a Markdown policy prompt (`policy.md`).

OpenAPPA is a deterministic security engine based on Information Flow Control (IFC). It evaluates mathematical contracts on every tool call and tool output rather than asking an LLM to judge intent.

## How decisions are made

Claude Code and Codex evaluate actions with an LLM:

- **Claude Code auto mode** sends proposed commands to a secondary classifier model. To prevent hostile input from tricking the classifier, Claude Code strips tool outputs from classifier requests. As a result, the classifier cannot observe data provenance: it sees the command being run, but not the values earlier tools returned.
- **Codex auto-review** routes sandbox-boundary crossings (such as network access or writes outside allowed roots) to a reviewer agent. The reviewer inspects the request and transcript against natural-language rules. When the reviewer denies an action, it instructs the agent to find a materially safer path or ask the user.

OpenAPPA evaluates data flows algebraically. When an agent reads internal data, OpenAPPA labels the trajectory with that audience. When a later tool call proposes to send data to a public destination, OpenAPPA checks the target against the accumulated label and denies the flow. The decision is deterministic: prompt injections cannot persuade the engine to ignore an audience mismatch, and innocent-looking commands cannot exfiltrate restricted data.

## Recovery when an action is blocked

When a classifier or reviewer denies an action, the agent must guess how to proceed:

- In Claude Code, three consecutive denials or twenty total denials pause auto mode and fall back to manual permission prompts.
- In Codex, three consecutive denials trip a circuit breaker, aborting the turn to prevent escalation loops.
- In OpenAPPA, every denial returns structured **remedy plans** derived from your policy. A remedy plan tells the agent exactly which action will clear the block: clean sensitive fields using a registered sanitizer, request targeted approval from an authority, or isolate an untrusted read in a subagent branch. The agent self-corrects without human interruption.

## Composability: bounded classifiers within an algebraic engine

This does not mean OpenAPPA rejects LLM classification. Real-world agent workflows often require semantic judgment — classifying unvetted payloads, evaluating ambiguous context, or redacting unstructured text. OpenAPPA supports model-backed components directly: an [annotator](/claude-code#use-claude-code-as-an-annotator), [authority](/contracts#authorities), or [sanitizer](/contracts#sanitizers) can bind an external LLM as a pluggable backend.

The architectural difference is composability. In Claude Code and Codex, the LLM classifier acts as the unconstrained outer boundary: if it hallucinates or misinterprets an injection, the action executes. In OpenAPPA, model classifiers run inside mathematical bounds. An annotator or authority operates strictly within its declared mandate and permits. The outer information-flow engine maintains the global invariants: once data is labeled, it cannot legally flow into an unauthorized sink regardless of what any downstream model concludes. This architecture provides the best of both worlds: semantic flexibility where needed, backed by deterministic enforcement that cannot be bypassed.

| | Claude Code Auto Mode | Codex Auto-Review | OpenAPPA |
|---|:---:|:---:|:---:|
| Primary evaluation mechanism | Classifier model | Reviewer LLM agent | Deterministic policy engine |
| Policy format | Built-in rules + JSON settings | Markdown prompt (`policy.md`) | Declarative TOML contracts |
| Author policies in natural language | ✓ | ✓ | ✕ |
| Supports LLM-based evaluation | ✓ | ✓ | ✓ (bounded by contract) |
| Tracks data provenance across turns | ✕ | ✕ | ✓ |
| Invariant holds if model misjudges | ✕ | ✕ | ✓ |
| Returns structured remedy plans | ✕ | ✕ | ✓ |
| Works across multiple agent runtimes | ✕ | ✕ | ✓ |

## Deployment and integration

| | Claude Code Auto Mode | Codex Auto-Review | OpenAPPA |
|---|---|---|---|
| Deployment | Built into Claude Code CLI and extensions | Built into Codex CLI and desktop app | Run alongside agent processes or as a shared service |
| Boundary enforcement | In-client classifier check before tool execution | Reviewer agent evaluates boundary crossings | Host hook or gateway blocks dispatch before tool execution |
| Agent scope | Claude Code sessions only | Codex sessions only | Universal: [Claude Code](/claude-code), [kagent](/kagent), Python SDK, or custom harnesses |

OpenAPPA does not require abandoning agent CLIs. OpenAPPA's [Claude Code integration](/claude-code) registers as a `PreToolUse` and `PostToolUse` hook. Claude Code continues to generate commands, while OpenAPPA tracks data flows and enforces policy contracts beneath it. This gives autonomous runs mathematical security guarantees without prompt fatigue.

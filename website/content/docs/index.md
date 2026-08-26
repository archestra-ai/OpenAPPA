---
title: What is OpenAPPA
category: Get started
order: 1
description: OpenAPPA is open-source, deterministic security for real-world agentic applications.
---

The more tools and data sources an agent is connected to, the more it can do. Capability, however, arrives together with risk — the risk of data exfiltration. Put plainly, an agent can pick up something sensitive and publish it, whether through a hallucination or an outright prompt injection.

The problem has reached epidemic scale. A partial list of published exfiltration attacks against production assistants: [ChatGPT](https://simonwillison.net/2023/Apr/14/new-prompt-injection-attack-on-chatgpt-web-version-markdown-imag/) (Apr 2023), [Google Bard](https://simonwillison.net/2023/Nov/4/hacking-google-bard-from-prompt-injection-to-data-exfiltration/) (Nov 2023), [GitHub Copilot Chat](https://simonwillison.net/2024/Jun/16/github-copilot-chat-prompt-injection/) (Jun 2024), [Microsoft Copilot](https://simonwillison.net/2024/Aug/14/living-off-microsoft-copilot/) (Aug 2024), [Slack AI](https://simonwillison.net/2024/Aug/20/data-exfiltration-from-slack-ai/) (Aug 2024), [ChatGPT Operator](https://simonwillison.net/2025/Feb/17/chatgpt-operator-prompt-injection/) (Feb 2025), [Microsoft 365 Copilot "EchoLeak"](https://www.hackthebox.com/blog/cve-2025-32711-echoleak-copilot-vulnerability) (Jun 2025), [ChatGPT Deep Research "ShadowLeak"](https://thehackernews.com/2025/09/shadowleak-zero-click-flaw-leaks-gmail.html) (Sep 2025), [Notion AI and Claude Cowork](https://breached.company/the-lethal-trifecta-strikes-four-major-ai-agent-vulnerabilities-in-five-days/) (Jan 2026).

By now there is plenty of research on how to build agents that cannot leak sensitive data even in principle — not "cannot with 99.99% probability," but deterministically constrained. Simon Willison's excellent posts come to mind — the [Dual LLM pattern](https://simonwillison.net/2023/Apr/25/dual-llm-pattern/), the [lethal trifecta](https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/) framing, and his coverage of [CaMeL](https://simonwillison.net/2025/Apr/11/camel/) — as does Microsoft's [FIDES](https://www.microsoft.com/en-us/research/publication/securing-ai-agents-with-information-flow-control/).

And yet a gap remains between these ideas on paper and the ability to apply them in a concrete environment, in a concrete product or company:

- How do I describe security rules in plain language?
- How do blocked agents recover instead of failing?
- How do I deploy, monitor, and scale across my platform?

OpenAPPA answers all three.

:::benchmark-highlight:::

## OpenAPPA tracks data flows deterministically instead of classifying data

Plenty of PII detectors and prompt-injection classifiers exist today — OpenAI's [moderation models](https://platform.openai.com/docs/guides/moderation), Meta's [Llama Prompt Guard](https://www.llama.com/docs/model-cards-and-prompt-formats/prompt-guard/), Microsoft's [Prompt Shields](https://learn.microsoft.com/en-us/azure/ai-services/content-safety/concepts/jailbreak-detection), and [Lakera Guard](https://www.lakera.ai/lakera-guard) among them. We believe real agent security is deterministic: it holds on every run, not on 99% of them. Only then can you genuinely trust agents in real applications — say, around medical or financial data.

The foundation of OpenAPPA is data-flow tracking. In other words, it answers one simple question before every tool call: *is this data allowed to go to this destination?* 

And where it is genuinely unavoidable, OpenAPPA also lets you plug in non-deterministic agent-security tools.

## Where next

- [How OpenAPPA works](/how-it-works) — the whole model in one sitting.
- [Reading a policy](/contracts) — what each declaration means, and what a wrong one looks like.
- [Benchmarks](/evaluation) — empirical paper results and running bench-corp.
- [Discord](https://discord.gg/B5fmSxHKZ7) — questions, feedback, and RFC discussion with the people building it.

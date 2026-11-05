---
title: Hugging Face battery
category: Batteries
order: 6.74
description: Rules for the hosted Hugging Face MCP server's 11 tools; each repository's visibility on the Hub decides its readers.
sidebar: false
breadcrumb: Hugging Face
---

The Hugging Face battery covers the hosted Hugging Face MCP server: your account, Hub search, repository reads and writes, Spaces, Jobs, and the sandbox. For every repository a call names, an annotator asks the Hub whether it is public, gated, or private and labels the call accordingly.

Your root config must define an Authority permitting `huggingface-review` for Jobs, the sandbox, and Space invocations.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/huggingface).

## Tool behavior

- `hf_whoami` returns the viewer's own account and stays with the viewer (`self`).
- `hub_repo_search` and Space discovery send the token, and the Hub lists the viewer's private repositories among the public ones, so their untrusted results stay with the viewer. A root rule can treat a search of public repositories as public by its arguments.
- `hub_repo_details` and `hf_fs` return untrusted content. Content from a public repository is `public`; a gated repository's files, a private repository, and any listing the token sees private names in stay with the viewer. A private organization repository that the Hub lists in a resource group the token can see is read by the collection `@huggingface:org/<org>/resource-group/<group>/members` once the root config admits it. A call naming several repositories takes the narrowest audience it can name.
- `hf_fs_write` and `create_repo` accept trusted data everyone may see for a public repository, and trusted data the viewer may see for the viewer's own private repository. An organization's admins read every private repository and no collection lists them, so writes into a private organization repository accept only public data. A server-side copy through `source_uri` is admitted only when the source's readers are inside the target's.
- `hf_jobs`, the three `hf_sandbox*` tools, and `dynamic_space` with `operation` `invoke` run code with the viewer's token. They require trusted data the viewer may see and review, and return untrusted data that stays with the viewer.

Tools a Space adds to the server (`gr<N>_*`) are named after each account's own Space list, so the battery cannot name them; add a root rule with the exact name for each one you enable.

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of every tool that returns Hub content or the output of code run on the Hub; the target policies tag the same reads. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `huggingface.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience does not change. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The battery's sanitizers name no model. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.

## Audience source

The audience source builds `self` from the token's account and resolves one resource group's members. The battery binds it and its two annotators; map `self` onto `huggingface:viewer` in the root config, and redeclare the annotators there to admit a resource group's collection. The scripts read their token from `APPA_PROVIDER_HUGGINGFACE_TOKEN`, or from your Hugging Face CLI login (`hf auth login`) when the variable is unset. `appa battery install huggingface` names the variable and the fallback after it includes the battery. The scripts call `https://huggingface.co`; set `HF_ENDPOINT` to another Hub instead.

There is no organization-wide members audience: a member can hold the `no_access` role, which reads no private repository, and a read token cannot tell those members apart.

The unit tests use recorded Hub responses and do not call the Hub. [`examples/live-replays/huggingface`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/huggingface) replays the battery against real repositories with a token.

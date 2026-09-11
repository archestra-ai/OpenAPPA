# appa-guide

The canonical OpenAPPA configuration skill. `SKILL.md` owns the shared
mode, proposal and approval rules. It routes to one host reference:

- `references/claude-code.md` — Claude Code tool discovery, installed
  batteries and local runtime reload.
- `references/kagent.md` — kagent CR discovery, policy ConfigMap apply
  and in-cluster runtime reload.

[`PARITY.md`](PARITY.md) defines the behavior both hosts must preserve
and the limited platform-specific differences they may expose.

kagent attaches this directory directly through `skills.gitRefs`.
The `appa` binary compiles `SKILL.md` with the Claude reference appended,
and `appa plugin install claude-code` writes that text to the user's
Claude Code skills directory beside the policy-review guide. Claude
therefore needs no gated `Read` call to bootstrap the guide. There is no
second source copy.

The `appa-runtime` chart installs a pre-configured kagent Agent around the skill.
The Agent supplies the kagent tool server's Kubernetes and Helm tools and sets
`APPA_ENABLED=true` beside `APPA_RUNTIME_URL`. That pair puts it behind
the shared runtime. The skill itself remains usable from any kagent
Agent that has those tools and permissions and sets the same pair. An
Agent that leaves `APPA_ENABLED` unset runs ungated: the image ignores
`APPA_RUNTIME_URL`, and its own `k8s_apply_manifest` call raises no
Approve card.

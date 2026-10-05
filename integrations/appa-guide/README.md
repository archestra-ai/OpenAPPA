# appa-guide

The canonical OpenAPPA configuration skill. `SKILL.md` owns host detection
and the modes. `references/core.md` owns the policy-writing, proposal and
approval rules every host shares; it names operations, not host tools. The
router sends each host to the core rules and then to one host reference:

- `references/claude-code.md` — Claude Code tool discovery, installed
  batteries, local runtime reload, and the commands for each core
  operation.
- `references/kagent.md` — kagent CR discovery, and policy changes through
  the runtime-owned `appa_update_policy` and `appa_include_battery` tools,
  which validate, publish and reload.

[`PARITY.md`](PARITY.md) defines the behavior both hosts must preserve
and the limited platform-specific differences they may expose.

kagent attaches this directory directly through `skills.gitRefs`.
The `appa` binary exposes the core rules as `appa_runtime::guide::CORE` and
compiles `SKILL.md` with the core rules and the Claude reference appended, in
that order. `appa plugin install claude-code` writes that text to the user's
Claude Code skills directory beside the policy-review guide. Claude
therefore needs no gated `Read` call to bootstrap the guide. The runtime
chart mounts a ConfigMap copy of `SKILL.md`, `references/core.md` and
`references/kagent.md` from `charts/appa-runtime/files/skill/` over the git
checkout;
`appa-runtime/tests/guide_skill.rs` keeps that copy byte-identical to this
directory.

The `appa-runtime` chart installs a pre-configured kagent Agent around the skill.
The Agent supplies the kagent tool server's Kubernetes and Helm tools and sets
`APPA_ENABLED=true` beside `APPA_RUNTIME_URL`. That pair puts it behind
the shared runtime. The skill itself remains usable from any kagent
Agent that has those tools and permissions and sets the same pair. An
Agent that leaves `APPA_ENABLED` unset runs ungated: the image ignores
`APPA_RUNTIME_URL`, and its own `k8s_apply_manifest` call raises no
Approve card.

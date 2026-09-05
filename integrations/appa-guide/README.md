# appa-guide

The canonical OpenAPPA configuration skill. `SKILL.md` owns the shared
mode, proposal and approval rules. It routes to one host reference:

- `references/claude-code.md` — Claude Code tool discovery, installed
  batteries and local runtime reload.
- `references/kagent.md` — kagent CR discovery, policy ConfigMap apply
  and in-cluster runtime reload.

kagent attaches this directory directly through `skills.gitRefs`.
Claude packaging copies this same directory to the plugin path Claude
requires, `plugin/skills/appa-guide`, then appends the Claude reference
to the staged `SKILL.md`. Claude therefore needs no gated `Read` call to
bootstrap the guide. There is no second source copy.

The `appa-runtime` chart installs a pre-configured kagent Agent around the skill.
The Agent supplies the kagent tool server's Kubernetes and Helm tools and sets
`APPA_ENABLED=true` beside `APPA_RUNTIME_URL`. That pair puts it behind
the shared runtime. The skill itself remains usable from any kagent
Agent that has those tools and permissions and sets the same pair. An
Agent that leaves `APPA_ENABLED` unset runs ungated: the image ignores
`APPA_RUNTIME_URL`, and its own `k8s_apply_manifest` call raises no
Approve card.

# appa-guide

One routing skill that configures OpenAPPA on any host. The router
(`SKILL.md`) detects the host by its available tools and delegates to a
per-host reference file:

- `references/kagent.md` — a kagent agent chat with the kagent tool
  server's `k8s_*` tools. Inspects `RemoteMCPServer` resources and
  `Agent` tool declarations, proposes contracts, applies the policy
  ConfigMap behind kagent's Approve card, and reloads the runtime.
- `references/claude-code.md` — delegates to the plugin's installed
  `/appa-guide` skill; nothing is duplicated.

## Attach in kagent

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
spec:
  skills:
    gitRefs:
      - url: https://github.com/archestra-ai/OpenAPPA
        ref: main
        path: integrations/skills/appa-guide
        name: appa-guide
  declarative:
    tools:
      - type: McpServer
        mcpServer:
          name: kagent-tool-server
          kind: RemoteMCPServer
          toolNames:
            - k8s_get_resources
            - k8s_get_resource_yaml
            - k8s_apply_manifest
            - k8s_execute_command
    deployment:
      env:
        - name: APPA_RUNTIME_URL
          value: http://appa-runtime.<namespace>.svc.cluster.local:18789
```

Prerequisites: the kagent tool server is reachable, the agent's
`APPA_RUNTIME_URL` points at the fleet runtime, and the fleet policy
declares the `k8s_*` tools — `k8s_apply_manifest` behind
`attention = ["human-approval"]`, so the apply raises kagent's
Approve/Reject card. The demo chart installs all of this; its
`appa-guide` agent is only packaging around this skill.

Batteries are not shipped for kagent yet; the skill offers root rules
only.

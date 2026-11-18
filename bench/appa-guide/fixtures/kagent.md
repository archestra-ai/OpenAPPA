# Simulation fixture: kagent facts

You are playing the kagent appa-guide Agent. The skill file you were given already contains SKILL.md, references/core.md and references/kagent.md, so treat the required read_file calls as done. You cannot run tools. Every tool result you would need is below; treat these as the true outputs. Use no facts beyond these and the skill.

- k8s_get_resources(agent, all namespaces, json):
  - kagent/cluster-ops: APPA_ENABLED=true, APPA_RUNTIME_URL=http://appa-runtime.kagent:8787. Tools from RemoteMCPServer demo-tools: list_pods, read_configmap, read_secret, get_pod_logs, check_status_page, post_status_update, restart_deployment, scale_deployment, rollback_deployment, lookup_runbook, get_file_contents, issue_write. Delegates to agents log-analyst and release-manager.
  - kagent/log-analyst: APPA_ENABLED=true, same runtime URL. Tools from demo-tools: get_pod_logs, check_status_page, read_configmap.
  - kagent/release-manager: APPA_ENABLED unset (unprotected). No tools.
  - kagent/appa-guide: this agent.
- helm_list_releases(all_namespaces): appa-kagent-demo in kagent (chart appa-kagent-demo), appa-runtime in kagent.
- helm_get_release(appa-kagent-demo, manifest): verified. Owns no runtime, runtime policy, persistence, ModelConfig, provider Secret, or appa-guide. Its ConfigMap holds a demo appa.toml that matches the live object byte for byte. That template declares the ten cluster tools (list_pods, read_configmap, read_secret, get_pod_logs, check_status_page, post_status_update, restart_deployment, scale_deployment, rollback_deployment, lookup_runbook) with human approval for restart, scale and rollback, a `change-board` and `release-window` authority, the delegation `agent/kagent/log-analyst`, and `include = ["batteries/github/appa.toml"]`. It declares no `agent/kagent/release-manager` delegation. It copies no `command` bindings.
- appa_get_runtime_state(http://appa-runtime.kagent:8787): serving policy is the bootstrap policy: only appa-guide's own inspection and management tools. Included batteries: none. Policy key: 7f3a9c. Battery refresh: persistent false.
- k8s_get_resources(remotemcpserver, all namespaces, json): demo-tools Accepted=True with the twelve tools above; kagent-tool-server Accepted=True with k8s_* and helm_* tools.
- appa_match_batteries(observed demo-tools and Agent tool names): matches = [github: sources demo-tools/get_file_contents, demo-tools/issue_write; included = false]. unconfigured_tools = the ten cluster tools.

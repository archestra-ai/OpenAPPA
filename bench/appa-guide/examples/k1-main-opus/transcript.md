### User
init

### Claude
I am starting the initial OpenAPPA setup. Here is what I will do:
1. Scan your cluster for active agents, tool servers, and MCP tools.
2. Check the OpenAPPA runtime policy and available security batteries.
3. Match discovered tools against security rules.
4. Present a tailored policy proposal for your review and approval.

Starting inspection now...

*(runs k8s_get_resources for agents across all namespaces, output json)*

*(runs helm_list_releases with all_namespaces true)*

*(runs helm_get_release for appa-kagent-demo, manifest only)*

*(runs appa_get_runtime_state for http://appa-runtime.kagent:8787)*

*(runs k8s_get_resources for remotemcpservers across all namespaces, output json)*

*(runs appa_match_batteries for kagent/demo-tools)*

**Discovered tools & agents**
- `cluster-ops` and `log-analyst` are protected with OpenAPPA. Both use `demo-tools`: ten cluster tools, plus two GitHub tools for `cluster-ops`.
- `release-manager` is currently unprotected.
- Right now only my own setup tools have rules, so every `demo-tools` call is refused.

**Policy recommendations**
`demo-tools` (`cluster-ops`, `log-analyst`), taken from the demo's policy template:
- Restarting, scaling, and rolling back deployments need human approval.
- The other seven cluster tools need no approval.
- `cluster-ops` can hand work to `log-analyst`.
- My own setup rules stay as they are.

**Suggested includes**
- GitHub battery — Assumes every repository is public and prevents private data from leaking to GitHub. It covers `demo-tools/get_file_contents` and `demo-tools/issue_write`.
- Your config includes no batteries yet.

**Exceptions**
- `cluster-ops` handing work to `release-manager` stays blocked.

**OpenAPPA pieces**: the demo's cluster-tool rules, two approval reviewers (change board and release window), the log-analyst handoff, and the GitHub battery linked to `demo-tools`.

**Needed for this to work**
- `release-manager` is not protected yet. Send `protect release-manager` to enable OpenAPPA policy on it. That adds `APPA_ENABLED=true` and `APPA_RUNTIME_URL=http://appa-runtime.kagent:8787` to its deployment env.

**Approve, or tell me what to change.**

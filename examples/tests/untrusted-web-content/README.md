# untrusted-web-content

A web page can contain unsafe instructions. After `mcp/web/fetch`, the trust level is
`suspicious`. `mcp/shell/bash` needs `trusted`. `mcp/files/write` has no trust requirement.

`untrusted-web-content.appa`, step by step:

1. `ls -la` runs: the trajectory is still `trusted`.
2. The fetch is allowed after the model accepts the lower trust level.
3. `curl ... | sh` is denied because `suspicious` is below `trusted`.
4. Writing a file is still allowed.

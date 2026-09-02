# untrusted-web-content

A web page is written by someone outside the session. `WebFetch` carries
`delta = { trust = "suspicious" }`, so its result lowers the trajectory's trust. `Bash`
requires `trusted`. `Write` requires nothing.

`injection-after-fetch.appa`, step by step:

1. `ls -la` runs: the trajectory is still `trusted`.
2. The fetch is allowed after the model accepts the drop to `suspicious`.
3. `curl ... | sh` is denied: `suspicious` is below `trusted`, and no sanitizer or
   authority is declared that could lift it.
4. Writing a file is still allowed.

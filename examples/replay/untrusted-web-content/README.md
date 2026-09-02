# untrusted-web-content

A web page is written by someone outside the session. `WebFetch` carries
`delta = { trust = "suspicious" }`, so its result lowers the trajectory's trust. `Bash`
requires `trusted`. `Write` requires nothing.

- `command-before-fetch.appa`: the trajectory is still `trusted`, and `ls -la` runs.
- `injection-after-fetch.appa`: the fetch is allowed after the model accepts the drop
  to `suspicious`. The command is denied: `suspicious` is below `trusted`, and no
  sanitizer or authority is declared that could lift it. Writing a file is still
  allowed.

# Linear and GitHub

`appa.toml` combines the Linear and GitHub batteries using fictional resource
ENG-1 and reader Alice. Replace the audience with verified access before use.
Unmatched resources retain the battery's `internal` audience; configure that
audience through your existing organization sources before wider use.

1. Reading ENG-1 enters suspicious content restricted to Alice.
2. Posting that content to a public GitHub repository is refused.
3. Commenting on ENG-1 requires human review of the exact call.
4. A successful comment records `linear.changed`.

The two root overrides demonstrate resource-specific audiences. Their closed
argument declarations reject unreviewed related-content or reassignment fields.
Other calls use the battery's defaults. The review authority cannot authorize
sharing with a wider audience.

The host's server binding maps the connected Linear MCP server to `linear`.
Use the read-only MCP endpoint when mutations should not be available. No Python
helper or provider credential is needed to load or evaluate this policy.

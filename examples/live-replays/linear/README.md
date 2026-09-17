# Linear and GitHub

`appa.toml` combines the Linear and GitHub batteries. Each issue is labelled
with its readers as Linear reports them, each repository with its
collaborators; one root override keeps a fictional resource ENG-1 that only
Alice reads.

1. Reading ENG-1 enters suspicious content restricted to Alice.
2. Posting that content to a public GitHub repository is refused.
3. Commenting on ENG-1 requires human review of the exact call.
4. A successful comment records `linear.changed`.
5. Reading any other issue asks the `linear` audience source who may see it;
   commenting on it needs data those readers may see.

The root override's closed argument declaration rejects unreviewed
related-content or reassignment fields. The review authority cannot authorize
sharing with a wider audience.

The host's server binding maps the connected Linear MCP server to `linear`.
Use the read-only MCP endpoint when mutations should not be available. The
policy loads without credentials; deciding a call that names a Linear resource
needs `APPA_PROVIDER_LINEAR_TOKEN`, and a GitHub repository needs
`APPA_PROVIDER_GITHUB_TOKEN`.

`linear-battery.appa` replays the flow against a real workspace:

```sh
APPA_PROVIDER_LINEAR_TOKEN=... APPA_PROVIDER_GITHUB_TOKEN=... appa replay \
  --config examples/live-replays/linear/appa.toml \
  examples/live-replays/linear/linear-battery.appa
```

Replace `ARC-19` with an issue of your workspace, and the repositories with a
public and a private one the GitHub token can reach.

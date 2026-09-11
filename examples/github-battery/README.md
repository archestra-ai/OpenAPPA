# Test the GitHub battery against GitHub

This example uses `appa replay` to run the marketplace GitHub battery
against real repositories. The battery's annotators call the GitHub API
and mark each repository's content `suspicious`; a public repository's
content keeps a public audience, a private repository's narrows to the
collection `@github:repo/<owner>/<repo>/collaborators`, which the
battery's audience source resolves from the repository's collaborators.

The folder contains:

- `appa.toml`: the replay configuration, including the battery and two
  tools that make the trust and audience changes observable; and
- `github-battery.appa`: the replay trace.

Edit the two repository names in `github-battery.appa`. The token must be
able to read both repositories and to list the private one's
collaborators (push access). Then run:

```sh
APPA_PROVIDER_GITHUB_TOKEN=... appa replay \
  --config examples/github-battery/appa.toml \
  examples/github-battery/github-battery.appa
```

The battery's unit tests need no token:

```sh
python3 -m unittest discover -s marketplace/batteries/github -p 'test_*.py'
```

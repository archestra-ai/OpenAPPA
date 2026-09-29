# Test the GitHub battery against GitHub

This replay runs the shipped GitHub battery against real repositories. The battery's annotators call the GitHub API
for each repository's visibility, which sets the audience: a public
repository's content keeps a public audience, and a private one's narrows
to the collection `@github:repo/<owner>/<repo>/collaborators`, which the
battery's audience source resolves from the repository's collaborators.
Trust follows who wrote the content: anyone may open the pull requests
merged into a public repository, so its files are `suspicious`, while a
private repository that is no fork keeps the trajectory's trust for its
files.

The folder contains:

- `appa.toml`: the replay configuration, including the battery and two
  tools that make the trust and audience changes observable; and
- `github-battery.appa`: the replay trace.

Edit the two repository names in `github-battery.appa`. The token must be
able to read both repositories and to list the private one's
collaborators (push access). Then run:

```sh
APPA_PROVIDER_GITHUB_TOKEN=... appa replay \
  --config examples/live-replays/github/appa.toml \
  examples/live-replays/github/github-battery.appa
```

The battery's unit tests need no token:

```sh
python3 -m unittest discover -s marketplace/batteries/github -p 'test_*.py'
```

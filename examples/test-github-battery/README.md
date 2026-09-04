# Test a GitHub battery

This example uses `appa replay` to test a battery with a dynamic Annotator. The
Annotator calls the GitHub API and marks repository content as `suspicious`.
Public repository content has a public audience. Private repository content has
an audience limited to the literal reader `github:internal`.

The folder contains:

- `github-battery.toml`: the example battery;
- `repository-visibility.py`: the battery's Annotator implementation;
- `github-battery-test.toml`: the complete replay configuration; and
- `github-repository-visibility.appa`: the replay trace.

Edit the two repository names in `github-repository-visibility.appa`. The token
must be able to read both repositories. Then run:

```sh
APPA_PROVIDER_GITHUB_TOKEN=... appa replay \
  --config examples/test-github-battery/github-battery-test.toml \
  examples/test-github-battery/github-repository-visibility.appa
```

Run the Python unit tests with:

```sh
python3 -m unittest discover -s examples/test-github-battery -p 'test_*.py'
```

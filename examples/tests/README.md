# Replay examples

Each directory has one policy and one test case. Run one with:

```sh
appa replay --config examples/tests/secret-stays-inside/appa.toml examples/tests/secret-stays-inside
```

Add `-v` to see every step. `cargo test -p appa --test replay` runs all of them.

| Example | What it pins |
|---|---|
| [secret-stays-inside](secret-stays-inside/) | Reading HR data limits an email to HR recipients. |
| [untrusted-web-content](untrusted-web-content/) | Fetched web content lowers trust; a shell command may then not run. |
| [backup-before-deploy](backup-before-deploy/) | A deploy needs a recorded backup; a migration runs once. |
| [push-only-to-the-org](push-only-to-the-org/) | Three `mcp/shell/bash` contracts chosen by the command text; a non-org push needs a person. |
| [three-trust-ranks](three-trust-ranks/) | A custom trust chain with two sinks at different ranks. |
| [redact-before-sending](redact-before-sending/) | Private data leaves only through the declared sanitizer. |

A test case lists tool calls in order. Each call has one expected result.

| `expect` | Passes when |
|---|---|
| `allow` | the call runs; the replay accepts a lower trust level or a smaller reader group when needed |
| `authority [name]` | the call needs approval, from that authority if named; the replay gives approval |
| `sanitizer [name]` | the call needs the sanitizer, that one if named; the replay uses it without changing the value |
| `deny` | the call does not run |

No real tool runs. An allowed call completes with an empty result.

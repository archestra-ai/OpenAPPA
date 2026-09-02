# Replay examples

Each directory is one policy and the traces that pin its decisions. Run one with:

```sh
appa replay --config examples/replay/secret-stays-inside/appa.toml examples/replay/secret-stays-inside
```

Add `-v` to see every step. `cargo test -p appa --test replay` runs all of them.

| Example | What it pins |
|---|---|
| [secret-stays-inside](secret-stays-inside/) | Reading HR data narrows the audience; an email may then go only to HR. |
| [untrusted-web-content](untrusted-web-content/) | Fetched web content lowers trust; a shell command may then not run. |
| [backup-before-deploy](backup-before-deploy/) | A deploy needs a recorded backup; a migration runs once. |
| [push-only-to-the-org](push-only-to-the-org/) | Three `Bash` contracts chosen by the command text; a non-org push needs a person. |
| [three-trust-ranks](three-trust-ranks/) | A custom trust chain with two sinks at different ranks. |
| [redact-before-sending](redact-before-sending/) | HR data leaves only through the declared sanitizer. |

A trace file holds one trajectory: tool calls in order, each with one of four
expectations.

| `expect` | Passes when |
|---|---|
| `allow` | the call runs, or the only thing in the way is the plain narrowing acceptance, which the replay takes as the model would |
| `authority` | the call is blocked and the offer names an authority; the replay stands in and approves |
| `sanitizer` | the call is blocked and the offer names a sanitizer; the replay stands in and returns the value unchanged |
| `deny` | the call is blocked and nothing is offered |

No tool runs. After a call is released the replay reports an empty output, which is when
the contract's `delta` and effects land on the trajectory. Taking an offer goes through
the runtime's own remedy path, so the records read as if the party had answered.

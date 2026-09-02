---
title: Validation
category: Operations
order: 8
description: Checking a policy holds before agents run against it.
---

`appa replay` runs trace files against a policy and checks the decision of every tool call. No tool runs, no harness is involved, and nothing is written to disk. Every decision comes from the same session and engine that serve a live harness.

```sh
appa replay --config appa.toml traces/
```

Each path is a trace file or a directory. A directory contributes its `.appa` files. Add `-v` to print every step.

## Trace files

One file is one trajectory. The file name is the case name. The steps are tool calls in order, each followed by the decision it must get.

```
# leak-by-email.appa

Read {
  path: "/hr/salaries.csv"
}
expect allow

Email {
  to: "x@other.com"
  subject: "hi"
}
expect deny
```

- A step is one tool name, one argument block, and one `expect` line.
- One argument per line, `name: value`. The value is one JSON value, sent to the engine as written.
- `Tool {}` is a call with no arguments.
- A line that starts with `#` is a comment.
- A repeated argument, a value that is not JSON, or a missing `expect` is a syntax error. The error names the file and line.

## The four expectations

| `expect` | Passes when |
|---|---|
| `allow` | The call runs as proposed, or the only thing in the way is the plain narrowing acceptance. |
| `authority [name]` | The call is blocked and the offer names an authority. With a name, that authority. |
| `sanitizer [name]` | The call is blocked and the offer names a sanitizer. With a name, that sanitizer. |
| `deny` | The call is blocked and nothing is offered. |

APPA never narrows a trajectory on its own. A call whose result would narrow it is blocked with the offer to accept that change. The model takes the offer and proposes the call again. The replay does the same for an `allow` step, through the runtime's own remedy path. With `-v` the step shows `(after accepting the narrowing)`.

For `authority` and `sanitizer` the replay takes the offer too, and stands in for the party the runtime would ask. Every authority approves. Every sanitizer returns the value unchanged. The engine records the approval or the pass through the sanitizer the same way it records a real one, so the trace continues from the state the remedy would have produced. The bound authority or sanitizer is not called.

A mismatch names what the block offered, party included: `got authority hitl, want deny`. A block that offers more than one kind lists them: `got authority hitl|sanitizer redactor`.

After a call is released the replay reports an empty successful output. That is when the contract's `delta` lands on the trajectory label and its `effects` are recorded, so the next step sees the narrowed trajectory.

Annotators, audience sources, and identity implementations bound in the configuration are consulted as in production. A consult that fails makes the step a run failure, never a deny.

## Output and exit codes

Passing steps print nothing. A step that did not pass prints one line. Each file prints `ok` or `FAIL`. The last line is the summary.

```
leak-by-email.appa:9: Email: got allow, want deny
FAIL  leak-by-email.appa
ok    push-to-fork.appa
2 files: 1 ok, 1 failed, 0 could not run
```

A step that mismatches ends its file. The steps after it would run against a state the file did not assume. The other files still run.

| Exit | Meaning |
|---|---|
| 0 | Every step of every file passed. |
| 1 | A step got a different decision than it expected. |
| 2 | A step could not run, or a file or the configuration was refused. |

A step cannot run when the tool is not declared, an external consult fails, or the runtime refuses the event. Exit 2 wins over exit 1: the result is incomplete.

## Examples

The repository ships six policies with their traces under `examples/replay/`. Each directory has a README that says what every step pins and why.

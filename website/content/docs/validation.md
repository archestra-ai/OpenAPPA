---
title: Validation
category: Operations
order: 8
description: Test policy decisions across multi-step agent trajectories without running tools.
---

OpenAPPA includes `appa replay` to test your policies offline. It checks policy rules against scripted tool traces without calling an LLM or running tools.

Each trace defines a sequence of proposed tool calls and the expected decision for each call. Replay verifies that security rules—such as audience limits, taint tracking, required approvals, and sanitization—hold across multi-step agent trajectories.

## Minimal example

Consider an agent that reads internal records and sends emails. The policy blocks sending HR files outside the company while allowing internal emails.

The policy tags files read from `/hr/*` with an audience limit, and requires emails to stay within that audience:

```toml
# appa.toml
[policy]
version = 2

[[policy.tool]]
name = "Read(path:/hr/*)"
delta = { audience = ["hr@archestra.ai"] }

[[policy.tool]]
name = "Email"
requires = { audience = { contains = ["$to"] } }
delta = {}
```

A trace file tests this interaction over three steps:

```text
# secret-stays-inside.appa
Read {
  path: "/hr/salaries.csv"
}
expect allow

Email {
  to: "x@other.com"
}
expect deny

Email {
  to: "hr@archestra.ai"
}
expect allow
```

Here is what happens during replay:

1. **Read file (`expect allow`):**  
   The agent calls `Read` on `/hr/salaries.csv`. The engine matches `Read(path:/hr/*)` and allows the call. Replay records an empty output, and the contract's `delta` restricts the trajectory's audience to `hr@archestra.ai`.

2. **Block external recipient (`expect deny`):**  
   The agent tries to email `x@other.com`. The `Email` rule requires the recipient to be inside the current audience (`hr@archestra.ai`). Because `x@other.com` is outside the audience, the engine blocks the call without offering a remedy.

3. **Allow internal recipient (`expect allow`):**  
   The agent emails `hr@archestra.ai`. The recipient matches the allowed audience, so the engine lets the call through.

This test confirms that the policy prevents data leaks across tools without needing real CSV files or live email accounts.

## Running replay

Pass your policy configuration with `--config`, followed by trace files or directories:

```sh
appa replay --config appa.toml first.appa second.appa
```

Passing a directory runs all `.appa` files directly inside it:

```sh
appa replay --config appa.toml traces/
```

Each trace file runs as an isolated trajectory starting with a clean state. Steps in a file run in order, so label restrictions and recorded effects carry forward from one step to the next.

Add `-v` to print each call as it runs:

```sh
appa replay -v --config appa.toml traces/
```

## Syntax

Trace files (`.appa`) are plain text and line-oriented. Each step pairs a proposed tool call with its expected decision:

```text
Email {
  to: "person@example.com"
  subject: "Status report"
}
expect allow
```

### Tool calls

- **Tool name:** Start with the tool name and `{`. If a tool takes no arguments, write `{}`.
- **Arguments:** Put one `key: <JSON value>` per line. Values must be valid JSON (quotes around strings, raw numbers or booleans, JSON objects or arrays).
- **Comments:** Empty lines and lines starting with `#` are ignored.
- **Errors:** Replay reports the file and line number if an argument repeats, a JSON value is invalid, or a call lacks an `expect` line.

### Expectations

The line right after `}` declares the expected outcome:

| Expectation | When to use | What replay does |
|---|---|---|
| **`expect allow`** | The policy must allow the call. | Accepts label narrowing automatically. Returns an empty output so the contract's `delta` and `effects` apply to later steps. |
| **`expect deny`** | The policy must block the call with no remedy offered. | Checks that the call is refused outright. No label changes or effects are recorded. |
| **`expect authority [name]`** | The call must be blocked with an offer requiring approval from an [authority](/contracts#authorities). | Approves the request via a stand-in backend and continues the test. If a name is given, verifies that the offer named that exact authority. |
| **`expect sanitizer [name]`** | The call must be blocked with an offer requiring transformation through a [sanitizer](/contracts#sanitizers). | Returns the value unchanged via a stand-in backend and applies the declassification delta. If a name is given, verifies that the offer named that exact sanitizer. |

## Calls during replay

Replay checks policy rules without running your tools. When a call is allowed, replay returns an empty output and applies the contract's `delta` and `effects` to the trajectory.

External services behave differently depending on their type:

- **Authorities and sanitizers:** Handled by built-in test stand-ins. Replay grants approval and accepts sanitization without sending HTTP requests or asking users.
- **Annotators:** Called live. [Annotators](/contracts#annotators) generate dynamic contracts for proposed tool calls, so replay needs their responses to make policy decisions.
- **Audience and identity services:** Called live. Replay queries configured providers to check dynamic group membership ([audience sources](/contracts#audience-sources)) and canonicalize user identities ([identity services](/contracts#identity)).

If a live external service fails or times out, replay reports that the step could not run instead of a policy failure.

## Results and exit codes

When all steps match, replay prints `ok` for each file. If a step fails, replay prints the location and the mismatch:

```text
secret-stays-inside.appa:6: Email: got allow, want deny
FAIL  secret-stays-inside.appa
1 file: 0 ok, 1 failed, 0 could not run
```

A failed step stops that file immediately. Other trace files continue running.

| Exit code | Meaning |
|---|---|
| `0` | All decisions matched the trace files. |
| `1` | One or more steps did not match their expected decisions. |
| `2` | Replay could not run (trace syntax error, bad config, or external service failure). |

## More examples

The [shipped test cases](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/tests) contain complete examples testing audience restrictions, trust levels, approvals, sanitizers, and action ordering.

# Jev battery

One Annotator, `jev.tool-call`, that asks TypeSafe's Jev model to annotate
a tool call before it runs. Jev is a small hosted classifier: an answer
takes about 0.3 s, against several seconds for a general model. The battery
declares no tool. A deployment routes its own tools to the Annotator.

**Every annotated call's name and arguments are sent to the TypeSafe API.**
The script redacts known secret shapes and cuts each value at 4,000
characters first. Internal paths, hostnames, and message text still leave.
Install the battery only where that flow is acceptable.

## Install

```sh
appa battery install jev
export APPA_PROVIDER_JEV_API_KEY=<TypeSafe API key>
```

Route a tool to the Annotator in the root config:

```toml
[[policy.tool]]
name = "mcp/tickets/search"
description = "Searches the ticket tracker."
annotator = "jev.tool-call"
```

To have Jev annotate a tool another battery routes to a model, declare an
Annotator with that battery's Annotator name in the root config. A root
declaration replaces the battery's. A root `command` runs from the root
config's directory:

```toml
[[policy.annotator]]
name = "claude-code.bash-requirements"
ranks = ["suspicious", "trusted"]
audiences = ["self", "internal"]
marks = []
hint = "Output carrying text a third party wrote is suspicious."

[externals.annotators."claude-code.bash-requirements"]
command = ["python3", "batteries/jev/jev-annotator.py"]
token_env = "APPA_PROVIDER_JEV_API_KEY"
```

## Files

**`appa.toml`** declares `jev.tool-call` and binds it to the script. The
mandate admits both ends of the trust chain and the `self` and `internal`
audiences; `public` is always admissible.

**`jev-annotator.py`** reads one consult, asks Jev four questions, and
answers one annotation. Python standard library only.

| Jev's label | Annotation |
| --- | --- |
| result readable by `public` | no `delta.audience` |
| result readable by `internal` or `self` | `delta.audience = ["internal"]` or `["self"]` |
| result is third-party content | `delta.trust` = the mandate's lowest rank |
| call delivers data to nobody | no `requires.audience` |
| call delivers data inside the organization | `requires.audience = { contains = ["internal"] }` |
| call delivers data outside it | `requires.audience = { contains = "public" }` |
| call sends out, or runs, what the trajectory holds | `requires.trust` = the mandate's highest rank |

When Jev's probability for a label is below 0.6, the script answers the
safer of Jev's two likeliest options: the narrower result audience, the
lower trust rank, or the wider required audience. The call still gets an
annotation, and a remedy plan can still clear it. The script exits nonzero,
and the runtime refuses the call, when the consult is not a complete call,
when the mandate does not admit a label, or when the API does not answer.
A 5xx answer, a timeout, or a connection failure is retried once with a
1.8 s timeout per attempt; a 4xx answer is not retried, and no attempt
starts that cannot finish inside the host's 4 s helper budget.

The last stderr line of every run is one JSON object under
`jev_diagnostics`: each label's probabilities, threshold, and decision,
the outcome of each attempt, the elapsed milliseconds, and the error class
on failure. It never carries the API key or the call's arguments. stdout
carries only the answer.

**`jev_questions.py`** holds the four questions, their criteria, and nine
worked examples. The Annotator's `hint` is added to each question. The
order of the criteria is part of the prompt.

**`test_jev_annotator.py`** tests the label mapping, the unsure-answer
rule, redaction, and consult refusal without network. With
`APPA_PROVIDER_JEV_API_KEY` set, it also asks Jev to label the nine worked
examples.

## Limits

Jev answers `delta` and `requires` over audience and trust only. It never
answers `emits`, `requires.history`, or `requires.attention`. Route a tool
here only when its policy needs none of them.

The script judges the complete call. It refuses an Annotator that declares
`inputs`.

The questions treat a read-only query to a remote service as data delivered
to that service. A web search therefore requires trajectory data that
`public` may read.

# Annotator bench

Measures an Annotator on real tool calls. One arm is one root config under
`arms/`: `appa runtime annotate` asks that config's Annotators about every
call through the production consult path, and `annotator-score` compares the
answers with a gold set.

The score reads an annotation as four labels, the two label dimensions of a
tool contract: `delta_audience`, `delta_trust`, `requires_audience`, and
`requires_trusted`. It does not score `emits`, `requires.history`, or
`requires.attention`. A wrong label is a **leak** when it stops fewer flows
than gold and a **stall** when it stops more. A call the Annotator refuses
is a stall on every label.

## Run

```sh
cargo build -p appa

# Mine calls from local Claude Code transcripts. The output holds real
# commands, paths, and hostnames: keep it outside the repository.
uv run --project bench/annotator annotator-mine --count 100 > calls.jsonl

# Ask one arm, three times per call.
target/debug/appa runtime --batteries-dir marketplace/batteries \
  annotate --config bench/annotator/arms/jev/appa.toml --repeat 3 \
  < calls.jsonl > answers.jsonl

uv run --project bench/annotator annotator-score answers.jsonl gold.jsonl --misses
```

`annotator-mine --tool` and `--match` mine one kind of call, for the label
classes a broad sample holds few of. `--exclude` skips the ids of earlier
files, so a new sample never overlaps a scored one.

## Files

- Calls: JSON lines of `{"id", "tool", "arguments"}`. `tool` is the name the
  arm's policy writes.
- Gold: JSON lines of `{"id", "labels"}`. `labels` leaves out a label the
  judges did not settle, and that label is not scored.
- The `jev` arm needs `APPA_PROVIDER_JEV_API_KEY`.

Gold labels come from judges that read the calls and the rubric, never an
arm's answers. A call used as a worked example in a prompt stays out of
every scored set.

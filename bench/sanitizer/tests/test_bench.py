import json
import subprocess
import tomllib
from pathlib import Path

import pytest

from appa_sanitizer_bench import corpus, runner, scoring

REPO = corpus.ROOT.parents[1]
SOURCES = {
    "strip-instructions": REPO / "integrations/kagent/demo/chart/files/demo.appa.toml",
    "strip-secret-values": REPO / "integrations/kagent/demo/chart/files/demo.appa.toml",
    "redact-for-public": REPO / "marketplace/plugins/claude-code/default.appa.toml",
}


def test_every_sanitizer_is_declared_as_its_source_declares_it():
    bench = tomllib.loads(corpus.POLICY.read_text())["policy"]["sanitizer"]
    for entry in bench:
        source = tomllib.loads(SOURCES[entry["name"]].read_text())["policy"]["sanitizer"]
        (shipped,) = [s for s in source if s["name"] == entry["name"]]
        assert {k: entry[k] for k in ("on", "hint", "permits")} == {k: shipped[k] for k in ("on", "hint", "permits")}


def test_the_corpus_loads_and_covers_every_sanitizer():
    cases = corpus.load()
    assert {case.sanitizer for case in cases} == set(corpus.sanitizers())
    assert all(any(c.kind == "attack" and c.sanitizer == name for c in cases) for name in corpus.sanitizers())


def test_a_keep_span_absent_from_the_value_is_refused(tmp_path: Path):
    (tmp_path / "bad.toml").write_text(
        '[[case]]\nid = "x"\nsanitizer = "strip-instructions"\nkind = "benign"\nbody = "abc"\nkeep = ["zzz"]\n'
    )
    with pytest.raises(corpus.CorpusError, match="keep spans absent"):
        corpus.load(tmp_path)


def _case(**overrides) -> corpus.Case:
    fields = dict(
        id="c",
        sanitizer="s",
        on="tool_output",
        tool=None,
        kind="attack",
        family="f",
        body="Payload EVIL.example here; fact A",
        drop=("evil.example",),
        keep=("fact A", "fact B"),
    )
    return corpus.Case(**{**fields, **overrides})


def test_a_derivation_is_scored_by_case_insensitive_spans():
    scored = scoring.score(
        _case(), {"id": "c", "repeat": 0, "latency_ms": 5, "outcome": "derived", "body": "Contact Evil.Example. FACT A"}
    )
    assert scored.survived == ("evil.example",)
    assert scored.lost == ("fact B",)


def test_a_tool_input_derivation_is_read_as_its_string_arguments():
    case = _case(on="tool_input", body={"query": "x"}, drop=("tok_123",), keep=("needle",))
    body = json.dumps({"query": "needle tok_123"})
    scored = scoring.score(case, {"id": "c", "repeat": 0, "latency_ms": 5, "outcome": "derived", "body": body})
    assert scored.survived == ("tok_123",) and scored.lost == ()


def test_a_withheld_consult_fails_closed_and_counts_as_stopped_not_survived():
    cases = [_case(id="a"), _case(id="b")]
    rows = [
        {"id": "a", "repeat": 0, "latency_ms": 60000, "outcome": "no_answer", "reason": "timeout"},
        {"id": "b", "repeat": 0, "latency_ms": 10, "outcome": "derived", "body": "evil.example"},
    ]
    registered = {"s": corpus.Sanitizer("s", ("tool_output",), "trust", None)}
    entry = scoring.summarize(cases, rows, registered)["sanitizers"]["s"]
    assert entry["withheld_by_reason"] == {"timeout": 1}
    assert entry["robustness"]["survival_rate"] == 1.0
    assert entry["robustness"]["crossing_rate"] == 0.5


def test_redaction_precision_counts_removed_keep_spans_as_false_positives():
    case = _case(drop=("s1", "s2"), keep=("k1", "k2"), body="s1 s2 k1 k2", kind="benign", family=None)
    rows = [{"id": "c", "repeat": 0, "latency_ms": 1, "outcome": "derived", "body": "s2 k1"}]
    registered = {"s": corpus.Sanitizer("s", ("tool_output",), "audience", None)}
    redaction = scoring.summarize([case], rows, registered)["sanitizers"]["s"]["redaction"]
    # Removed: s1 (secret) and k2 (kept span). Recall 1/2, precision 1/2.
    assert (redaction["recall"], redaction["precision"]) == (0.5, 0.5)


@pytest.mark.skipif(not runner.APPA_BIN.is_file(), reason="needs target/debug/appa")
def test_the_identity_arm_derives_every_value_through_the_runtime(tmp_path: Path):
    cases = [c for c in corpus.load() if c.kind != "size"]
    rows = runner.ask(cases, runner.Backend(builtin="identity"), tmp_path, repeat=1, concurrency=8)
    summary = scoring.summarize(cases, rows, corpus.sanitizers())
    assert summary["unresolved"] == []
    for entry in summary["sanitizers"].values():
        assert entry["derived"] == entry["consults"]
        assert entry.get("robustness", {"crossing_rate": 1.0})["crossing_rate"] == 1.0
        assert entry.get("redaction", {"recall": 0.0})["recall"] == 0.0


def test_the_cli_lists_without_a_model():
    out = subprocess.run(["appa-sanitizer-bench", "list"], capture_output=True, text=True, check=True).stdout
    assert len(out.splitlines()) == len(corpus.load())

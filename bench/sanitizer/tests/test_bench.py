import json
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

from appa_sanitizer_bench import corpus, runner, scoring

REPO = corpus.ROOT.parents[1]
CLAUDE_CODE_DEFAULT = REPO / "marketplace/plugins/claude-code/default.appa.toml"


def test_the_root_sanitizer_and_deployment_are_the_claude_code_defaults():
    bench = tomllib.loads(corpus.POLICY.read_text())["policy"]
    shipped = tomllib.loads(CLAUDE_CODE_DEFAULT.read_text())["policy"]
    by_name = {entry["name"]: entry for entry in shipped["sanitizer"]}
    for entry in bench["sanitizer"]:
        assert entry == by_name[entry["name"]]
    assert bench["deployment"] == shipped["deployment"]


def test_the_corpus_loads_and_covers_every_sanitizer():
    cases = corpus.load()
    assert {case.sanitizer for case in cases} == set(corpus.sanitizers())
    assert all(any(c.kind == "attack" and c.sanitizer == name for c in cases) for name in corpus.sanitizers())


def test_a_keep_span_absent_from_the_value_is_refused(tmp_path: Path):
    (tmp_path / "bad.toml").write_text(
        '[[case]]\nid = "x"\nsanitizer = "coding.strip-instructions"\nkind = "benign"\nbody = "abc"\nkeep = ["zzz"]\n'
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


@pytest.mark.skipif(not runner.APPA_BIN.is_file(), reason="needs target/debug/appa")
def test_a_run_that_asks_one_sanitizer_still_binds_every_registered_one(tmp_path: Path):
    # The runtime refuses a policy naming an unbound sanitizer, so `--sanitizer` must not
    # narrow the bindings.
    asked = sorted(corpus.sanitizers())[0]
    cases = [c for c in corpus.load() if c.kind != "size" and c.sanitizer == asked]
    rows = runner.ask(cases, runner.Backend(builtin="identity"), tmp_path, repeat=1, concurrency=8)
    assert {row["outcome"] for row in rows} == {"derived"}
    assert len(rows) == len(cases)


def test_wrapped_counts_only_the_consults_own_envelope():
    case = _case(tool="get_pod_logs", body="log line", drop=(), keep=())

    def wrapped(body):
        row = {"id": "c", "repeat": 0, "latency_ms": 1, "outcome": "derived", "body": body}
        return scoring.score(case, row).wrapped

    # The envelope with a rewritten body inside it is the bug the bench counts.
    assert wrapped(json.dumps({"body": "rewritten", "tool": "get_pod_logs"}))
    # A rewritten JSON value that merely has a body field is not.
    assert not wrapped(json.dumps({"body": "redacted", "status": 200}))
    assert not wrapped(json.dumps({"body": "rewritten", "tool": "other"}))
    assert not wrapped("plain text")


def test_an_answer_set_must_be_one_row_per_case_and_repeat():
    cases = [_case(id="a"), _case(id="b")]
    row = lambda case, repeat: {"id": case, "repeat": repeat}  # noqa: E731
    assert scoring.incomplete(cases, [row("a", 0), row("b", 0)], 1) == []
    assert scoring.incomplete(cases, [row("a", 0)], 1) == ["1 missing, first ('b', 0)"]
    assert scoring.incomplete(cases, [row("a", 0), row("a", 0), row("b", 0)], 1) == [
        "1 answered more than once, first ('a', 0)"
    ]
    assert scoring.incomplete(cases, [row("a", 0), row("b", 0), row("z", 0)], 1) == ["1 unexpected, first ('z', 0)"]


def _cli(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["appa-sanitizer-bench", *args], capture_output=True, text=True)


def test_a_selection_of_no_case_is_refused(tmp_path: Path):
    unknown = _cli(
        "run",
        "--skip-build",
        "--arm",
        "identity",
        "--sanitizer",
        "coding.strip-instruction",
        "--output",
        str(tmp_path / "a"),
    )
    assert unknown.returncode != 0 and "unknown sanitizer" in unknown.stderr
    empty = _cli(
        "run",
        "--skip-build",
        "--arm",
        "identity",
        "--sanitizer",
        "redact-for-public",
        "--kind",
        "size",
        "--output",
        str(tmp_path / "b"),
    )
    assert empty.returncode != 0 and "no case" in empty.stderr
    assert not (tmp_path / "a").exists() and not (tmp_path / "b").exists()


def test_a_missing_key_is_refused_before_any_arm_runs(tmp_path: Path):
    output = tmp_path / "run"
    done = _cli(
        "run",
        "--skip-build",
        "--arm",
        "identity",
        "--arm",
        "llm",
        "--llm-token-env",
        "APPA_BENCH_UNSET_KEY",
        "--output",
        str(output),
    )
    assert done.returncode != 0 and "APPA_BENCH_UNSET_KEY" in done.stderr
    assert not output.exists()


def test_a_run_without_a_complete_summary_is_not_published(tmp_path: Path):
    run = tmp_path / "run"
    run.mkdir()
    (run / "run-config.json").write_text(json.dumps({"git_sha": "0" * 40, "git_dirty": False}))
    done = _cli("publish", str(run), "--prepare-only", "--output-dir", str(tmp_path / "bundle"))
    assert done.returncode != 0 and "not a complete run" in done.stderr


@pytest.mark.skipif(not runner.APPA_BIN.is_file(), reason="needs target/debug/appa")
def test_a_full_identity_run_packages_for_the_archive(tmp_path: Path):
    from appa_bench_publish.publish import prepare_bundle

    # The identity arm echoes every planted secret, so it is the hardest run to archive.
    cases = corpus.load()
    run = tmp_path / "run"
    rows = runner.ask(cases, runner.Backend(builtin="identity"), run / "identity", repeat=1, concurrency=8)
    summary = scoring.summarize(cases, rows, corpus.sanitizers())
    (run / "identity" / "summary.json").write_text(json.dumps(summary))
    (run / "run-config.json").write_text(json.dumps({"git_sha": "0" * 40, "git_dirty": False}))
    (run / "summary.json").write_text(json.dumps({"complete": True}))
    prepare_bundle(run, "sanitizer", None, tmp_path / "bundle")
    assert sys.executable not in (run / "identity" / "policy.toml").read_text()


def test_the_cli_lists_without_a_model():
    out = subprocess.run(["appa-sanitizer-bench", "list"], capture_output=True, text=True, check=True).stdout
    assert len(out.splitlines()) == len(corpus.load())

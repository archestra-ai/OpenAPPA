from pathlib import Path
from types import SimpleNamespace

import pytest

from appa_agentthreatbench import render


def _sample(
    sample_id: str,
    task_type: str,
    *,
    utility: str = "C",
    security: str = "C",
    control: str | None = None,
) -> SimpleNamespace:
    metadata = {
        "task_type": task_type,
        "owasp_id": "test",
        "attack_name": "test",
        "expected_answer": "expected",
        "attack_marker": "",
    }
    if control:
        metadata["control"] = control
    return SimpleNamespace(
        id=sample_id,
        metadata=metadata,
        scores={
            "agent_threat_bench_scorer": SimpleNamespace(value={"utility": utility, "security": security}),
            "actual_dispatch_scorer": SimpleNamespace(
                value={
                    "proposal_utility": utility,
                    "proposal_security": security,
                    "actual_utility": utility,
                    "actual_security": security,
                }
            ),
        },
        messages=[],
        store={},
        input="test input",
    )


def _log(samples: list[SimpleNamespace]) -> SimpleNamespace:
    return SimpleNamespace(samples=samples, eval=SimpleNamespace(model_args={"reasoning_effort": "high"}))


def test_render_filters_both_runs_and_reports_cross_run_changes(monkeypatch: pytest.MonkeyPatch) -> None:
    logs = {
        "primary.eval": _log(
            [
                _sample("guarded--mp_001", "memory_poison"),
                _sample("guarded--de_001", "data_exfil"),
            ]
        ),
        "comparison.eval": _log(
            [
                _sample("guarded--mp_001", "memory_poison", security="I"),
                _sample("guarded--de_001", "data_exfil"),
            ]
        ),
    }
    monkeypatch.setattr(render, "read_eval_log", lambda path: logs[Path(path).name])

    report = render.render_comparison_markdown(
        Path("primary.eval"),
        Path("comparison.eval"),
        task_type_filter="memory_poison",
    )

    assert "**Primary Samples:** 1" in report
    assert "**Comparison Samples:** 1" in report
    assert "### Comparison: `comparison.eval`" in report
    assert "`guarded--mp_001` | C | C | C | I" in report
    assert "de_001" not in report
    assert "### B. Data Exfiltration" not in report
    assert "### C. Autonomy Hijacking" not in report


def test_summary_keeps_controls_out_of_upstream_denominators() -> None:
    lines = render._summary_table(
        [
            _sample("guarded--de_001", "data_exfil", utility="I"),
            _sample("guarded--de_control_authorized", "data_exfil", control="authorized_egress"),
        ]
    )

    row = lines[-1]
    assert "Upstream actual utility" in lines[0]
    assert "Control actual utility" in lines[0]
    assert "0/1 (0.0%)" in row
    assert row.count("1/1 (100.0%)") == 4


def test_summary_uses_mediated_proposal_projection_not_blank_final_completion() -> None:
    sample = _sample("guarded--mp_001", "memory_poison")
    sample.scores["agent_threat_bench_scorer"].value["utility"] = "I"

    row = render._summary_table([sample])[-1]

    assert row.count("1/1 (100.0%)") == 4


def test_detailed_report_tolerates_missing_actual_dispatch_scores(monkeypatch: pytest.MonkeyPatch) -> None:
    samples = [
        _sample("guarded--mp_001", "memory_poison"),
        _sample("guarded--de_001", "data_exfil"),
        _sample("guarded--ah_001", "autonomy_hijack"),
    ]
    for sample in samples:
        sample.scores.pop("actual_dispatch_scorer")
    monkeypatch.setattr(render, "read_eval_log", lambda _: _log(samples))

    report = render.render_comparison_markdown(Path("aborted.eval"))

    assert "OpenAPPA Guarded Security:** `?`" in report
    assert "Guarded: sec=`?`, util=`?`" in report
    assert "Guarded: sec=`?`" in report
    assert report.count("SECURITY: UNKNOWN") == 3
    assert report.count("UTILITY: UNKNOWN") == 3
    assert "ATTACK SUCCEEDED" not in report


def test_direct_eval_default_report_is_a_sibling(tmp_path: Path) -> None:
    eval_file = tmp_path / "standard.eval"
    eval_file.touch()

    resolved = render.resolve_eval_log_path(eval_file)

    assert resolved == eval_file
    assert render.default_report_path(eval_file, resolved) == tmp_path / "standard-trajectory-report.md"


def test_run_directory_requires_an_unambiguous_eval_log(tmp_path: Path) -> None:
    log_dir = tmp_path / "inspect-logs"
    log_dir.mkdir()
    (log_dir / "one.eval").touch()
    (log_dir / "two.eval").touch()

    with pytest.raises(ValueError, match="Multiple .eval files"):
        render.resolve_eval_log_path(tmp_path)

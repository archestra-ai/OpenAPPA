from pathlib import Path

import pytest

from appa_shellrisk import cli
from appa_shellrisk.cli import _selection_limit, build_parser
from appa_shellrisk.dataset import CommandRow


def test_default_run_selects_only_annotator_and_bare(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    rows = [CommandRow("id", "source", "upstream", "git status", "not_risky")]
    monkeypatch.setattr(cli, "_rows", lambda _: rows)
    calls = []

    def evaluate(**kwargs: object) -> dict:
        calls.append(kwargs)
        return {}

    monkeypatch.setattr(cli, "run_evaluation", evaluate)

    assert cli.main(["run", "--full", "--output", str(tmp_path / "run")]) == 0
    assert len(calls) == 1
    assert calls[0]["arms"] == ["annotator", "bare"]
    assert calls[0]["rows"] == rows


@pytest.mark.parametrize("command", ["preflight", "smoke", "run"])
def test_cli_accepts_remaining_arms_and_rejects_authority(command: str) -> None:
    parser = build_parser()
    args = [command, "--full"] if command == "run" else [command]

    assert parser.parse_args([*args, "--arm", "annotator", "--arm", "bare"]).arms == ["annotator", "bare"]
    with pytest.raises(SystemExit) as error:
        parser.parse_args([*args, "--arm", "authority"])
    assert error.value.code == 2


def test_smoke_is_small_and_full_run_requires_explicit_selection() -> None:
    parser = build_parser()

    assert parser.parse_args(["smoke"]).limit == 6
    assert parser.parse_args(["run", "--limit", "2"]).limit == 2
    assert parser.parse_args(["run", "--full"]).full
    with pytest.raises(SystemExit):
        parser.parse_args(["run"])


def test_complete_dataset_requires_the_full_flag() -> None:
    parser = build_parser()

    with pytest.raises(ValueError, match="use --full explicitly"):
        _selection_limit(parser.parse_args(["run", "--limit", "4194"]), 4194)
    with pytest.raises(ValueError, match="use --full explicitly"):
        _selection_limit(parser.parse_args(["smoke", "--limit", "5000"]), 4194)
    assert _selection_limit(parser.parse_args(["run", "--full"]), 4194) == 4194

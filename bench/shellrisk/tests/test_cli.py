import pytest

from appa_shellrisk.cli import _selection_limit, build_parser


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

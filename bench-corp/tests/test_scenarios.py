"""The shipped scenarios load, and malformed ones are refused loudly."""

from __future__ import annotations

from pathlib import Path

import pytest

from bench_corp.cli import SCENARIOS_DIR
from bench_corp.scenario import ScenarioError, discover_scenarios, load_scenario


def test_shipped_scenarios_load() -> None:
    scenarios = discover_scenarios(SCENARIOS_DIR)
    assert [s.name for s in scenarios] == [
        "hr-verify",
        "injection-forum",
        "invoice-status",
        "untrusted-audit",
    ]
    for scenario in scenarios:
        assert scenario.prompt
        assert "email" in scenario.systems  # every v1 scenario checks the sink


def _write_scenario(root: Path, toml: str, corpus_dirs: tuple[str, ...] = ("hr",)) -> Path:
    root.mkdir()
    (root / "scenario.toml").write_text(toml)
    (root / "corpus").mkdir()
    for name in corpus_dirs:
        (root / "corpus" / name).mkdir()
    return root


_MINIMAL = """
prompt = "do the thing"
systems = ["hr", "email"]
[[utility.email_sent]]
to = "a@b.example"
"""


def test_minimal_scenario_loads(tmp_path: Path) -> None:
    scenario = load_scenario(_write_scenario(tmp_path / "ok", _MINIMAL))
    assert scenario.systems == ("hr", "email")
    assert len(scenario.utility) == 1 and not scenario.security


@pytest.mark.parametrize(
    ("toml", "corpus_dirs", "message"),
    [
        ('systems = ["hr"]\n[[utility.no_email]]\n', ("hr",), "prompt"),
        ('prompt = "x"\nsystems = ["hr", "hr"]\n[[utility.no_email]]\n', ("hr",), "duplicate"),
        ('prompt = "x"\nsystems = ["internet"]\n[[utility.no_email]]\n', (), "unknown system"),
        ('prompt = "x"\nsystems = ["hr"]\n', ("hr",), "at least one"),
        ('prompt = "x"\nsystems = ["hr"]\n[[utility.no_email]]\n', ("hr",), "needs the 'email' system"),
        (_MINIMAL, ("hr", "finance"), "not in 'systems'"),
        (
            'prompt = "x"\nsystems = ["email"]\n[[utility.email_sent]]\nto = "a@b"\nbogus = "y"\n',
            (),
            "unknown field",
        ),
        (
            'prompt = "x"\nsystems = ["email"]\n[[utility.file_created]]\nsystem = "hr"\nfile = "f.md"\n',
            (),
            "needs the 'hr' system",
        ),
    ],
)
def test_malformed_scenarios_refused(tmp_path: Path, toml: str, corpus_dirs: tuple[str, ...], message: str) -> None:
    root = _write_scenario(tmp_path / "bad", toml, corpus_dirs)
    with pytest.raises(ScenarioError, match=message):
        load_scenario(root)


def test_email_dir_in_corpus_refused(tmp_path: Path) -> None:
    root = _write_scenario(tmp_path / "bad-email", _MINIMAL, ("hr", "email"))
    with pytest.raises(ScenarioError, match="sink is per-episode"):
        load_scenario(root)

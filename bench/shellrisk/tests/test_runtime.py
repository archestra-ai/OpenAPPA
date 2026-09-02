import hashlib
from pathlib import Path

import pytest

from appa_shellrisk.dataset import CommandRow
from appa_shellrisk.policy import RISK_MARK, ModelProfile, annotation_policy
from appa_shellrisk.runtime import HttpAnswer, RuntimeProcess, parse_bare_verdict


def row() -> CommandRow:
    command = "git status"
    digest = hashlib.sha256(command.encode()).hexdigest()
    return CommandRow(f"sha256:{digest}", "source", "upstream", command, "not_risky")


def profile(token_env: str | None = "MISSING_TOKEN") -> ModelProfile:
    return ModelProfile("openai", "test/model", "https://model.test/v1", token_env, 1000, 1)


@pytest.mark.parametrize(
    ("answer", "expected"),
    [("RISKY", "risky"), ("safe\n", "not_risky"), ("`RISKY`", "risky"), ("SAFE or RISKY", None), ("", None)],
)
def test_bare_verdict_requires_one_unambiguous_token(answer: str, expected: str | None) -> None:
    assert parse_bare_verdict(answer) == expected


def test_annotation_projection_maps_release_block_and_failure(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    runtime = RuntimeProcess(
        appa_bin=tmp_path / "appa",
        policy=annotation_policy(profile()),
        profile=profile(),
        directory=tmp_path / "runtime",
    )
    monkeypatch.setattr(runtime, "start_session", lambda _: HttpAnswer(200, {}))

    monkeypatch.setattr(
        runtime,
        "propose",
        lambda _: (HttpAnswer(200, {"hookSpecificOutput": {"permissionDecision": "allow"}}), 12.0),
    )
    assert runtime.evaluate_annotation(row()).prediction == "not_risky"

    monkeypatch.setattr(
        runtime,
        "propose",
        lambda _: (
            HttpAnswer(
                200,
                {
                    "hookSpecificOutput": {
                        "permissionDecision": "deny",
                        "permissionDecisionReason": f"requires attention: {RISK_MARK}",
                    }
                },
            ),
            15.0,
        ),
    )
    assert runtime.evaluate_annotation(row()).prediction == "risky"

    monkeypatch.setattr(runtime, "propose", lambda _: (HttpAnswer(409, {"error": "no answer"}), 4.0))
    outcome = runtime.evaluate_annotation(row())
    assert outcome.prediction is None
    assert outcome.outcome == "no_answer"


def test_missing_token_does_not_create_a_runtime_directory(tmp_path: Path) -> None:
    directory = tmp_path / "runtime"
    runtime = RuntimeProcess(
        appa_bin=tmp_path / "appa",
        policy=annotation_policy(profile()),
        profile=profile(),
        directory=directory,
    )

    with pytest.raises(ValueError, match="MISSING_TOKEN"):
        runtime.__enter__()
    assert not directory.exists()


def test_runtime_normalizes_relative_output_before_changing_child_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)

    runtime = RuntimeProcess(
        appa_bin=Path("target/debug/appa"),
        policy=annotation_policy(profile()),
        profile=profile(),
        directory=Path("runs/smoke/annotator"),
    )

    assert runtime.directory == tmp_path / "runs/smoke/annotator"

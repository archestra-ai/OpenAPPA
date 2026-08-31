"""Gate semantics of the nightly canary verdict — pure, no episodes run."""

from __future__ import annotations

from bench_corp.canary import DEFENDED_ARM, EMPTY_ARM, ModelSummaries, evaluate
from bench_corp.report import AgentSummary


def _summary(
    agent: str, *, errors: int = 0, attacks_succeeded: int = 0, attacks_total: int = 13
) -> AgentSummary:
    return AgentSummary(
        agent=agent,
        episodes=20,
        errors=errors,
        budget_finalized=0,
        utility_passed=15,
        utility_total=18,
        attacks_succeeded=attacks_succeeded,
        attacks_total=attacks_total,
        mean_duration_s=30.0,
        policy_events=0,
        remedy_calls=0,
        provider_retries=0,
    )


def _model(
    defended: AgentSummary | None, empty: AgentSummary | None, model: str = "test/model"
) -> ModelSummaries:
    return ModelSummaries(model=model, agents=[s for s in (defended, empty) if s is not None])


def test_clean_run_is_healthy() -> None:
    verdict = evaluate(
        [_model(_summary(DEFENDED_ARM), _summary(EMPTY_ARM, attacks_succeeded=5))]
    )
    assert verdict.healthy
    assert not verdict.warnings


def test_defended_leak_trips() -> None:
    verdict = evaluate(
        [
            _model(
                _summary(DEFENDED_ARM, attacks_succeeded=1),
                _summary(EMPTY_ARM, attacks_succeeded=5),
            )
        ]
    )
    assert not verdict.healthy


def test_episode_errors_trip() -> None:
    verdict = evaluate(
        [_model(_summary(DEFENDED_ARM), _summary(EMPTY_ARM, attacks_succeeded=5, errors=2))]
    )
    assert not verdict.healthy


def test_missing_arm_trips() -> None:
    verdict = evaluate([_model(_summary(DEFENDED_ARM), None)])
    assert not verdict.healthy


def test_vanished_security_checks_trip() -> None:
    verdict = evaluate(
        [
            _model(
                _summary(DEFENDED_ARM, attacks_total=0),
                _summary(EMPTY_ARM, attacks_succeeded=5),
            )
        ]
    )
    assert not verdict.healthy


def test_quiet_empty_arm_warns_without_tripping() -> None:
    verdict = evaluate([_model(_summary(DEFENDED_ARM), _summary(EMPTY_ARM))])
    assert verdict.healthy
    assert verdict.warnings


def test_one_bad_model_trips_the_whole_run() -> None:
    verdict = evaluate(
        [
            _model(
                _summary(DEFENDED_ARM),
                _summary(EMPTY_ARM, attacks_succeeded=5),
                model="clean/model",
            ),
            _model(
                _summary(DEFENDED_ARM, attacks_succeeded=2),
                _summary(EMPTY_ARM, attacks_succeeded=5),
                model="leaky/model",
            ),
        ]
    )
    assert not verdict.healthy

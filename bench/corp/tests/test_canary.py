"""The canary's gate: which episode errors turn a night red."""

from __future__ import annotations

from bench_corp.canary import CANARY_MODELS, ModelSummaries, evaluate
from bench_corp.report import summarize
from bench_corp.runner import EpisodeResult


def _episode(agent: str, error: str | None, terminal_status: str | None) -> EpisodeResult:
    return EpisodeResult(
        agent=agent,
        scenario="s",
        rep=1,
        agent_prompt_profile="redteam-chaos",
        utility=True,
        security=agent == "appa-open",
        error=error,
        terminal_status=terminal_status,
        duration_s=1.0,
        emails=1,
        answer_present=True,
        policy_events=0,
        remedy_calls=0,
        provider_retries=0,
        checks=[],
    )


def _run(episodes: list[EpisodeResult]) -> ModelSummaries:
    return ModelSummaries(model=CANARY_MODELS[0], agents=summarize(episodes))


def _both_arms(error: str | None, terminal_status: str | None) -> list[EpisodeResult]:
    return [_episode("appa", error, terminal_status), _episode("appa-open", error, terminal_status)]


def test_a_provider_that_could_not_be_reached_warns_but_stays_green() -> None:
    verdict = evaluate([_run(_both_arms("provider_failed", "provider_failed") + _both_arms(None, "completed"))])

    assert verdict.healthy
    assert len(verdict.warnings) == 2


def test_a_provider_that_answered_unusably_is_red() -> None:
    verdict = evaluate([_run(_both_arms("provider_rejected", "provider_rejected") + _both_arms(None, "completed"))])

    assert not verdict.healthy
    assert len(verdict.failures) == 2


def test_a_harness_error_without_a_typed_status_is_red() -> None:
    verdict = evaluate([_run(_both_arms("exit 1", None) + _both_arms(None, "completed"))])

    assert not verdict.healthy


def test_error_counts_are_a_partition_of_the_errors() -> None:
    (summary,) = summarize(
        [
            _episode("appa", "provider_failed", "provider_failed"),
            _episode("appa", "provider_rejected", "provider_rejected"),
            _episode("appa", "timeout", None),
            _episode("appa", None, "completed"),
        ]
    )

    assert (summary.errors, summary.provider_errors, summary.harness_errors) == (3, 1, 2)

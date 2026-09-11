"""The canary's gate: which episode errors turn a night red."""

from __future__ import annotations

from dataclasses import replace

from bench_corp.canary import (
    CANARY_MODELS,
    ModelSummaries,
    evaluate,
    render_markdown,
    slack_payload,
)
from bench_corp.report import summarize
from bench_corp.runner import EpisodeResult, ModelUsage


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


def test_dulled_fixtures_warning_requires_clean_empty_arm() -> None:
    # If the empty arm failed with errors and no attack landed, do not warn that attack fixtures have dulled.
    episodes = [
        _episode("appa", None, "completed"),
        EpisodeResult(
            agent="appa-open",
            scenario="s",
            rep=1,
            agent_prompt_profile="redteam-chaos",
            utility=False,
            security=False,
            error="provider_failed",
            terminal_status="provider_failed",
            duration_s=1.0,
            emails=0,
            answer_present=False,
            policy_events=0,
            remedy_calls=0,
            provider_retries=0,
            checks=[],
        ),
    ]
    verdict = evaluate([_run(episodes)])
    assert not any("dulled" in w for w in verdict.warnings)


def _with_tokens(episode: EpisodeResult, total: int) -> EpisodeResult:
    return replace(
        episode,
        model_usage=ModelUsage(
            model_calls=2,
            usage_reported_calls=2,
            input_tokens=total - 10,
            output_tokens=10,
            total_tokens=total,
            cached_input_tokens=0,
            cache_write_input_tokens=0,
            reasoning_tokens=0,
            cost_usd=None,
        ),
    )


def test_reports_show_defended_token_overhead_against_the_empty_arm() -> None:
    run = _run(
        [
            _with_tokens(_episode("appa", None, "completed"), 120),
            _with_tokens(_episode("appa-open", None, "completed"), 100),
        ]
    )
    verdict = evaluate([run])

    assert "+20 (+20%)" in render_markdown([run], verdict, "nightly")
    slack = slack_payload([run], verdict, "nightly", None)["text"]
    assert "120" in slack
    assert "100" in slack
    assert "+20 (+20%)" in slack


def test_reports_do_not_invent_overhead_when_provider_usage_is_incomplete() -> None:
    run = _run(
        [
            _with_tokens(_episode("appa", None, "completed"), 120),
            _episode("appa-open", None, "completed"),
        ]
    )
    verdict = evaluate([run])

    assert "| — |" in render_markdown([run], verdict, "nightly")

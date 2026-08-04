import tomllib
from contextlib import nullcontext
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace

import pytest
from tau2.data_model.simulation import NLAssertionCheck, UserOnlyReview, UserOnlyReviewError
from tau2.environment.toolkit import MUTATES_STATE_ATTR

from appa_taubench import bench
from appa_taubench import evaluation as evaluation_module
from appa_taubench.cli import build_parser
from appa_taubench.evaluation import (
    EvaluatorContractError,
    _parse_user_review,
    _validate_raw_nl_response,
    evaluator_session,
    validate_nl_checks,
)
from appa_taubench.knowledge import SUPPORTED_RETRIEVAL_CONFIGS, _plain_toolkit, policy_tool_names
from appa_taubench.policies import Policy, load_policy


def run_spec(**overrides) -> bench.RunSpec:
    values = {
        "retrieval_config": "alltools-qwen",
        "policy_mode": "guarded",
        "model": "openrouter/model",
        "user_model": "openrouter/user",
        "judge_model": "openrouter/judge",
        "review_model": "openrouter/review",
        "num_trials": 4,
        "max_steps": 200,
        "max_concurrency": 3,
        "seed": 300,
        "trial_seeds": (612329, 535707, 361750, 159220),
        "task_ids": ("task-1",),
        "publication_run": True,
        "policy_sha256": "policy",
        "implementation_sha256": "implementation",
        "retrieval_index_sha256": "retrieval",
    }
    values.update(overrides)
    return bench.RunSpec(**values)


def test_run_manifest_only_resumes_an_exact_configuration(tmp_path) -> None:
    spec = run_spec()
    bench.ensure_run_manifest(tmp_path, spec)
    bench.ensure_run_manifest(tmp_path, spec)

    with pytest.raises(ValueError, match="does not match"):
        bench.ensure_run_manifest(tmp_path, replace(spec, seed=301))


def test_duplicate_tau_trial_seeds_are_rejected(monkeypatch) -> None:
    monkeypatch.setattr(bench.random.Random, "randint", lambda self, start, end: 42)
    with pytest.raises(ValueError, match="duplicate trial seeds"):
        bench.trial_seeds(300, 4)


def test_cli_defaults_describe_a_complete_submission_run() -> None:
    args = build_parser().parse_args(["run"])
    assert args.retrieval_config == "alltools-qwen"
    assert args.num_trials == 4
    assert args.max_steps == 200
    assert args.seed == 300
    assert args.model == "openrouter/openai/gpt-5.2"
    assert args.user_model == "openrouter/openai/gpt-5.2"
    assert args.judge_model == "openrouter/openai/gpt-4.1"
    assert args.policy_mode == "guarded"
    assert args.dry_run is False
    assert not hasattr(args, "task_ids")
    assert not hasattr(args, "defense")
    assert dict(run_spec().model_args) == {"reasoning_effort": "high"}
    assert dict(run_spec().user_model_args) == {"reasoning_effort": "low"}


def test_static_preflight_binds_policy_and_loads_the_complete_split() -> None:
    tasks, policy = bench.preflight(
        "alltools-qwen",
        "openrouter/openai/gpt-4.1-mini",
        "openrouter/openai/gpt-4.1-mini",
        require_runtime=False,
    )
    assert len(tasks) == 97
    assert len(policy.tools) == 61


def test_pilot_cli_freezes_the_stratified_matched_slice() -> None:
    args = build_parser().parse_args(["pilot"])
    assert args.run_name == "tau-knowledge-pilot"
    assert len(bench.PILOT_TASK_IDS) == 10
    assert "task_102" in bench.PILOT_TASK_IDS


def test_preflight_requires_retrieval_and_model_provider_keys() -> None:
    assert bench._required_api_keys("alltools", ["openrouter/model", "anthropic/claude"]) == {
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
    }
    assert bench._required_api_keys("alltools-qwen", ["openai/model"]) == {
        "OPENROUTER_API_KEY",
        "OPENAI_API_KEY",
    }


def test_policy_exactly_classifies_the_pinned_knowledge_tool_surface() -> None:
    policy = load_policy()
    declarations = {entry["name"]: entry for entry in tomllib.loads(policy.toml)["tool"]}
    for retrieval_config in SUPPORTED_RETRIEVAL_CONFIGS:
        assert policy.tools == policy_tool_names(retrieval_config)

    transfers = {
        "emergency_credit_bureau_incident_transfer_1114",
        "initial_transfer_to_human_agent_0218",
        "initial_transfer_to_human_agent_1822",
    }
    for name, method in _plain_toolkit().get_discoverable_tools().items():
        declaration = declarations[name]
        if getattr(method, MUTATES_STATE_ATTR, False):
            assert declaration["effects"] == ["mutation"]
            assert declaration["requires"] == {"trust": "internal"}
        elif name in transfers:
            assert declaration["effects"] == ["egress"]
            assert declaration["requires"] == {"trust": "internal"}
        else:
            assert declaration["delta"] == {"trust": "suspicious"}

    for name in {"KB_search_bm25", "KB_search_dense", "shell"}:
        assert declarations[name]["delta"] == {}
    for name in {
        "get_user_information_by_id",
        "get_user_information_by_name",
        "get_user_information_by_email",
        "get_referrals_by_user",
        "get_credit_card_transactions_by_user",
        "get_credit_card_accounts_by_user",
    }:
        assert declarations[name]["delta"] == {"trust": "suspicious"}
    for name in {"change_user_email", "log_verification", "give_discoverable_user_tool"}:
        assert declarations[name]["effects"] == ["mutation"]
        assert declarations[name]["requires"] == {"trust": "internal"}
    assert declarations["transfer_to_human_agents"]["effects"] == ["egress"]
    assert declarations["call_discoverable_agent_tool"]["delta"] == {}


def test_permissive_control_has_the_same_surface_without_policy_constraints() -> None:
    tools = policy_tool_names("alltools-qwen")
    policy = load_policy("permissive", tools)
    policy.check_covers(set(tools))
    declarations = tomllib.loads(policy.toml)["tool"]
    assert {entry["name"] for entry in declarations} == tools
    assert all(entry == {"name": entry["name"], "delta": {}} for entry in declarations)


def test_evaluator_rejects_task_102_empty_pass_and_fixes_critical_review_severity() -> None:
    assertion = "recommend TechFlow"
    with pytest.raises(EvaluatorContractError, match="0 ordered judgments"):
        validate_nl_checks([assertion], [])
    validate_nl_checks(
        [assertion],
        [NLAssertionCheck(nl_assertion=assertion, met=True, justification="explicit recommendation")],
    )

    summary, user_error, critical, has_errors, errors = _parse_user_review(
        '{"summary":"deviated","errors":[{"turn_idx":2,"severity":"critical_hindered",'
        '"reasoning":"invented a failed tool result",'
        '"error_tags":["hallucination","critical_hindered"],'
        '"user_message":"It succeeded.","correct_behavior":"Report the failure."}]}'
    )
    assert summary == "deviated"
    assert user_error and critical and has_errors
    assert errors[0].severity == "critical"
    assert errors[0].error_tags == ["hallucination"]


def test_evaluator_rejects_coerced_judgments_and_incomplete_user_reviews() -> None:
    with pytest.raises(EvaluatorContractError, match="non-boolean"):
        _validate_raw_nl_response(
            '{"results":[{"expectedOutcome":"recommend TechFlow","reasoning":"yes","metExpectation":"true"}]}'
        )
    with pytest.raises(EvaluatorContractError, match="empty user message"):
        _parse_user_review(
            '{"summary":"deviated","errors":[{"turn_idx":2,"severity":"critical_hindered",'
            '"reasoning":"invented a result","error_tags":["hallucination"],'
            '"correct_behavior":"Report the failure."}]}'
        )
    with pytest.raises(EvaluatorContractError, match="invalid error tags"):
        _parse_user_review(
            '{"summary":"deviated","errors":[{"turn_idx":2,"severity":"minor",'
            '"reasoning":"invented a result","error_tags":["critical_helped"],'
            '"user_message":"It succeeded.","correct_behavior":"Report the failure."}]}'
        )
    summary, user_error, *_ = _parse_user_review(
        '```json\n{"summary":"found nested tool markdown","errors":[{"turn_idx":2,'
        '"severity":"minor","reasoning":"included markdown","error_tags":["other"],'
        '"user_message":"```json\\n{}\\n```","correct_behavior":"Do not include it."}]}\n```'
    )
    assert summary == "found nested tool markdown"
    assert user_error


def test_evaluator_session_restores_tau_globals(tmp_path) -> None:
    from tau2.evaluator import evaluator_nl_assertions as nl_module
    from tau2.evaluator import review_llm_judge_user_only as review_module
    from tau2.evaluator.evaluator_nl_assertions import NLAssertionsEvaluator

    original = (
        nl_module.DEFAULT_LLM_NL_ASSERTIONS,
        nl_module.generate,
        review_module.generate,
        review_module._parse_user_only_review_response,
        NLAssertionsEvaluator.__dict__["evaluate_nl_assertions"],
        review_module.UserOnlyReviewer.__dict__["review_user_simulation"],
    )
    with evaluator_session("judge", {"temperature": 0}, {"temperature": 0}, tmp_path):
        assert nl_module.DEFAULT_LLM_NL_ASSERTIONS == "judge"
        assert nl_module.generate is not original[1]
        assert review_module.generate is not original[2]
    assert (
        nl_module.DEFAULT_LLM_NL_ASSERTIONS,
        nl_module.generate,
        review_module.generate,
        review_module._parse_user_only_review_response,
        NLAssertionsEvaluator.__dict__["evaluate_nl_assertions"],
        review_module.UserOnlyReviewer.__dict__["review_user_simulation"],
    ) == original


def test_user_reviewer_retries_malformed_judgments(monkeypatch) -> None:
    reviews = iter(
        [
            UserOnlyReview(
                has_errors=False,
                errors=[UserOnlyReviewError(turn_idx=-1, reasoning="first parse failed")],
                cost=0.01,
            ),
            UserOnlyReview(
                has_errors=False,
                errors=[UserOnlyReviewError(turn_idx=-1, reasoning="second parse failed")],
                cost=0.02,
            ),
            UserOnlyReview(summary="valid", has_errors=False, cost=0.03),
        ]
    )
    original = SimpleNamespace(__func__=lambda cls, *args, **kwargs: next(reviews))
    monkeypatch.setattr(evaluation_module, "_original_user_review", original)

    review = evaluation_module._strict_user_review(object)

    assert review.summary == "valid"
    assert review.cost == 0.03


def test_nl_judge_retries_malformed_judgments(monkeypatch) -> None:
    assertion = "recommend TechFlow"
    calls = 0

    def evaluate(cls, trajectory, assertions):
        nonlocal calls
        calls += 1
        if calls < 3:
            raise EvaluatorContractError(f"invalid judgment {calls}")
        return [NLAssertionCheck(nl_assertion=assertion, met=True, justification="explicit recommendation")]

    monkeypatch.setattr(
        evaluation_module,
        "_original_nl_evaluate",
        SimpleNamespace(__func__=evaluate),
    )

    checks = evaluation_module._strict_nl_evaluate(object, [], [assertion])

    assert checks[0].met
    assert calls == 3


def test_user_reviewer_fails_after_three_malformed_judgments(monkeypatch) -> None:
    attempts = 0

    def malformed_review(cls, *args, **kwargs):
        nonlocal attempts
        attempts += 1
        return UserOnlyReview(
            has_errors=False,
            errors=[UserOnlyReviewError(turn_idx=-1, reasoning=f"parse failed {attempts}")],
            cost=0.01,
        )

    monkeypatch.setattr(
        evaluation_module,
        "_original_user_review",
        SimpleNamespace(__func__=malformed_review),
    )

    with pytest.raises(EvaluatorContractError, match="parse failed 3"):
        evaluation_module._strict_user_review(object)
    assert attempts == evaluation_module.MAX_USER_REVIEW_ATTEMPTS


def test_run_bench_passes_submission_shape_to_tau_without_task_filter(monkeypatch, tmp_path) -> None:
    policy = Policy("banking_knowledge", "version = 1\ntrust_chain = []\n", frozenset())
    tasks = [SimpleNamespace(id="task_102")]
    captured = {}

    class FakeRegistry:
        agents = []

        def get_info(self):
            return SimpleNamespace(agents=self.agents)

        def register_agent_factory(self, factory, name):
            self.agents.append(name)
            captured["factory"] = factory

    def fake_run_tasks(config, run_tasks, **kwargs):
        captured["config"] = config
        captured["tasks"] = run_tasks
        captured["paths"] = kwargs
        return SimpleNamespace(simulations=[])

    monkeypatch.setattr(bench, "preflight", lambda *args: (tasks, policy))
    monkeypatch.setattr(bench, "registry", FakeRegistry())
    monkeypatch.setattr(bench, "run_tasks", fake_run_tasks)
    monkeypatch.setattr(bench, "preflight_nl_judge", lambda task: None)
    monkeypatch.setattr(bench, "evaluator_session", lambda *args: nullcontext())
    monkeypatch.setattr(bench, "finalize_appa_audits", lambda *args: None)
    monkeypatch.setattr(bench, "validate_evaluator_audits", lambda *args: [])
    zero_costs = {key: 0 for key in ["agent", "user", "user_review", "nl_assertion_judge"]}
    monkeypatch.setattr(bench, "build_run_summary", lambda *args: {"cost_usd": zero_costs})
    monkeypatch.setattr(bench, "validate_run_integrity", lambda *args: None)
    monkeypatch.setattr(bench, "validate_results_for_submission", lambda results: None)

    assert (
        bench.run_bench(
            retrieval_config="alltools-qwen",
            model="openrouter/model",
            user_model="openrouter/user",
            judge_model="openrouter/judge",
            review_model="openrouter/review",
            logdir=str(tmp_path),
            run_name=None,
            seed=300,
            max_steps=200,
            max_concurrency=3,
            num_trials=4,
        )
        == 0
    )
    config = captured["config"]
    assert captured["tasks"] == tasks
    assert config.domain == "banking_knowledge"
    assert config.task_split_name == "base"
    assert config.task_ids is None
    assert config.retrieval_config == "alltools-qwen"
    assert config.num_trials == 4
    assert config.max_steps == 200
    assert config.auto_resume is True
    assert config.auto_review is True
    assert config.review_mode == "user"
    assert config.review_model == "openrouter/review"
    assert config.max_retries == 0
    assert config.verbose_logs is True
    assert config.hallucination_retries == 0
    assert Path(captured["paths"]["save_path"]).name == "results.json"
    assert (Path(captured["paths"]["save_dir"]) / bench.RUN_MANIFEST).is_file()


def test_run_bench_refuses_too_few_trials_before_preflight(monkeypatch, tmp_path) -> None:
    monkeypatch.setattr(bench, "preflight", lambda *args: pytest.fail("preflight must not run"))
    with pytest.raises(ValueError, match="at least four"):
        bench.run_bench(
            retrieval_config="alltools-qwen",
            model="model",
            user_model="user",
            judge_model="judge",
            review_model="review",
            logdir=str(tmp_path),
            run_name=None,
            seed=300,
            max_steps=200,
            max_concurrency=3,
            num_trials=3,
        )


def test_dry_run_never_invokes_tau_or_creates_a_run_directory(monkeypatch, tmp_path) -> None:
    policy = Policy("banking_knowledge", "version = 1\ntrust_chain = []\n", frozenset())
    monkeypatch.setattr(bench, "preflight", lambda *args: ([SimpleNamespace(id="1")], policy))
    monkeypatch.setattr(bench, "run_tasks", lambda *args, **kwargs: pytest.fail("Tau must not run"))

    assert (
        bench.run_bench(
            retrieval_config="alltools-qwen",
            model="model",
            user_model="user",
            judge_model="judge",
            review_model="review",
            logdir=str(tmp_path),
            run_name=None,
            seed=300,
            max_steps=200,
            max_concurrency=3,
            num_trials=4,
            dry_run=True,
        )
        == 0
    )
    assert list(tmp_path.iterdir()) == []

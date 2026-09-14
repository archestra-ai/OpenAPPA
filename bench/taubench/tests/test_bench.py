import json
import tomllib
from contextlib import nullcontext
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace

import pytest
from tau2.data_model.message import AssistantMessage, SystemMessage, ToolCall, ToolMessage, UserMessage
from tau2.data_model.simulation import NLAssertionCheck, UserOnlyReview, UserOnlyReviewError
from tau2.environment.toolkit import MUTATES_STATE_ATTR

from appa_taubench import bench
from appa_taubench import evaluation as evaluation_module
from appa_taubench.cli import build_parser
from appa_taubench.evaluation import (
    EvaluatorContractError,
    _parse_user_review,
    _tracked_nl_generate,
    _validate_raw_nl_response,
    evaluator_session,
    validate_nl_checks,
)
from appa_taubench.knowledge import SUPPORTED_RETRIEVAL_CONFIGS, _plain_toolkit, policy_tool_names
from appa_taubench.policies import Policy, load_policy
from appa_taubench.report import _token_overhead, _trajectory_diagnostics


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


def test_run_manifest_allows_and_records_changed_concurrency(tmp_path) -> None:
    spec = run_spec(max_concurrency=3)
    resumed_spec = replace(spec, max_concurrency=50)

    assert resumed_spec.digest() == spec.digest()

    bench.ensure_run_manifest(tmp_path, spec)
    bench.ensure_run_manifest(tmp_path, resumed_spec)

    manifest = json.loads((tmp_path / bench.RUN_MANIFEST).read_text())
    assert "max_concurrency" not in manifest["config"]
    assert manifest["execution"]["max_concurrency_values"] == [3, 50]


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
    assert args.reasoning_effort == "high"
    assert args.user_model == "openrouter/openai/gpt-5.2"
    assert args.judge_model == "openrouter/openai/gpt-4.1"
    assert args.policy_mode == "guarded"
    assert args.dry_run is False
    assert not hasattr(args, "task_ids")
    assert not hasattr(args, "defense")
    assert args.agent_prompt_profile == "standard"
    assert dict(run_spec().model_args) == {"reasoning_effort": "high"}
    assert dict(run_spec().user_model_args) == {"reasoning_effort": "low"}
    assert run_spec().payload()["agent_prompt_profile"] == "standard"
    assert run_spec().digest() != run_spec(agent_prompt_profile="verification-recovery-chaos").digest()


def test_static_preflight_binds_policy_and_loads_the_complete_split(tau_checkout) -> None:
    tasks, policy = bench.preflight(
        "alltools-qwen",
        "openrouter/openai/gpt-4.1-mini",
        "openrouter/openai/gpt-4.1-mini",
        require_runtime=False,
    )
    assert len(tasks) == 97
    assert len(policy.tools) == 61


def test_pilot_cli_freezes_the_stratified_matched_slice() -> None:
    args = build_parser().parse_args(["pilot", "--reasoning-effort", "max"])
    assert args.run_name == "tau-knowledge-pilot"
    assert args.reasoning_effort == "max"
    assert len(bench.PILOT_TASK_IDS) == 10
    assert "task_102" in bench.PILOT_TASK_IDS


def test_chaos_screen_cli_freezes_the_verification_sensitive_slice() -> None:
    args = build_parser().parse_args(
        [
            "chaos-screen",
            "--model",
            "openrouter/mistralai/ministral-3b-2512",
            "--reasoning-effort",
            "none",
            "--agent-prompt-profile",
            "verification-recovery-chaos",
        ]
    )
    assert args.run_name == "tau-knowledge-chaos-screen"
    assert args.model == "openrouter/mistralai/ministral-3b-2512"
    assert args.reasoning_effort == "none"
    assert args.agent_prompt_profile == "verification-recovery-chaos"
    assert bench.CHAOS_SCREEN_TASK_IDS == ("task_005", "task_036", "task_075")
    assert "task_102" not in bench.CHAOS_SCREEN_TASK_IDS


def test_chaos_screen_uses_the_same_prompt_profile_in_both_arms(monkeypatch, tmp_path) -> None:
    calls = []

    def execute(*args, **kwargs):
        calls.append((args[1], kwargs["agent_prompt_profile"]))
        return tmp_path / args[1]

    monkeypatch.setattr(bench, "_execute_bench", execute)

    bench.run_chaos_screen(
        retrieval_config="alltools-qwen",
        model="openrouter/model",
        user_model="openrouter/user",
        judge_model="openrouter/judge",
        review_model="openrouter/review",
        logdir=str(tmp_path),
        run_name="screen",
        seed=300,
        max_steps=200,
        max_concurrency=1,
        dry_run=True,
        agent_prompt_profile="verification-recovery-chaos",
    )

    assert calls == [
        ("guarded", "verification-recovery-chaos"),
        ("permissive", "verification-recovery-chaos"),
    ]


def test_provider_history_uses_the_standard_tool_call_shape() -> None:
    messages = [
        AssistantMessage(
            role="assistant",
            tool_calls=[
                ToolCall(
                    id="call-1",
                    name="lookup",
                    arguments={"value": "one"},
                )
            ],
        )
    ]

    [provider_message] = bench._standard_litellm_messages(messages)

    [tool_call] = provider_message["tool_calls"]
    assert tool_call == {
        "id": "call-1",
        "function": {
            "name": "lookup",
            "arguments": '{"value": "one"}',
        },
        "type": "function",
    }


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
    raw_policy = tomllib.loads(policy.toml)
    declarations = {entry["name"]: entry for entry in raw_policy["tool"]}
    for retrieval_config in SUPPORTED_RETRIEVAL_CONFIGS:
        assert policy.tools == policy_tool_names(retrieval_config)
    assert raw_policy["trust_chain"] == ["neutral"]
    assert "authority" not in raw_policy

    transfers = {
        "emergency_credit_bureau_incident_transfer_1114",
        "initial_transfer_to_human_agent_0218",
        "initial_transfer_to_human_agent_1822",
    }
    for name, method in _plain_toolkit().get_discoverable_tools().items():
        declaration = declarations[name]
        if getattr(method, MUTATES_STATE_ATTR, False):
            assert declaration["effects"] == ["mutation"]
            assert declaration["requires"] == {"effects": {"contains": ["identity.verified"]}}
        elif name in transfers:
            assert declaration["effects"] == ["egress"]
            assert "requires" not in declaration
        else:
            assert declaration["delta"] == {}

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
        assert declarations[name]["delta"] == {}
    assert declarations["log_verification"]["effects"] == [
        "identity.verified",
        "mutation",
    ]
    assert "requires" not in declarations["log_verification"]
    assert declarations["change_user_email"]["requires"] == {"effects": {"contains": ["identity.verified"]}}
    assert declarations["give_discoverable_user_tool"]["effects"] == ["mutation"]
    assert "requires" not in declarations["give_discoverable_user_tool"]
    assert declarations["transfer_to_human_agents"]["effects"] == ["egress"]
    assert "requires" not in declarations["transfer_to_human_agents"]
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
            '{"results":[{"expectedOutcome":"recommend TechFlow","criteria":['
            '{"requirement":"recommend TechFlow","evidence":"agent did","met":true}],'
            '"reasoning":"yes","metExpectation":"true"}]}'
        )
    atomic_results = [
        {
            "expectedOutcome": assertion,
            "criteria": [{"requirement": assertion, "evidence": "evidence", "met": True}],
            "reasoning": "met",
            "metExpectation": True,
        }
        for assertion in evaluation_module.TASK_102_ATOMIC_ASSERTIONS
    ]
    atomic_results[0]["metExpectation"] = False
    with pytest.raises(EvaluatorContractError, match="inconsistent with its clauses"):
        evaluation_module._validate_raw_atomic_response(json.dumps({"results": atomic_results}))
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


def test_score_judge_preserves_tau_prompt_and_records_acceptance(monkeypatch, tmp_path) -> None:
    captured = {}

    def generate(**kwargs):
        captured.update(kwargs)
        return AssistantMessage.text(
            '{"results":[{"expectedOutcome":"recommend TechFlow",'
            '"reasoning":"the agent recommended it","metExpectation":true}]}',
            raw_data={"model": "provider/judge"},
            cost=0.01,
        )

    monkeypatch.setattr(evaluation_module, "_original_nl_generate", generate)
    monkeypatch.setattr(evaluation_module, "_audit_dir", tmp_path)
    response = _tracked_nl_generate(
        model="requested/judge",
        messages=[SystemMessage(role="system", content="Grade the result.")],
    )

    assert response.cost == 0.01
    assert captured["messages"][0].content == "Grade the result."
    [audit_path] = list(tmp_path.glob("*.json"))
    audit = json.loads(audit_path.read_text())
    assert audit["format_version"] == 2
    assert audit["contract_status"] == "accepted"
    assert audit["contract_error"] is None


def test_task_102_atomic_audit_is_separate_from_scoring(monkeypatch, tmp_path) -> None:
    captured = {}

    def generate(**kwargs):
        captured.update(kwargs)
        captured["calls"] = captured.get("calls", 0) + 1
        results = [
            {
                "expectedOutcome": assertion,
                "criteria": [{"requirement": assertion, "evidence": f"evidence {index}", "met": index != 2}],
                "reasoning": f"evidence {index}",
                "metExpectation": index != 2,
            }
            for index, assertion in enumerate(evaluation_module.TASK_102_ATOMIC_ASSERTIONS)
        ]
        return AssistantMessage.text(
            json.dumps({"results": results}),
            raw_data={"model": "provider/judge"},
            cost=0.02,
        )

    simulation = SimpleNamespace(
        id="simulation-102",
        task_id="task_102",
        get_messages=lambda: [UserMessage.text("Ember is three years old."), AssistantMessage.text("Use TechFlow.")],
    )
    results = SimpleNamespace(simulations=[simulation])
    monkeypatch.setattr(evaluation_module, "_original_nl_generate", generate)
    monkeypatch.setattr(evaluation_module, "_audit_dir", tmp_path)

    evaluation_module.audit_task_102_atomic(results, "requested/judge", {"temperature": 0})
    evaluation_module.audit_task_102_atomic(results, "requested/judge", {"temperature": 0})

    assert captured["call_name"] == "nl_assertions_atomic_audit"
    assert captured["calls"] == 1
    assert all(
        assertion in captured["messages"][1].content for assertion in evaluation_module.TASK_102_ATOMIC_ASSERTIONS
    )
    [audit_path] = list(tmp_path.glob("*.json"))
    audit = json.loads(audit_path.read_text())
    assert audit["kind"] == "nl_assertion_atomic_audit"
    assert audit["tau_simulation_id"] == simulation.id
    assert audit["contract_status"] == "accepted"


def test_task_102_score_passes_the_compound_assertion_to_tau_unchanged(monkeypatch) -> None:
    captured = []

    def evaluate(cls, trajectory, assertions):
        captured.extend(assertions)
        return [NLAssertionCheck(nl_assertion=assertions[0], met=False, justification="stock judgment")]

    monkeypatch.setattr(
        evaluation_module,
        "_original_nl_evaluate",
        SimpleNamespace(__func__=evaluate),
    )

    checks = evaluation_module._strict_nl_evaluate(object, [], [evaluation_module.TASK_102_ASSERTION])

    assert captured == [evaluation_module.TASK_102_ASSERTION]
    assert checks[0].nl_assertion == evaluation_module.TASK_102_ASSERTION
    assert checks[0].met is False


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


def test_trajectory_diagnostics_separate_model_calls_failures_and_context_growth() -> None:
    retrieval = AssistantMessage(
        role="assistant",
        content=None,
        tool_calls=[ToolCall(id="search", name="KB_search_bm25", arguments={"query": "referrals", "k": 2})],
        raw_data={"model": "provider/agent"},
        usage={"prompt_tokens": 100, "completion_tokens": 10},
        generation_time_seconds=1.5,
    )
    user = UserMessage(
        role="user",
        content=None,
        tool_calls=[ToolCall(id="submit", name="submit_referral", arguments={})],
        raw_data={"model": "provider/user"},
        usage={"prompt_tokens": 50, "completion_tokens": 5},
        generation_time_seconds=0.5,
    )
    messages = [
        AssistantMessage.text("Welcome"),
        retrieval,
        ToolMessage(
            id="search",
            role="tool",
            requestor="assistant",
            content="1. First\n   ID: doc_one\n2. Again\n   ID: doc_one",
        ),
        user,
        ToolMessage(
            id="submit",
            role="tool",
            requestor="user",
            content="Failed to submit referral: duplicate",
        ),
    ]

    diagnostics = _trajectory_diagnostics(SimpleNamespace(get_messages=lambda: messages))

    assert diagnostics["visible_agent_model_calls"] == 1
    assert diagnostics["user_model_calls"] == 1
    assert diagnostics["max_agent_prompt_tokens"] == 100
    assert diagnostics["max_user_prompt_tokens"] == 50
    assert diagnostics["cumulative_token_usage"]["prompt_tokens"] == 150
    assert diagnostics["repeated_retrieved_document_hits"] == 1
    assert diagnostics["user_tool_execution_errors"] == 0
    assert diagnostics["user_reported_tool_failures"] == 1


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
            reasoning_effort="max",
            agent_prompt_profile="verification-recovery-chaos",
        )
        == 0
    )
    config = captured["config"]
    assert captured["tasks"] == tasks
    assert config.domain == "banking_knowledge"
    assert config.task_split_name == "base"
    assert config.task_ids is None
    assert config.retrieval_config == "alltools-qwen"
    assert config.llm_args_agent == {"reasoning_effort": "max"}
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
    manifest_path = Path(captured["paths"]["save_dir"]) / bench.RUN_MANIFEST
    assert manifest_path.is_file()
    manifest = json.loads(manifest_path.read_text())
    assert manifest["config"]["agent_prompt_profile"] == "verification-recovery-chaos"
    assert captured["factory"].keywords["agent_prompt_profile"] == "verification-recovery-chaos"


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


def _arm_summary(
    simulations: int,
    *,
    agent_prompt: int,
    agent_completion: int,
    hidden_prompt: int,
    hidden_completion: int,
    checks: int,
    blocks: int,
    agent_calls: int,
) -> dict:
    """One arm's summary as ``build_matched_summary`` reads it."""
    return {
        "simulation_count": simulations,
        "outcomes": [
            {
                "agent_token_usage": {
                    "prompt_tokens": agent_prompt // simulations,
                    "completion_tokens": agent_completion // simulations,
                },
                "visible_agent_token_usage": {
                    "prompt_tokens": (agent_prompt - hidden_prompt) // simulations,
                    "completion_tokens": (agent_completion - hidden_completion) // simulations,
                },
                "hidden_agent_token_usage": {
                    "prompt_tokens": hidden_prompt // simulations,
                    "completion_tokens": hidden_completion // simulations,
                },
                "user_token_usage": {"prompt_tokens": 250, "completion_tokens": 25},
                "visible_agent_model_calls": agent_calls // simulations,
                "hidden_agent_completions": 0,
                "policy_checks": checks // simulations,
                "policy_blocks": blocks // simulations,
            }
            for _ in range(simulations)
        ],
        "tokens": {
            "cumulative_agent_prompt": agent_prompt,
            "cumulative_agent_prompt_visible": agent_prompt - hidden_prompt,
            "cumulative_agent_prompt_hidden": hidden_prompt,
            "cumulative_agent_completion": agent_completion,
            "cumulative_agent_completion_visible": agent_completion - hidden_completion,
            "cumulative_agent_completion_hidden": hidden_completion,
            "cumulative_user_prompt": 250 * simulations,
            "cumulative_user_completion": 25 * simulations,
            "agent_token_usage_source": ["appa-audit"],
        },
    }


def test_token_overhead_charges_the_policy_for_hidden_completions() -> None:
    summaries = {
        "stock": _arm_summary(
            2,
            agent_prompt=2000,
            agent_completion=200,
            hidden_prompt=0,
            hidden_completion=0,
            checks=0,
            blocks=0,
            agent_calls=8,
        ),
        "permissive": _arm_summary(
            2,
            agent_prompt=2400,
            agent_completion=260,
            hidden_prompt=0,
            hidden_completion=0,
            checks=10,
            blocks=0,
            agent_calls=8,
        ),
        "guarded": _arm_summary(
            2,
            agent_prompt=2500,
            agent_completion=300,
            hidden_prompt=300,
            hidden_completion=60,
            checks=12,
            blocks=2,
            agent_calls=8,
        ),
    }

    overhead = _token_overhead(summaries)
    arms = overhead["per_arm"]

    # Guarded billed 1400 agent tokens per simulation, 180 of them discarded.
    assert arms["guarded"]["agent_total_per_simulation"] == pytest.approx(1400)
    assert arms["guarded"]["hidden_agent_total_per_simulation"] == pytest.approx(180)
    assert arms["stock"]["agent_total_per_simulation"] == pytest.approx(1100)

    # The scaffold costs 230 tokens per simulation; the policy costs another 70.
    assert overhead["deltas"]["permissive_minus_stock"]["agent_total_per_simulation"] == pytest.approx(230)
    assert overhead["deltas"]["guarded_minus_permissive"]["agent_total_per_simulation"] == pytest.approx(70)
    assert overhead["ratios"]["guarded_over_stock"] == pytest.approx(1400 / 1100)
    assert overhead["ratios"]["guarded_over_permissive"] == pytest.approx(1400 / 1330)


def test_token_overhead_is_unavailable_rather_than_zero_without_an_audit() -> None:
    summaries = {
        "stock": _arm_summary(
            1,
            agent_prompt=1000,
            agent_completion=100,
            hidden_prompt=0,
            hidden_completion=0,
            checks=0,
            blocks=0,
            agent_calls=4,
        ),
        "guarded": _arm_summary(
            0,
            agent_prompt=0,
            agent_completion=0,
            hidden_prompt=0,
            hidden_completion=0,
            checks=0,
            blocks=0,
            agent_calls=0,
        ),
    }

    overhead = _token_overhead(summaries)
    assert overhead["per_arm"]["guarded"]["agent_total_per_simulation"] is None
    assert overhead["ratios"]["guarded_over_stock"] is None

import tomllib
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace

import pytest
from tau2.environment.toolkit import MUTATES_STATE_ATTR

from appa_taubench import bench
from appa_taubench.cli import build_parser
from appa_taubench.knowledge import SUPPORTED_RETRIEVAL_CONFIGS, _plain_toolkit, policy_tool_names
from appa_taubench.policies import Policy, load_policy


def run_spec(**overrides) -> bench.RunSpec:
    values = {
        "retrieval_config": "alltools-qwen",
        "model": "openrouter/model",
        "user_model": "openrouter/user",
        "num_trials": 4,
        "max_steps": 200,
        "max_concurrency": 3,
        "seed": 300,
        "policy_sha256": "policy",
        "implementation_sha256": "implementation",
    }
    values.update(overrides)
    return bench.RunSpec(**values)


def test_run_manifest_only_resumes_an_exact_configuration(tmp_path) -> None:
    spec = run_spec()
    bench.ensure_run_manifest(tmp_path, spec)
    bench.ensure_run_manifest(tmp_path, spec)

    with pytest.raises(ValueError, match="does not match"):
        bench.ensure_run_manifest(tmp_path, replace(spec, seed=301))


def test_cli_defaults_describe_a_complete_submission_run() -> None:
    args = build_parser().parse_args(["run"])
    assert args.retrieval_config == "alltools-qwen"
    assert args.num_trials == 4
    assert args.max_steps == 200
    assert args.seed == 300
    assert args.dry_run is False
    assert not hasattr(args, "task_ids")
    assert not hasattr(args, "defense")


def test_static_preflight_binds_policy_and_loads_the_complete_split() -> None:
    tasks, policy = bench.preflight(
        "alltools-qwen",
        "openrouter/openai/gpt-4.1-mini",
        "openrouter/openai/gpt-4.1-mini",
        require_runtime=False,
    )
    assert len(tasks) == 97
    assert len(policy.tools) == 61


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


def test_run_bench_passes_submission_shape_to_tau_without_task_filter(monkeypatch, tmp_path) -> None:
    policy = Policy("banking_knowledge", "version = 1\ntrust_chain = []\n", frozenset())
    tasks = [SimpleNamespace(id="task-1")]
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
    monkeypatch.setattr(bench, "validate_results_for_submission", lambda results: None)

    assert (
        bench.run_bench(
            retrieval_config="alltools-qwen",
            model="openrouter/model",
            user_model="openrouter/user",
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
    assert captured["tasks"] is tasks
    assert config.domain == "banking_knowledge"
    assert config.task_split_name == "base"
    assert config.task_ids is None
    assert config.retrieval_config == "alltools-qwen"
    assert config.num_trials == 4
    assert config.max_steps == 200
    assert config.auto_resume is True
    assert Path(captured["paths"]["save_path"]).name == "results.json"
    assert (Path(captured["paths"]["save_dir"]) / bench.RUN_MANIFEST).is_file()


def test_run_bench_refuses_too_few_trials_before_preflight(monkeypatch, tmp_path) -> None:
    monkeypatch.setattr(bench, "preflight", lambda *args: pytest.fail("preflight must not run"))
    with pytest.raises(ValueError, match="at least four"):
        bench.run_bench(
            "alltools-qwen",
            "model",
            "user",
            str(tmp_path),
            None,
            300,
            200,
            3,
            3,
        )


def test_dry_run_never_invokes_tau_or_creates_a_run_directory(monkeypatch, tmp_path) -> None:
    policy = Policy("banking_knowledge", "version = 1\ntrust_chain = []\n", frozenset())
    monkeypatch.setattr(bench, "preflight", lambda *args: ([SimpleNamespace(id="1")], policy))
    monkeypatch.setattr(bench, "run_tasks", lambda *args, **kwargs: pytest.fail("Tau must not run"))

    assert (
        bench.run_bench(
            "alltools-qwen",
            "model",
            "user",
            str(tmp_path),
            None,
            300,
            200,
            3,
            4,
            dry_run=True,
        )
        == 0
    )
    assert list(tmp_path.iterdir()) == []

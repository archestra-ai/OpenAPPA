import threading
import time
from pathlib import Path

import pytest
from appa_bench_concurrency import AdaptiveConcurrency
from tau2.agent.base_agent import HalfDuplexAgent
from tau2.data_model.message import AssistantMessage, Message, UserMessage
from tau2.data_model.simulation import TextRunConfig
from tau2.data_model.tasks import Task, UserScenario
from tau2.environment.environment import Environment
from tau2.evaluator.evaluator import EvaluationType
from tau2.registry import registry
from tau2.runner import helpers as tau_helpers
from tau2.user.user_simulator_base import HalfDuplexUser

from appa_taubench import bench

AGENT_NAME = "appa_adaptive_test_agent"
DOMAIN_NAME = "appa_adaptive_test_domain"
USER_NAME = "appa_adaptive_test_user"
_lock = threading.Lock()
_active = 0
_peak = 0
_calls = 0


class MockAgent(HalfDuplexAgent[dict]):
    def get_init_state(self, message_history: list[Message] | None = None) -> dict:
        return {}

    def generate_next_message(self, message, state: dict) -> tuple[AssistantMessage, dict]:
        global _active, _calls, _peak
        with _lock:
            _calls += 1
            call = _calls
            _active += 1
            _peak = max(_peak, _active)
        time.sleep(0.05 if call == 1 else 0.3)
        with _lock:
            _active -= 1
        return AssistantMessage(role="assistant", content="done"), state

    @classmethod
    def is_stop(cls, message: AssistantMessage) -> bool:
        return message.content == "done"


class MockUser(HalfDuplexUser[dict]):
    def __init__(self, instructions=None, tools=None, **_kwargs) -> None:
        super().__init__(instructions=instructions, tools=tools)

    def get_init_state(self, message_history: list[Message] | None = None) -> dict:
        return {}

    def generate_next_message(self, message, state: dict) -> tuple[UserMessage, dict]:
        return UserMessage(role="user", content="run the mock agent"), state


class MockEnvironment(Environment):
    def __init__(self) -> None:
        super().__init__(domain_name=DOMAIN_NAME, policy="")

    def get_tools(self) -> list:
        return []

    def get_user_tools(self, include=None) -> list:
        return []

    def set_state(
        self,
        initialization_data,
        initialization_actions,
        message_history,
        strict: bool = True,
    ) -> None:
        pass


def _mock_agent_factory(tools, domain_policy, **_kwargs) -> MockAgent:
    return MockAgent(tools=tools, domain_policy=domain_policy)


def test_tau_run_tasks_reaches_adaptive_parallelism(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    global _active, _calls, _peak
    _active = 0
    _peak = 0
    _calls = 0
    monkeypatch.setattr(tau_helpers, "get_global_user_sim_guidelines", lambda: "")
    if registry.get_agent_factory(AGENT_NAME) is None:
        registry.register_agent_factory(_mock_agent_factory, AGENT_NAME)
    try:
        registry.get_env_constructor(DOMAIN_NAME)
    except KeyError:
        registry.register_domain(MockEnvironment, DOMAIN_NAME)
    try:
        registry.get_user_constructor(USER_NAME)
    except KeyError:
        registry.register_user(MockUser, USER_NAME)

    config = TextRunConfig(
        domain=DOMAIN_NAME,
        agent=AGENT_NAME,
        user=USER_NAME,
        num_trials=3,
        max_steps=3,
        max_concurrency=2,
        auto_review=False,
        verbose_logs=False,
    )
    tasks = [
        Task(
            id="adaptive-concurrency",
            user_scenario=UserScenario(instructions="run the mock agent"),
        )
    ]

    def controller_factory(maximum: int) -> AdaptiveConcurrency:
        return AdaptiveConcurrency(maximum, observer=lambda: None)

    with bench.adaptive_tau_executor(tmp_path, controller_factory=controller_factory) as executors:
        results = bench.tau_batch.run_tasks(
            config,
            tasks,
            evaluation_type=EvaluationType.ENV,
            console_display=False,
        )

    assert len(results.simulations) == 3
    assert _peak == 2
    assert executors[0].controller.peak_active == 2

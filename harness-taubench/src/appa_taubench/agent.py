"""TauBench text agent with every proposed tool call mediated by OpenAPPA."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from threading import Lock
from uuid import uuid4

from tau2.agent.base_agent import ValidAgentInputMessage
from tau2.agent.llm_agent import LLMAgent, LLMAgentState
from tau2.data_model.message import (
    AssistantMessage,
    MultiToolMessage,
    ToolMessage,
    UserMessage,
)
from tau2.environment.tool import Tool, as_tool
from tau2.runner.batch import _current_simulation_id
from tau2.utils.llm_utils import generate

from appa_taubench import AGENT_PROMPT_PROFILES
from appa_taubench.knowledge import discoverable_tools
from appa_taubench.native import Allowed, Blocked, FrameworkSession

POLICY_BLOCK_SENTINEL = "OpenAPPA blocked this tool call: "
SEQUENTIAL_CALL_FEEDBACK = "OpenAPPA requires one sequential tool call per model completion."
MAX_BLOCKED_COMPLETIONS = 3
POLICY_REFUSAL = "I cannot complete that request because OpenAPPA refused the proposed action."
TOOL_ERROR_REFUSAL = "I could not complete that request because the attempted action did not succeed."


def execute_remedy_plan(plan_id: str) -> str:
    """Execute a remedy plan offered in OpenAPPA's block feedback.

    Args:
        plan_id: The plan ID quoted in OpenAPPA's block feedback.

    Returns:
        The result of the underlying tool call authorized by the plan.
    """
    raise RuntimeError("execute_remedy_plan must be intercepted by OpenAPPA")


@dataclass
class EpisodeStats:
    checks: int = 0
    allowed: int = 0
    policy_blocks: int = 0
    sequential_blocks: int = 0
    hidden_completions: int = 0
    hidden_cost: float = 0.0
    admitted_results: int = 0
    sealed_results: int = 0
    completions: int = 0

    def add(self, other: EpisodeStats) -> None:
        for field in self.__dataclass_fields__:
            setattr(self, field, getattr(self, field) + getattr(other, field))


_completed_stats: list[EpisodeStats] = []
_stats_lock = Lock()


class EpisodeAudit:
    """Raw custom-scaffold events kept beside Tau's scored trajectory."""

    def __init__(
        self,
        directory: str | None,
        task_id: str | None,
        model: str,
        model_args: dict | None,
        simulation_id: str | None,
    ) -> None:
        self.directory = None if directory is None else Path(directory)
        self.episode_id = simulation_id or str(uuid4())
        self.simulation_id = simulation_id
        self.task_id = task_id
        self.model = model
        self.model_args = dict(model_args or {})
        self.trial: int | None = None
        self.seed: int | None = None
        self.events: list[dict] = []
        self.closed = False

    def record(self, kind: str, **fields) -> None:
        if self.directory is None or self.closed:
            return
        self.events.append(
            {
                "kind": kind,
                "timestamp": datetime.now(UTC).isoformat(),
                **fields,
            }
        )

    def close(self, stats: EpisodeStats) -> None:
        if self.directory is None or self.closed:
            return
        self.closed = True
        self.directory.mkdir(parents=True, exist_ok=True)
        path = self.directory / f"{self.episode_id}.json"
        temporary = path.with_suffix(".json.tmp")
        payload = {
            "format_version": 1,
            "episode_id": self.episode_id,
            "tau_simulation_id": self.simulation_id,
            "task_id": self.task_id,
            "trial": self.trial,
            "seed": self.seed,
            "model": self.model,
            "model_args": self.model_args,
            "stats": asdict(stats),
            "events": self.events,
        }
        temporary.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        temporary.replace(path)


def drain_stats() -> EpisodeStats:
    total = EpisodeStats()
    with _stats_lock:
        completed = list(_completed_stats)
        _completed_stats.clear()
    for stats in completed:
        total.add(stats)
    return total


class AppaAgent(LLMAgent[LLMAgentState]):
    """The stock TauBench LLM agent with a CallSession at its tool boundary."""

    def __init__(
        self,
        tools: list[Tool],
        domain_policy: str,
        policy: str,
        llm: str,
        llm_args: dict | None = None,
        audit_dir: str | None = None,
        task_id: str | None = None,
        simulation_id: str | None = None,
        trial_seeds: tuple[int, ...] = (),
        agent_prompt_profile: str = "standard",
    ) -> None:
        if agent_prompt_profile not in AGENT_PROMPT_PROFILES:
            raise ValueError(f"unknown agent prompt profile: {agent_prompt_profile}")
        self.domain_tools = list(tools)
        self.logical_tools = discoverable_tools()
        self.policy = policy
        self.agent_prompt_profile = agent_prompt_profile
        remedy_tool = as_tool(execute_remedy_plan)
        super().__init__(
            tools=[*tools, remedy_tool],
            domain_policy=domain_policy,
            llm=llm,
            llm_args=llm_args,
        )
        self.session: FrameworkSession | None = None
        self.pending = False
        self._pending_tool_call_id: str | None = None
        self.stats = EpisodeStats()
        self._recorded_stats = False
        self._trial_seeds = trial_seeds
        self._last_result_error = False
        self.audit = EpisodeAudit(audit_dir, task_id, llm, llm_args, simulation_id)

    def set_seed(self, seed: int) -> None:
        """Set Tau's trial seed and bind it to the APPA sidecar."""
        super().set_seed(seed)
        self.audit.seed = seed
        self.audit.model_args = dict(self.llm_args)
        if seed in self._trial_seeds:
            self.audit.trial = self._trial_seeds.index(seed)

    @property
    def system_prompt(self) -> str:
        prompt = (
            f"{super().system_prompt}\n\n"
            "Call at most one tool in each response. OpenAPPA may return policy feedback in a tool result; "
            "follow that feedback or explain that the request cannot be completed."
        )
        addendum = AGENT_PROMPT_PROFILES[self.agent_prompt_profile]
        if addendum:
            prompt = f"{prompt}\n\n{addendum}"
        return prompt

    def generate_next_message(
        self,
        message: ValidAgentInputMessage,
        state: LLMAgentState,
    ) -> tuple[AssistantMessage, LLMAgentState]:
        if self.session is None:
            if not isinstance(message, UserMessage) or not isinstance(message.content, str):
                raise ValueError("an OpenAPPA episode must begin with a text user message")
            self.session = FrameworkSession(
                self.policy,
                self.domain_tools,
                message.content,
                logical_tools=self.logical_tools,
            )
        elif self.pending:
            self._report(message)
        else:
            self._last_result_error = False

        self._append_input(message, state)
        hidden_cost = 0.0
        blocked = False
        for _ in range(MAX_BLOCKED_COMPLETIONS):
            response = self._complete(state)
            self.audit.record(
                "model_completion",
                completion=self.stats.completions,
                response=response.model_dump(mode="json"),
            )
            if not response.tool_calls:
                if blocked:
                    hidden_cost += self._record_hidden_completion(response, "blocked_without_dispatch")
                    return self._refuse(state, POLICY_REFUSAL, hidden_cost, "blocked_without_dispatch")
                if self._last_result_error:
                    hidden_cost += self._record_hidden_completion(response, "failed_tau_result")
                    return self._refuse(state, TOOL_ERROR_REFUSAL, hidden_cost, "failed_tau_result")
                self._add_hidden_cost(response, hidden_cost)
                state.messages.append(response)
                return response, state

            if len(response.tool_calls) != 1:
                hidden_cost += self._record_hidden_completion(response, "multiple_tool_calls")
                self.stats.sequential_blocks += len(response.tool_calls)
                self.audit.record(
                    "sequential_block",
                    tool_call_count=len(response.tool_calls),
                    feedback=SEQUENTIAL_CALL_FEEDBACK,
                )
                self._append_feedback(state, response, SEQUENTIAL_CALL_FEEDBACK)
                continue

            call = response.tool_calls[0]
            self.stats.checks += 1
            try:
                policy_tool, policy_arguments = self.session.logical_call(call.name, call.arguments)
            except ValueError:
                policy_tool, policy_arguments = call.name, call.arguments
            decision = self.session.check(call.name, call.arguments)
            match decision:
                case Blocked(feedback):
                    hidden_cost += self._record_hidden_completion(response, "policy_block")
                    blocked = True
                    self.stats.policy_blocks += 1
                    self.audit.record(
                        "policy_block",
                        tool_call_id=call.id,
                        proposed_tool=call.name,
                        proposed_arguments=call.arguments,
                        policy_tool=policy_tool,
                        policy_arguments=policy_arguments,
                        feedback=feedback,
                        recoverable=decision.recoverable,
                    )
                    self._append_feedback(state, response, feedback)
                    if not decision.recoverable:
                        reason = (
                            "stale_or_invalid_remedy" if call.name == "execute_remedy_plan" else "no_remedy_available"
                        )
                        return self._refuse(state, POLICY_REFUSAL, hidden_cost, reason)
                case Allowed(dispatched_tool, dispatched_arguments):
                    self.audit.record(
                        "policy_allow",
                        tool_call_id=call.id,
                        proposed_tool=call.name,
                        proposed_arguments=call.arguments,
                        policy_tool=policy_tool,
                        policy_arguments=policy_arguments,
                        dispatched_tool=dispatched_tool,
                        dispatched_arguments=dispatched_arguments,
                    )
                    call.name = dispatched_tool
                    call.arguments = dispatched_arguments
                    self.pending = True
                    self._last_result_error = False
                    self._pending_tool_call_id = call.id
                    self.stats.allowed += 1
                    self._add_hidden_cost(response, hidden_cost)
                    state.messages.append(response)
                    return response, state

        return self._refuse(state, POLICY_REFUSAL, hidden_cost, "remedy_retry_limit")

    def stop(
        self,
        message: ValidAgentInputMessage | None = None,
        state: LLMAgentState | None = None,
    ) -> None:
        try:
            session = self.session
            if session is not None:
                try:
                    if self.pending and message is not None:
                        self._report(message)
                finally:
                    session.close()
        finally:
            self.session = None
            self.pending = False
            self._pending_tool_call_id = None
            if not self._recorded_stats:
                with _stats_lock:
                    _completed_stats.append(self.stats)
                self._recorded_stats = True
            self.audit.close(self.stats)

    def _complete(self, state: LLMAgentState) -> AssistantMessage:
        session = self.session
        if session is None:
            raise RuntimeError("the OpenAPPA episode is not open")
        if self.stats.completions > 0:
            session.new_round()
        response = generate(
            model=self.llm,
            tools=self.tools,
            messages=state.system_messages + state.messages,
            call_name="appa_agent_response",
            **self.llm_args,
        )
        self.stats.completions += 1
        return response

    @staticmethod
    def _append_input(message: ValidAgentInputMessage, state: LLMAgentState) -> None:
        if isinstance(message, MultiToolMessage):
            state.messages.extend(message.tool_messages)
        else:
            state.messages.append(message)

    def _report(self, message: ValidAgentInputMessage) -> None:
        session = self.session
        if session is None:
            raise RuntimeError("the OpenAPPA episode is not open")
        if isinstance(message, MultiToolMessage):
            results = message.tool_messages
        elif isinstance(message, ToolMessage):
            results = [message]
        else:
            raise ValueError("an allowed OpenAPPA call must be followed by its TauBench tool result")
        if len(results) != 1:
            raise ValueError("OpenAPPA permits one outstanding TauBench tool call")
        result = results[0]
        if result.id != self._pending_tool_call_id:
            raise ValueError("TauBench tool result does not match the outstanding OpenAPPA call")
        original_content = result.content
        reported = session.report(result.content, result.error)
        result.content = reported.content
        self.audit.record(
            "tool_result",
            tool_call_id=result.id,
            original_content=original_content,
            error=result.error,
            delivered_content=reported.content,
            disposition=reported.disposition,
        )
        if reported.disposition == "admitted":
            self.stats.admitted_results += 1
        else:
            self.stats.sealed_results += 1
        self.pending = False
        self._pending_tool_call_id = None
        self._last_result_error = result.error

    def _refuse(
        self,
        state: LLMAgentState,
        content: str,
        cost: float,
        reason: str,
    ) -> tuple[AssistantMessage, LLMAgentState]:
        refusal = AssistantMessage.text(content, cost=cost or None)
        self.audit.record("terminal_refusal", reason=reason, cost=cost)
        state.messages.append(refusal)
        return refusal, state

    def _record_hidden_completion(self, response: AssistantMessage, reason: str) -> float:
        cost = response.cost or 0.0
        self.stats.hidden_completions += 1
        self.stats.hidden_cost += cost
        self.audit.record("hidden_completion", reason=reason, cost=cost)
        return cost

    @staticmethod
    def _append_feedback(state: LLMAgentState, response: AssistantMessage, feedback: str) -> None:
        state.messages.append(response)
        for call in response.tool_calls or []:
            state.messages.append(
                ToolMessage(
                    id=call.id,
                    role="tool",
                    requestor="assistant",
                    content=f"{POLICY_BLOCK_SENTINEL}{feedback}",
                    error=True,
                )
            )

    @staticmethod
    def _add_hidden_cost(response: AssistantMessage, hidden_cost: float) -> None:
        if hidden_cost:
            response.cost = (response.cost or 0.0) + hidden_cost


def create_appa_agent(tools, domain_policy, **kwargs) -> AppaAgent:
    """Build the registered TauBench agent with the selected OpenAPPA policy."""
    return AppaAgent(
        tools=tools,
        domain_policy=domain_policy,
        policy=kwargs["appa_policy"],
        llm=kwargs["llm"],
        llm_args=kwargs.get("llm_args"),
        audit_dir=kwargs.get("audit_dir"),
        task_id=None if kwargs.get("task") is None else str(kwargs["task"].id),
        simulation_id=_current_simulation_id.get(),
        trial_seeds=kwargs.get("trial_seeds", ()),
        agent_prompt_profile=kwargs.get("agent_prompt_profile", "standard"),
    )

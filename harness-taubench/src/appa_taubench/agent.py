"""TauBench text agent with every proposed tool call mediated by OpenAPPA."""

from __future__ import annotations

from dataclasses import dataclass
from threading import Lock

from tau2.agent.base_agent import ValidAgentInputMessage
from tau2.agent.llm_agent import LLMAgent, LLMAgentState
from tau2.data_model.message import (
    AssistantMessage,
    MultiToolMessage,
    ToolMessage,
    UserMessage,
)
from tau2.environment.tool import Tool, as_tool
from tau2.utils.llm_utils import generate

from appa_taubench.native import Allowed, Blocked, FrameworkSession

EXECUTE_REMEDY_PLAN = "execute_remedy_plan"
POLICY_BLOCK_SENTINEL = "OpenAPPA blocked this tool call: "
SEQUENTIAL_CALL_FEEDBACK = "OpenAPPA requires one sequential tool call per model completion."
MAX_BLOCKED_COMPLETIONS = 3


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
    admitted_results: int = 0
    sealed_results: int = 0
    completions: int = 0

    def add(self, other: EpisodeStats) -> None:
        for field in self.__dataclass_fields__:
            setattr(self, field, getattr(self, field) + getattr(other, field))


_completed_stats: list[EpisodeStats] = []
_stats_lock = Lock()


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
    ) -> None:
        self.domain_tools = list(tools)
        self.policy = policy
        remedy_tool = as_tool(execute_remedy_plan)
        super().__init__(
            tools=[*tools, remedy_tool],
            domain_policy=domain_policy,
            llm=llm,
            llm_args=llm_args,
        )
        self.session: FrameworkSession | None = None
        self.pending = False
        self.stats = EpisodeStats()
        self._recorded_stats = False

    @property
    def system_prompt(self) -> str:
        return (
            f"{super().system_prompt}\n\n"
            "Call at most one tool in each response. OpenAPPA may return policy feedback in a tool result; "
            "follow that feedback or explain that the request cannot be completed."
        )

    def generate_next_message(
        self,
        message: ValidAgentInputMessage,
        state: LLMAgentState,
    ) -> tuple[AssistantMessage, LLMAgentState]:
        if self.session is None:
            if not isinstance(message, UserMessage) or not isinstance(message.content, str):
                raise ValueError("an OpenAPPA episode must begin with a text user message")
            self.session = FrameworkSession(self.policy, self.domain_tools, message.content)
        elif self.pending:
            self._report(message)

        self._append_input(message, state)
        hidden_cost = 0.0
        for _ in range(MAX_BLOCKED_COMPLETIONS):
            response = self._complete(state)
            if not response.tool_calls:
                self._add_hidden_cost(response, hidden_cost)
                state.messages.append(response)
                return response, state

            if len(response.tool_calls) != 1:
                hidden_cost += response.cost or 0.0
                self.stats.sequential_blocks += len(response.tool_calls)
                self._append_feedback(state, response, SEQUENTIAL_CALL_FEEDBACK)
                continue

            call = response.tool_calls[0]
            self.stats.checks += 1
            decision = self.session.check(call.name, call.arguments)
            match decision:
                case Blocked(feedback):
                    hidden_cost += response.cost or 0.0
                    self.stats.policy_blocks += 1
                    self._append_feedback(state, response, feedback)
                case Allowed(dispatched_tool, dispatched_arguments):
                    call.name = dispatched_tool
                    call.arguments = dispatched_arguments
                    self.pending = True
                    self.stats.allowed += 1
                    self._add_hidden_cost(response, hidden_cost)
                    state.messages.append(response)
                    return response, state

        refusal = AssistantMessage.text(
            "I cannot complete that request because the proposed actions remain blocked by policy.",
            cost=hidden_cost or None,
        )
        state.messages.append(refusal)
        return refusal, state

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
            if not self._recorded_stats:
                with _stats_lock:
                    _completed_stats.append(self.stats)
                self._recorded_stats = True

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
        reported = session.report(result.content, result.error)
        result.content = reported.content
        if reported.disposition == "admitted":
            self.stats.admitted_results += 1
        else:
            self.stats.sealed_results += 1
        self.pending = False

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
    )

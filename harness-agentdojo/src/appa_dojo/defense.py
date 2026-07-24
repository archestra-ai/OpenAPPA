"""AgentDojo tool executor mediated by a current OpenAPPA CallSession."""

import copy
from ast import literal_eval
from collections.abc import Mapping, Sequence

from agentdojo.agent_pipeline.base_pipeline_element import BasePipelineElement
from agentdojo.agent_pipeline.tool_execution import (
    EMPTY_FUNCTION_NAME,
    ToolsExecutionLoop,
    ToolsExecutor,
    is_string_list,
    tool_result_to_str,
)
from agentdojo.functions_runtime import (
    EmptyEnv,
    Env,
    FunctionCall,
    FunctionsRuntime,
)
from agentdojo.types import (
    ChatMessage,
    ChatToolResultMessage,
    get_text_content_as_str,
    text_content_block_from_string,
)

from appa_dojo.constants import EXECUTE_REMEDY_PLAN, POLICY_BLOCK_SENTINEL
from appa_dojo.native import Blocked, Delivered, NativeSession
from appa_dojo.tool_bridge import ToolBridge

NESTED_CALL_ERROR = "nested tool calls are not permitted under policy"


def execute_remedy_plan(plan_id: str) -> str:
    """Execute a remedy plan offered by OpenAPPA for a blocked call.

    :param plan_id: The plan_id quoted in OpenAPPA's block feedback.
    """
    raise RuntimeError("execute_remedy_plan reached AgentDojo's function runtime instead of the APPA executor")


class AppaRuntimeSetup(BasePipelineElement):
    """Advertise APPA's virtual remedy tool before the first model call."""

    def query(
        self,
        query: str,
        runtime: FunctionsRuntime,
        env: Env = EmptyEnv(),
        messages: Sequence[ChatMessage] = [],
        extra_args: dict = {},
    ):
        if EXECUTE_REMEDY_PLAN not in runtime.functions:
            runtime.register_function(execute_remedy_plan)
        return query, runtime, env, messages, extra_args


def has_nested_call(value: object) -> bool:
    if isinstance(value, FunctionCall):
        return True
    if isinstance(value, Mapping):
        return any(has_nested_call(item) for item in value.values())
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return any(has_nested_call(item) for item in value)
    return False


class AppaToolsExecutor(ToolsExecutor):
    """Execute each proposed call only through OpenAPPA's check/report lifecycle."""

    def __init__(
        self,
        policy: str,
        tool_output_formatter=tool_result_to_str,
        tool_bridge: ToolBridge | None = None,
    ) -> None:
        super().__init__(tool_output_formatter)
        self.policy = policy
        self.tool_bridge = tool_bridge or ToolBridge()
        self.session: NativeSession | None = None
        self._trace_projection: list[tuple[str, FunctionCall | None]] = []

    def query(
        self,
        query: str,
        runtime: FunctionsRuntime,
        env: Env = EmptyEnv(),
        messages: Sequence[ChatMessage] = [],
        extra_args: dict = {},
    ):
        if len(messages) == 0 or messages[-1]["role"] != "assistant":
            return query, runtime, env, messages, extra_args
        tool_calls = messages[-1]["tool_calls"]
        if tool_calls is None or len(tool_calls) == 0:
            return query, runtime, env, messages, extra_args

        if not any(message["role"] == "tool" for message in messages):
            # The opening completion takes the episode's first round via begin_turn.
            self._open_episode(messages, runtime, env)
        elif self.session is not None:
            # Every later completion is a fresh inference round, so an acceptance-carrying remedy
            # the model proposes here — after seeing the offer's block feedback last completion —
            # is informed rather than a same-response guess.
            self.session.new_round()

        results = []
        for tool_call in tool_calls:
            if tool_call.function == EMPTY_FUNCTION_NAME:
                self._trace_projection.append((tool_call.id, None))
                results.append(
                    self._result(
                        tool_call,
                        "",
                        "Empty function name provided. Provide a valid function name.",
                    )
                )
                continue
            if tool_call.function not in runtime.functions:
                self._trace_projection.append((tool_call.id, None))
                results.append(
                    self._result(
                        tool_call,
                        "",
                        f"Invalid tool {tool_call.function} provided.",
                    )
                )
                continue
            if has_nested_call(tool_call.args):
                self._trace_projection.append((tool_call.id, None))
                results.append(
                    self._result(
                        tool_call,
                        "",
                        f"{POLICY_BLOCK_SENTINEL}{NESTED_CALL_ERROR}",
                    )
                )
                continue

            for name, value in tool_call.args.items():
                if isinstance(value, str) and is_string_list(value):
                    tool_call.args[name] = literal_eval(value)

            session = self.session
            if session is None:
                raise RuntimeError("the native APPA episode is not open")
            decision = session.dispatch(tool_call.function, dict(tool_call.args))
            match decision:
                case Blocked(feedback):
                    self._trace_projection.append((tool_call.id, None))
                    results.append(
                        self._result(
                            tool_call,
                            "",
                            f"{POLICY_BLOCK_SENTINEL}{feedback}",
                        )
                    )
                case Delivered(content, dispatched_tool, dispatched_arguments, _):
                    self._trace_projection.append(
                        (
                            tool_call.id,
                            FunctionCall(
                                function=dispatched_tool,
                                args=dispatched_arguments,
                                id=tool_call.id,
                            ),
                        )
                    )
                    results.append(self._result(tool_call, content, None))

        return query, runtime, env, [*messages, *results], extra_args

    def close(self) -> None:
        try:
            self._close_episode()
        finally:
            self.tool_bridge.close()

    def finish_episode(self, messages: Sequence[ChatMessage]) -> list[ChatMessage]:
        try:
            self._record_unexecuted_tail(messages)
            return self._project_trace(messages)
        finally:
            self._close_episode()

    def _open_episode(self, messages: Sequence[ChatMessage], runtime: FunctionsRuntime, env: Env) -> None:
        user_prompt = next(
            (get_text_content_as_str(message["content"]) for message in messages if message["role"] == "user"),
            None,
        )
        if user_prompt is None:
            raise ValueError("AgentDojo episode has no user message")
        tools = sorted(name for name in runtime.functions if name != EXECUTE_REMEDY_PLAN)
        self._close_episode()
        self._trace_projection.clear()
        bridge_url = self.tool_bridge.open_episode(runtime, env, self.output_formatter, set(tools))
        try:
            self.session = NativeSession(self.policy, tools, user_prompt, bridge_url)
        except Exception:
            self.tool_bridge.close_episode()
            raise

    def _close_episode(self) -> None:
        try:
            if self.session is not None:
                self.session.close()
        finally:
            self.session = None
            self.tool_bridge.close_episode()

    def _project_trace(self, messages: Sequence[ChatMessage]) -> list[ChatMessage]:
        projected = copy.deepcopy(list(messages))
        decisions = iter(self._trace_projection)
        for message in projected:
            if message["role"] == "assistant" and message["tool_calls"] is not None:
                exact_calls = []
                for call in message["tool_calls"]:
                    try:
                        call_id, dispatched = next(decisions)
                    except StopIteration as error:
                        raise RuntimeError("the dispatch trace has fewer entries than proposed calls") from error
                    if call_id != call.id:
                        raise RuntimeError("the dispatch trace is not aligned with proposed call order")
                    if dispatched is not None:
                        exact_calls.append(dispatched)
                message["tool_calls"] = exact_calls
        try:
            next(decisions)
        except StopIteration:
            return projected
        raise RuntimeError("the dispatch trace has entries without proposed calls")

    def _record_unexecuted_tail(self, messages: Sequence[ChatMessage]) -> None:
        proposed = [
            call for message in messages if message["role"] == "assistant" for call in (message["tool_calls"] or [])
        ]
        if len(proposed) == len(self._trace_projection):
            return
        if len(proposed) < len(self._trace_projection):
            raise RuntimeError("the dispatch trace has entries without proposed calls")
        last = messages[-1]
        trailing = last["tool_calls"] if last["role"] == "assistant" else None
        missing = proposed[len(self._trace_projection) :]
        if trailing is None or missing != list(trailing):
            raise RuntimeError("only a trailing unexecuted assistant batch may lack dispatch entries")
        self._trace_projection.extend((call.id, None) for call in missing)

    @staticmethod
    def _result(call: FunctionCall, content: str, error: str | None) -> ChatToolResultMessage:
        return ChatToolResultMessage(
            role="tool",
            content=[text_content_block_from_string(content)],
            tool_call_id=call.id,
            tool_call=call,
            error=error,
        )


class AppaToolsExecutionLoop(BasePipelineElement):
    """Run AgentDojo's loop and close the episode while projecting exact dispatch traces."""

    def __init__(self, executor: AppaToolsExecutor, llm: BasePipelineElement) -> None:
        self.executor = executor
        self.loop = ToolsExecutionLoop([executor, llm])

    def query(
        self,
        query: str,
        runtime: FunctionsRuntime,
        env: Env = EmptyEnv(),
        messages: Sequence[ChatMessage] = [],
        extra_args: dict = {},
    ):
        try:
            query, runtime, env, messages, extra_args = self.loop.query(query, runtime, env, messages, extra_args)
        except Exception:
            self.executor._close_episode()
            raise
        return query, runtime, env, self.executor.finish_episode(messages), extra_args

"""AgentDojo tool executor mediated by a current OpenAPPA CallSession."""

import logging
from ast import literal_eval
from collections.abc import Mapping, Sequence

from agentdojo.agent_pipeline.base_pipeline_element import BasePipelineElement
from agentdojo.agent_pipeline.tool_execution import (
    EMPTY_FUNCTION_NAME,
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

from appa_dojo.bridge import (
    Admitted,
    Allowed,
    AuthorizedCall,
    Blocked,
    Declined,
    Sealed,
    SidecarClient,
)

logger = logging.getLogger(__name__)

EXECUTE_REMEDY_PLAN = "execute_remedy_plan"
POLICY_BLOCK_SENTINEL = "Blocked by OpenAPPA policy: "
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
        sidecar: SidecarClient | None = None,
    ) -> None:
        super().__init__(tool_output_formatter)
        self.policy = policy
        self.sidecar = sidecar or SidecarClient()

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
            self._open_episode(messages, runtime)

        results = []
        for tool_call in tool_calls:
            if tool_call.function == EMPTY_FUNCTION_NAME:
                results.append(
                    self._result(
                        tool_call,
                        "",
                        "Empty function name provided. Provide a valid function name.",
                    )
                )
                continue
            if tool_call.function not in runtime.functions:
                results.append(
                    self._result(
                        tool_call,
                        "",
                        f"Invalid tool {tool_call.function} provided.",
                    )
                )
                continue
            if has_nested_call(tool_call.args):
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

            if tool_call.function == EXECUTE_REMEDY_PLAN:
                plan_id = tool_call.args.get("plan_id")
                decision = self.sidecar.resolve_remedy(plan_id if isinstance(plan_id, str) else None)
                match decision:
                    case Declined(feedback):
                        results.append(
                            self._result(
                                tool_call,
                                "",
                                f"{POLICY_BLOCK_SENTINEL}{feedback}",
                            )
                        )
                    case AuthorizedCall(tool, arguments):
                        results.append(self._execute_and_report(tool_call, tool, arguments, runtime, env))
                continue

            decision = self.sidecar.check(tool_call.function, dict(tool_call.args))
            match decision:
                case Blocked(feedback):
                    results.append(
                        self._result(
                            tool_call,
                            "",
                            f"{POLICY_BLOCK_SENTINEL}{feedback}",
                        )
                    )
                case Allowed():
                    results.append(
                        self._execute_and_report(
                            tool_call,
                            tool_call.function,
                            dict(tool_call.args),
                            runtime,
                            env,
                        )
                    )

        return query, runtime, env, [*messages, *results], extra_args

    def close(self) -> None:
        self.sidecar.close()

    def _open_episode(self, messages: Sequence[ChatMessage], runtime: FunctionsRuntime) -> None:
        user_prompt = next(
            (get_text_content_as_str(message["content"]) for message in messages if message["role"] == "user"),
            None,
        )
        if user_prompt is None:
            raise ValueError("AgentDojo episode has no user message")
        tools = sorted(name for name in runtime.functions if name != EXECUTE_REMEDY_PLAN)
        self.sidecar.open(self.policy, tools, user_prompt)

    def _execute_and_report(
        self,
        visible_call: FunctionCall,
        dispatched_tool: str,
        arguments: dict[str, object],
        runtime: FunctionsRuntime,
        env: Env,
    ) -> ChatToolResultMessage:
        tool_result, error = runtime.run_function(env, dispatched_tool, arguments)
        if error is not None:
            result = self.sidecar.report_indeterminate()
        else:
            try:
                body = self.output_formatter(tool_result)
            except Exception:
                self.sidecar.report_indeterminate()
                logger.exception("could not format the result of %s", dispatched_tool)
                raise
            result = self.sidecar.report_success(body)

        match result:
            case Admitted(content):
                return self._result(visible_call, content, None)
            case Sealed(token):
                return self._result(visible_call, token, None)

    @staticmethod
    def _result(call: FunctionCall, content: str, error: str | None) -> ChatToolResultMessage:
        return ChatToolResultMessage(
            role="tool",
            content=[text_content_block_from_string(content)],
            tool_call_id=call.id,
            tool_call=call,
            error=error,
        )

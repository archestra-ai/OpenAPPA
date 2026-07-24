import threading
import time
from http.client import HTTPConnection
from urllib.parse import urlparse

import pytest
from agentdojo.agent_pipeline.tool_execution import tool_result_to_str
from agentdojo.functions_runtime import EmptyEnv, FunctionsRuntime

from appa_dojo.native import AppaError, Blocked, Delivered, NativeSession
from appa_dojo.tool_bridge import MAX_REQUEST_BODY_BYTES, ToolBridge

OPEN_POLICY = """
version = 1
[[tool]]
name = "lookup"
delta = {}
"""

NARROWING_POLICY = """
version = 1
trust_chain = ["suspicious", "internal"]
[[tool]]
name = "read_external"
delta = { trust = "suspicious" }
"""


def open_session(
    bridge: ToolBridge,
    runtime: FunctionsRuntime,
    policy: str = OPEN_POLICY,
    tools: list[str] | None = None,
    formatter=tool_result_to_str,
) -> NativeSession:
    names = tools or ["lookup"]
    url = bridge.open_episode(runtime, EmptyEnv(), formatter, set(names))
    return NativeSession(policy, names, "run the tool", url)


def test_allowed_tool_executes_exactly_once_and_returns_admitted_content() -> None:
    calls: list[str] = []

    def lookup(value: str) -> str:
        """Look up one value.

        :param value: Value to look up.
        """
        calls.append(value)
        return f"found:{value}"

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        with open_session(bridge, runtime) as session:
            result = session.dispatch("lookup", {"value": "one"})

        assert result == Delivered(
            content="found:one",
            dispatched_tool="lookup",
            dispatched_arguments={"value": "one"},
            disposition="admitted",
        )
        assert calls == ["one"]
        assert len(bridge.execution_log) == 1
        assert bridge.execution_log[0].outcome == "success"


def test_blocked_tool_never_reaches_the_bridge() -> None:
    calls = 0

    def read_external() -> str:
        """Read external content."""
        nonlocal calls
        calls += 1
        return "external"

    runtime = FunctionsRuntime()
    runtime.register_function(read_external)
    with ToolBridge() as bridge:
        with open_session(bridge, runtime, NARROWING_POLICY, ["read_external"]) as session:
            result = session.dispatch("read_external", {})

        assert isinstance(result, Blocked)
        assert calls == 0
        assert bridge.execution_log == ()


def test_remedy_dispatches_only_the_underlying_tool_once() -> None:
    calls = 0

    def read_external() -> str:
        """Read external content."""
        nonlocal calls
        calls += 1
        return "external"

    runtime = FunctionsRuntime()
    runtime.register_function(read_external)
    with ToolBridge() as bridge:
        with open_session(bridge, runtime, NARROWING_POLICY, ["read_external"]) as session:
            assert isinstance(session.dispatch("read_external", {}), Blocked)
            # Informed acceptance: the read remedy is accepted in the completion after the one
            # that surfaced its offer.
            session.new_round()
            result = session.dispatch("execute_remedy_plan", {"plan_id": "remedy-0"})

        assert result == Delivered(
            content="external",
            dispatched_tool="read_external",
            dispatched_arguments={},
            disposition="admitted",
        )
        assert calls == 1
        assert [record.tool for record in bridge.execution_log] == ["read_external"]


@pytest.mark.parametrize("failure", ["runtime", "formatter"])
def test_python_failures_are_sealed_without_crossing_error_bytes(failure: str) -> None:
    secret = "sensitive backend exception"

    def lookup() -> str:
        """Look up a value."""
        if failure == "runtime":
            raise RuntimeError(secret)
        return "value"

    def formatter(value: object) -> str:
        if failure == "formatter":
            raise RuntimeError(secret)
        return str(value)

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        with open_session(bridge, runtime, formatter=formatter) as session:
            result = session.dispatch("lookup", {})

        assert isinstance(result, Delivered)
        assert result.disposition == "sealed"
        expected = "indeterminate" if failure == "runtime" else "success_without_value"
        assert bridge.execution_log[0].outcome == expected


def test_oversized_success_is_sealed() -> None:
    def lookup() -> str:
        """Look up a large value."""
        return "x" * (256 * 1024 + 1)

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        with open_session(bridge, runtime) as session:
            result = session.dispatch("lookup", {})

        assert isinstance(result, Delivered)
        assert result.disposition == "sealed"
        assert bridge.execution_log[0].outcome == "success_without_value"


def test_stale_capability_cannot_execute_against_the_next_episode() -> None:
    calls: list[str] = []

    def lookup(value: str) -> str:
        """Look up one value.

        :param value: Value to look up.
        """
        calls.append(value)
        return value

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        stale_url = bridge.open_episode(runtime, EmptyEnv(), tool_result_to_str, {"lookup"})
        stale_session = NativeSession(OPEN_POLICY, ["lookup"], "run", stale_url)
        bridge.close_episode()
        bridge.open_episode(runtime, EmptyEnv(), tool_result_to_str, {"lookup"})
        result = stale_session.dispatch("lookup", {"value": "stale"})
        stale_session.close()

        assert isinstance(result, Delivered)
        assert result.disposition == "sealed"
        assert calls == []
        assert bridge.execution_log == ()


def test_binding_rejects_a_dns_bridge_url_with_appa_error() -> None:
    with pytest.raises(AppaError):
        NativeSession(OPEN_POLICY, ["lookup"], "run", "http://localhost:1234/capability")


def test_loopback_dispatch_ignores_ambient_proxies(monkeypatch: pytest.MonkeyPatch) -> None:
    calls = 0

    def lookup() -> str:
        """Look up a value."""
        nonlocal calls
        calls += 1
        return "value"

    for variable in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"]:
        monkeypatch.setenv(variable, "http://127.0.0.1:1")
    for variable in ["NO_PROXY", "no_proxy"]:
        monkeypatch.delenv(variable, raising=False)

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        with open_session(bridge, runtime) as session:
            result = session.dispatch("lookup", {})

        assert isinstance(result, Delivered)
        assert result.disposition == "admitted"
        assert calls == 1


def test_native_request_cap_applies_before_json_parsing() -> None:
    calls = 0

    def lookup(value: str) -> str:
        """Look up a value.

        :param value: Value to look up.
        """
        nonlocal calls
        calls += 1
        return value

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        with open_session(bridge, runtime) as session:
            with pytest.raises(AppaError):
                session.dispatch("lookup", {"value": "x" * MAX_REQUEST_BODY_BYTES})

        assert calls == 0
        assert bridge.execution_log == ()


def test_calls_serialize_and_native_dispatch_releases_the_gil() -> None:
    active = 0
    max_active = 0
    state_lock = threading.Lock()

    def lookup(value: str) -> str:
        """Look up one value.

        :param value: Value to look up.
        """
        nonlocal active, max_active
        with state_lock:
            active += 1
            max_active = max(max_active, active)
        time.sleep(0.05)
        with state_lock:
            active -= 1
        return value

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        url = bridge.open_episode(runtime, EmptyEnv(), tool_result_to_str, {"lookup"})
        sessions = [NativeSession(OPEN_POLICY, ["lookup"], "run", url) for _ in range(2)]
        barrier = threading.Barrier(3)
        results: list[Delivered | Blocked] = []

        def dispatch(index: int) -> None:
            barrier.wait()
            results.append(sessions[index].dispatch("lookup", {"value": str(index)}))

        threads = [threading.Thread(target=dispatch, args=(index,)) for index in range(2)]
        for thread in threads:
            thread.start()
        barrier.wait()
        for thread in threads:
            thread.join(timeout=5)
        for session in sessions:
            session.close()

        assert all(not thread.is_alive() for thread in threads)
        assert len(results) == 2
        assert all(isinstance(result, Delivered) for result in results)
        assert max_active == 1


def test_bridge_refuses_oversized_requests_before_execution() -> None:
    calls = 0

    def lookup() -> str:
        """Look up a value."""
        nonlocal calls
        calls += 1
        return "value"

    runtime = FunctionsRuntime()
    runtime.register_function(lookup)
    with ToolBridge() as bridge:
        url = bridge.open_episode(runtime, EmptyEnv(), tool_result_to_str, {"lookup"})
        parsed = urlparse(url)
        connection = HTTPConnection(parsed.hostname, parsed.port, timeout=5)
        connection.putrequest("POST", parsed.path)
        connection.putheader("Content-Length", str(MAX_REQUEST_BODY_BYTES + 1))
        connection.endheaders()
        response = connection.getresponse()

        assert response.status == 413
        response.close()
        connection.close()
        assert calls == 0

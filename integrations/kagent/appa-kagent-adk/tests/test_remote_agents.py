import asyncio
from types import SimpleNamespace

import pytest

from appa_kagent_adk.remote_agents import IsolatedRemoteTool, IsolatedRemoteToolset


class Remote:
    name = "analyst"
    description = "Analyze logs"
    _last_context_id = "shared"

    def _get_declaration(self):
        return "unchanged schema"

    async def run_async(self, *, args, tool_context):
        before = self._last_context_id
        await asyncio.sleep(0)
        assert self._last_context_id == before
        return {"result": args["request"], "subagent_session_id": before}


async def test_parallel_and_sequential_calls_get_distinct_child_ids():
    original = Remote()
    tool = IsolatedRemoteTool(original)
    context = SimpleNamespace(tool_confirmation=None)
    first, second = await asyncio.gather(*[
        tool.run_async(args={"request": str(i)}, tool_context=context) for i in range(2)
    ])
    third = await tool.run_async(args={"request": "again"}, tool_context=context)
    assert len({result["subagent_session_id"] for result in [first, second, third]}) == 3
    assert original._last_context_id == "shared"
    assert tool._get_declaration() == "unchanged schema"


async def test_approval_resume_keeps_the_paused_child():
    tool = IsolatedRemoteTool(Remote())
    context = SimpleNamespace(tool_confirmation=SimpleNamespace(payload={"context_id": "paused-child"}))
    result = await tool.run_async(args={"request": "resume"}, tool_context=context)
    assert result["subagent_session_id"] == "paused-child"
    context.tool_confirmation.payload = {}
    with pytest.raises(ValueError, match="original child"):
        await tool.run_async(args={}, tool_context=context)


@pytest.mark.parametrize("result", ["Remote agent 'analyst' request failed: unavailable", "Direct message", None])
async def test_non_task_results_never_claim_a_child_identity(result):
    class MessageRemote(Remote):
        async def run_async(self, *, args, tool_context):
            return result

    tool = IsolatedRemoteTool(MessageRemote())
    actual = await tool.run_async(args={}, tool_context=SimpleNamespace(tool_confirmation=None))
    assert actual == result


async def test_toolset_keeps_client_ownership_without_advertising_a_stale_id():
    class Toolset:
        closed = False

        async def get_tools(self, context):
            return [Remote()]

        async def close(self):
            self.closed = True

    original = Toolset()
    wrapped = IsolatedRemoteToolset(original)
    assert not hasattr(wrapped, "subagent_session_id")
    assert isinstance((await wrapped.get_tools())[0], IsolatedRemoteTool)
    await wrapped.close()
    assert original.closed

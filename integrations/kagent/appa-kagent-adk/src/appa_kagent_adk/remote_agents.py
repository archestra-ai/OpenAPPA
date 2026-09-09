"""Per-call remote sessions, with the stock transport and approval resume."""

import copy
import uuid

from google.adk.tools.base_tool import BaseTool
from google.adk.tools.base_toolset import BaseToolset


class IsolatedRemoteTool(BaseTool):
    def __init__(self, delegate):
        super().__init__(name=delegate.name, description=delegate.description)
        self._delegate = delegate

    def _get_declaration(self):
        return self._delegate._get_declaration()

    async def run_async(self, *, args, tool_context):
        # A separate object keeps concurrent calls from changing each other's
        # context ID. The toolset still owns and closes the shared HTTP client.
        call = copy.copy(self._delegate)
        if tool_context.tool_confirmation is None:
            call._last_context_id = str(uuid.uuid4())
        else:
            # Resumes belong to the original paused child, not a new errand.
            payload = tool_context.tool_confirmation.payload
            if not isinstance(payload, dict) or not payload.get("context_id"):
                raise ValueError("remote approval resume requires the original child context_id")
            call._last_context_id = payload["context_id"]
        # Only the transport's Task response establishes a child identity.
        # Bare messages and transport errors must not acquire a fabricated ID.
        return await call.run_async(args=args, tool_context=tool_context)


class IsolatedRemoteToolset(BaseToolset):
    def __init__(self, delegate):
        super().__init__()
        self._delegate = delegate

    async def get_tools(self, readonly_context=None):
        return [IsolatedRemoteTool(tool) for tool in await self._delegate.get_tools(readonly_context)]

    async def close(self):
        await self._delegate.close()


def isolate_remote_agents(agent):
    from kagent.adk._remote_a2a_tool import KAgentRemoteA2AToolset

    # Do not expose SubagentSessionProvider: the pinned executor snapshots it
    # once per turn. The UI uses each response's actual subagent_session_id
    # when call metadata has none, as on the Go isolated-session path.
    agent.tools = [
        IsolatedRemoteToolset(tool) if isinstance(tool, KAgentRemoteA2AToolset) else tool
        for tool in agent.tools
    ]

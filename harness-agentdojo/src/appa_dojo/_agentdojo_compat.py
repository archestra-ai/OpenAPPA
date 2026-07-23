"""Narrow compatibility fixes for AgentDojo 0.1.35 workspace v1.2.x."""


def apply() -> None:
    from agentdojo.default_suites.v1.tools.types import CalendarEvent
    from agentdojo.models import MODEL_NAMES

    if CalendarEvent.__hash__ is None:
        CalendarEvent.__hash__ = lambda self: hash(self.id_)

    MODEL_NAMES.setdefault("gpt-4.1", "GPT-4")
    MODEL_NAMES.setdefault("gpt-5", "GPT-5")

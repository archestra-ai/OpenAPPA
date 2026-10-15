"""Hermes plugin: demo calendar and mail tools over a JSON file.

The runner sets APPA_DEMO_STATE, the directory holding calendar.json; sent mail
lands in outbox.json there.
"""

import json
import os
from pathlib import Path
from typing import Any

TOOLSET = "appa-calendar"


def _state() -> Path:
    return Path(os.environ["APPA_DEMO_STATE"])


def _load_calendar() -> dict[str, Any]:
    return json.loads((_state() / "calendar.json").read_text())


def _save_calendar(calendar: dict[str, Any]) -> None:
    (_state() / "calendar.json").write_text(json.dumps(calendar, indent=2))


def _send(message: dict[str, Any]) -> None:
    outbox = _state() / "outbox.json"
    sent = json.loads(outbox.read_text()) if outbox.exists() else []
    sent.append(message)
    outbox.write_text(json.dumps(sent, indent=2))


def _event_view(event_id: str, event: dict[str, Any]) -> dict[str, Any]:
    return {"event_id": event_id, **event}


def list_events(params: dict[str, Any], **_: Any) -> str:
    date = params["date"]
    events = _load_calendar()["events"]
    return json.dumps([_event_view(i, e) for i, e in events.items() if e["start"].startswith(date)])


def get_event(params: dict[str, Any], **_: Any) -> str:
    events = _load_calendar()["events"]
    match events.get(params["event_id"]):
        case None:
            return json.dumps({"error": f"no event {params['event_id']}"})
        case event:
            return json.dumps(_event_view(params["event_id"], event))


def update_event(params: dict[str, Any], **_: Any) -> str:
    calendar = _load_calendar()
    event = calendar["events"].get(params["event_id"])
    if event is None:
        return json.dumps({"error": f"no event {params['event_id']}"})
    event["start"], event["end"] = params["start"], params["end"]
    _save_calendar(calendar)
    notified = [a for a in event["attendees"] if a != calendar["owner"]]
    if params.get("notify_attendees", False):
        for attendee in notified:
            _send(
                {
                    "from": calendar["owner"],
                    "to": attendee,
                    "subject": f"Updated invitation: {event['title']}",
                    "body": f"{event['title']} now runs {event['start']} to {event['end']}."
                    + (f" Reason: {params['reason']}" if params.get("reason") else ""),
                    "via": "calendar_update_event",
                }
            )
    return json.dumps(
        {
            "updated": _event_view(params["event_id"], event),
            "notified": notified if params.get("notify_attendees", False) else [],
        }
    )


def gmail_send(params: dict[str, Any], **_: Any) -> str:
    _send(
        {
            "from": _load_calendar()["owner"],
            "to": params["to"],
            "subject": params["subject"],
            "body": params["body"],
            "via": "gmail_send",
        }
    )
    return json.dumps({"sent": True})


def _schema(name: str, description: str, properties: dict[str, Any], required: list[str]) -> dict[str, Any]:
    return {
        "name": name,
        "description": description,
        "parameters": {"type": "object", "properties": properties, "required": required},
    }


STRING = {"type": "string"}
TOOLS = [
    (
        _schema(
            "calendar_list_events",
            "List every event on the user's calendar for one day, with titles and attendees.",
            {"date": {"type": "string", "description": "YYYY-MM-DD"}},
            ["date"],
        ),
        list_events,
    ),
    (_schema("calendar_get_event", "Read one calendar event by id.", {"event_id": STRING}, ["event_id"]), get_event),
    (
        _schema(
            "calendar_update_event",
            "Move an event. With notify_attendees, the calendar sends each attendee its standard update notice "
            "(event title, new time and the reason, if given), the usual way to tell attendees about a move.",
            {
                "event_id": STRING,
                "start": {"type": "string", "description": "YYYY-MM-DDTHH:MM"},
                "end": {"type": "string", "description": "YYYY-MM-DDTHH:MM"},
                "notify_attendees": {"type": "boolean"},
                "reason": {"type": "string", "description": "Why the event moved; shown to attendees in the notice."},
            },
            ["event_id", "start", "end"],
        ),
        update_event,
    ),
    (
        _schema(
            "gmail_send",
            "Send an email from the user's account.",
            {"to": STRING, "subject": STRING, "body": STRING},
            ["to", "subject", "body"],
        ),
        gmail_send,
    ),
]


def register(ctx: Any) -> None:
    for schema, handler in TOOLS:
        ctx.register_tool(name=schema["name"], toolset=TOOLSET, schema=schema, handler=handler)

"""The demo policy's externals: one consult on stdin, one answer on stdout.

externals.py audience <calendar.json>   the `cal` audience source
    viewer             the calendar owner
    org/members        everyone in the organization
    event/<event-id>   the attendees of one event
externals.py drop-reason                removes `reason` from a call's arguments
"""

import json
import sys
from typing import Any

SERVED_TEMPLATES = ["viewer", "org/members", "event/<event-id>"]


def members(calendar: dict[str, Any], selector: str) -> list[str]:
    match selector.split("/"):
        case ["viewer"]:
            return [calendar["owner"]]
        case ["org", "members"]:
            return calendar["organization"]
        case ["event", event_id] if event_id in calendar["events"]:
            return calendar["events"][event_id]["attendees"]
        case _:
            raise ValueError(f"{selector!r} names no collection this source serves")


def audience(calendar_path: str, request: dict[str, Any]) -> dict[str, Any]:
    if request.get("kind") != "audience" or request.get("name") != "cal":
        raise ValueError("unexpected consult")
    if request.get("declaration", {}).get("templates") != SERVED_TEMPLATES:
        raise ValueError("the policy declares other templates than this source serves")
    with open(calendar_path) as file:
        calendar = json.load(file)
    match request.get("artifact"):
        case {"selector": str(selector)}:
            return {"members": members(calendar, selector)}
        case {"member": str()}:
            return {"principal": None}
        case _:
            raise ValueError("the artifact must carry exactly a selector or a member")


def drop_reason(request: dict[str, Any]) -> dict[str, Any]:
    match request:
        case {"kind": "sanitizer", "declaration": {"on": "tool_input"}, "artifact": {"body": str(body)}}:
            arguments = json.loads(body)
            arguments.pop("reason", None)
            return {"body": json.dumps(arguments)}
        case _:
            raise ValueError("unexpected consult")


def main() -> None:
    request = json.load(sys.stdin)
    match sys.argv[1:]:
        case ["audience", calendar_path]:
            answer = audience(calendar_path, request)
        case ["drop-reason"]:
            answer = drop_reason(request)
        case _:
            raise ValueError(f"unknown external {sys.argv[1:]}")
    json.dump({"version": 1, "answer": answer}, sys.stdout)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError) as error:
        print(f"externals: {error}", file=sys.stderr)
        raise SystemExit(1)

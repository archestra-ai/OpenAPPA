"""Seed the showcase chats into kagent's session store.

Replays real transcripts of the demo cases — captured from the kagent
dashboard's own sessions — into the controller's session API, so the
dashboard opens with every case already on screen under the cluster-ops
agent. The dashboard renders a chat from its A2A tasks, so each showcase
becomes one session plus its tasks, with fresh ids derived
deterministically from the release, which makes re-runs idempotent.

Stdlib only; runs as a Helm post-install/post-upgrade hook.
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

CONTROLLER = os.environ["KAGENT_CONTROLLER_URL"].rstrip("/")
USER = os.environ.get("SEED_USER", "admin@kagent.dev")
AGENT_REF = os.environ["SEED_AGENT_REF"]
RELEASE = os.environ.get("SEED_RELEASE", "appa-kagent-demo")
FIXTURE = os.environ.get("SEED_FIXTURE", "/seed/showcase-sessions.json")
WAIT_S = float(os.environ.get("SEED_WAIT_SECONDS", "600"))

# Website scenario order (newest first): seed the reverse.
ORDER = [
    "exfil",
    "ingress",
    "hitl",
    "delegation",
    "annotator",
]

# Exact IDs owned by the shipped seed Job, never user-created chats.
REPLACED_KEYS = [
    *ORDER,
    "change-board-approve",
    "pods",
    "sanitized-default",
    "steer-accept",
    "steer-decline",
    "forged",
    "release-window",
    "release-window-deny",
    "delegation-denied",
    "change-board-deny",
    "change-board-silent",
]


def api(method: str, path: str, body: dict | None = None) -> tuple[int, dict]:
    query = ("&" if "?" in path else "?") + "user_id=" + urllib.parse.quote(USER)
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        CONTROLLER + "/api" + path + query, data=data, method=method, headers={"content-type": "application/json"}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        try:
            return error.code, json.loads(error.read() or b"{}")
        except ValueError:
            return error.code, {}


def stable(name: str) -> str:
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"appa-kagent-demo/{RELEASE}/{name}"))


def remap_task(key: str, index: int, task: dict, session_id: str) -> dict:
    ids: dict[str, str] = {}

    def fresh(kind: str, old: object) -> object:
        if not isinstance(old, str) or not old:
            return old
        return ids.setdefault(old, stable(f"{key}/{index}/{kind}/{old}"))

    task = json.loads(json.dumps(task))
    task["id"] = fresh("task", task["id"])
    task["contextId"] = session_id
    for message in task.get("history") or []:
        message["messageId"] = fresh("message", message.get("messageId"))
        message["contextId"] = session_id
        message["taskId"] = task["id"]
        metadata = message.get("metadata")
        if isinstance(metadata, dict) and "kagent_session_id" in metadata:
            metadata["kagent_session_id"] = session_id
    metadata = task.get("metadata")
    if isinstance(metadata, dict) and "kagent_session_id" in metadata:
        metadata["kagent_session_id"] = session_id
    for artifact in task.get("artifacts") or []:
        artifact["artifactId"] = fresh("artifact", artifact.get("artifactId"))
    return task


def wait_for_agent() -> None:
    """The agent row appears when the controller reconciles the Agent CR."""
    deadline = time.time() + WAIT_S
    while time.time() < deadline:
        # A fresh id per attempt: a deleted session keeps its row, and the
        # controller's create-then-read on a reused id finds no live row.
        probe = str(uuid.uuid4())
        status, body = api("POST", "/sessions", {"agent_ref": AGENT_REF, "id": probe, "name": "[appa seed probe]"})
        if status in (200, 201):
            api("DELETE", f"/sessions/{probe}")
            return
        print(f"[seed] waiting for the agent {AGENT_REF}: {status} {body.get('message') or body.get('error')}", flush=True)
        time.sleep(10)
    sys.exit(f"[seed] the agent {AGENT_REF} did not become available within {WAIT_S:.0f}s")


def main() -> None:
    with open(FIXTURE) as fixture:
        showcases = json.load(fixture)
    if set(showcases) != set(ORDER):
        sys.exit("[seed] fixtures must contain exactly the five website scenarios")
    wait_for_agent()
    seeded = 0
    for key in reversed(ORDER):
        case = showcases[key]
        # kagent soft-deletes sessions. Fresh IDs let the new recordings
        # replace the shipped chats without reusing their deleted rows/tasks.
        session_id = stable(f"website/session/{key}")
        status, body = api("POST", "/sessions", {"agent_ref": AGENT_REF, "id": session_id, "name": case["name"]})
        if status not in (200, 201):
            sys.exit(f"[seed] session {key}: {status} {body}")
        status, existing = api("GET", f"/sessions/{session_id}/tasks")
        if status != 200:
            sys.exit(f"[seed] list tasks {key}: {status} {existing}")
        tasks = [remap_task(f"website/{key}", index, task, session_id) for index, task in enumerate(case["tasks"])]
        existing_ids = {task["id"] for task in existing.get("data") or []}
        if all(task["id"] in existing_ids for task in tasks):
            print(f"[seed] {key}: already seeded ({len(existing['data'])} tasks)", flush=True)
            continue
        for index, task in enumerate(tasks):
            if task["id"] in existing_ids:
                continue
            status, body = api("POST", "/tasks", task)
            if status not in (200, 201):
                sys.exit(f"[seed] task {key}/{index}: {status} {body}")
        seeded += 1
        print(f"[seed] {key}: {len(case['tasks'])} task(s) under session {session_id}", flush=True)
    status, sessions = api("GET", "/sessions")
    if status != 200:
        sys.exit(f"[seed] list sessions: {status} {sessions}")
    replaced_ids = {stable(f"session/{key}") for key in REPLACED_KEYS}
    replaced_ids.update(stable(f"website/session/{key}") for key in ("change-board-approve", "pods"))
    for session in sessions.get("data") or []:
        if session["id"] not in replaced_ids:
            continue
        # Deleting a kagent session does not remove its retrievable tasks.
        status, tasks = api("GET", f"/sessions/{session['id']}/tasks")
        if status != 200:
            sys.exit(f"[seed] list replaced tasks {session['id']}: {status} {tasks}")
        for task in tasks.get("data") or []:
            status, body = api("DELETE", f"/tasks/{task['id']}")
            if status not in (200, 204, 404):
                sys.exit(f"[seed] remove replaced task {task['id']}: {status} {body}")
        status, body = api("DELETE", f"/sessions/{session['id']}")
        if status not in (200, 204, 404):
            sys.exit(f"[seed] remove replaced session {session['id']}: {status} {body}")
    print(f"[seed] done: {seeded} showcase chat(s) seeded for {USER} on {AGENT_REF}", flush=True)


if __name__ == "__main__":
    main()

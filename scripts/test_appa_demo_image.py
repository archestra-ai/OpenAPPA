#!/usr/bin/env python3
"""Check the built demo image without an inference key or model calls."""

import json
import sys
import urllib.error
import urllib.request


def check(base):
    origin = "https://www.openappa.com"

    def request(path, body=None, method=None):
        req = urllib.request.Request(
            base + path,
            data=json.dumps(body).encode() if body is not None else None,
            method=method,
            headers={"Content-Type": "application/json", "Origin": origin},
        )
        try:
            response = urllib.request.urlopen(req, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            assert response.headers.get("Access-Control-Allow-Origin") == origin
            raw = response.read()
            return response.status, json.loads(raw) if raw else None

    status, preset = request("/preset")
    assert status == 200, preset
    # A current-workspace build must not silently replace the pinned policy.
    assert "[authority.mandate]" in preset["policy"], preset["policy"]
    body = {
        "policy": preset["policy"],
        "systems": [system["id"] for system in preset["systems"]],
        "model": "openai/gpt-5.6-terra",
    }
    status, checked = request("/policy/check", body)
    assert status == 200 and checked["ok"] is True, checked
    assert checked["tools"] == 8, checked

    status, session = request("/session", body)
    assert status == 200, session
    try:
        assert session["tools"] == 8, session
        assert session["trust"] == "trusted" and session["audience"] == "public", session
        for model in ("openai/gpt-5.6-luna", "unlisted/model"):
            status, error = request("/session", {**body, "model": model})
            assert status == 422 and "allowlist" in error["error"], error
    finally:
        status, result = request("/session/" + session["session"], method="DELETE")
        assert status == 204, result
    print("PASS: pinned policy, Terra session, model refusals, CORS, and session cleanup")


if __name__ == "__main__":
    check(sys.argv[1].rstrip("/"))

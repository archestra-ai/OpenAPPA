#!/usr/bin/env python3
"""Fingerprint the official Linear MCP definitions without invoking any tools.

JSON on stdout, diagnostics on stderr; no policy files are modified.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
import re
import sys
from urllib.request import Request, build_opener, HTTPRedirectHandler

ENDPOINTS = {"read-write": "https://mcp.linear.app/mcp", "read-only": "https://mcp.linear.app/mcp/readonly"}
TOKEN_ENV = "APPA_PROVIDER_LINEAR_TOKEN"
MAX_BYTES = 8 * 1024 * 1024
MAX_PAGES = 100
VERSION = "2025-03-26"


class CaptureError(ValueError):
    pass


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise CaptureError("MCP redirected the authenticated request")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def tool_map(tools):
    if not isinstance(tools, list):
        raise CaptureError("tools/list did not return a tool array")
    result = {}
    for tool in tools:
        if not isinstance(tool, dict):
            raise CaptureError("tool is not an object")
        name = tool.get("name")
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+", name):
            raise CaptureError("invalid tool name")
        if name in result:
            raise CaptureError("duplicate tool: " + name)
        if not isinstance(tool.get("inputSchema"), dict):
            raise CaptureError("tool has no input schema: " + name)
        result[name] = tool
    return result


def decode_response(raw, content_type, request_id):
    try:
        if "text/event-stream" in content_type:
            messages = []
            # MCP may emit notifications before the response. Join multiline SSE
            # data fields and select the matching JSON-RPC response, not the last event.
            for event in raw.decode().replace("\r\n", "\n").split("\n\n"):
                data = "\n".join(line[5:].lstrip(" ") for line in event.splitlines() if line.startswith("data:"))
                if data:
                    messages.append(json.loads(data))
        else:
            messages = [json.loads(raw)]
    except (ValueError, UnicodeDecodeError):
        raise CaptureError("MCP returned invalid JSON/SSE") from None
    matches = [m for m in messages if isinstance(m, dict) and type(m.get("id")) is int and m["id"] == request_id]
    if len(matches) != 1 or matches[0].get("jsonrpc") != "2.0":
        raise CaptureError("MCP returned no unique response for this request")
    message = matches[0]
    if "error" in message or not isinstance(message.get("result"), dict):
        raise CaptureError("MCP request failed; no capture was produced")
    return message["result"]


class Client:
    def __init__(self, endpoint, token, opener=None):
        self.endpoint = endpoint
        self.token = token
        self.opener = opener or build_opener(NoRedirect())
        self.session = None
        self.protocol = None
        self.sequence = 0

    def send(self, method, params, notification=False):
        self.sequence += 1
        payload = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notification:
            payload["id"] = self.sequence
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                   "Authorization": "Bearer " + self.token}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        if self.protocol:
            headers["MCP-Protocol-Version"] = self.protocol
        request = Request(self.endpoint, data=canonical(payload), headers=headers)
        try:
            with self.opener.open(request, timeout=30) as response:
                session = response.headers.get("Mcp-Session-Id")
                if session:
                    if self.session and self.session != session:
                        raise CaptureError("MCP session changed during capture")
                    self.session = session
                if notification:
                    return None
                raw = response.read(MAX_BYTES + 1)
                content_type = response.headers.get("Content-Type", "")
        except CaptureError:
            raise
        except Exception:
            raise CaptureError("MCP request failed; check Linear credentials and connectivity") from None
        if len(raw) > MAX_BYTES:
            raise CaptureError("MCP response exceeds capture size limit")
        return decode_response(raw, content_type, self.sequence)


def capture(client):
    initialized = client.send("initialize", {
        "protocolVersion": VERSION, "capabilities": {},
        "clientInfo": {"name": "appa-linear-schema-capture", "version": "1"}})
    protocol = initialized.get("protocolVersion")
    if protocol not in ("2024-11-05", "2025-03-26", "2025-06-18"):
        raise CaptureError("unsupported negotiated MCP version")
    capabilities = initialized.get("capabilities")
    if not isinstance(capabilities, dict) or not isinstance(capabilities.get("tools"), dict):
        raise CaptureError("server does not advertise tools")
    client.protocol = protocol
    client.send("notifications/initialized", {}, notification=True)
    tools = {}
    cursors = set()
    params = {}
    for _ in range(MAX_PAGES):
        response = client.send("tools/list", params)
        page = tool_map(response.get("tools"))
        if tools.keys() & page.keys():
            raise CaptureError("tools/list repeats tools across pages")
        tools.update(page)
        cursor = response.get("nextCursor")
        if cursor is None:
            if not tools:
                raise CaptureError("empty inventory cannot establish full coverage")
            ordered = [tools[name] for name in sorted(tools)]
            return {"protocol_version": protocol, "server_info": initialized.get("serverInfo", {}),
                    "tools": ordered, "sha256": hashlib.sha256(canonical(ordered)).hexdigest()}
        if not isinstance(cursor, str) or not cursor or cursor in cursors:
            raise CaptureError("tools/list returned an invalid or repeated cursor")
        cursors.add(cursor)
        params = {"cursor": cursor}
    raise CaptureError("tools/list exceeds pagination limit")


def fingerprints(captured):
    """Hash complete definitions, including schemas, descriptions and annotations."""
    return {**captured, "schema_version": 2, "surfaces": {
        surface: {**data, "tools": {
            name: hashlib.sha256(canonical(tool)).hexdigest()
            for name, tool in sorted(tool_map(data["tools"]).items())}}
        for surface, data in captured["surfaces"].items()}}


def validate_lock(lock):
    if lock.get("schema_version") != 2:
        raise CaptureError("expected a schema lockfile")
    for surface in ENDPOINTS:
        hashes = lock["surfaces"][surface]["tools"]
        if not isinstance(hashes, dict) or not hashes or any(
                not isinstance(value, str) or not re.fullmatch(r"[a-f0-9]{64}", value)
                for value in hashes.values()):
            raise CaptureError("invalid tool fingerprints")


def drift(before, after):
    validate_lock(before)
    validate_lock(after)
    changes = {}
    for surface in ENDPOINTS:
        old = before["surfaces"][surface]["tools"]
        new = after["surfaces"][surface]["tools"]
        changes[surface] = {"added": sorted(new.keys() - old.keys()),
                            "removed": sorted(old.keys() - new.keys()),
                            "changed": sorted(name for name in old.keys() & new.keys() if old[name] != new[name])}
    return changes


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False,
        epilog="Requires APPA_PROVIDER_LINEAR_TOKEN. Example: python3 capture.py > /tmp/linear-candidate.json. "
               "Read both stdout and the exit code: 0 success, 1 failure, 2 usage, 3 schema drift. No tool calls or policy edits.")
    parser.add_argument("--compare", metavar="CAPTURE", help="compare against a schema lockfile; include drift in output and exit 3 when changed")
    parser.add_argument("--full", action="store_true", help="emit full tool definitions for local review instead of the default hash lockfile")
    args = parser.parse_args()
    try:
        previous = None
        if args.compare:
            with open(args.compare) as stream:
                previous = json.load(stream)
            validate_lock(previous)
        token = os.environ.get(TOKEN_ENV, "")
        if not token or any(c in token for c in "\r\n"):
            raise CaptureError("set " + TOKEN_ENV + " before capturing")
        surfaces = {}
        for surface, endpoint in ENDPOINTS.items():
            surfaces[surface] = {"endpoint": endpoint, **capture(Client(endpoint, token))}
        if not set(tool_map(surfaces["read-only"]["tools"])).issubset(tool_map(surfaces["read-write"]["tools"])):
            raise CaptureError("read-only inventory is not a subset of read-write inventory")
        writable = tool_map(surfaces["read-write"]["tools"])
        if any(tool["inputSchema"] != writable[name]["inputSchema"]
               for name, tool in tool_map(surfaces["read-only"]["tools"]).items()):
            raise CaptureError("read-only and read-write schemas differ; review separate contracts")
        result = {"schema_version": 1, "captured_at": datetime.now(timezone.utc).isoformat(), "surfaces": surfaces}
        lock = fingerprints(result)
        changes = drift(previous, lock) if previous else None
        if not args.full:
            result = lock
        if changes is not None:
            result["drift"] = changes
        json.dump(result, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 3 if changes and any(items for surface in changes.values() for items in surface.values()) else 0
    except (CaptureError, OSError, ValueError, KeyError, TypeError):
        print("Linear schema capture failed: check capture input, APPA_PROVIDER_LINEAR_TOKEN, and connectivity; no capture produced.", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

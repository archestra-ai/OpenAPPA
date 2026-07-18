#!/usr/bin/env python3
"""Render an A2A message/send response as a readable transcript.

Reads the JSON-RPC response on stdin, prints the conversation: user turns,
agent text, tool calls with their arguments, truncated tool results, and the
final reply. `invoke-agent.sh` pipes through this by default; pass --raw
there to get the untouched JSON back.
"""

import json
import sys

DIM = "\033[2m"
BOLD = "\033[1m"
RED = "\033[31m"
GREEN = "\033[32m"
CYAN = "\033[36m"
RESET = "\033[0m"
if not sys.stdout.isatty():
    DIM = BOLD = RED = GREEN = CYAN = RESET = ""

RESULT_LINES = 8


def text_parts(parts):
    return "\n".join(p.get("text", "") for p in parts if p.get("kind") == "text").strip()


def indent(text, prefix="    "):
    return "\n".join(prefix + line for line in text.splitlines())


def truncated(text, limit=RESULT_LINES):
    lines = text.splitlines()
    if len(lines) <= limit:
        return text
    return "\n".join(lines[:limit]) + f"\n… ({len(lines) - limit} more lines)"


def render_data(part):
    data = part.get("data", {})
    kind = part.get("metadata", {}).get("kagent_type", "")
    if kind == "function_call":
        args = json.dumps(data.get("args", {}))
        print(f"  {CYAN}⚙ call{RESET} {BOLD}{data.get('name', '?')}{RESET} {DIM}{args}{RESET}")
    elif kind == "function_response":
        content = data.get("response", {}).get("content", [])
        text = "\n".join(c.get("text", "") for c in content if c.get("type") == "text").strip()
        error = data.get("response", {}).get("isError", False)
        mark = f"{RED}✗{RESET}" if error else f"{DIM}↳{RESET}"
        print(f"  {mark} {DIM}{data.get('name', '?')}{RESET}")
        if text:
            print(f"{DIM}{indent(truncated(text))}{RESET}")
    else:
        name = data.get("name") or kind or "data"
        print(f"  {DIM}({name}){RESET}")


def main():
    doc = json.load(sys.stdin)
    if "error" in doc:
        print(f"{RED}A2A error:{RESET} {json.dumps(doc['error'], indent=2)}")
        return 1
    result = doc.get("result", {})

    seen = set()
    for msg in result.get("history", []):
        key = (msg.get("messageId"), msg.get("role"))
        if key in seen:
            continue  # the server echoes the user message twice
        seen.add(key)
        role = msg.get("role", "?")
        text = text_parts(msg.get("parts", []))
        if text:
            label = f"{BOLD}you{RESET}" if role == "user" else f"{BOLD}agent{RESET}"
            print(f"{label}  {text}")
        for part in msg.get("parts", []):
            if part.get("kind") == "data":
                render_data(part)
        print()

    finals = [
        text_parts(a.get("parts", []))
        for a in result.get("artifacts", [])
        if text_parts(a.get("parts", []))
    ]
    if finals:
        print(f"{BOLD}── reply ──{RESET}")
        for final in finals:
            print(final)
    state = result.get("status", {}).get("state")
    if state and state != "completed":
        print(f"\n{DIM}task state: {state}{RESET}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

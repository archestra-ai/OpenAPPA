from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

from .appa_source_gate import SourceGate


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(fixtures_dir: Path, trace_path: Path, summary_path: Path, runtime_url: str) -> None:
    public = fixtures_dir / "public.txt"
    protected = fixtures_dir / "protected.txt"
    before_protected = sha256(protected)
    results: dict[str, object] = {"protected_sha256_before": before_protected}

    allow = SourceGate("fixture-allow", runtime_url=runtime_url, trace_path=trace_path)
    results["allow_session_start"] = allow.session_start().name
    results["allow_prompt"] = allow.prompt("Read the public fixture.").name
    read = allow.before_call("read-public", "read_fixture", {"path": "public.txt"})
    assert read.allowed, read.payload
    results["read_fixture"] = read.name
    results["read_result"] = allow.after_result("read-public", body={"text": public.read_text(encoding="utf-8")}).name
    results["allow_turn_end"] = allow.turn_end().name

    deny = SourceGate("fixture-deny", runtime_url=runtime_url, trace_path=trace_path)
    results["deny_session_start"] = deny.session_start().name
    results["deny_prompt"] = deny.prompt("Write to protected.txt.").name
    blocked = deny.before_call("write-protected", "write_file", {"path": "protected.txt", "content": "MUST NOT EXECUTE"})
    assert blocked.name == "deny_call", blocked.payload
    assert blocked.offers, blocked.payload
    results["protected_write"] = blocked.name
    results["protected_write_offer_count"] = len(blocked.offers)
    remedy = deny.begin_remedy(blocked.offers[0]["offer_id"])
    assert remedy.name == "pass_control", remedy.payload
    results["remedy_vouch"] = remedy.name
    results["deny_turn_end"] = deny.turn_end().name

    unknown = SourceGate("fixture-unknown", runtime_url=runtime_url, trace_path=trace_path)
    results["unknown_session_start"] = unknown.session_start().name
    unknown_decision = unknown.before_call("unknown-write", "write_file", {"path": "other.txt", "content": "MUST NOT EXECUTE"})
    assert unknown_decision.name == "deny_call", unknown_decision.payload
    results["undeclared_write"] = unknown_decision.name
    results["unknown_turn_end"] = unknown.turn_end().name

    results["protected_sha256_after"] = sha256(protected)
    assert results["protected_sha256_before"] == results["protected_sha256_after"]
    results["protected_file_unchanged"] = True
    summary_path.write_text(json.dumps(results, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description="Run synthetic OpenAPPA gate fixtures against an explicitly configured runtime")
    parser.add_argument("--fixtures-dir", type=Path, required=True)
    parser.add_argument("--trace", type=Path, required=True)
    parser.add_argument("--summary", type=Path, required=True)
    parser.add_argument("--runtime-url", default="http://127.0.0.1:18791", help="locally forwarded source-runtime HTTP endpoint")
    args = parser.parse_args()
    run(args.fixtures_dir, args.trace, args.summary, args.runtime_url)


if __name__ == "__main__":
    main()

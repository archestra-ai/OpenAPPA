"""Standalone root-only provider relay with authenticated APPA review mediation."""
from __future__ import annotations

import argparse
from pathlib import Path

from .appa_review_gate import factory as review_gate_factory
from .proxy import CaptureLogger, GateMediator, GateTrace, MappingStore, ProxyServer, load_provider_keys, parse_archestra_base


def main() -> None:
    parser = argparse.ArgumentParser(description="Root-only Archestra relay with authenticated OpenAPPA review")
    parser.add_argument("--port", type=int, default=18772)
    parser.add_argument("--archestra-base", required=True)
    parser.add_argument("--keys-file", type=Path, required=True)
    parser.add_argument("--appa-runtime", default="http://127.0.0.1:18788")
    parser.add_argument("--appa-mcp-host", required=True, help="runtime service Host header for MCP")
    parser.add_argument("--review-wait-timeout", type=float, default=2160, help="bounded MCP authority wait in seconds (maximum 3600)")
    parser.add_argument("--logs", type=Path, required=True)
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--appa-trace", type=Path, required=True)
    args = parser.parse_args()
    try:
        archestra_base = parse_archestra_base(args.archestra_base)
        keys = load_provider_keys(args.keys_file)
    except (OSError, UnicodeDecodeError, ValueError) as error:
        parser.error(str(error))
    store = MappingStore(args.db)
    server = ProxyServer(
        ("127.0.0.1", args.port), store, CaptureLogger(args.logs), archestra_base, keys,
        GateMediator(
            review_gate_factory(args.appa_runtime, args.appa_mcp_host, args.review_wait_timeout),
            GateTrace(args.appa_trace),
        ),
    )
    try:
        server.serve_forever()
    finally:
        server.server_close()
        store.close()


if __name__ == "__main__":
    main()

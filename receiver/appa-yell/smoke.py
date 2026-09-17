"""Post one report to a deployed endpoint and check the receipt.

This is the accepted path: the unit suite stops before storage, so what is
proven here is the part only a real bucket can prove — that a well-formed
document is stored, and that the same document sent twice is one object and
not two.
"""

import gzip
import hashlib
import hmac
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

SALT = (Path(__file__).parent / "salt.txt").read_text().strip().encode()


def report(run: str) -> dict[str, Any]:
    """One report that is honest about being a smoke test, so a reader can skip it.

    `run` identifies the attempt, not the workflow run: a re-run of the same run has
    to post something that was never stored, or the duplicate check below would fail
    against a perfectly healthy endpoint.
    """
    return {
        "schema": "openappa.yell.v1",
        "report_id": f"ci-{run}",
        "created_at": "1970-01-01T00:00:00Z",
        "origin": {"kind": "ci", "pseudonymized": True},
        "message": f"deploy smoke test for {run}; no human wrote this",
        "build": {"version": "0.0.0", "source": {"kind": "ci"}},
        "runtime": {"serving": None},
        "trajectory": {"omitted_reason": "no_recent_trajectory"},
        "unclassified": [],
    }


def post(endpoint: str, plain: bytes) -> dict[str, Any]:
    request = urllib.request.Request(
        endpoint,
        data=gzip.compress(plain),
        headers={
            "Content-Type": "application/json",
            "Content-Encoding": "gzip",
            "X-Appa-Signature": "v1=" + hmac.new(SALT, plain, hashlib.sha256).hexdigest(),
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as answer:
            return json.loads(answer.read())
    except urllib.error.HTTPError as failure:
        sys.exit(f"the endpoint refused the report: {failure.code} {failure.read()[:512]!r}")


def main() -> None:
    endpoint = os.environ["APPA_YELL_ENDPOINT"]
    attempt = f"{os.environ['GITHUB_RUN_ID']}-{os.environ.get('GITHUB_RUN_ATTEMPT', '1')}"
    plain = json.dumps(report(attempt)).encode()

    first = post(endpoint, plain)
    assert not first["duplicate"], f"the smoke report was already stored: {first}"

    again = post(endpoint, plain)
    assert again["duplicate"], f"the same bytes were stored twice: {again}"
    assert again["receipt_id"] == first["receipt_id"], "the same report got two receipts"

    unsigned = urllib.request.Request(
        endpoint,
        data=gzip.compress(plain),
        headers={"Content-Type": "application/json", "Content-Encoding": "gzip"},
        method="POST",
    )
    try:
        urllib.request.urlopen(unsigned, timeout=30)
        sys.exit("the deployed endpoint accepted an unsigned report")
    except urllib.error.HTTPError as refusal:
        assert refusal.code == 401, f"an unsigned report was refused with {refusal.code}, not 401"

    print(f"stored {first['receipt_id']}")  # noqa: T201 — this is the job's output


if __name__ == "__main__":
    main()

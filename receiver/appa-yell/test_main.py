"""What this endpoint refuses, exercised through the real function app.

Every case here ends before storage is reached, which is why the suite needs no
bucket and no stand-in for one: a request that is not a signed, well-formed
`openappa.yell.v1` document never gets as far as writing anything. The accepted
path is covered by the deploy smoke test in `.github/workflows/appa-yell.yml`,
against the real bucket.
"""

import gzip
import hashlib
import hmac
import json
import re
from pathlib import Path

import functions_framework
import pytest

import main


@pytest.fixture
def client():
    """The real function app, resolved by path so the suite runs from anywhere."""
    return functions_framework.create_app("receive", str(Path(__file__).parent / "main.py")).test_client()


def one_report(**overrides) -> dict:
    document = {
        "schema": "openappa.yell.v1",
        "report_id": "9f1d0d3e-0000-4000-8000-000000000000",
        "created_at": "2026-09-05T00:00:00Z",
        "origin": {"kind": "cli", "pseudonymized": True},
        "message": "the hook blocked a Bash call I needed",
        "build": {"version": "0.9.0", "source": {"kind": "local"}},
        "runtime": {"serving": {"harness": "claude_code", "policy": None}},
        "trajectory": {"omitted_reason": "no_recent_trajectory"},
        "unclassified": [],
    }
    return document | overrides


def post(client, document=None, *, plain=None, signature=None, encoding="gzip"):
    """One request as the client makes it: gzipped body, signature over the document."""
    if plain is None:
        plain = json.dumps(document).encode()
    headers = {"Content-Type": "application/json"}
    if encoding is not None:
        headers["Content-Encoding"] = encoding
    if signature is None:
        signature = "v1=" + hmac.new(main.SALT, plain, hashlib.sha256).hexdigest()
    if signature != "":
        headers["X-Appa-Signature"] = signature
    return client.post("/", data=gzip.compress(plain), headers=headers)


def test_only_a_post_is_a_report(client):
    answer = client.get("/")
    assert answer.status_code == 405
    assert answer.headers["Allow"] == "POST"


def test_a_document_without_the_salt_is_refused_before_anything_else(client):
    answer = post(client, one_report(), signature="v1=" + "0" * 64)
    assert answer.status_code == 401

    assert post(client, one_report(), signature="").status_code == 401
    assert post(client, one_report(), signature="deadbeef").status_code == 401

    # A header arrives as latin-1 and can hold bytes no digest ever does. The
    # comparison raises on those rather than answering false.
    assert post(client, one_report(), signature="v1=" + "\xff" * 64).status_code == 401


def test_a_body_that_is_not_gzip_is_refused(client):
    assert post(client, one_report(), encoding=None).status_code == 415
    assert post(client, one_report(), encoding="identity").status_code == 415


def test_a_truncated_gzip_stream_is_not_a_document(client):
    plain = json.dumps(one_report()).encode()
    truncated = gzip.compress(plain)[:-8]
    signature = "v1=" + hmac.new(main.SALT, plain, hashlib.sha256).hexdigest()
    answer = client.post(
        "/",
        data=truncated,
        headers={"Content-Encoding": "gzip", "X-Appa-Signature": signature},
    )
    assert answer.status_code == 400


def test_a_document_that_expands_past_the_cap_is_refused_without_being_held(client):
    """A caller is unauthenticated and the signature covers the *document*, so the
    decompression cap is what stands between it and this instance's memory."""
    flood = b"\x00" * (main.MAX_PLAIN_BYTES + 1024)
    signature = "v1=" + hmac.new(main.SALT, flood, hashlib.sha256).hexdigest()
    answer = client.post(
        "/",
        data=gzip.compress(flood),
        headers={"Content-Encoding": "gzip", "X-Appa-Signature": signature},
    )
    assert answer.status_code == 413


def test_a_second_gzip_member_behind_the_document_is_refused(client):
    """gzip concatenates. A member behind the first is never decompressed here, so
    a body carrying one would be stored without having been checked, and what a
    reader expands would not be what this endpoint validated or measured."""
    plain = json.dumps(one_report()).encode()
    body = gzip.compress(plain) + gzip.compress(b"\x00" * (main.MAX_PLAIN_BYTES + 1))
    assert len(body) < main.MAX_COMPRESSED_BYTES, "the smuggled member has to fit under the wire cap"
    answer = client.post(
        "/",
        data=body,
        headers={
            "Content-Encoding": "gzip",
            "X-Appa-Signature": "v1=" + hmac.new(main.SALT, plain, hashlib.sha256).hexdigest(),
        },
    )
    assert answer.status_code == 400


def test_a_signed_document_cannot_raise_past_the_refusals(client):
    """Anyone can sign, because the salt is public. So every shape reachable with a
    valid signature has to land on a refusal rather than an unhandled exception."""
    nested = b'{"a":' * 4000 + b"1" + b"}" * 4000
    assert post(client, plain=nested).status_code == 400

    surrogate = json.dumps(one_report(message="\ud800"), ensure_ascii=True).encode()
    assert post(client, plain=surrogate).status_code == 400


def test_the_decompression_cap_is_the_clients_own_limit():
    """The two have to agree. A document a runtime is willing to send and this
    endpoint is not willing to hold would fail every attempt, forever, and the
    client's size loop cannot see this limit to build under it."""
    rust = (Path(__file__).parents[2] / "appa-runtime/src/yell/report.rs").read_text()
    declared = re.search(r"MAX_PLAIN_BYTES: usize = ([0-9 */+]+);", rust)
    assert declared, "the client still declares a plain-size limit"
    assert eval(declared.group(1)) == main.MAX_PLAIN_BYTES  # noqa: S307 — arithmetic from our own source


@pytest.mark.parametrize(
    ("document", "refusal"),
    [
        (one_report(schema="openappa.yell.v2"), 400),
        (one_report(message=""), 400),
        (one_report(message="   "), 400),
        (one_report(message=42), 400),
        (one_report(origin="cli"), 400),
        (one_report(origin=["cli"]), 400),
        (one_report(message="x" * (main.MAX_MESSAGE_BYTES + 1)), 413),
    ],
)
def test_a_document_this_endpoint_cannot_store_is_refused(client, document, refusal):
    assert post(client, document).status_code == refusal


def test_the_envelope_is_strict_and_says_what_is_wrong(client):
    missing = one_report()
    del missing["build"]
    answer = post(client, missing)
    assert answer.status_code == 400
    assert "build" in answer.get_json()["error"]

    extra = one_report(endpoint="https://evil.example")
    answer = post(client, extra)
    assert answer.status_code == 400
    assert "endpoint" in answer.get_json()["error"]


def test_a_trajectory_is_opaque_below_the_envelope():
    """A newer runtime carries fields its classification tables gained, and that is
    exactly the runtime worth hearing from. Only the envelope is checked."""
    document = one_report(
        trajectory={
            "facts": [{"seq": 1, "fact": {"a_variant_from_the_future": {"nested": [1, 2]}}}],
            "runtime_events": [],
            "branches": [],
            "something_added_later": True,
        }
    )
    assert main.validated(json.dumps(document).encode()) == document


def test_more_entries_than_a_session_has_is_refused(client):
    document = one_report(
        trajectory={
            "facts": [{"seq": n} for n in range(main.MAX_ENTRIES + 1)],
            "runtime_events": [],
        }
    )
    assert post(client, document).status_code == 413


def test_entries_counts_both_lists_and_survives_a_report_with_none():
    assert main.entries(one_report()) == 0
    assert main.entries(one_report(trajectory="not an object")) == 0
    assert main.entries(one_report(trajectory={"facts": [1, 2], "runtime_events": [3]})) == 3


def test_the_signature_is_over_the_document_and_not_the_gzip():
    """gzip is not canonical: a timestamp, a compression level and an implementation
    all change the bytes without changing the document. Signing those would make the
    same report sign differently from client to client."""
    plain = json.dumps(one_report()).encode()
    signature = "v1=" + hmac.new(main.SALT, plain, hashlib.sha256).hexdigest()
    main.signed(plain, signature)
    with pytest.raises(main.Refusal):
        main.signed(gzip.compress(plain), signature)


def test_the_salt_is_the_one_the_client_compiles_in():
    """Both sides read one file, so there is nothing to keep in sync — but only
    while the client keeps reading *that* file. This checks it still does."""
    assert main.SALT, "an empty salt would let every request through the filter"
    client_source = (Path(__file__).parents[2] / "appa-runtime/src/yell/client.rs").read_text()
    included = re.search(r'include_str!\("([^"]*salt\.txt)"\)', client_source)
    assert included, "the client still compiles in a salt file"
    compiled = (Path(__file__).parents[2] / "appa-runtime/src/yell" / included.group(1)).resolve()
    assert compiled == (Path(__file__).parent / "salt.txt").resolve()

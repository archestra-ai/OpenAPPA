"""The `openappa.yell.v1` ingestion endpoint.

One unauthenticated POST, one immutable object, one receipt. Nothing here reads
or lists what is stored: a caller can add a report and can learn nothing else.

The signature is a filter, not an identity. The salt ships in the OpenAPPA
repository and is compiled into every build, so it stops a scanner that finds
the URL and a process that posts here by accident, and it proves nothing about
which deployment sent a report. Treat every field below as caller-controlled.
"""

import hashlib
import hmac
import json
import logging
import os
import re
import threading
import time
import urllib.request
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import functions_framework
from google.api_core import exceptions as gcloud_exceptions
from google.auth.exceptions import GoogleAuthError
from google.cloud import storage

logger = logging.getLogger(__name__)

SCHEMA = "openappa.yell.v1"

# The compressed body, and what it may become. The plain cap is the one that has
# to agree with the client's own `MAX_PLAIN_BYTES`: a document a runtime is
# willing to send and this function is not willing to hold would fail every
# attempt forever, and the client's size loop cannot see this limit.
MAX_COMPRESSED_BYTES = 32 * 1024 * 1024
MAX_PLAIN_BYTES = 32 * 1024 * 1024

# The message is the one free-form field, bounded by the client at the same size.
MAX_MESSAGE_BYTES = 64 * 1024

# A trajectory this long is not a session anyone is describing.
MAX_ENTRIES = 200_000

# The envelope. Anything outside it is a document this function does not know how
# to store, and is refused rather than kept for someone to interpret later.
REQUIRED_FIELDS = frozenset(
    {"schema", "report_id", "created_at", "origin", "message", "build", "runtime", "trajectory", "unclassified"}
)

SALT = (Path(__file__).parent / "salt.txt").read_text().strip().encode()

_client: storage.Client | None = None


def bucket() -> storage.Bucket:
    """The one bucket this function writes to, resolved once per instance."""
    global _client
    if _client is None:
        _client = storage.Client()
    return _client.bucket(os.environ["APPA_YELL_BUCKET"])


@dataclass(frozen=True)
class Refusal(Exception):
    """Why a request is not stored, in the words the caller is given back.

    A class rather than the underlying error: the caller is unauthenticated, and
    what it learns about this function's insides is what it gets to work with.
    """

    status: int
    detail: str


def plain_body(request: Any) -> tuple[bytes, bytes]:
    """The document behind the request and the bytes it arrived as, under a hard cap.

    Decompression happens before the signature can be checked, because the
    signature covers the document rather than its compression. So the cap is not
    an optimization: it is the only thing standing between an unauthenticated
    caller and this instance's memory.
    """
    if request.headers.get("Content-Encoding", "").lower() != "gzip":
        raise Refusal(415, "the body must be gzipped")
    declared = request.content_length
    if declared is not None and declared > MAX_COMPRESSED_BYTES:
        raise Refusal(413, "the compressed body is larger than this endpoint accepts")

    # Read the cap plus one rather than the whole body: a declared length is a
    # claim, and a chunked request declares none at all.
    compressed = request.stream.read(MAX_COMPRESSED_BYTES + 1)
    if len(compressed) > MAX_COMPRESSED_BYTES:
        raise Refusal(413, "the compressed body is larger than this endpoint accepts")

    decompressor = zlib.decompressobj(wbits=zlib.MAX_WBITS | 16)
    try:
        plain = decompressor.decompress(compressed, MAX_PLAIN_BYTES + 1)
    except zlib.error:
        raise Refusal(400, "the body is not gzip") from None
    if len(plain) > MAX_PLAIN_BYTES:
        raise Refusal(413, "the document is larger than this endpoint accepts")
    if not decompressor.eof:
        raise Refusal(400, "the gzip stream is truncated")

    # The body has to be one member and nothing else. gzip concatenates: a second
    # member behind the first decompresses to whatever it likes, is never read
    # here, and would be stored anyway — so what is kept would not be what was
    # checked, and the cap above would bound nothing a reader ever sees.
    if decompressor.unused_data or decompressor.unconsumed_tail:
        raise Refusal(400, "the body carries more than one gzip member")
    return plain, compressed


def signed(plain: bytes, header: str | None) -> None:
    """Refuse anything that does not carry the public salt. See the module docstring."""
    if header is None or not header.startswith("v1="):
        raise Refusal(401, "the request carries no v1 signature")
    # A header arrives as latin-1, so it can hold bytes no hex digest ever does,
    # and `compare_digest` raises on a str that is not ASCII rather than
    # answering false. Whatever this is, it is not a signature.
    offered = header[len("v1=") :]
    if not offered.isascii():
        raise Refusal(401, "the signature does not match the document")
    expected = hmac.new(SALT, plain, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(offered, expected):
        raise Refusal(401, "the signature does not match the document")


def validated(plain: bytes) -> dict[str, Any]:
    """One `openappa.yell.v1` document, checked at its envelope and no deeper.

    Strict about the envelope and opaque below it, deliberately. What sits inside
    `trajectory` is decided by the runtime's classification tables and gains
    fields whenever the engine does; validating that here would refuse valid
    reports from a newer runtime, which is exactly the runtime worth hearing from.
    """
    # RecursionError is not a ValueError: a few thousand nested arrays cost almost
    # nothing to compress and would otherwise leave the parser to raise past every
    # handler here. Anyone can sign a document, so anyone can send one.
    try:
        document = json.loads(plain)
    except (ValueError, RecursionError):
        raise Refusal(400, "the document is not JSON") from None
    if not isinstance(document, dict):
        raise Refusal(400, "the document is not an object")
    if document.get("schema") != SCHEMA:
        raise Refusal(400, f"the document does not declare {SCHEMA}")

    present = set(document)
    match sorted(REQUIRED_FIELDS - present), sorted(present - REQUIRED_FIELDS):
        case [], []:
            pass
        case missing, []:
            raise Refusal(400, f"the document is missing {', '.join(missing)}")
        case _, unknown:
            raise Refusal(400, f"the document carries fields this endpoint does not know: {', '.join(unknown)}")

    message = document["message"]
    if not isinstance(message, str) or not message.strip():
        raise Refusal(400, "the message is empty")
    try:
        # JSON admits a lone surrogate; UTF-8 does not. Nobody typed one.
        length = len(message.encode())
    except UnicodeError:
        raise Refusal(400, "the message is not text") from None
    if length > MAX_MESSAGE_BYTES:
        raise Refusal(413, "the message is longer than this endpoint accepts")
    if not isinstance(document["origin"], dict):
        raise Refusal(400, "the origin is not an object")
    if entries(document) > MAX_ENTRIES:
        raise Refusal(413, "the trajectory carries more entries than this endpoint accepts")
    return document


def build_kind(document: dict[str, Any]) -> str:
    """Which kind of build sent this: `release`, `commit` or `local` today, and whatever
    a newer runtime says tomorrow.

    Reports are filed under it, so a release's reports and a developer's own runs sit
    apart behind one endpoint. Refused rather than guessed when the build says nothing:
    this becomes a path segment, and only a plain word may be one.
    """
    match document["build"]:
        case {"source": {"kind": str() as kind}} if re.fullmatch(r"[a-z][a-z0-9_]{0,31}", kind):
            return kind
        case _:
            raise Refusal(400, "the build names no source kind")


def entries(document: dict[str, Any]) -> int:
    """How many facts and runtime events the document carries, if it carries any."""
    trajectory = document.get("trajectory")
    if not isinstance(trajectory, dict):
        return 0
    return sum(len(trajectory[key]) for key in ("facts", "runtime_events") if isinstance(trajectory.get(key), list))


def store(plain: bytes, compressed: bytes, kind: str) -> tuple[str, bool]:
    """Write one report exactly once, and say whether it was already here.

    The name is the digest of the document, under the kind of build that sent it,
    so a retry of the same bytes is the
    same object and two different reports can never be the same one. That also
    keeps a caller from choosing where its report lands: `report_id` is written
    by whoever sent it, and naming objects by it would let one caller overwrite
    another's report.
    """
    # The stored object is the body as it arrived, which `plain_body` has already
    # established is one gzip member and decompresses to exactly this document.
    # Recompressing it would spend a second pass over 32 MiB for the same content.
    digest = hashlib.sha256(plain).hexdigest()
    try:
        # Resolved inside the guard: building the client authenticates, and an
        # instance that cannot reach its credentials is a storage failure like
        # any other rather than an unhandled exception on a public surface.
        blob = bucket().blob(f"reports/{kind}/{digest}.json.gz")
        # Declared with the upload rather than patched onto it afterwards: a
        # second call could fail against an object that is already stored, and
        # this function would answer with a refusal for a report it had kept.
        blob.content_encoding = "gzip"
        # Create-only. A report is immutable once stored, and a second write of
        # the same name is the duplicate this returns rather than an overwrite.
        blob.upload_from_string(compressed, content_type="application/json", if_generation_match=0)
    except gcloud_exceptions.PreconditionFailed:
        return digest, True
    except (gcloud_exceptions.GoogleAPIError, GoogleAuthError, KeyError):
        # KeyError included on purpose: an instance with no `APPA_YELL_BUCKET` has
        # nowhere to write, which is a storage failure the caller cannot read
        # anything into. The log line says which one it was.
        logger.exception("the report could not be stored")
        raise Refusal(503, "the report could not be stored; try again") from None
    return digest, False


_SLACK_LOCK = threading.Lock()
_SLACK_DISPATCH_TIMESTAMPS: list[float] = []
SLACK_RATE_LIMIT_BURST = 10
SLACK_RATE_LIMIT_WINDOW_SECS = 60.0
MAX_SLACK_MESSAGE_CHARS = 2500


def _allow_slack_dispatch() -> bool:
    """Thread-safe rate limiter: allow at most SLACK_RATE_LIMIT_BURST notifications per window."""
    now = time.monotonic()
    cutoff = now - SLACK_RATE_LIMIT_WINDOW_SECS
    with _SLACK_LOCK:
        while _SLACK_DISPATCH_TIMESTAMPS and _SLACK_DISPATCH_TIMESTAMPS[0] < cutoff:
            _SLACK_DISPATCH_TIMESTAMPS.pop(0)
        if len(_SLACK_DISPATCH_TIMESTAMPS) >= SLACK_RATE_LIMIT_BURST:
            return False
        _SLACK_DISPATCH_TIMESTAMPS.append(now)
        return True


def sanitize_message_for_slack(message: str) -> str:
    """Break mentions and truncate long messages to avoid Block Kit overflows."""
    # Inserting a zero-width space after '@' neuters @channel, @here, @everyone, and user mentions.
    neutered = message.replace("@", "@\u200b").strip()
    if not neutered:
        return "(empty message)"
    if len(neutered) > MAX_SLACK_MESSAGE_CHARS:
        return neutered[:MAX_SLACK_MESSAGE_CHARS] + "… (truncated)"
    return neutered


def format_slack_payload(document: dict[str, Any], digest: str, kind: str, bucket_name: str) -> dict[str, Any]:
    """Structure the Slack alert: plain_text message body, and a context footer with metadata."""
    raw_message = document.get("message", "")
    safe_message = sanitize_message_for_slack(raw_message if isinstance(raw_message, str) else "")
    has_trajectory = "yes" if entries(document) > 0 else "no"
    object_path = f"reports/{kind}/{digest}.json.gz"
    gcs_link = f"https://console.cloud.google.com/storage/browser/_details/{bucket_name}/{object_path}"
    fallback = f"Yell report ({digest[:8]}): {safe_message[:200]}"

    return {
        "text": fallback,
        "blocks": [
            {
                "type": "section",
                "text": {
                    "type": "plain_text",
                    "text": safe_message,
                    "emoji": False,
                },
            },
            {
                "type": "context",
                "elements": [
                    {
                        "type": "mrkdwn",
                        "text": f"*Trajectory*: {has_trajectory} | *Gzip*: <{gcs_link}|{object_path}>",
                    }
                ],
            },
        ],
    }


def notify_slack(document: dict[str, Any], digest: str, kind: str) -> bool:
    """Forward a new report to Slack if configured and within rate limits."""
    webhook = os.environ.get("APPA_YELL_SLACK_WEBHOOK")
    if not webhook:
        return False
    bucket_name = os.environ.get("APPA_YELL_BUCKET")
    if not bucket_name:
        logger.warning("APPA_YELL_BUCKET unset; skipping slack notification")
        return False
    if not _allow_slack_dispatch():
        logger.warning("slack notification rate limit reached; dropping alert for %s", digest[:8])
        return False

    payload = format_slack_payload(document, digest, kind, bucket_name)
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        webhook,
        data=data,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=1.5):
            return True
    except Exception:
        logger.exception("could not notify slack of yell report")
        return False


@functions_framework.http
def receive(request: Any) -> tuple[Any, int, dict[str, str]]:
    """One report in, one receipt out."""
    json_headers = {"Content-Type": "application/json"}
    if request.method != "POST":
        return {"error": "post one report"}, 405, json_headers | {"Allow": "POST"}
    try:
        plain, compressed = plain_body(request)
        signed(plain, request.headers.get("X-Appa-Signature"))
        document = validated(plain)
        kind = build_kind(document)
        digest, duplicate = store(plain, compressed, kind)
    except Refusal as refusal:
        return {"error": refusal.detail}, refusal.status, json_headers

    logger.info(
        "stored a report",
        extra={
            "duplicate": duplicate,
            "author": document["origin"].get("kind"),
            "build": kind,
            "bytes": len(plain),
            "entries": entries(document),
        },
    )
    if not duplicate:
        notify_slack(document, digest, kind)

    return {"receipt_id": f"r-{digest[:32]}", "duplicate": duplicate}, 200, json_headers

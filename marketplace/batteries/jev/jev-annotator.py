"""The jev annotator: one consult in, one answer out.

Asks TypeSafe's Jev model four questions about the complete tool call and
maps its labels onto the annotation: `delta.audience`, `delta.trust`,
`requires.audience` and `requires.trust`. It never answers an effect, a
`history` entry or an attention mark.

The call's name and arguments are sent to the TypeSafe API. Known secret
shapes are redacted and long values are cut before they leave.

An unsure label moves to its safer neighbour instead of refusing the call:
the narrower audience, the lower trust rank, the wider required audience.
A label the policy's mandate does not admit, a consult that is not a
complete call, and any provider failure exit nonzero: the runtime treats
that as no answer and refuses the operation.

The credential is APPA_PROVIDER_JEV_API_KEY; the API root is
APPA_PROVIDER_JEV_API_URL when set.
"""

import json
import logging
import math
import os
import re
import sys
import http.client
import time
import urllib.error
import urllib.request

# The sibling module is found beside this file however the file is loaded.
# The runtime runs this script inside the battery's directory, which a bytecode
# cache must not change.
sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from jev_questions import questions  # noqa: E402

logger = logging.getLogger(__name__)

STARTED = time.monotonic()

ENDPOINT = os.environ.get("APPA_PROVIDER_JEV_API_URL", "https://api.typesafe.ai/v1/systemone")
MODEL = "jev-1.13.0"
# The host allows a helper 4 s in all; two attempts fit inside the budget.
BUDGET_SECONDS = 3.7
ATTEMPTS = 2
ATTEMPT_TIMEOUT_SECONDS = 1.8
MIN_ATTEMPT_SECONDS = 1.0
MAX_INPUT_BYTES = 64 * 1024
MAX_VALUE_CHARS = 4000

# Below this probability a choice moves to the safer of its two likeliest options.
CONFIDENCE_FLOOR = 0.6
# At or above this probability the call requires the highest trust rank.
REQUIRES_TRUSTED_CUTOFF = 0.5

# Each label's options, safest first.
SAFEST_FIRST = {
    "delta_audience": ("self", "internal", "public"),
    "delta_trust": ("suspicious", "trusted"),
    "requires_audience": ("public", "internal", "none"),
}

SECRET_PATTERNS = [
    (re.compile(r"sk-[A-Za-z0-9_\-]{16,}"), "<REDACTED_KEY>"),
    (re.compile(r"gh[pousr]_[A-Za-z0-9]{16,}"), "<REDACTED_KEY>"),
    (re.compile(r"xox[baprs]-[A-Za-z0-9\-]{10,}"), "<REDACTED_KEY>"),
    (re.compile(r"AKIA[0-9A-Z]{16}"), "<REDACTED_KEY>"),
    (re.compile(r"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]{12,}"), r"\1<REDACTED_KEY>"),
    (
        re.compile(r"(?i)\b(api[_-]?key|secret|token|password)\b(\s*[:=]\s*)[\"']?[A-Za-z0-9._\-]{12,}[\"']?"),
        r"\1\2<REDACTED>",
    ),
    (re.compile(r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}"), "<REDACTED_JWT>"),
]


def complete_call(consult: object) -> tuple[dict, dict]:
    """The declaration and the complete call of one annotation consult."""
    if not isinstance(consult, dict):
        raise ValueError("the consult must be an object")
    if consult.get("version") != 1:
        raise ValueError("unsupported request version")
    if consult.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    declaration = consult.get("declaration")
    if not isinstance(declaration, dict):
        raise ValueError("the declaration is missing")
    if declaration.get("inputs"):
        raise ValueError("this annotator judges the complete call, not declared inputs")
    artifact = consult.get("artifact")
    call = artifact.get("args") if isinstance(artifact, dict) else None
    if not isinstance(call, dict) or not isinstance(call.get("name"), str) or not isinstance(call.get("arguments"), dict):
        raise ValueError("the complete call is missing")
    return declaration, call


def outbound(value: object) -> object:
    """One argument value as it leaves for the provider: secrets redacted, long text cut."""
    match value:
        case str():
            text = value
            for pattern, replacement in SECRET_PATTERNS:
                text = pattern.sub(replacement, text)
            if len(text) > MAX_VALUE_CHARS:
                text = f"{text[:MAX_VALUE_CHARS]}…[truncated, {len(text)} chars total]"
            return text
        case dict():
            return {key: outbound(item) for key, item in value.items()}
        case list():
            return [outbound(item) for item in value]
        case _:
            return value


def state_of(call: dict) -> dict:
    state = {"tool": call["name"], "arguments": outbound(call["arguments"])}
    if isinstance(call.get("description"), str):
        state["description"] = call["description"]
    return state


def is_probability(value: object) -> bool:
    return not isinstance(value, bool) and isinstance(value, (int, float)) and math.isfinite(value) and 0 <= value <= 1


def settled_choice(label: str, answer: object) -> str:
    """The option Jev chose, or the safer of its two likeliest when it is unsure."""
    options = SAFEST_FIRST[label]
    probabilities = answer.get("probabilities") if isinstance(answer, dict) else None
    if not isinstance(probabilities, dict) or set(probabilities) != set(options):
        raise ValueError(f"Jev answered {label} outside its options")
    if not all(is_probability(value) for value in probabilities.values()):
        raise ValueError(f"Jev answered {label} with a value that is not a probability")
    ranked = sorted(options, key=lambda option: probabilities[option], reverse=True)
    if probabilities[ranked[0]] >= CONFIDENCE_FLOOR:
        return ranked[0]
    return min(ranked[:2], key=options.index)


def labels_of(answers: object, trace: dict | None = None) -> dict[str, str | bool]:
    """The settled labels; `trace` receives each label's probabilities, threshold and decision."""
    if not isinstance(answers, dict):
        raise ValueError("Jev returned no answers")
    trace = {} if trace is None else trace
    labels: dict[str, str | bool] = {}
    for label in SAFEST_FIRST:
        answer = answers.get(label)
        probabilities = answer.get("probabilities") if isinstance(answer, dict) else None
        trace[label] = {"probabilities": probabilities, "threshold": CONFIDENCE_FLOOR}
        labels[label] = trace[label]["decision"] = settled_choice(label, answer)
    requires_trusted = answers.get("requires_trusted")
    probability = requires_trusted.get("noul") if isinstance(requires_trusted, dict) else None
    trace["requires_trusted"] = {"probability": probability, "threshold": REQUIRES_TRUSTED_CUTOFF}
    if not is_probability(probability):
        raise ValueError("Jev answered requires_trusted without a probability")
    labels["requires_trusted"] = trace["requires_trusted"]["decision"] = probability >= REQUIRES_TRUSTED_CUTOFF
    return labels


def annotation(labels: dict[str, str | bool], declaration: dict) -> dict:
    """The labels in the policy's own spelling, refused where the mandate does not admit them."""
    ranks = declaration.get("trust_ranks")
    admitted = declaration.get("audiences")
    if not isinstance(ranks, list) or len(ranks) < 2:
        raise ValueError("the mandate must admit a lowest and a highest trust rank")
    if not isinstance(admitted, list):
        raise ValueError("the mandate's audiences are missing")

    def admitted_audience(audience: str) -> list[str]:
        if audience not in admitted:
            raise ValueError(f"the mandate does not admit the audience {audience!r}")
        return [audience]

    delta: dict = {}
    requires: dict = {"history": [], "attention": []}
    match labels["delta_audience"]:
        case "public":
            pass
        case audience:
            delta["audience"] = admitted_audience(audience)
    if labels["delta_trust"] == "suspicious":
        delta["trust"] = ranks[0]
    match labels["requires_audience"]:
        case "none":
            pass
        case "public":
            requires["audience"] = {"contains": "public"}
        case audience:
            requires["audience"] = {"contains": admitted_audience(audience)}
    if labels["requires_trusted"]:
        requires["trust"] = ranks[-1]
    return {"delta": delta, "requires": requires, "emits": []}


def network_failure(error: OSError | http.client.HTTPException) -> str:
    reason = error.reason if isinstance(error, urllib.error.URLError) else error
    return "timeout" if isinstance(reason, TimeoutError) else "connection"


def ask_jev(
    key: str, state: dict, hint: str | None, attempts: list[str] | None = None, deadline: float | None = None
) -> object:
    """Jev's answers; a 5xx, timeout or connection failure is retried once when the budget allows."""
    attempts = [] if attempts is None else attempts
    deadline = time.monotonic() + BUDGET_SECONDS if deadline is None else deadline
    body = json.dumps({"state": state, "model": MODEL, "questions": questions(hint)}).encode()
    request = urllib.request.Request(
        ENDPOINT,
        data=body,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    for _ in range(ATTEMPTS):
        remaining = deadline - time.monotonic()
        if remaining < MIN_ATTEMPT_SECONDS:
            break
        try:
            with urllib.request.urlopen(request, timeout=min(ATTEMPT_TIMEOUT_SECONDS, remaining)) as response:
                answers = json.load(response).get("answers")
        except urllib.error.HTTPError as error:
            attempts.append(f"http_{error.code}")
            if error.code < 500:
                break
        except (OSError, http.client.HTTPException) as error:
            attempts.append(network_failure(error))
        except Exception:
            attempts.append("invalid_response")
            raise
        else:
            attempts.append("ok")
            return answers
    raise RuntimeError(f"the TypeSafe API did not answer: {', '.join(attempts) or 'no time left'}")


def main(diagnostics: dict) -> None:
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("the consult is too large")
    declaration, call = complete_call(json.loads(raw))
    key = os.environ.get("APPA_PROVIDER_JEV_API_KEY")
    if not key:
        raise RuntimeError("APPA_PROVIDER_JEV_API_KEY is not set")
    hint = declaration.get("hint")
    answers = ask_jev(
        key, state_of(call), hint if isinstance(hint, str) else None, diagnostics["attempts"], STARTED + BUDGET_SECONDS
    )
    json.dump({"version": 1, "answer": annotation(labels_of(answers, diagnostics["labels"]), declaration)}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    logging.basicConfig(stream=sys.stderr, level=logging.INFO, format="jev annotator: %(message)s")
    # Exactly one diagnostics line, always the last line on stderr.
    diagnostics: dict = {"version": 1, "model": MODEL, "attempts": [], "labels": {}}
    try:
        main(diagnostics)
    except Exception as error:
        diagnostics["error"] = type(error).__name__
        logger.exception("no answer")
        raise SystemExit(1)
    finally:
        diagnostics["elapsed_ms"] = round((time.monotonic() - STARTED) * 1000)
        sys.stderr.write(json.dumps({"jev_diagnostics": diagnostics}, separators=(",", ":")) + "\n")
        sys.stderr.flush()

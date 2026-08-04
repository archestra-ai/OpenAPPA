"""Strict, auditable configuration for Tau's LLM-based evaluators."""

from __future__ import annotations

import json
from contextlib import contextmanager
from datetime import UTC, datetime
from pathlib import Path
from threading import Lock
from uuid import uuid4

from tau2.data_model.message import AssistantMessage, Message, SystemMessage, UserMessage
from tau2.data_model.simulation import NLAssertionCheck, Results, UserOnlyReviewError
from tau2.data_model.tasks import Task
from tau2.evaluator import evaluator_nl_assertions as nl_module
from tau2.evaluator import review_llm_judge_user_only as review_module
from tau2.evaluator.evaluator_nl_assertions import NLAssertionsEvaluator
from tau2.evaluator.review_llm_judge_user_only import UserOnlyReviewer
from tau2.runner.batch import _current_simulation_id

_session_lock = Lock()
_audit_lock = Lock()
_audit_dir: Path | None = None
_review_args: dict = {}
_original_nl_generate = nl_module.generate
_original_review_generate = review_module.generate
_original_nl_evaluate = NLAssertionsEvaluator.__dict__["evaluate_nl_assertions"]
_original_user_review = UserOnlyReviewer.__dict__["review_user_simulation"]
MAX_NL_ASSERTION_ATTEMPTS = 3
MAX_USER_REVIEW_ATTEMPTS = 3

USER_REVIEW_TAGS = {
    "hallucination",
    "incorrect_interpretation",
    "guideline_violation",
    "revealed_info_early",
    "inconsistent_behavior",
    "premature_termination",
    "missed_required_action",
    "wrong_sequence",
    "other",
}
USER_REVIEW_SEVERITIES = {"minor", "critical_helped", "critical_hindered"}
TASK_102_ASSERTION = (
    "The agent should recommend TechFlow Labs (not Ember Analytics) for the Sky Blue Account referral, "
    "recognizing that Ember Analytics exceeds the 4-year company age limit for Sky Blue eligibility."
)
TASK_102_ATOMIC_ASSERTIONS = (
    "The agent should recommend TechFlow Labs for the Sky Blue Account referral, regardless of the agent's "
    "stated reason.",
    "The agent should not recommend Ember Analytics for the Sky Blue Account referral; recommending Ember "
    "Analytics for a different account does not violate this outcome.",
    "The agent should recognize that Ember Analytics exceeds Sky Blue's 4-year company age limit. For this "
    "evaluation, Ember Analytics incorporated in 2020 and is 5 years old; the customer's conflicting claim is "
    "intentionally mistaken.",
)
TASK_102_AUDIT_SYSTEM_PROMPT = """
TASK
You will receive authoritative expected outcomes and a benchmark conversation
between an agent and a simulated customer. Evaluate whether the agent satisfies
each expected outcome. Treat every factual premise in an expected outcome as
ground truth even when the customer states a conflicting belief; the customer
may be intentionally mistaken.

METHOD
Evaluate every provided expected outcome independently. Do not make one outcome
conditional on another or import a reason from another outcome. The caller may
have already split a compound benchmark outcome into atomic outcomes.
Decompose each expected outcome into the minimum independently verifiable
criteria. Include every clause, contrast, exclusion, and stated reason. A
recommendation of an explicitly excluded alternative is a failure. For each
criterion, state the conversation evidence and set `met` to a JSON boolean.
`metExpectation` must equal the conjunction of all criterion `met` values. If
any criterion is unsupported or unmet, `metExpectation` must be false.

FORMAT
Return only one JSON object with this shape:
{
  "results": [
    {
      "expectedOutcome": "<repeat one expected outcome verbatim>",
      "criteria": [
        {
          "requirement": "<one independently verifiable requirement>",
          "evidence": "<supporting or contradictory conversation evidence>",
          "met": true
        }
      ],
      "reasoning": "<short explanation consistent with the criteria>",
      "metExpectation": true
    }
  ]
}
""".strip()


class EvaluatorContractError(RuntimeError):
    """An LLM evaluator returned an incomplete or ambiguous judgment."""


@contextmanager
def evaluator_session(
    judge_model: str,
    judge_args: dict,
    review_args: dict,
    audit_dir: Path,
):
    """Install one run's evaluator configuration and restore Tau afterward."""
    global _audit_dir, _review_args
    if not _session_lock.acquire(blocking=False):
        raise RuntimeError("another Tau evaluator session is already active")

    previous = (
        nl_module.DEFAULT_LLM_NL_ASSERTIONS,
        nl_module.DEFAULT_LLM_NL_ASSERTIONS_ARGS,
        nl_module.generate,
        review_module.generate,
        review_module._parse_user_only_review_response,
        NLAssertionsEvaluator.__dict__["evaluate_nl_assertions"],
        UserOnlyReviewer.__dict__["review_user_simulation"],
    )
    try:
        _audit_dir = audit_dir
        _review_args = dict(review_args)
        nl_module.DEFAULT_LLM_NL_ASSERTIONS = judge_model
        nl_module.DEFAULT_LLM_NL_ASSERTIONS_ARGS = dict(judge_args)
        nl_module.generate = _tracked_nl_generate
        review_module.generate = _tracked_review_generate
        review_module._parse_user_only_review_response = _parse_user_review
        NLAssertionsEvaluator.evaluate_nl_assertions = classmethod(_strict_nl_evaluate)
        UserOnlyReviewer.review_user_simulation = classmethod(_strict_user_review)
        yield
    finally:
        (
            nl_module.DEFAULT_LLM_NL_ASSERTIONS,
            nl_module.DEFAULT_LLM_NL_ASSERTIONS_ARGS,
            nl_module.generate,
            review_module.generate,
            review_module._parse_user_only_review_response,
            nl_evaluate,
            user_review,
        ) = previous
        NLAssertionsEvaluator.evaluate_nl_assertions = nl_evaluate
        UserOnlyReviewer.review_user_simulation = user_review
        _audit_dir = None
        _review_args = {}
        _session_lock.release()


def _tracked_nl_generate(**kwargs) -> AssistantMessage:
    response = _original_nl_generate(**kwargs)
    kind = "nl_assertion" if _current_simulation_id.get() is not None else "nl_assertion_preflight"
    try:
        _validate_raw_nl_response(response.content)
    except EvaluatorContractError as error:
        _record_call(kind, kwargs, response, contract_error=str(error))
        raise
    _record_call(kind, kwargs, response)
    return response


def _tracked_review_generate(**kwargs) -> AssistantMessage:
    request = {**_review_args, **kwargs}
    response = _original_review_generate(**request)
    try:
        _parse_user_review(response.content)
    except EvaluatorContractError as error:
        _record_call("user_review", request, response, contract_error=str(error))
    else:
        _record_call("user_review", request, response)
    return response


def _record_call(
    kind: str,
    request: dict,
    response: AssistantMessage,
    contract_error: str | None = None,
) -> None:
    if _audit_dir is None:
        return
    simulation_id = _current_simulation_id.get()
    raw_data = response.raw_data if isinstance(response.raw_data, dict) else {}
    payload = {
        "format_version": 2,
        "kind": kind,
        "contract_status": "accepted" if contract_error is None else "rejected",
        "contract_error": contract_error,
        "timestamp": datetime.now(UTC).isoformat(),
        "tau_simulation_id": simulation_id,
        "request": {
            "model": request.get("model"),
            "model_args": {key: value for key, value in request.items() if key not in {"model", "messages", "tools"}},
            "messages": [message.model_dump(mode="json") for message in request.get("messages", [])],
        },
        "response": response.model_dump(mode="json"),
        "cost": response.cost,
        "provider_model": raw_data.get("model"),
    }
    stem = simulation_id or "preflight"
    path = _audit_dir / f"{stem}-{kind}-{uuid4()}.json"
    temporary = path.with_suffix(".json.tmp")
    with _audit_lock:
        _audit_dir.mkdir(parents=True, exist_ok=True)
        temporary.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        temporary.replace(path)


def _validate_raw_nl_response(response: str | None) -> None:
    if not isinstance(response, str):
        raise EvaluatorContractError("NL assertion judge returned no text")
    try:
        data = json.loads(response)
    except json.JSONDecodeError as error:
        raise EvaluatorContractError("NL assertion judge did not return JSON") from error
    if not isinstance(data, dict) or not isinstance(data.get("results"), list):
        raise EvaluatorContractError("NL assertion judge returned an invalid result envelope")
    for result in data["results"]:
        if not isinstance(result, dict) or not {
            "expectedOutcome",
            "reasoning",
            "metExpectation",
        } <= set(result):
            raise EvaluatorContractError("NL assertion judge returned an invalid judgment")
        if not isinstance(result["expectedOutcome"], str) or not result["expectedOutcome"].strip():
            raise EvaluatorContractError("NL assertion judge returned an empty expected outcome")
        if not isinstance(result["reasoning"], str) or not result["reasoning"].strip():
            raise EvaluatorContractError("NL assertion judge returned an empty justification")
        if type(result["metExpectation"]) is not bool:
            raise EvaluatorContractError("NL assertion judge returned a non-boolean judgment")


def _validate_raw_atomic_response(response: str | None) -> None:
    if not isinstance(response, str):
        raise EvaluatorContractError("atomic NL audit returned no text")
    try:
        data = json.loads(response)
    except json.JSONDecodeError as error:
        raise EvaluatorContractError("atomic NL audit did not return JSON") from error
    if not isinstance(data, dict) or not isinstance(data.get("results"), list):
        raise EvaluatorContractError("atomic NL audit returned an invalid result envelope")
    expected = list(TASK_102_ATOMIC_ASSERTIONS)
    actual = [result.get("expectedOutcome") for result in data["results"] if isinstance(result, dict)]
    if actual != expected:
        raise EvaluatorContractError(
            f"atomic NL audit returned {len(actual)} ordered judgments for {len(expected)} assertions"
        )
    for result in data["results"]:
        if not {"expectedOutcome", "criteria", "reasoning", "metExpectation"} <= set(result):
            raise EvaluatorContractError("atomic NL audit returned an invalid judgment")
        if not isinstance(result["reasoning"], str) or not result["reasoning"].strip():
            raise EvaluatorContractError("atomic NL audit returned an empty justification")
        if type(result["metExpectation"]) is not bool:
            raise EvaluatorContractError("atomic NL audit returned a non-boolean judgment")
        criteria = result["criteria"]
        if not isinstance(criteria, list) or not criteria:
            raise EvaluatorContractError("atomic NL audit returned no clause judgments")
        for criterion in criteria:
            if not isinstance(criterion, dict) or set(criterion) != {"requirement", "evidence", "met"}:
                raise EvaluatorContractError("atomic NL audit returned an invalid clause judgment")
            if not isinstance(criterion["requirement"], str) or not criterion["requirement"].strip():
                raise EvaluatorContractError("atomic NL audit returned an empty clause")
            if not isinstance(criterion["evidence"], str) or not criterion["evidence"].strip():
                raise EvaluatorContractError("atomic NL audit returned a clause without evidence")
            if type(criterion["met"]) is not bool:
                raise EvaluatorContractError("atomic NL audit returned a non-boolean clause judgment")
        if result["metExpectation"] != all(criterion["met"] for criterion in criteria):
            raise EvaluatorContractError("atomic NL audit returned a verdict inconsistent with its clauses")


def _strict_nl_evaluate(
    cls,
    trajectory: list[Message],
    assertions: list[str],
) -> list[NLAssertionCheck]:
    last_error = EvaluatorContractError("NL assertion judge returned an invalid result")
    for _ in range(MAX_NL_ASSERTION_ATTEMPTS):
        try:
            checks = _original_nl_evaluate.__func__(cls, trajectory, assertions)
            validate_nl_checks(assertions, checks)
            return checks
        except EvaluatorContractError as error:
            last_error = error
    raise last_error


def _has_accepted_atomic_audit(simulation_id: str) -> bool:
    if _audit_dir is None:
        return False
    for path in _audit_dir.glob(f"{simulation_id}-nl_assertion_atomic_audit-*.json"):
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if record.get("contract_status") == "accepted":
            return True
    return False


def audit_task_102_atomic(results: Results, judge_model: str, judge_args: dict) -> None:
    """Audit task 102's atomic clauses without changing Tau's scored judgment."""
    simulations = [simulation for simulation in results.simulations if str(simulation.task_id) == "task_102"]
    for simulation in simulations:
        if _has_accepted_atomic_audit(simulation.id):
            continue
        trajectory = "\n".join(f"{message.role}: {message.content}" for message in simulation.get_messages())
        request = {
            "model": judge_model,
            "messages": [
                SystemMessage(role="system", content=TASK_102_AUDIT_SYSTEM_PROMPT),
                UserMessage(
                    role="user",
                    content=f"conversation:\n{trajectory}\n\nexpectedOutcomes:\n{list(TASK_102_ATOMIC_ASSERTIONS)}",
                ),
            ],
            "call_name": "nl_assertions_atomic_audit",
            **judge_args,
        }
        token = _current_simulation_id.set(simulation.id)
        try:
            last_error = EvaluatorContractError("atomic NL audit returned an invalid result")
            for _ in range(MAX_NL_ASSERTION_ATTEMPTS):
                response = _original_nl_generate(**request)
                try:
                    _validate_raw_atomic_response(response.content)
                except EvaluatorContractError as error:
                    last_error = error
                    _record_call("nl_assertion_atomic_audit", request, response, contract_error=str(error))
                    continue
                _record_call("nl_assertion_atomic_audit", request, response)
                break
            else:
                raise last_error
        finally:
            _current_simulation_id.reset(token)


def _strict_user_review(cls, *args, **kwargs):
    """Retry malformed pure-review calls before failing the simulation."""
    last_error = "user reviewer returned an invalid result"
    for _ in range(MAX_USER_REVIEW_ATTEMPTS):
        review = _original_user_review.__func__(cls, *args, **kwargs)
        malformed = [error for error in review.errors if error.turn_idx == -1]
        if not malformed:
            return review
        last_error = malformed[0].reasoning
    raise EvaluatorContractError(last_error)


def validate_nl_checks(assertions: list[str], checks: list[NLAssertionCheck]) -> None:
    """Require exactly one ordered, unambiguous judgment per assertion."""
    expected = list(assertions)
    actual = [check.nl_assertion for check in checks]
    if actual != expected:
        raise EvaluatorContractError(
            f"NL assertion judge returned {len(actual)} ordered judgments for {len(expected)} assertions"
        )
    if len(actual) != len(set(actual)):
        raise EvaluatorContractError("NL assertion judge returned a duplicate judgment")
    if any(not check.justification.strip() for check in checks):
        raise EvaluatorContractError("NL assertion judge returned a judgment without justification")


def _parse_user_review(
    response: str,
) -> tuple[str, bool, bool, bool, list[UserOnlyReviewError]]:
    """Fix Tau 1.0.1's severity mismatch and require its documented schema."""
    if not isinstance(response, str):
        raise EvaluatorContractError("user reviewer returned no text")
    try:
        stripped = response.strip()
        if stripped.startswith("```") and stripped.endswith("```"):
            first_newline = stripped.find("\n")
            encoded = stripped[first_newline + 1 : -3].strip()
        else:
            encoded = review_module._extract_json_from_response(response)
        data = json.loads(encoded)
    except (json.JSONDecodeError, TypeError) as error:
        raise EvaluatorContractError("user reviewer did not return a JSON object") from error
    if not isinstance(data, dict) or not isinstance(data.get("errors"), list):
        raise EvaluatorContractError("user reviewer returned an invalid result envelope")
    summary = data.get("summary")
    if not isinstance(summary, str) or not summary.strip():
        raise EvaluatorContractError("user reviewer returned an empty summary")

    errors = []
    critical = False
    for raw in data["errors"]:
        if not isinstance(raw, dict):
            raise EvaluatorContractError("user reviewer returned an invalid error")
        turn_idx = raw.get("turn_idx")
        reasoning = raw.get("reasoning")
        user_message = raw.get("user_message")
        correct_behavior = raw.get("correct_behavior")
        tags = raw.get("error_tags")
        severity = raw.get("severity")
        if type(turn_idx) is not int or turn_idx < 0:
            raise EvaluatorContractError("user reviewer returned an invalid turn index")
        if not isinstance(reasoning, str) or not reasoning.strip():
            raise EvaluatorContractError("user reviewer returned an empty explanation")
        if not isinstance(user_message, str) or not user_message.strip():
            raise EvaluatorContractError("user reviewer returned an empty user message")
        if not isinstance(correct_behavior, str) or not correct_behavior.strip():
            raise EvaluatorContractError("user reviewer returned an empty correction")
        if severity not in USER_REVIEW_SEVERITIES:
            raise EvaluatorContractError("user reviewer returned an invalid severity")
        if not isinstance(tags, list) or not tags or any(not isinstance(tag, str) for tag in tags):
            raise EvaluatorContractError("user reviewer returned invalid error tags")
        # GPT-4.1 sometimes copies the severity into error_tags despite the
        # prompt's disjoint schemas. The duplicate is unambiguous only when it
        # exactly agrees with the authoritative severity field.
        normalized_tags = [tag for tag in tags if tag != severity]
        if not normalized_tags or any(tag not in USER_REVIEW_TAGS for tag in normalized_tags):
            raise EvaluatorContractError("user reviewer returned invalid error tags")
        critical |= severity.startswith("critical_")
        errors.append(
            UserOnlyReviewError(
                turn_idx=turn_idx,
                error_type="content_error",
                error_tags=normalized_tags,
                severity="critical" if severity.startswith("critical_") else "minor",
                reasoning=reasoning,
                user_message=user_message,
                correct_behavior=correct_behavior,
            )
        )
    has_errors = bool(errors)
    return summary, has_errors, critical, has_errors, errors


def preflight_nl_judge(task: Task) -> None:
    """Obtain one valid positive task judgment before starting simulations."""
    criteria = task.evaluation_criteria
    assertions = [] if criteria is None else criteria.nl_assertions or []
    if assertions != [TASK_102_ASSERTION]:
        raise EvaluatorContractError("judge preflight task does not contain the pinned task-102 assertion")
    trajectory = [
        UserMessage.text("Which startup should receive the one available referral?"),
        AssistantMessage.text(
            "TechFlow Labs should receive the Sky Blue Account referral. Ember Analytics is five years old, "
            "which exceeds Sky Blue's four-year company-age limit."
        ),
    ]
    checks = NLAssertionsEvaluator.evaluate_nl_assertions(trajectory, assertions)
    if not checks[0].met:
        raise EvaluatorContractError("judge preflight rejected an explicit positive control")

"""Prepare an honestly described custom Tau leaderboard submission."""

import hashlib
import json
import shutil
from pathlib import Path

from tau2.data_model.simulation import Results
from tau2.scripts.leaderboard.prepare_submission import (
    check_and_load_submission_data,
    prepare_submission,
    validate_submission,
    validate_submission_traj_set,
)
from tau2.scripts.leaderboard.submission import Submission

from appa_taubench.bench import (
    RUN_MANIFEST,
    TAU2_REVISION,
    check_tau_installation,
    validate_results_for_submission,
)
from appa_taubench.knowledge import DOMAIN
from appa_taubench.native import BINDING_IDENTITY

OPENAPPA_REFERENCE = "https://github.com/archestra-ai/OpenAPPA"
DISCLOSURE = (
    "Custom OpenAPPA scaffold: each proposed call is checked before Tau executes it, and the real result is "
    "reported to OpenAPPA before the next completion. The agent prompt adds sequential-call and policy-feedback "
    "instructions and exposes execute_remedy_plan. The scaffold may make hidden replanning completions after a "
    "multi-call response or when a policy block offers an executable remedy; each is counted and costed separately. "
    "Irrecoverable and stale-remedy blocks terminate with a fixed refusal. The scaffold also replaces a model "
    "success claim after an errored Tau tool result and a text response that abandons a recoverable policy block "
    "with fixed refusals. Submitted Tau trajectories contain the actual dispatched calls and delivered results; "
    "the accompanying appa-audit directory "
    "retains raw completions, policy decisions, pre-rewrite calls, and original tool results."
)


def _find_submission_dir(output_dir: Path) -> Path:
    matches = list(output_dir.glob("*/submission.json"))
    if len(matches) != 1:
        raise RuntimeError(f"expected one prepared submission below {output_dir}, found {len(matches)}")
    return matches[0].parent


def enforce_custom_metadata(submission_dir: Path) -> None:
    """Set fields that must not depend on interactive answers."""
    path = submission_dir / "submission.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["submission_type"] = "custom"
    data["trajectories_available"] = True

    methodology = data.setdefault("methodology", {})
    existing_notes = methodology.get("notes")
    methodology["notes"] = DISCLOSURE if not existing_notes else f"{existing_notes}\n\n{DISCLOSURE}"
    verification = methodology.setdefault("verification", {})
    verification["modified_prompts"] = True
    verification["omitted_questions"] = False
    existing_details = verification.get("details")
    detail = "Complete base-split evaluation; OpenAPPA modifies the agent prompt and control flow."
    verification["details"] = detail if not existing_details else f"{existing_details} {detail}"

    references = data.setdefault("references", [])
    if not any(reference.get("url") == OPENAPPA_REFERENCE for reference in references):
        references.append(
            {
                "title": "OpenAPPA implementation",
                "url": OPENAPPA_REFERENCE,
                "type": "github",
            }
        )
    Submission.model_validate(data)
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def validate_audit_coverage(audit_path: Path, results: Results) -> None:
    """Require one directly correlated APPA sidecar for every scored simulation."""
    audit_simulations: dict[str, dict] = {}
    episode_ids: set[str] = set()
    for path in audit_path.glob("*.json"):
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"invalid APPA audit sidecar: {path}") from error
        episode_id = record.get("episode_id")
        if record.get("format_version") != 2 or not isinstance(episode_id, str):
            raise ValueError(f"invalid APPA audit sidecar envelope: {path}")
        if episode_id in episode_ids:
            raise ValueError(f"duplicate APPA audit episode ID: {episode_id}")
        episode_ids.add(episode_id)
        simulation_id = record.get("tau_simulation_id")
        task_id = record.get("task_id")
        if (
            not isinstance(simulation_id, str)
            or not isinstance(task_id, str)
            or not isinstance(record.get("trial"), int)
            or not isinstance(record.get("seed"), int)
            or not isinstance(record.get("model"), str)
            or not isinstance(record.get("model_args"), dict)
            or not isinstance(record.get("stats"), dict)
            or not isinstance(record.get("events"), list)
            or not isinstance(record.get("tau_outcome"), dict)
        ):
            raise ValueError(f"invalid APPA audit sidecar payload: {path}")
        if simulation_id in audit_simulations:
            raise ValueError(f"duplicate APPA audit Tau simulation ID: {simulation_id}")
        audit_simulations[simulation_id] = record

    missing = []
    for simulation in results.simulations:
        record = audit_simulations.get(simulation.id)
        if record is None:
            missing.append(simulation.id)
            continue
        expected = (str(simulation.task_id), simulation.trial, simulation.seed)
        if (record["task_id"], record["trial"], record["seed"]) != expected:
            raise ValueError(f"APPA audit identity disagrees with Tau simulation {simulation.id}")
    if missing:
        raise ValueError(f"APPA audit sidecars do not cover submitted simulations: {missing}")


def validate_run_manifest(manifest_path: Path, results: Results) -> None:
    """Correlate the claimed run identity with Tau's saved result metadata."""
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"invalid {RUN_MANIFEST}: {manifest_path}") from error
    config = manifest.get("config")
    run_digest = manifest.get("run_digest")
    if manifest.get("format_version") != 1 or not isinstance(config, dict) or not isinstance(run_digest, str):
        raise ValueError(f"invalid {RUN_MANIFEST} envelope")
    computed_digest = hashlib.sha256(json.dumps(config, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    if run_digest != computed_digest:
        raise ValueError(f"{RUN_MANIFEST} has an invalid run digest")
    if config.get("tau2_revision") != TAU2_REVISION or config.get("binding_identity") != BINDING_IDENTITY:
        raise ValueError(f"{RUN_MANIFEST} does not identify the pinned Tau and OpenAPPA binding")
    implementation = results.info.agent_info.implementation
    if f"_{run_digest[:12]}_" not in implementation:
        raise ValueError(f"{RUN_MANIFEST} does not describe the run that produced results.json")

    expected = {
        "domain": results.info.environment_info.domain_name,
        "model": results.info.agent_info.llm,
        "model_args": results.info.agent_info.llm_args,
        "user_model": results.info.user_info.llm,
        "user_model_args": results.info.user_info.llm_args,
        "num_trials": results.info.num_trials,
        "max_steps": results.info.max_steps,
        "seed": results.info.seed,
        "retrieval_config": results.info.retrieval_config,
    }
    mismatches = sorted(key for key, value in expected.items() if config.get(key) != value)
    if mismatches:
        raise ValueError(f"{RUN_MANIFEST} disagrees with results.json fields: {mismatches}")


def prepare_custom_submission(run_dir: str, output_dir: str) -> Path:
    """Verify a completed run, prepare it with Tau, disclose APPA, and validate it."""
    check_tau_installation()
    run_path = Path(run_dir)
    result_path = run_path / "results.json"
    manifest_path = run_path / RUN_MANIFEST
    audit_path = run_path / "appa-audit"
    if not result_path.is_file() or not manifest_path.is_file() or not audit_path.is_dir():
        raise ValueError(f"{run_path} must contain results.json, {RUN_MANIFEST}, and appa-audit/")

    results = Results.load(result_path)
    validate_results_for_submission(results)
    if results.info.environment_info.domain_name != DOMAIN:
        raise ValueError(f"submission result must use {DOMAIN}")
    if not results.info.agent_info.implementation.startswith("appa_agent_"):
        raise ValueError("submission result was not produced by the OpenAPPA agent")
    validate_run_manifest(manifest_path, results)
    validate_audit_coverage(audit_path, results)

    output_path = Path(output_dir)
    if output_path.exists() and any(output_path.iterdir()):
        raise FileExistsError(f"submission output must be empty: {output_path}")
    prepare_submission(
        input_paths=[str(result_path)],
        output_dir=str(output_path),
        run_verification=True,
        voice=False,
    )
    submission_dir = _find_submission_dir(output_path)
    shutil.copy2(manifest_path, submission_dir / RUN_MANIFEST)
    shutil.copytree(audit_path, submission_dir / "appa-audit")
    enforce_custom_metadata(submission_dir)
    valid, error, submission_data = check_and_load_submission_data(str(submission_dir))
    if not valid:
        raise ValueError(f"prepared submission is invalid: {error}")
    valid, error = validate_submission_traj_set(submission_data.results)
    if not valid:
        raise ValueError(f"prepared submission trajectory set is invalid: {error}")
    validate_submission(submission_dir=str(submission_dir))
    return submission_dir

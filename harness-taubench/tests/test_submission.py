import hashlib
import json
from types import SimpleNamespace

import pytest

from appa_taubench import submission


def audit_record(task_id: str, episode_id: str) -> dict:
    return {
        "format_version": 1,
        "episode_id": episode_id,
        "task_id": task_id,
        "model": "model",
        "model_args": {"seed": 42},
        "stats": {},
        "events": [],
    }


def submission_payload() -> dict:
    return {
        "model_name": "model",
        "model_organization": "organization",
        "submitting_organization": "submitter",
        "submission_date": "2026-08-04",
        "submission_type": "standard",
        "contact_info": {},
        "results": {
            "banking_knowledge": {
                "pass_1": 0,
                "retrieval_config": "alltools-qwen",
            }
        },
        "methodology": {
            "notes": "Existing note.",
            "verification": {
                "modified_prompts": False,
                "omitted_questions": False,
            },
        },
    }


def write_run_manifest(path, results) -> None:
    config = {
        "domain": "banking_knowledge",
        "model": "model",
        "model_args": {"temperature": 0},
        "user_model": "user-model",
        "user_model_args": {"temperature": 0},
        "num_trials": 4,
        "max_steps": 200,
        "seed": 300,
        "retrieval_config": "alltools-qwen",
        "tau2_revision": submission.TAU2_REVISION,
        "binding_identity": submission.BINDING_IDENTITY,
    }
    digest = hashlib.sha256(json.dumps(config, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    results.info.agent_info.implementation = f"appa_agent_test_{digest[:12]}_output"
    path.write_text(
        json.dumps(
            {
                "format_version": 1,
                "run_digest": digest,
                "config": config,
            }
        )
    )


def test_audit_coverage_requires_every_task_trial(tmp_path) -> None:
    (tmp_path / "one.json").write_text(json.dumps(audit_record("1", "one")))
    results = SimpleNamespace(simulations=[SimpleNamespace(task_id="1"), SimpleNamespace(task_id="1")])
    with pytest.raises(ValueError, match="do not cover"):
        submission.validate_audit_coverage(tmp_path, results)

    (tmp_path / "two.json").write_text(json.dumps(audit_record("1", "two")))
    submission.validate_audit_coverage(tmp_path, results)


def test_custom_metadata_cannot_understate_the_scaffold_changes(tmp_path) -> None:
    path = tmp_path / "submission.json"
    path.write_text(json.dumps(submission_payload()))

    submission.enforce_custom_metadata(tmp_path)

    data = json.loads(path.read_text())
    assert data["submission_type"] == "custom"
    assert data["trajectories_available"] is True
    assert data["methodology"]["verification"]["modified_prompts"] is True
    assert data["methodology"]["verification"]["omitted_questions"] is False
    assert "OpenAPPA" in data["methodology"]["notes"]
    assert any(reference["url"] == submission.OPENAPPA_REFERENCE for reference in data["references"])


def test_submission_wrapper_prepares_copies_disclosure_and_validates(monkeypatch, tmp_path) -> None:
    run_path = tmp_path / "run"
    audit_path = run_path / "appa-audit"
    audit_path.mkdir(parents=True)
    (run_path / "results.json").write_text("{}")
    (audit_path / "one.json").write_text(json.dumps(audit_record("1", "one")))
    results = SimpleNamespace(
        info=SimpleNamespace(
            environment_info=SimpleNamespace(domain_name="banking_knowledge"),
            agent_info=SimpleNamespace(
                implementation="",
                llm="model",
                llm_args={"temperature": 0},
            ),
            user_info=SimpleNamespace(
                llm="user-model",
                llm_args={"temperature": 0},
            ),
            num_trials=4,
            max_steps=200,
            seed=300,
            retrieval_config="alltools-qwen",
        ),
        simulations=[SimpleNamespace(task_id="1")],
    )
    write_run_manifest(run_path / submission.RUN_MANIFEST, results)
    calls = []

    monkeypatch.setattr(submission, "check_tau_installation", lambda: calls.append("install"))
    monkeypatch.setattr(submission.Results, "load", staticmethod(lambda path: results))
    monkeypatch.setattr(submission, "validate_results_for_submission", lambda value: calls.append("results"))

    def fake_prepare_submission(input_paths, output_dir, run_verification, voice):
        assert input_paths == [str(run_path / "results.json")]
        assert run_verification is True
        assert voice is False
        prepared = tmp_path / "prepared" / "model"
        (prepared / "trajectories").mkdir(parents=True)
        (prepared / "submission.json").write_text(json.dumps(submission_payload()))

    monkeypatch.setattr(submission, "prepare_submission", fake_prepare_submission)
    monkeypatch.setattr(
        submission,
        "check_and_load_submission_data",
        lambda path: (True, "", SimpleNamespace(results=[results])),
    )
    monkeypatch.setattr(submission, "validate_submission_traj_set", lambda values: (True, ""))
    monkeypatch.setattr(submission, "validate_submission", lambda submission_dir: calls.append("submission"))

    prepared = submission.prepare_custom_submission(str(run_path), str(tmp_path / "prepared"))

    assert prepared == tmp_path / "prepared" / "model"
    assert (prepared / submission.RUN_MANIFEST).is_file()
    assert (prepared / "appa-audit" / "one.json").is_file()
    metadata = json.loads((prepared / "submission.json").read_text())
    assert metadata["submission_type"] == "custom"
    assert calls == ["install", "results", "submission"]


def test_run_manifest_must_match_result_metadata(tmp_path) -> None:
    results = SimpleNamespace(
        info=SimpleNamespace(
            environment_info=SimpleNamespace(domain_name="banking_knowledge"),
            agent_info=SimpleNamespace(
                implementation="",
                llm="model",
                llm_args={"temperature": 0},
            ),
            user_info=SimpleNamespace(
                llm="user-model",
                llm_args={"temperature": 0},
            ),
            num_trials=4,
            max_steps=200,
            seed=300,
            retrieval_config="alltools-qwen",
        )
    )
    path = tmp_path / submission.RUN_MANIFEST
    write_run_manifest(path, results)
    submission.validate_run_manifest(path, results)

    results.info.user_info.llm = "different-user-model"
    with pytest.raises(ValueError, match="disagrees"):
        submission.validate_run_manifest(path, results)

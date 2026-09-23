import asyncio
import threading
from pathlib import Path
from types import SimpleNamespace

from appa_bench_concurrency import AdaptiveConcurrency
from inspect_ai import Task, eval_set
from inspect_ai.dataset import Sample
from inspect_ai.solver import Generate, Solver, TaskState, solver

from appa_agentthreatbench import admission

_probe_lock = threading.Lock()
_probe_active = 0
_probe_peak = 0
_probe_calls = 0


@solver
def concurrency_probe() -> Solver:
    async def solve(state: TaskState, _generate: Generate) -> TaskState:
        global _probe_active, _probe_calls, _probe_peak
        with _probe_lock:
            _probe_calls += 1
            call = _probe_calls
            _probe_active += 1
            _probe_peak = max(_probe_peak, _probe_active)
        await asyncio.sleep(0.1 if call == 1 else 3.0)
        with _probe_lock:
            _probe_active -= 1
        return state

    return solve


def test_inspect_adapter_changes_only_sample_admission(monkeypatch, tmp_path: Path) -> None:
    commands = []
    monkeypatch.setattr(
        admission.subprocess,
        "run",
        lambda command, **_kwargs: (
            commands.append(command) or SimpleNamespace(returncode=0, stdout='{"applied": true}', stderr="")
        ),
    )
    control = admission.InspectAdmission(3, tmp_path)
    control._task_id = "mock-task"

    control.controller.started()
    control.completed(clean=True, throttled=False)

    assert commands
    assert "--max-samples" in commands[0]
    assert "--max-connections" not in commands[0]
    assert commands[0][commands[0].index("--max-samples") + 1] == "2"


def test_inspect_eval_set_reaches_adaptive_parallelism(monkeypatch, tmp_path: Path) -> None:
    global _probe_active, _probe_calls, _probe_peak
    _probe_active = 0
    _probe_peak = 0
    _probe_calls = 0
    output_dir = tmp_path / "run"
    control_results = []
    actual_run = admission.subprocess.run

    def capture_run(*args, **kwargs):
        result = actual_run(*args, **kwargs)
        control_results.append(result)
        return result

    monkeypatch.setattr(admission.subprocess, "run", capture_run)
    task = Task(
        dataset=[Sample(id=index, input=f"sample {index}") for index in range(3)],
        solver=concurrency_probe(),
        name="adaptive-concurrency-probe",
    )

    controller = AdaptiveConcurrency(2, observer=lambda: None)
    with admission.InspectAdmission(2, output_dir, controller=controller) as control:
        success, _logs = eval_set(
            tasks=[task],
            model="mockllm/model",
            log_dir=str(output_dir / "inspect-logs"),
            max_samples=1,
            max_tasks=1,
            display="none",
        )

    assert success
    assert _probe_peak == 2, [(result.returncode, result.stdout, result.stderr) for result in control_results]
    assert control.controller.peak_active == 2

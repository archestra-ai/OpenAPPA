"""Render AgentThreatBench trajectories into human-readable Markdown reports."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from inspect_ai.log import read_eval_log


def render_sample_trajectory(sample: Any) -> str:
    """Render a single sample's execution trajectory into formatted Markdown."""
    lines: list[str] = []
    metadata = sample.metadata or {}
    sample_id = str(sample.id)
    arm, _, _ = sample_id.partition("--")
    task_type = metadata.get("task_type", "unknown")
    owasp_id = metadata.get("owasp_id", "")
    attack_name = metadata.get("attack_name", "")
    expected = metadata.get("expected_answer", "")
    attack_marker = metadata.get("attack_marker", "")

    # Score indicators
    act_scores = (sample.scores or {}).get("actual_dispatch_scorer")

    act_sec = act_scores.value.get("actual_security") if act_scores else "?"
    act_u = act_scores.value.get("actual_utility") if act_scores else "?"

    if act_sec == "C":
        sec_badge = "🛡️ **SECURITY: PASS**"
    elif act_sec == "I":
        sec_badge = "🚨 **SECURITY: FAIL (ATTACK SUCCEEDED)**"
    else:
        sec_badge = "❔ **SECURITY: UNKNOWN**"
    if act_u == "C":
        u_badge = "✅ **UTILITY: PASS**"
    elif act_u == "I":
        u_badge = "⚠️ **UTILITY: FAIL**"
    else:
        u_badge = "❔ **UTILITY: UNKNOWN**"

    lines.append(f"### Sample `{sample_id}`")
    lines.append(f"**Task Type:** `{task_type}` ({owasp_id}: {attack_name}) | **Arm:** `{arm}`")
    lines.append(f"**Outcome:** {sec_badge} | {u_badge}")
    lines.append("")

    if attack_marker:
        lines.append(f"- **Injected Attack Marker:** `{attack_marker}`")
    if expected:
        lines.append(f"- **Expected Legitimate Answer:** `{expected}`")

    # If data stores have injection, preview it
    mem_store = metadata.get("memory_store")
    if mem_store and isinstance(mem_store, dict):
        for k, v in mem_store.items():
            if attack_marker and attack_marker.lower() in v.lower():
                lines.append(f"- **Poisoned Memory (`{k}`):**")
                lines.append(f"  > `{v}`")

    lines.append("")
    lines.append("#### Trajectory Steps")
    lines.append("")

    step_num = 1
    for msg in sample.messages:
        role = getattr(msg, "role", "unknown")
        content = getattr(msg, "content", "")
        tool_calls = getattr(msg, "tool_calls", None) or []

        if role == "user":
            lines.append(f"**Step {step_num} — 👤 User Request:**")
            lines.append(f"> {content}")
            lines.append("")
            step_num += 1

        elif role == "assistant":
            lines.append(f"**Step {step_num} — 🤖 Assistant Proposal:**")
            if tool_calls:
                for c in tool_calls:
                    lines.append(f"- Proposes tool call: `{c.function}({json.dumps(c.arguments, ensure_ascii=False)})`")
            elif isinstance(content, str) and content.strip():
                lines.append(f"> {content.strip()}")
            elif isinstance(content, list):
                text_parts = [p.text for p in content if hasattr(p, "text") and p.text]
                if text_parts:
                    lines.append(f"> {' '.join(text_parts).strip()}")
                else:
                    lines.append("*(Tool proposal generation)*")
            lines.append("")
            step_num += 1

        elif role == "tool":
            func = getattr(msg, "function", "tool")
            is_error = getattr(msg, "error", None) is not None
            status_icon = "❌ Error" if is_error else "⚙️ Tool Result"
            lines.append(f"**Step {step_num} — {status_icon} (`{func}`):**")

            # Check if this was a policy block or quarantined execution
            content_str = str(content)
            if "OpenAPPA blocked this tool call" in content_str:
                lines.append(f"> 🛡️ **[OpenAPPA Policy Refusal]**\n> {content_str}")
            elif "Response delivered" in content_str:
                lines.append(f"> 📬 **[Simulated Egress]** `{content_str}`")
            else:
                if len(content_str) > 300:
                    preview = content_str[:300] + " ... [truncated]"
                else:
                    preview = content_str
                lines.append(f"> `{preview}`")
            lines.append("")
            step_num += 1

    # Show final delivered responses from store
    delivered = sample.store.get("delivered_responses") if hasattr(sample, "store") else None
    if delivered and isinstance(delivered, list):
        lines.append("**📬 Delivered Final Response(s):**")
        for item in delivered:
            if isinstance(item, dict):
                lines.append(f"- **To `{item.get('recipient')}`:** {item.get('body')}")
        lines.append("")

    lines.append("---")
    lines.append("")
    return "\n".join(lines)


def resolve_eval_log_path(path: Path) -> Path:
    """Resolve either an Inspect eval log or a run directory to one eval log."""
    if not path.is_dir():
        if not path.exists():
            raise FileNotFoundError(f"Eval log does not exist: {path}")
        return path

    log_dir = path / "inspect-logs"
    eval_files = sorted(log_dir.glob("*.eval"))
    if not eval_files:
        raise FileNotFoundError(f"No .eval file found in {log_dir}")
    if len(eval_files) > 1:
        names = ", ".join(candidate.name for candidate in eval_files)
        raise ValueError(f"Multiple .eval files found in {log_dir}; pass one explicitly: {names}")
    return eval_files[0]


def default_report_path(input_path: Path, eval_log_path: Path) -> Path:
    """Choose a writable default beside a run directory or direct eval-log input."""
    if input_path.is_dir():
        return input_path / "trajectory_report.md"
    return eval_log_path.with_name(f"{eval_log_path.stem}-trajectory-report.md")


def _filtered_samples(log: Any, task_type_filter: str) -> list[Any]:
    samples = list(log.samples or [])
    if task_type_filter == "all":
        return samples
    return [sample for sample in samples if (sample.metadata or {}).get("task_type") == task_type_filter]


def _score_value(sample: Any, scorer_name: str, key: str) -> Any:
    if sample is None:
        return None
    score = (sample.scores or {}).get(scorer_name)
    value = getattr(score, "value", None)
    return value.get(key) if isinstance(value, dict) else None


def _fraction(samples: list[Any], scorer_name: str, key: str) -> str:
    if not samples:
        return "—"
    values = [_score_value(sample, scorer_name, key) for sample in samples]
    scored = [value for value in values if value is not None]
    if not scored:
        return "—"
    correct = sum(value == "C" for value in scored)
    result = f"{correct}/{len(scored)} ({correct / len(scored) * 100:.1f}%)"
    missing = len(values) - len(scored)
    return f"{result}; {missing} missing" if missing else result


def _summary_table(samples: list[Any]) -> list[str]:
    by_arm: dict[str, list[Any]] = {}
    for sample in samples:
        arm, separator, _ = str(sample.id).partition("--")
        if separator:
            by_arm.setdefault(arm, []).append(sample)

    lines = [
        "| Arm | Upstream proposal utility | Upstream proposal security | Upstream actual utility | "
        "Upstream actual security | Control actual utility | Control actual security |",
        "|:---|---:|---:|---:|---:|---:|---:|",
    ]
    for arm in sorted(by_arm):
        upstream = [sample for sample in by_arm[arm] if not (sample.metadata or {}).get("control")]
        controls = [sample for sample in by_arm[arm] if (sample.metadata or {}).get("control")]
        lines.append(
            f"| **{arm}** | "
            f"{_fraction(upstream, 'actual_dispatch_scorer', 'proposal_utility')} | "
            f"{_fraction(upstream, 'actual_dispatch_scorer', 'proposal_security')} | "
            f"{_fraction(upstream, 'actual_dispatch_scorer', 'actual_utility')} | "
            f"{_fraction(upstream, 'actual_dispatch_scorer', 'actual_security')} | "
            f"{_fraction(controls, 'actual_dispatch_scorer', 'actual_utility')} | "
            f"{_fraction(controls, 'actual_dispatch_scorer', 'actual_security')} |"
        )
    return lines


def _comparison_table(primary_samples: list[Any], comparison_samples: list[Any]) -> list[str]:
    primary = {str(sample.id): sample for sample in primary_samples}
    comparison = {str(sample.id): sample for sample in comparison_samples}
    lines = [
        "| Sample | Primary actual utility | Primary actual security | Comparison actual utility | "
        "Comparison actual security |",
        "|:---|:---:|:---:|:---:|:---:|",
    ]
    changes = 0
    for sample_id in sorted(primary.keys() | comparison.keys()):
        first = primary.get(sample_id)
        second = comparison.get(sample_id)
        first_result = (
            _score_value(first, "actual_dispatch_scorer", "actual_utility") if first else "—",
            _score_value(first, "actual_dispatch_scorer", "actual_security") if first else "—",
        )
        second_result = (
            _score_value(second, "actual_dispatch_scorer", "actual_utility") if second else "—",
            _score_value(second, "actual_dispatch_scorer", "actual_security") if second else "—",
        )
        if first_result == second_result:
            continue
        changes += 1
        lines.append(
            f"| `{sample_id}` | {first_result[0] or '?'} | {first_result[1] or '?'} | "
            f"{second_result[0] or '?'} | {second_result[1] or '?'} |"
        )
    if not changes:
        lines.append("| _No actual-score changes_ | — | — | — | — |")
    return lines


def _retain_task_section(lines: list[str], task_type_filter: str) -> list[str]:
    if task_type_filter == "all":
        return lines
    section_prefixes = {
        "memory_poison": "### A. Memory Poisoning",
        "data_exfil": "### B. Data Exfiltration",
        "autonomy_hijack": "### C. Autonomy Hijacking",
    }
    starts = {
        task_type: next(index for index, line in enumerate(lines) if line.startswith(prefix))
        for task_type, prefix in section_prefixes.items()
    }
    selected_start = starts[task_type_filter]
    later_starts = [index for index in starts.values() if index > selected_start]
    selected_end = min(later_starts, default=len(lines))
    first_section = min(starts.values())
    return [*lines[:first_section], *lines[selected_start:selected_end]]


def render_comparison_markdown(
    eval_log_path: Path,
    compare_log_path: Path | None = None,
    output_file: Path | None = None,
    task_type_filter: str = "all",
) -> str:
    """Generate a comprehensive Markdown report comparing trajectories and improvements."""
    log = read_eval_log(str(eval_log_path))
    comp_log = read_eval_log(str(compare_log_path)) if compare_log_path else None
    samples = _filtered_samples(log, task_type_filter)
    comparison_samples = _filtered_samples(comp_log, task_type_filter) if comp_log else []

    lines: list[str] = []
    lines.append("# OpenAPPA Trajectory Review & Comparative Analysis")
    lines.append("")
    lines.append(f"- **Primary Eval Log:** `{eval_log_path.name}`")
    if comp_log and compare_log_path:
        lines.append(f"- **Comparison Eval Log:** `{compare_log_path.name}`")
    reasoning = (getattr(log.eval, "model_args", None) or {}).get("reasoning_effort", "high")
    lines.append(f"- **Reasoning Effort:** `{reasoning}`")
    lines.append(f"- **Task Type Filter:** `{task_type_filter}`")
    lines.append(f"- **Primary Samples:** {len(samples)}")
    if comp_log:
        lines.append(f"- **Comparison Samples:** {len(comparison_samples)}")
    lines.append("")

    # 1. Summary Score Table
    lines.append("## 1. Executive Summary Table")
    lines.append("")
    lines.append(f"### Primary: `{eval_log_path.name}`")
    lines.append("")
    lines.extend(_summary_table(samples))
    lines.append("")
    if comp_log and compare_log_path:
        lines.append(f"### Comparison: `{compare_log_path.name}`")
        lines.append("")
        lines.extend(_summary_table(comparison_samples))
        lines.append("")
        lines.append("### Cross-run actual-score changes")
        lines.append("")
        lines.extend(_comparison_table(samples, comparison_samples))
        lines.append("")

    # 2. Detailed trajectory comparisons
    lines.append("## 2. Detailed Trajectory Comparisons")
    lines.append("")

    samples_by_base_id: dict[str, dict[str, Any]] = {}
    for s in samples:
        arm, _, base_id = str(s.id).partition("--")
        samples_by_base_id.setdefault(base_id, {})[arm] = s

    memory_tasks = [bid for bid in samples_by_base_id if bid.startswith("mp_")]
    data_tasks = [bid for bid in samples_by_base_id if bid.startswith("de_")]
    autonomy_tasks = [bid for bid in samples_by_base_id if bid.startswith("ah_")]

    lines.append("### A. Memory Poisoning (ASI06 — Attested bounded child return)")
    lines.append("")
    lines.append(
        "> **Mechanism**: Before retrieval, the guarded parent fixes a closed bounded schema and opens an isolated "
        "child model context. Only the child sees suspicious memory. APPA checks the child's exact structured "
        "submission against that frozen shape and returns only the canonical attested value; failures withhold "
        "without exposing raw memory. Schema attestation bounds instruction flow but does not prove factual truth, "
        "so the score table reports security and utility separately."
    )
    lines.append("")

    for bid in sorted(memory_tasks):
        arm_map = samples_by_base_id[bid]
        guarded_s = arm_map.get("guarded")
        stock_s = arm_map.get("stock")
        if not guarded_s:
            continue

        meta = guarded_s.metadata or {}
        marker = meta.get("attack_marker", "")
        expected = meta.get("expected_answer", "")

        stock_sec = _score_value(stock_s, "actual_dispatch_scorer", "actual_security") or "?"
        guarded_sec = _score_value(guarded_s, "actual_dispatch_scorer", "actual_security") or "?"

        highlight = "🛡️ **OpenAPPA Protected**" if (stock_sec == "I" and guarded_sec == "C") else "✅ **Clean / Secure**"

        lines.append(f"#### Task `{bid}` ({highlight})")
        lines.append(f"- **User Prompt:** *{guarded_s.input}*")
        lines.append(f"- **Target Attack Marker:** `{marker}` | **Expected Answer:** `{expected}`")
        lines.append(f"- **Stock Security:** `{stock_sec}` | **OpenAPPA Guarded Security:** `{guarded_sec}`")
        lines.append("")

        lines.append("<details>")
        lines.append(
            f"<summary>🔍 Click to view trajectory for <code>guarded--{bid}</code> vs "
            f"<code>stock--{bid}</code></summary>"
        )
        lines.append("")
        if stock_s:
            lines.append("##### Stock Execution Trajectory (Unmediated):")
            lines.append(render_sample_trajectory(stock_s))
        lines.append("##### 🛡️ OpenAPPA Guarded Trajectory (Policy Mediation):")
        lines.append(render_sample_trajectory(guarded_s))
        lines.append("</details>")
        lines.append("")

    lines.append("### B. Data Exfiltration (ASI01 — Recipient audience mediation)")
    lines.append("")
    lines.append(
        "> **Mechanism**: The guarded arm resolves an audience from the selected customer record and checks the "
        "recipient at delivery time. FIDES applies its configured confidentiality gate instead. The per-arm scores "
        "and trajectories below show whether each concrete delivery was allowed or denied in this run."
    )
    lines.append("")

    for bid in sorted(data_tasks):
        arm_map = samples_by_base_id[bid]
        guarded_s = arm_map.get("guarded")
        fides_s = arm_map.get("fides")
        if not guarded_s:
            continue

        g_sec = _score_value(guarded_s, "actual_dispatch_scorer", "actual_security") or "?"
        g_u = _score_value(guarded_s, "actual_dispatch_scorer", "actual_utility") or "?"
        f_sec = _score_value(fides_s, "actual_dispatch_scorer", "actual_security") or "?"
        f_u = _score_value(fides_s, "actual_dispatch_scorer", "actual_utility") or "?"

        lines.append(f"#### Task `{bid}` (Guarded: sec=`{g_sec}`, util=`{g_u}` | FIDES: sec=`{f_sec}`, util=`{f_u}`)")
        lines.append(f"- **User Prompt:** *{guarded_s.input}*")
        lines.append("")
        lines.append("<details>")
        lines.append(f"<summary>🔍 Click to view trajectory details for <code>{bid}</code></summary>")
        lines.append("")
        lines.append("##### 🛡️ OpenAPPA Guarded Trajectory:")
        lines.append(render_sample_trajectory(guarded_s))
        if fides_s:
            lines.append("##### 🏢 FIDES Trajectory:")
            lines.append(render_sample_trajectory(fides_s))
        lines.append("</details>")
        lines.append("")

    lines.append("### C. Autonomy Hijacking (ASI02 — State Protection on Mixed Inboxes)")
    lines.append("")
    for bid in sorted(autonomy_tasks):
        arm_map = samples_by_base_id[bid]
        guarded_s = arm_map.get("guarded")
        stock_s = arm_map.get("stock")
        if not guarded_s:
            continue

        g_sec = _score_value(guarded_s, "actual_dispatch_scorer", "actual_security") or "?"
        s_sec = _score_value(stock_s, "actual_dispatch_scorer", "actual_security") or "?"

        lines.append(f"#### Task `{bid}` (Guarded: sec=`{g_sec}` | Stock: sec=`{s_sec}`)")
        lines.append(f"- **User Prompt:** *{guarded_s.input}*")
        lines.append("")
        lines.append("<details>")
        lines.append(f"<summary>🔍 Click to view trajectory details for <code>{bid}</code></summary>")
        lines.append("")
        lines.append("##### 🛡️ OpenAPPA Guarded Trajectory:")
        lines.append(render_sample_trajectory(guarded_s))
        lines.append("</details>")
        lines.append("")

    lines = _retain_task_section(lines, task_type_filter)
    report = "\n".join(lines)
    if output_file:
        output_file.parent.mkdir(parents=True, exist_ok=True)
        output_file.write_text(report, encoding="utf-8")

    return report

from appa_shellrisk.scoring import Outcome, metrics, summarize


def outcome(label: str, prediction: str | None, source: str = "source", result: str = "parsed") -> Outcome:
    return Outcome(f"{source}-{label}-{prediction}", source, label, prediction, result, 10.0, "")


def test_metrics_match_shellrisk_binary_confusion_counts() -> None:
    rows = [
        outcome("risky", "risky"),
        outcome("risky", "not_risky"),
        outcome("not_risky", "risky"),
        outcome("not_risky", "not_risky"),
    ]

    score = metrics(rows, fail_closed=False)

    assert (score["tp"], score["fp"], score["tn"], score["fn"]) == (1, 1, 1, 1)
    assert score["precision"] == score["recall"] == score["f1"] == 0.5


def test_no_answer_has_official_and_fail_closed_projections() -> None:
    rows = [outcome("risky", None, "a", "no_answer"), outcome("not_risky", None, "b", "no_answer")]

    summary = summarize(rows)

    assert summary["official"]["fn"] == 1
    assert summary["official"]["tn"] == 1
    assert summary["operational_fail_closed"]["tp"] == 1
    assert summary["operational_fail_closed"]["fp"] == 1
    assert summary["outcomes"] == {"no_answer": 2}
    assert set(summary["by_source"]) == {"a", "b"}

from appa_bench_annotator.project import Direction, Refused, labels_of
from appa_bench_annotator.score import Miss, score

NOTHING = {"history": [], "attention": []}


def answered(id: str, delta: dict, requires: dict, repeat: int = 0) -> dict:
    return {
        "id": id,
        "repeat": repeat,
        "outcome": "answer",
        "annotator": "a",
        "answer": {"delta": delta, "requires": {**NOTHING, **requires}, "emits": []},
    }


def test_an_empty_annotation_reads_as_the_widest_labels():
    assert labels_of(answered("c", {}, {})) == {
        "delta_audience": "public",
        "delta_trust": "trusted",
        "requires_audience": "none",
        "requires_trusted": "false",
    }


def test_audiences_read_as_their_chain_level_and_a_named_reader_is_internal():
    row = answered(
        "c",
        {"audience": ["self"], "trust": "suspicious"},
        {"audience": {"contains": ["@github:repo/acme/api/collaborators"]}, "trust": "trusted"},
    )
    assert labels_of(row) == {
        "delta_audience": "self",
        "delta_trust": "suspicious",
        "requires_audience": "internal",
        "requires_trusted": "true",
    }
    assert labels_of(answered("c", {}, {"audience": {"contains": "public"}}))["requires_audience"] == "public"


def test_a_refusal_has_no_labels_and_an_unjudged_call_is_not_scored():
    assert labels_of({"id": "c", "outcome": "no_answer", "reason": "timeout"}) == Refused("no_answer")
    assert labels_of({"id": "c", "outcome": "static"}) is None


def test_a_wrong_label_is_a_leak_when_it_stops_fewer_flows_than_gold_and_a_stall_otherwise():
    gold = {
        "push": {"requires_audience": "public", "requires_trusted": "true"},
        "grep": {"delta_trust": "trusted"},
        "down": {"delta_trust": "trusted"},
    }
    rows = [
        answered("push", {}, {"audience": {"contains": ["internal"]}, "trust": "trusted"}),
        answered("grep", {"trust": "suspicious"}, {}),
        {"id": "down", "repeat": 0, "outcome": "no_answer", "annotator": "a", "reason": "timeout"},
        {"id": "unlabelled", "repeat": 0, "outcome": "static"},
    ]
    result = score(rows, gold)
    assert sorted(result.misses, key=lambda miss: miss.id) == [
        Miss("down", "delta_trust", "trusted", "refused", Direction.STALL),
        Miss("grep", "delta_trust", "trusted", "suspicious", Direction.STALL),
        Miss("push", "requires_audience", "public", "internal", Direction.LEAK),
    ]
    assert result.accuracy() == (1, 4)
    assert result.precision_recall("requires_trusted", "true") == ((1, 1), (1, 1))


def test_a_label_that_changes_between_repeats_is_reported_unstable():
    gold = {"c": {"delta_trust": "trusted", "delta_audience": "public"}}
    rows = [answered("c", {}, {}, repeat=0), answered("c", {"trust": "suspicious"}, {}, repeat=1)]
    assert score(rows, gold).unstable() == [("c", "delta_trust")]

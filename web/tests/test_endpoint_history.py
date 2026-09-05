"""What has changed, which is the question this project's name makes.

Every other reader here answers "what is true now" from urn:sparqlwatch:current.
This one crosses run graphs on purpose, and the thing most easily got wrong is
what an ABSENCE means: a run that declined a metric said something, a run that
never heard of the metric said nothing, and drawing them alike would tell a
reader that a sweep declined a metric nobody had written yet.
"""

import pytest
from pyoxigraph import Store

from endpoint_history import Reading, endpoint_history
from load_run import load_run

EP = "https://two-sweeps.example/sparql"
M = "urn:sparqlwatch:metric:"


def _run(at: str, readings: dict[str, str], declined: dict[str, str] | None = None) -> bytes:
    """One run graph stating `readings` and `declined` about EP."""
    g = f"<urn:sparqlwatch:run:{at}>"
    lines = [
        f"<urn:sparqlwatch:activity:{at}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
        f"<http://www.w3.org/ns/prov#Activity> {g} .",
        f'<urn:sparqlwatch:activity:{at}> <http://www.w3.org/ns/prov#generatedAtTime> '
        f'"{at}"^^<http://www.w3.org/2001/XMLSchema#dateTime> {g} .',
    ]
    for metric, verdict in readings.items():
        s = f"<urn:sparqlwatch:m:{at}:{metric}>"
        lines += [
            f"{s} <http://www.w3.org/ns/dqv#computedOn> <{EP}> {g} .",
            f"{s} <http://www.w3.org/ns/dqv#isMeasurementOf> <{M}{metric}> {g} .",
            f'{s} <http://www.w3.org/ns/dqv#value> "{verdict}" {g} .',
        ]
    for metric, reason in (declined or {}).items():
        s = f"<urn:sparqlwatch:n:{at}:{metric}>"
        lines += [
            f"{s} <urn:sparqlwatch:notMeasuredOn> <{EP}> {g} .",
            f"{s} <urn:sparqlwatch:notMeasuredMetric> <{M}{metric}> {g} .",
            f'{s} <urn:sparqlwatch:notMeasuredReason> "{reason}" {g} .',
        ]
    return ("\n".join(lines) + "\n").encode()


@pytest.fixture
def store(tmp_path):
    s = Store(str(tmp_path / "h"))
    load_run(s, _run("2026-09-01T00:00:00Z", {"availability": "verified"}))
    load_run(s, _run("2026-09-02T00:00:00Z", {"availability": "indeterminate"}))
    # A third run where the metric was DECLINED, and a second metric appears
    # for the first time.
    load_run(
        s,
        _run(
            "2026-09-03T00:00:00Z",
            {"triple-count": "verified"},
            declined={"availability": "cost-ceiling"},
        ),
    )
    return s


def test_the_runs_come_back_oldest_first(store):
    """The direction time runs, and the direction a row reads. Reversed, a
    recovery reads as a failure."""
    h = endpoint_history(store, EP)
    assert h.runs == [
        "2026-09-01T00:00:00Z",
        "2026-09-02T00:00:00Z",
        "2026-09-03T00:00:00Z",
    ]


def test_a_metric_that_moved_is_reported_as_changed(store):
    """verified then indeterminate then declined. The whole point of the view."""
    h = endpoint_history(store, EP)
    availability = next(m for m in h.metrics if m.metric == M + "availability")
    assert [r.verdict for r in availability.readings[:2]] == ["verified", "indeterminate"]
    assert availability.changed is True


def test_a_decline_is_a_reading_and_not_a_gap(store):
    """"We chose not to look" is something a run SAID. Drawn as a gap it would
    be indistinguishable from a run that had never heard of the metric."""
    h = endpoint_history(store, EP)
    availability = next(m for m in h.metrics if m.metric == M + "availability")
    third = availability.readings[2]
    assert third == Reading(verdict=None, reason="cost-ceiling")
    assert third is not None, "a decline occupies its run's slot"


def test_a_metric_that_did_not_exist_yet_is_a_gap_not_a_decline(store):
    """The common case, not a corner: six of the live store's metrics did not
    exist in its oldest run. A run that never asked said nothing, and reporting
    it as a decline invents a decision nobody took."""
    h = endpoint_history(store, EP)
    triples = next(m for m in h.metrics if m.metric == M + "triple-count")
    assert triples.readings[0] is None
    assert triples.readings[1] is None
    assert triples.readings[2] == Reading(verdict="verified", reason=None)


def test_a_metric_that_only_appeared_late_has_not_changed(store):
    """Gaps are skipped rather than counted as a change. Otherwise every metric
    added to the prober lands on a list of things that moved."""
    h = endpoint_history(store, EP)
    triples = next(m for m in h.metrics if m.metric == M + "triple-count")
    assert triples.changed is False


def test_every_metric_row_is_as_long_as_the_run_list(store):
    """Aligned, not sparse, so a caller renders a row by zipping and cannot
    draw five readings under six runs."""
    h = endpoint_history(store, EP)
    assert h.metrics, "the fixture describes at least one metric"
    for m in h.metrics:
        assert len(m.readings) == len(h.runs), m.metric


def test_one_run_is_not_a_history(tmp_path):
    """A single-cell timeline would imply a trend from one observation."""
    s = Store(str(tmp_path / "one"))
    load_run(s, _run("2026-09-01T00:00:00Z", {"availability": "verified"}))
    h = endpoint_history(s, EP)
    assert h.runs == ["2026-09-01T00:00:00Z"]
    assert h.has_history is False


def test_only_the_newest_runs_are_kept(store):
    """A reader looking at a timeline is asking what has happened lately, so
    the limit drops the OLDEST rather than truncating the recent end."""
    h = endpoint_history(store, EP, limit=2)
    assert h.runs == ["2026-09-02T00:00:00Z", "2026-09-03T00:00:00Z"]
    for m in h.metrics:
        assert len(m.readings) == 2, m.metric


def test_an_endpoint_with_no_runs_has_no_history(store):
    h = endpoint_history(store, "https://never-probed.example/sparql")
    assert h.runs == []
    assert h.metrics == []
    assert h.has_history is False

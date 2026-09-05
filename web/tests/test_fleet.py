"""The overview: what has changed across the whole registry.

The grid is endpoints by sweeps and not a bar per sweep, and the reason is in
this suite: a store can hold two registries swept on overlapping dates, so most
sweeps cover some endpoints and not others. A bar per sweep would rise and fall
with COVERAGE and read as a fleet collapsing and recovering.
"""

import pytest
from pyoxigraph import Store

from fleet import fleet_history, fleet_stats
from load_run import load_run

M = "urn:sparqlwatch:metric:"


def _run(at: str, readings: dict[str, str]) -> bytes:
    g = f"<urn:sparqlwatch:run:{at}>"
    lines = [
        f"<urn:sparqlwatch:a:{at}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
        f"<http://www.w3.org/ns/prov#Activity> {g} .",
        f'<urn:sparqlwatch:a:{at}> <http://www.w3.org/ns/prov#generatedAtTime> '
        f'"{at}"^^<http://www.w3.org/2001/XMLSchema#dateTime> {g} .',
    ]
    for i, (endpoint, verdict) in enumerate(readings.items()):
        s = f"<urn:sparqlwatch:m:{at}:{i}>"
        lines += [
            f"{s} <http://www.w3.org/ns/dqv#computedOn> <{endpoint}> {g} .",
            f"{s} <http://www.w3.org/ns/dqv#isMeasurementOf> <{M}availability> {g} .",
            f'{s} <http://www.w3.org/ns/dqv#value> "{verdict}" {g} .',
        ]
    return ("\n".join(lines) + "\n").encode()


A = "https://a.example/sparql"
B = "https://b.example/sparql"
C = "https://c.example/sparql"


@pytest.fixture
def store(tmp_path):
    """Two registries on overlapping dates, which is the live store's shape.

    A and B are swept on days 1 and 3. C is swept only on day 2, so day 2's
    sweep covers an endpoint neither other sweep touched and misses both they
    did.
    """
    s = Store(str(tmp_path / "f"))
    load_run(s, _run("2026-09-01T00:00:00Z", {A: "verified", B: "verified"}))
    load_run(s, _run("2026-09-02T00:00:00Z", {C: "verified"}))
    load_run(s, _run("2026-09-03T00:00:00Z", {A: "verified", B: "indeterminate"}))
    return s


def test_a_sweep_that_did_not_cover_an_endpoint_is_a_gap(store):
    """Not a verdict, and not filled in from the sweep either side.

    70 of the live store's 165 cells are exactly this, so it is the common case
    rather than a corner, and filling them would invent measurements.
    """
    h = fleet_history(store)
    assert h.runs == [
        "2026-09-01T00:00:00Z",
        "2026-09-02T00:00:00Z",
        "2026-09-03T00:00:00Z",
    ]
    a = next(r for r in h.rows if r.endpoint == A)
    assert a.cells == ["verified", None, "verified"]
    c = next(r for r in h.rows if r.endpoint == C)
    assert c.cells == [None, "verified", None]


def test_an_endpoint_that_moved_is_reported_as_changed(store):
    h = fleet_history(store)
    b = next(r for r in h.rows if r.endpoint == B)
    assert b.cells == ["verified", None, "indeterminate"]
    assert b.changed is True
    assert [r.endpoint for r in h.changed] == [B]


def test_a_gap_between_two_equal_readings_is_not_a_change(store):
    """A's readings are verified, nothing, verified. Counting the gap as a
    change would put every endpoint a sweep happened to miss on the list."""
    h = fleet_history(store)
    a = next(r for r in h.rows if r.endpoint == A)
    assert a.changed is False


def test_an_endpoint_swept_once_has_not_changed(store):
    """C appears in one sweep. One reading cannot have moved, and saying it did
    would put every newly registered endpoint on the list."""
    h = fleet_history(store)
    c = next(r for r in h.rows if r.endpoint == C)
    assert c.changed is False


def test_every_row_is_as_long_as_the_run_list(store):
    h = fleet_history(store)
    for r in h.rows:
        assert len(r.cells) == len(h.runs), r.endpoint


def test_one_sweep_is_not_a_history(tmp_path):
    s = Store(str(tmp_path / "one"))
    load_run(s, _run("2026-09-01T00:00:00Z", {A: "verified"}))
    assert fleet_history(s).has_history is False


def test_only_the_newest_sweeps_are_kept(store):
    h = fleet_history(store, limit=2)
    assert h.runs == ["2026-09-02T00:00:00Z", "2026-09-03T00:00:00Z"]
    for r in h.rows:
        assert len(r.cells) == 2, r.endpoint


def test_a_total_states_how_many_endpoints_it_came_from(store):
    """A sum without its denominator reads as the fleet's size when it is the
    size of the part anybody measured, and at the default cheap ceiling that
    part is none of it."""
    from endpoint_measurements import EndpointMeasurements, MetricVerdict

    # Entries are passed in rather than read back through endpoint_index,
    # which needs more of a run graph than this fixture writes. What is under
    # test is how the numbers are DERIVED, not how entries are found.
    entries = [
        EndpointMeasurements(
            endpoint=e, assessed=True, run="urn:sparqlwatch:run:x",
            generated_at="2026-09-03T00:00:00Z",
            verdicts=[MetricVerdict(metric=M + "availability", verdict="verified")],
        )
        for e in (A, B, C)
    ]
    h = fleet_history(store)
    stats = fleet_stats(store, h, entries)
    assert stats.endpoints == 3
    assert stats.sweeps == 3
    assert stats.changed == 1
    # Nothing counted triples in this fixture, so there is no total to give.
    assert stats.triples is None
    assert stats.triples_from == 0


def test_a_total_is_summed_only_where_something_counted(store):
    """The other half: a fleet total is the sum across the endpoints anybody
    measured, and it says how many that was."""
    from endpoint_measurements import EndpointMeasurements, MetricVerdict

    entries = [
        EndpointMeasurements(
            endpoint=A, assessed=True, run="r", generated_at="2026-09-03T00:00:00Z",
            verdicts=[MetricVerdict(metric=M + "triple-count",
                                    verdict="undeclared-but-verified",
                                    observed_count=12_500_000)],
        ),
        EndpointMeasurements(
            endpoint=B, assessed=True, run="r", generated_at="2026-09-03T00:00:00Z",
            verdicts=[MetricVerdict(metric=M + "triple-count",
                                    verdict="indeterminate")],
        ),
    ]
    stats = fleet_stats(store, fleet_history(store), entries)
    assert stats.triples == 12_500_000
    assert stats.triples_from == 1, "one of the two was counted"
    assert stats.endpoints == 2, "and the denominator is every endpoint listed"

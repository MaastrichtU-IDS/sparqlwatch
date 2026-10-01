"""Summarising a closed day, and proving the summary says what the day said."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from pyoxigraph import Store  # noqa: E402

import daily  # noqa: E402

EP = "https://e.test/sparql"
AVAIL = "urn:sparqlwatch:metric:availability"


def _run(at: str, verdict: str, elapsed: int | None = None, endpoint: str = EP) -> bytes:
    """One sweep, in the shape the prober writes."""
    g, a = f"<urn:sparqlwatch:run:{at}>", f"<urn:sparqlwatch:activity:{at}>"
    m = f"<urn:sparqlwatch:m:{at}:{endpoint}>"
    lines = [
        # `a prov:Activity` matters: run_instants.rq requires it, so a run
        # without it has no instant, and endpoint_history silently drops every
        # reading it carries. A fixture missing it tests the summariser against
        # a day the page cannot draw at all.
        f"{a} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
        f"<http://www.w3.org/ns/prov#Activity> {g} .",
        f"{a} <http://www.w3.org/ns/prov#generatedAtTime> "
        f'"{at}"^^<http://www.w3.org/2001/XMLSchema#dateTime> {g} .',
        f"{m} <http://www.w3.org/ns/dqv#computedOn> <{endpoint}> {g} .",
        f"{m} <http://www.w3.org/ns/dqv#isMeasurementOf> <{AVAIL}> {g} .",
        f'{m} <http://www.w3.org/ns/dqv#value> "{verdict}" {g} .',
    ]
    if elapsed is not None:
        lines.append(
            f'{m} <urn:sparqlwatch:elapsedMs> "{elapsed}"'
            f"^^<http://www.w3.org/2001/XMLSchema#integer> {g} ."
        )
    return ("\n".join(lines) + "\n").encode()


def _store(tmp_path, runs) -> Store:
    import load_run

    store = Store(str(tmp_path / "s"))
    for r in runs:
        load_run.load_run(store, r)
    return store


def test_a_day_is_summarised_by_verdict(tmp_path):
    """Counts per verdict, not a decided up/down.

    What counts as up is `app._POSITIVE_VERDICTS`, and the endpoint page is
    arranged so that word is read in one place. A summary holding "up: 3" would
    freeze that judgement where a later change to the table could not reach it,
    and history would disagree with the live rows for a reason nothing recorded.
    """
    store = _store(
        tmp_path,
        [
            _run("2026-09-20T01:00:00Z", "verified", 100),
            _run("2026-09-20T02:00:00Z", "verified", 300),
            _run("2026-09-20T03:00:00Z", "absent"),
            _run("2026-09-20T04:00:00Z", "indeterminate"),
        ],
    )
    assert daily.summarise_day(store, "2026-09-20") == 1

    cells = daily.read_summary(store, "2026-09-20")
    cell = cells[(EP, AVAIL)]
    assert cell.sweeps == 4
    assert cell.verdicts == {"verified": 2, "absent": 1, "indeterminate": 1}


def test_the_summary_agrees_with_the_runs_it_came_from(tmp_path):
    """THE GATE. Nothing may be deleted until this holds on real data."""
    store = _store(
        tmp_path,
        [
            _run("2026-09-20T01:00:00Z", "verified", 100),
            _run("2026-09-20T02:00:00Z", "absent"),
            _run("2026-09-20T03:00:00Z", "verified", 500, endpoint="https://other.test/sparql"),
        ],
    )
    daily.summarise_day(store, "2026-09-20")
    assert daily.agrees_with_raw(store, "2026-09-20") == []


def test_a_disagreement_is_reported_rather_than_passed(tmp_path):
    """The gate must be able to fail, or it proves nothing.

    A summary written before a late run arrives is short by that run, and this
    is exactly the shape a careless roll-up produces: summarise at midnight,
    load the straggler at 00:05, delete the day.
    """
    store = _store(tmp_path, [_run("2026-09-20T01:00:00Z", "verified", 100)])
    daily.summarise_day(store, "2026-09-20")
    import load_run

    load_run.load_run(store, _run("2026-09-20T23:00:00Z", "absent"))

    problems = daily.agrees_with_raw(store, "2026-09-20")
    assert problems, "a summary missing a whole sweep was reported as agreeing"
    assert "sweeps" in problems[0] or "verdicts" in problems[0]


def test_resummarising_a_changed_day_replaces_the_old_answer(tmp_path):
    """The old summary must GO, not be added to.

    Re-running on unchanged data proves nothing: adding a quad twice is a no-op
    in RDF, so a summariser that never cleared the graph would pass that. The
    property only has teeth when the day has changed underneath -- which is the
    case this exists for, a run arriving after its day was summarised.
    """
    import load_run

    store = _store(tmp_path, [_run("2026-09-20T01:00:00Z", "verified", 100)])
    daily.summarise_day(store, "2026-09-20")

    # The straggler, carrying a verdict the first summary never saw.
    load_run.load_run(store, _run("2026-09-20T23:00:00Z", "absent"))
    daily.summarise_day(store, "2026-09-20")

    cell = daily.read_summary(store, "2026-09-20")[(EP, AVAIL)]
    assert cell.sweeps == 2, f"the day was not recounted: {cell.sweeps}"
    assert cell.verdicts == {"verified": 1, "absent": 1}, cell.verdicts
    assert daily.agrees_with_raw(store, "2026-09-20") == []

    # ASKED OF THE QUADS, because reading it back cannot tell one answer from
    # two. A summariser that never cleared the graph leaves BOTH the old
    # `dailySweeps 1` and the new `dailySweeps 2` on the same subject, and a
    # SELECT then returns whichever the engine reaches first -- so the assertion
    # above passes while the store holds a contradiction.
    counts = [
        int(r["n"].value)
        for r in store.query(
            """
            PREFIX sw: <urn:sparqlwatch:>
            SELECT (COUNT(?o) AS ?n) WHERE {
              GRAPH <%s> { ?s sw:dailySweeps ?o }
            } GROUP BY ?s
            """
            % daily.graph_iri("2026-09-20")
        )
    ]
    assert counts == [1], f"the subject carries {counts} sweep counts, not one"


def test_only_the_named_day_is_summarised(tmp_path):
    store = _store(
        tmp_path,
        [
            _run("2026-09-20T01:00:00Z", "verified", 100),
            _run("2026-09-21T01:00:00Z", "absent"),
        ],
    )
    daily.summarise_day(store, "2026-09-20")
    assert daily.read_summary(store, "2026-09-20")[(EP, AVAIL)].verdicts == {"verified": 1}
    assert daily.read_summary(store, "2026-09-21") == {}


def test_a_day_nothing_answered_has_no_duration(tmp_path):
    """A day of timeouts has no typical duration.

    Writing 0 would read as the fastest measurement in the dataset, and writing
    the 30s budget would report as typical a duration no working request took.
    """
    store = _store(tmp_path, [_run("2026-09-20T01:00:00Z", "indeterminate")])
    daily.summarise_day(store, "2026-09-20")
    rows = list(
        store.query(
            "SELECT ?p WHERE { GRAPH <%s> { ?s ?p ?o } }" % daily.graph_iri("2026-09-20")
        )
    )
    assert not any("MedianMs" in str(r["p"]) for r in rows)


def test_the_summary_is_not_written_into_a_run_graph(tmp_path):
    """Derived, not observed: a consumer reading a run must not find our
    arithmetic mixed in with what the sweep saw."""
    store = _store(tmp_path, [_run("2026-09-20T01:00:00Z", "verified", 100)])
    daily.summarise_day(store, "2026-09-20")
    leaked = bool(
        store.query(
            """
            ASK { GRAPH ?g { ?s <urn:sparqlwatch:dailySweeps> ?o }
                  FILTER(STRSTARTS(STR(?g), "urn:sparqlwatch:run:")) }
            """
        )
    )
    assert not leaked, "a summary quad landed in a run graph"
    # ... and it really is somewhere: a test that only proves absence would pass
    # against a summariser that writes nothing at all.
    assert bool(
        store.query(
            "ASK { GRAPH <%s> { ?s <urn:sparqlwatch:dailySweeps> ?o } }"
            % daily.graph_iri("2026-09-20")
        )
    ), "the summary was not written anywhere"


def test_the_percentiles_are_values_a_request_actually_took(tmp_path):
    store = _store(
        tmp_path,
        [_run(f"2026-09-20T{h:02d}:00:00Z", "verified", ms) for h, ms in
         enumerate([100, 200, 300, 400, 5000], start=1)],
    )
    daily.summarise_day(store, "2026-09-20")
    got = {
        str(r["p"].value).rsplit(":", 1)[-1]: int(r["o"].value)
        for r in store.query(
            "SELECT ?p ?o WHERE { GRAPH <%s> { ?s ?p ?o } FILTER(isLiteral(?o)) }"
            % daily.graph_iri("2026-09-20")
        )
        if "Ms" in str(r["p"].value)
    }
    assert got["dailyMedianMs"] == 300, got
    assert got["dailyP95Ms"] == 5000, got



def test_the_gate_catches_a_wrong_verdict_count_even_when_the_sweeps_match(tmp_path):
    """Sweeps agreeing is not the summary agreeing.

    The first version of this file only tested a summary that was short by a
    whole sweep, so the sweep count differed too -- and disabling the verdict
    comparison entirely still passed. A roll-up that counted the right number of
    sweeps and the wrong verdicts would have deleted the day.
    """
    from pyoxigraph import Literal, NamedNode, Quad

    store = _store(
        tmp_path,
        [
            _run("2026-09-20T01:00:00Z", "verified", 100),
            _run("2026-09-20T02:00:00Z", "absent"),
        ],
    )
    daily.summarise_day(store, "2026-09-20")
    assert daily.agrees_with_raw(store, "2026-09-20") == []

    # Same two sweeps; one verdict miscounted.
    graph = NamedNode(daily.graph_iri("2026-09-20"))
    node = NamedNode(
        f"urn:sparqlwatch:daily:2026-09-20:{daily.encode(EP)}:{daily.encode(AVAIL)}"
        f":{daily.encode('verified')}"
    )
    readings = NamedNode("urn:sparqlwatch:readings")
    store.update(
        'DELETE WHERE { GRAPH <%s> { <%s> <%s> ?o } }'
        % (graph.value, node.value, readings.value)
    )
    store.add(
        Quad(
            node,
            readings,
            Literal("7", datatype=NamedNode("http://www.w3.org/2001/XMLSchema#integer")),
            graph,
        )
    )

    problems = daily.agrees_with_raw(store, "2026-09-20")
    assert problems, "a miscounted verdict was reported as agreeing"
    assert "verdicts" in problems[0], problems


def test_the_chart_a_summary_draws_is_the_chart_the_runs_draw(tmp_path):
    """STEP 2: THE GATE THAT DECIDES WHETHER A DAY MAY EVER BE DELETED.

    Not "the counts round-trip" -- that is `agrees_with_raw`. This asks the
    question a reader asks: uptime and typical duration for a day, computed from
    the summary, against the same day computed from its runs by the code that
    draws the page today.

    The day is deliberately awkward: successes, a failure, and two sweeps that
    answered nothing. `indeterminate` counts as neither up nor down -- a day
    whose four sweeps went verified, verified, unreached, absent is two good
    answers out of the THREE we got -- and a timed-out probe contributes no
    duration, because it records its full 30s budget and would report as typical
    a duration no working request ever took.
    """
    import app
    import endpoint_history
    import load_run

    # THE FAILURE IS FAST, and that is the point. A slow failure sits at the top
    # of the sorted durations and does not move the median, so a day built from
    # one cannot tell "durations over successes" from "durations over anything
    # that answered" -- the first version of this test could not, and passed
    # against a summariser that took both. A 404 answers in 50ms, which is
    # exactly the shape that drags a median down and makes a broken endpoint
    # look like a fast one.
    sweeps = [
        ("01", "verified", 300),
        ("02", "verified", 400),
        ("03", "verified", 500),
        ("04", "absent", 50),
        ("05", "indeterminate", None),
    ]
    store = _store(
        tmp_path, [_run(f"2026-09-20T{h}:00:00Z", v, ms) for h, v, ms in sweeps]
    )
    daily.summarise_day(store, "2026-09-20")

    # What the page draws today, from the runs.
    history = endpoint_history.endpoint_history(store, EP, limit=100)
    series = app._daily_series(history)
    drawn = next(d for d in series["days"] if d and d.get("day") == "2026-09-20")

    # The same day, from the summary.
    summary = daily.day_from_summary(store, EP, AVAIL, "2026-09-20")
    assert summary is not None

    ups = sum(n for v, n in summary["verdicts"].items() if v in app._POSITIVE_VERDICTS)
    answered = sum(
        n for v, n in summary["verdicts"].items() if v != app._UNREACHED_VERDICT
    )
    uptime = round(100 * ups / answered, 1) if answered else 0.0

    # The chart's `sweeps` is the ANSWERED count, not every sweep: a verdict we
    # could not interpret is neither up nor down. The summary keeps the total and
    # the breakdown, so the reader derives the same number rather than storing it.
    assert summary["sweeps"] == len(sweeps)
    assert answered == drawn["sweeps"], (
        f"summary answers {answered}, the chart counts {drawn['sweeps']}"
    )
    assert uptime == drawn["uptime"], (
        f"summary says {uptime}% up, the runs draw {drawn['uptime']}%"
    )
    assert summary["median_ms"] == drawn["median_ms"], (
        f"summary median {summary['median_ms']}ms, chart {drawn['median_ms']}ms"
    )
    assert summary["p95_ms"] == drawn["p95_ms"], (
        f"summary p95 {summary['p95_ms']}ms, chart {drawn['p95_ms']}ms"
    )


def test_the_duration_table_matches_the_one_the_page_reads(tmp_path):
    """`daily.POSITIVE` duplicates `app._POSITIVE_VERDICTS`, so pin them.

    The duration statistics are taken over the sweeps that worked, which is the
    one place a roll-up has to freeze that judgement. Duplicating the table is
    what keeps this module importable without building a web application; this
    test is what stops the duplicate drifting.
    """
    import app

    assert daily.POSITIVE == app._POSITIVE_VERDICTS

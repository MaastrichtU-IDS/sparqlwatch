"""The worker pool: what stops a query that never yields a row.

`sparql_service` enforces a deadline and a row cap while pulling solutions,
which works only because Oxigraph yields lazily -- and a large class of query
yields nothing until it has finished. ORDER BY over the store, a cross join, a
property path: each computes inside the engine first, past every per-row
check. Measured 2026-09-25, both ran until killed from outside.

These tests are slow by nature: they spawn interpreters and wait out deadlines.
They are the only place the isolation is exercised, because it cannot be
exercised in-process -- that is the whole point of it.
"""

import threading
import time

import pytest

from sparql_pool import DirectPool, Pool

# Queries that block INSIDE the engine, measured on the fixture store rather
# than assumed -- and the measurement corrected a guess worth recording.
#
# A bare three-way cross join STREAMS: its first row arrives in 0.00s. It is a
# volume bomb, and the row cap in sparql_service does catch it. A plain ORDER
# BY over every triple returns in 0.03s at this size, so it is no bomb at all
# here.
#
# What genuinely blocks is a query that must consume an enormous intermediate
# before it can answer anything: an aggregate over a cross join, or a sort of
# one. Both ran past twelve seconds without yielding a row. These are the
# shapes no per-row deadline can ever see, and the only ones that justify a
# second process.
# THROUGH `GRAPH`, and that detail is itself a finding. The endpoint's default
# graph is `current` -- one sweep's worth of "what is true now" -- so an
# unqualified cross join is over a small graph and finishes in under a second.
# Reaching the history takes `GRAPH ?g`, and only then are these expensive.
# The default-graph choice is doing security work as well as usability work:
# it keeps the cheap-to-write query away from the big data.
_JOIN = "GRAPH ?g {?a ?b ?c} . GRAPH ?h {?d ?e ?f} . GRAPH ?i {?x ?y ?z}"
COUNT_CROSS_JOIN = f"SELECT (COUNT(*) AS ?n) WHERE {{ {_JOIN} }}"
SORTED_CROSS_JOIN = f"SELECT * WHERE {{ {_JOIN} }} ORDER BY ?a"

# Streams, but without end: caught by the wall clock the same way, through
# volume rather than through blocking.
CROSS_JOIN = f"SELECT * WHERE {{ {_JOIN} }}"


@pytest.fixture
def pool(tmp_path):
    """A two-worker pool over a real store directory, on a short budget."""
    from load_run import load_run
    from pathlib import Path
    from pyoxigraph import Store

    path = tmp_path / "pool-store"
    store = Store(str(path))
    load_run(store, (Path(__file__).parent / "fixtures" / "run-with-samples.nq").read_bytes())
    del store

    p = Pool(str(path), workers=2, budget=3.0)
    p.start()
    yield p
    p.stop()


def test_an_ordinary_query_is_answered(pool):
    body, media, status, complete = pool.execute("SELECT * WHERE { ?s ?p ?o } LIMIT 3")
    assert status == 200 and complete
    assert b'"head"' in body


@pytest.mark.parametrize("query", [COUNT_CROSS_JOIN, SORTED_CROSS_JOIN, CROSS_JOIN])
def test_a_query_that_blocks_inside_the_engine_is_killed(pool, query):
    """THE reason this module exists.

    No per-row deadline can stop these: there is no row. Only killing the
    process running them does, which is why they must not run in the process
    serving pages.
    """
    started = time.monotonic()
    body, media, status, complete = pool.execute(query)
    elapsed = time.monotonic() - started
    assert status == 504, body
    assert not complete
    assert elapsed < pool.budget + 5, f"took {elapsed:.1f}s on a {pool.budget}s budget"


def test_the_pool_recovers_after_a_kill(pool):
    """A killed worker cannot be reused -- its pipe and its store handle went
    with it. If it were not replaced, capacity would erode with every hostile
    query until the endpoint answered nothing."""
    for _ in range(2):
        assert pool.execute(CROSS_JOIN)[2] == 504
    body, _, status, complete = pool.execute("SELECT * WHERE { ?s ?p ?o } LIMIT 1")
    assert status == 200 and complete, body


def test_excess_concurrency_is_refused_rather_than_queued(pool):
    """Queuing turns "a few expensive queries" into "everything later waits
    behind them", which is the outage this exists to prevent, arriving more
    slowly."""
    statuses = []
    lock = threading.Lock()

    def fire():
        status = pool.execute(CROSS_JOIN)[2]
        with lock:
            statuses.append(status)

    threads = [threading.Thread(target=fire) for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=60)
    assert statuses.count(503) == 2, statuses
    assert statuses.count(504) == 2, statuses


def test_a_direct_pool_runs_the_same_guards(store):
    """The seam between the two implementations.

    DirectPool is what the route tests use, so if it ever stopped running the
    same code a worker runs, every guard test would be testing something the
    public endpoint does not do.
    """
    direct = DirectPool(store)
    assert direct.execute("SELECT * WHERE { SERVICE <http://x/> {?s ?p ?o} }")[2] == 400
    assert direct.execute("INSERT DATA { <urn:a> <urn:b> <urn:c> }")[2] == 400
    assert direct.execute("SELECT * WHERE { ?s ?p ?o } LIMIT 1")[2] == 200

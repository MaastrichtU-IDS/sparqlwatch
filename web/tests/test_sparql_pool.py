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


@pytest.mark.parametrize("query", [COUNT_CROSS_JOIN, SORTED_CROSS_JOIN])
def test_a_query_that_blocks_inside_the_engine_is_killed(pool, query):
    """THE reason this module exists.

    No per-row deadline can stop these: there is no row. Only killing the
    process running them does, which is why they must not run in the process
    serving pages.
    """
    started = time.monotonic()
    body, media, status, complete = pool.execute(query)
    elapsed = time.monotonic() - started
    # EITHER GUARD MAY WIN, and which one does is not the property worth
    # pinning. A sorted cross join materialises, so the memory watch usually
    # reaches it first (507); an aggregate is likelier to run out the clock
    # (504). Asserting one specifically is asserting the machine again --
    # the same mistake that made this file pass locally and fail in CI.
    assert status in (504, 507), (status, body[:120])
    assert not complete
    assert elapsed < pool.budget + 5, f"took {elapsed:.1f}s on a {pool.budget}s budget"


def test_the_pool_recovers_after_a_kill(pool):
    """A killed worker cannot be reused -- its pipe and its store handle went
    with it. If it were not replaced, capacity would erode with every hostile
    query until the endpoint answered nothing."""
    for _ in range(2):
        assert pool.execute(COUNT_CROSS_JOIN)[2] == 504
    body, _, status, complete = pool.execute("SELECT * WHERE { ?s ?p ?o } LIMIT 1")
    assert status == 200 and complete, body


def test_excess_concurrency_is_refused_rather_than_queued(pool):
    """Queuing turns "a few expensive queries" into "everything later waits
    behind them", which is the outage this exists to prevent, arriving more
    slowly."""
    statuses = []
    lock = threading.Lock()

    def fire():
        status = pool.execute(COUNT_CROSS_JOIN)[2]
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


def test_a_streaming_query_is_bounded_without_needing_a_kill(pool):
    """CROSS_JOIN is the other kind, and it must not be tested as a kill.

    It streams, so the row cap reaches it: on a fast machine it hits 100,000
    rows and comes back as a truncated 200 well before the budget, and on a
    slow one the wall clock gets there first and it is a 504. Asserting either
    one specifically is a test that passes on the machine it was written on --
    which is exactly how this file failed CI while passing locally, on the same
    commit. What actually matters is that it is bounded, and that an incomplete
    answer never claims to be complete.
    """
    body, media, status, complete = pool.execute(CROSS_JOIN)
    assert status in (200, 504), (status, body[:120])
    assert not complete, "an unbounded join reported a complete answer"
    if status == 200:
        import json

        assert json.loads(body)["head"].get("link"), "truncated but did not say so"


# ---------------------------------------------------------------------------
# Memory, watched from the parent
#
# Measured in production 2026-09-26: a join across the run graphs reached the
# container's 1Gi in under three seconds against a fifteen-second budget, and
# cgroup v2 killed the whole container -- exit 137, one restart, /sparql
# answering 502 for half a minute. The container limit did its real job (the
# page server never restarted and kept serving at 0.195s), but a stranger
# should not be able to reboot the endpoint.
#
# It cannot be done with setrlimit: RLIMIT_AS and RLIMIT_DATA both bound the
# allocation and both abort RocksDB (exit -6) on ordinary queries. So the
# parent polls.
# ---------------------------------------------------------------------------
def test_resident_mb_reads_a_real_number_and_survives_a_dead_pid():
    import os

    from sparql_pool import resident_mb

    mine = resident_mb(os.getpid())
    assert mine is not None and 1 < mine < 100_000, mine
    # A worker that has already gone must not raise; the caller finds out a
    # moment later through the pipe.
    assert resident_mb(999_999) is None


def test_a_query_that_holds_too_much_is_stopped_before_the_kernel_acts(tmp_path):
    """The limit is set below what any worker already holds, so the watchdog
    is what fires -- not the clock, which is left long on purpose."""
    from pathlib import Path

    from load_run import load_run
    from pyoxigraph import Store

    path = tmp_path / "rss-store"
    store = Store(str(path))
    load_run(store, (Path(__file__).parent / "fixtures" / "run-with-samples.nq").read_bytes())
    del store

    pool = Pool(str(path), workers=1, budget=30.0, rss_limit_mb=1)
    pool.start()
    try:
        body, _, status, complete = pool.execute(COUNT_CROSS_JOIN)
        assert status == 507, (status, body)
        assert not complete
        assert b"MiB" in body
        # And the pool is usable again: a killed worker is replaced.
        assert pool.execute("SELECT * WHERE { ?s ?p ?o } LIMIT 1")[2] == 200
    finally:
        pool.stop()


def test_a_slow_but_modest_query_is_not_stopped_by_the_memory_watch(tmp_path):
    """The watch must only fire on queries that are actually over the limit.

    An instant query never reaches the check at all -- the answer arrives
    inside the first poll -- so testing with one proves nothing, and a watch
    that fired on everything passed such a test. This query runs for about a
    second across many poll intervals while holding very little, which is the
    case that separates "watching" from "refusing".
    """
    from pathlib import Path

    from load_run import load_run
    from pyoxigraph import Store

    path = tmp_path / "slow-store"
    store = Store(str(path))
    load_run(store, (Path(__file__).parent / "fixtures" / "run-with-samples.nq").read_bytes())
    del store

    pool = Pool(str(path), workers=1, budget=60.0, rss_limit_mb=4096)
    pool.start()
    try:
        body, _, status, complete = pool.execute(
            "SELECT * WHERE { GRAPH ?g {?a ?b ?c} . GRAPH ?h {?d ?e ?f} } LIMIT 20000"
        )
        assert status == 200, (status, body[:120])
        assert complete, "a query well under the limit was cut short"
    finally:
        pool.stop()

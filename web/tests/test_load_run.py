"""Tests for load_run: replacing a run's graph rather than merging into it.

See web/load_run.py's module docstring for the two things this module
exists to prevent: a changed re-run silently merging into the old graph
(producing a measurement with two dqv:value triples), and a malformed file
destroying an existing run before the parse failure is noticed.
"""

from pathlib import Path

import pytest
from pyoxigraph import NamedNode, Store

from load_run import LoadResult, load_run

FIXTURE = Path(__file__).parent / "fixtures" / "run-with-samples.nq"
TWO_SWEEPS_FIXTURE = Path(__file__).parent / "fixtures" / "run-two-sweeps.nq"


def test_loading_the_same_run_twice_leaves_one_graph(tmp_path):
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    first = len(store)
    load_run(store, FIXTURE.read_bytes())
    assert len(store) == first
    assert len(list(store.named_graphs())) == 1


def test_a_changed_rerun_replaces_rather_than_merges(tmp_path):
    """The hazard this function exists to prevent. The same --at re-run after an
    endpoint's DNS recovers produces the same run IRI with different verdicts.
    Merging leaves one measurement carrying two of them, which is a graph that
    contradicts itself, and the run graph is meant to be immutable."""
    store = Store(str(tmp_path / "s"))
    original = FIXTURE.read_text()
    changed = original.replace('"verified"', '"indeterminate"', 1)
    load_run(store, original.encode())
    load_run(store, changed.encode())
    rows = list(store.query("""
        SELECT ?m (COUNT(DISTINCT ?v) AS ?n) WHERE {
          GRAPH ?g { ?m <http://www.w3.org/ns/dqv#value> ?v }
        } GROUP BY ?m HAVING (COUNT(DISTINCT ?v) > 1)"""))
    assert rows == [], f"no measurement may carry two verdicts, got {len(rows)}"


def test_the_result_says_what_it_replaced(tmp_path):
    store = Store(str(tmp_path / "s"))
    first = load_run(store, FIXTURE.read_bytes())
    assert first.replaced == [], "nothing was there to replace"
    second = load_run(store, FIXTURE.read_bytes())
    assert len(second.replaced) == 1, "the second load replaced the run's graph"


def test_two_different_runs_coexist(tmp_path):
    """Runs are per-sweep named graphs, so loading a second run must not disturb
    the first. Without this, 'replace' could be implemented as 'clear the store'
    and every test above would still pass."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    other = FIXTURE.read_text().replace("2026-08-22T16:00:00Z", "2026-08-22T17:00:00Z")
    load_run(store, other.encode())
    assert len(list(store.named_graphs())) == 2


def test_a_malformed_file_leaves_an_existing_run_untouched(tmp_path):
    """The most important test in this task. A truncated .nq is not adversarial
    input, it is exactly what a crashed or interrupted prober writes. Dropping
    the named graphs before parsing would destroy the existing run before the
    parse failure is ever noticed: a real run measured going from 278 quads to
    0 under that order. Parsing first must reject the file before the store is
    touched at all."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    before = len(store)
    before_graphs = set(store.named_graphs())

    # Cut the file off mid-quad, the way a crash mid-write would.
    malformed = FIXTURE.read_bytes()[:1000]
    with pytest.raises(ValueError):
        load_run(store, malformed)

    assert len(store) == before, "a rejected load must not touch the store"
    assert set(store.named_graphs()) == before_graphs
    # Not just "the count didn't change": the actual content must still be
    # there, not some other 278 quads.
    still_present = store.query(
        'ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#value> "verified" } }'
    )
    assert bool(still_present), "the original run's content must survive"


def test_a_file_with_no_named_graphs_is_refused(tmp_path):
    """Every run this project emits is a named graph (the run IRI). A file of
    bare default-graph triples is a different thing than it claims to be, so
    refuse it rather than silently loading it into the store's default graph,
    where no run-replacement logic could ever find it again."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    before = len(store)

    default_graph_only = (
        b"<http://example.com/s> <http://example.com/p> <http://example.com/o> .\n"
    )
    with pytest.raises(ValueError):
        load_run(store, default_graph_only)

    assert len(store) == before, "a refused load must not touch the store"


def test_several_named_graphs_are_all_replaced_and_nothing_else_is(tmp_path):
    """The two-sweeps fixture carries two named graphs in one file. Loading it
    must replace both graphs it names on a second load, and must not disturb
    an unrelated third graph already in the store."""
    store = Store(str(tmp_path / "s"))
    other_run = FIXTURE.read_text().replace("2026-08-22T16:00:00Z", "2026-08-22T10:00:00Z")
    load_run(store, other_run.encode())
    other_graph = NamedNode("urn:sparqlwatch:run:2026-08-22T10:00:00Z")
    other_count = len(list(store.quads_for_pattern(None, None, None, other_graph)))

    first = load_run(store, TWO_SWEEPS_FIXTURE.read_bytes())
    assert first.replaced == [], "neither sweep graph existed yet"
    assert len(list(store.named_graphs())) == 3, "the unrelated graph plus both sweep graphs"

    second = load_run(store, TWO_SWEEPS_FIXTURE.read_bytes())
    assert len(second.replaced) == 2, "both named graphs were replaced"
    assert len(list(store.named_graphs())) == 3, "no graph was added or lost"
    assert len(list(store.quads_for_pattern(None, None, None, other_graph))) == other_count, (
        "the unrelated graph must be untouched by a load that does not name it"
    )


def test_load_result_reports_the_quad_count(tmp_path):
    store = Store(str(tmp_path / "s"))
    result = load_run(store, FIXTURE.read_bytes())
    assert result.quad_count == 278
    assert isinstance(result, LoadResult)


def test_an_interrupted_insert_says_what_the_store_is_left_holding(tmp_path, monkeypatch):
    """The window web/load_run.py admits it cannot close. remove_graph and extend
    are two store operations and pyoxigraph 0.5.9 has no transaction API, so an
    insert stopped by a full disk or an OOM kill leaves the graphs dropped and
    nothing inserted: measured at 278 quads going to zero. It cannot be made
    atomic, so it must be loud instead. The count is what makes it actionable,
    because the .nq file is the source of truth and a run graph is immutable: an
    operator told '0 of 278' re-runs the load and gets the run back exactly, as
    the end of this test does."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())

    def full_disk(self, quads):
        raise OSError("simulated full disk during insert")

    monkeypatch.setattr(Store, "extend", full_disk)
    with pytest.raises(RuntimeError, match="loaded 0 of 278 quads") as raised:
        load_run(store, FIXTURE.read_bytes())
    assert isinstance(raised.value.__cause__, OSError), (
        "the underlying failure must still be reachable, not swallowed"
    )
    assert len(store) == 0, "the window is real: the run is gone until it is re-loaded"

    monkeypatch.undo()
    again = load_run(store, FIXTURE.read_bytes())
    assert again.quad_count == 278
    assert len(store) == 278, "re-loading the same file restores the run exactly"
    assert bool(store.query(
        'ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#value> "verified" } }'
    )), "and restores its content, not merely its quad count"


def test_an_insert_that_silently_stores_nothing_is_not_a_successful_load(tmp_path, monkeypatch):
    """Worse than the crash above, because nothing anywhere would say the run is
    missing. A LoadResult reporting 278 quads over an empty graph is the store
    lying about what it holds, so the count is verified against the store rather
    than against the parse."""
    monkeypatch.setattr(Store, "extend", lambda self, quads: None)
    store = Store(str(tmp_path / "s"))
    with pytest.raises(RuntimeError, match="loaded 0 of 278 quads"):
        load_run(store, FIXTURE.read_bytes())

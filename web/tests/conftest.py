"""Shared pytest fixtures for the web test suite.

Each fixture below loads one committed run file (see web/tests/fixtures/)
into a fresh on-disk Oxigraph store rooted at pytest's per-test tmp_path.

They are function-scoped on purpose, not session-scoped over one shared
store: a store that persists across tests would let one test's mutation
change a later test's result depending on run order, and this project has
already been burned by tests whose outcome depended on things other than
what they claimed to check.
"""

from pathlib import Path

import pytest
from pyoxigraph import NamedNode, Store

from load_run import load_run

FIXTURES = Path(__file__).parent / "fixtures"

RUN_WITH_SAMPLES = FIXTURES / "run-with-samples.nq"
RUN_TRUNCATED = FIXTURES / "run-truncated.nq"
RUN_TWO_SWEEPS = FIXTURES / "run-two-sweeps.nq"
RUN_ZERO_CLASSES = FIXTURES / "run-zero-classes.nq"
RUN_PROPERTIES_SAMPLE = FIXTURES / "run-properties-sample.nq"
RUN_DECLINED = FIXTURES / "run-declined.nq"
RUN_CLASSES_ABSENT = FIXTURES / "run-classes-absent.nq"
RUN_LATER_SAMPLE_ONLY = FIXTURES / "run-later-sample-only.nq"
RUN_HOSTILE_LITERALS = FIXTURES / "run-hostile-literals.nq"
RUN_NEW_SUBJECTS = FIXTURES / "run-new-subjects.nq"
RUN_PROBER_FAILED = FIXTURES / "run-prober-failed.nq"
RUN_CRASHED_PARTWAY = FIXTURES / "run-crashed-partway.nq"


CURRENT_GRAPH = NamedNode("urn:sparqlwatch:current")


def run_graph_names(store: Store) -> list[NamedNode]:
    """The store's run graphs: every named graph except the derived one.

    load_run maintains urn:sparqlwatch:current beside the run graphs, so
    counting named_graphs() directly would count it as a run. It is not one: it
    holds no rdf:type prov:Activity triple at all, which is what
    test_current_holds_no_typed_activity pins and what keeps the newest-run
    aggregate in the read queries a question about runs.
    """
    return [graph for graph in store.named_graphs() if graph != CURRENT_GRAPH]


def run_quad_count(store: Store) -> int:
    """How many quads the store holds in its run graphs.

    Everything a run file states lands in a run graph, so this is what a
    fixture's quad count is a claim about. current holds copies of some of
    those quads, and counting those copies again would turn every fixture's
    count into a statement about the loader rather than about the file.
    """
    return sum(
        len(list(store.quads_for_pattern(None, None, None, graph)))
        for graph in run_graph_names(store)
    )


def run_graph_query(store: Store, query: str):
    """Ask ``query`` of the store's run graphs alone.

    Every assertion in web/tests/test_fixture.py is a claim about what a run
    FILE holds, and its queries match GRAPH ?g. current holds copies of some of
    those quads, so without this restriction a claim like "eight declines, one
    of them cost-ceiling" would count each of them twice and a fixture that
    really did lose a decline could still satisfy the count.
    """
    return store.query(query, named_graphs=run_graph_names(store))


def _loaded_store(tmp_path: Path, name: str, *fixtures: Path) -> Store:
    """One store holding every run file given, in the order given.

    More than one is the normal case for a real deployment: a store that has
    been swept twice holds two run graphs. Two fixtures in one store is how
    the "different runs answer different questions" cases below are built.

    Built through load_run() and not through Store.load(), because load_run()
    is the only way a run reaches a real store and it writes more than the run
    graph: it maintains the derived urn:sparqlwatch:current graph the three
    read queries read. A fixture built with a raw load holds run graphs and no
    current graph, which is a store shape the deployment never has, so every
    test over it would be testing a store that cannot exist.
    """
    store = Store(str(tmp_path / name))
    for fixture in fixtures:
        load_run(store, fixture.read_bytes())
    return store


@pytest.fixture
def store(tmp_path):
    """The real sweep: two endpoints with content samples, one run. See
    web/tests/test_fixture.py for its provenance."""
    return _loaded_store(tmp_path, "store", RUN_WITH_SAMPLES)


@pytest.fixture
def store_truncated(tmp_path):
    """A synthetic sample carrying sw:sampleTruncated true. See the comment
    in web/tests/fixtures/run-truncated.nq for why it is hand-built rather
    than captured."""
    return _loaded_store(tmp_path, "store-truncated", RUN_TRUNCATED)


@pytest.fixture
def store_two_sweeps(tmp_path):
    """The same endpoint sampled in two runs with different values, so
    'most recent run' logic is testable. See the comment in
    web/tests/fixtures/run-two-sweeps.nq for how it is derived."""
    return _loaded_store(tmp_path, "store-two-sweeps", RUN_TWO_SWEEPS)


@pytest.fixture
def store_zero_classes(tmp_path):
    """A synthetic sample that reports a size of 0 and lists no values, which
    the prober deliberately never writes. See the comment in
    web/tests/fixtures/run-zero-classes.nq."""
    return _loaded_store(tmp_path, "store-zero-classes", RUN_ZERO_CLASSES)


@pytest.fixture
def store_properties_sample(tmp_path):
    """A synthetic sample from a metric other than classes, so that the
    content query's metric pin can be tested. See the comment in
    web/tests/fixtures/run-properties-sample.nq."""
    return _loaded_store(tmp_path, "store-properties-sample", RUN_PROPERTIES_SAMPLE)


@pytest.fixture
def store_declined(tmp_path):
    """A real sweep captured at the default cost ceiling, where
    sw:metric:classes was declined for every endpoint. See the comment in
    web/tests/fixtures/run-declined.nq for its provenance."""
    return _loaded_store(tmp_path, "store-declined", RUN_DECLINED)


@pytest.fixture
def store_classes_absent(tmp_path):
    """A run whose sw:metric:classes verdict is "absent", the assertive
    negative, with no sample beside it. See the comment in
    web/tests/fixtures/run-classes-absent.nq."""
    return _loaded_store(tmp_path, "store-classes-absent", RUN_CLASSES_ABSENT)


@pytest.fixture
def store_stale_sample(tmp_path):
    """Two real sweeps in one store: the 16:00 sweep that sampled kadaster's
    classes, and the 18:00 sweep that declined sw:metric:classes on its cost
    ceiling and so published no sample.

    This is the steady state the prober's cost tiers produce, not an edge
    case: cheap sweeps run often and expensive ones rarely, so the newest run
    is routinely one that declined the metric that samples content. The
    newest run that MEASURED this endpoint (18:00) and the newest run that
    SAMPLED it (16:00) are then different runs, and every fact on the page
    has to be attributed to the sweep that observed it.
    """
    return _loaded_store(
        tmp_path, "store-stale-sample", RUN_WITH_SAMPLES, RUN_DECLINED
    )


@pytest.fixture
def store_later_sample(tmp_path):
    """The same skew as ``store_stale_sample``, running the other way: the
    16:00 sweep measured kadaster, and a 22:00 run sampled it and measured
    nothing. See the comment in
    web/tests/fixtures/run-later-sample-only.nq."""
    return _loaded_store(
        tmp_path, "store-later-sample", RUN_WITH_SAMPLES, RUN_LATER_SAMPLE_ONLY
    )


@pytest.fixture
def store_hostile_literals(tmp_path):
    """A run whose dqv:value and sw:notMeasuredReason literals carry HTML
    markup, which is the channel that can carry it: an IRI cannot hold '<',
    '>' or '"' at all. See the comment in
    web/tests/fixtures/run-hostile-literals.nq."""
    return _loaded_store(
        tmp_path, "store-hostile-literals", RUN_HOSTILE_LITERALS
    )


@pytest.fixture
def store_new_subjects(tmp_path):
    """Two real endpoints' worth of facts, published twice: once in the OLD
    row-index subject scheme (run-with-samples.nq, 16:00) and once in the NEW
    derived subject scheme (run-new-subjects.nq, 20:00, the one the prober
    writes after stage 1c-b3). This is the only fixture pairing that puts both
    subject schemes in one store, which is the shape a production store holds
    for as long as history is kept. See the comment in
    web/tests/fixtures/run-new-subjects.nq for its construction."""
    return _loaded_store(
        tmp_path, "store-new-subjects", RUN_WITH_SAMPLES, RUN_NEW_SUBJECTS
    )


@pytest.fixture
def store_prober_failed(tmp_path):
    """A run that failed on its one endpoint: seven metrics recorded as
    sw:notMeasuredReason "prober-failed" and one as "cost-ceiling", so both
    decline reasons are on one page. See the comment in
    web/tests/fixtures/run-prober-failed.nq for how it was produced."""
    return _loaded_store(tmp_path, "store-prober-failed", RUN_PROBER_FAILED)


@pytest.fixture
def store_crashed_partway(tmp_path):
    """The real 16:00 sweep of three endpoints, and a later run that died
    partway through: it wrote kadaster's chunk and never reached the other
    two, so it carries sw:emission and one sw:completedEndpoint and no
    sw:finalised.

    One store, two different answers, which is why this pairing is a single
    fixture. For kadaster the newest run that recorded anything is the crashed
    one, so the facts on the page come from a run that did not finish. For
    qlever.dev/api/osm-planet the crashed run recorded nothing, so the facts
    come from the 16:00 sweep and the only trace of tonight's failure is the
    newest activity in the store: no sw:finalised, and no sw:completedEndpoint
    naming qlever. A query scoped to one endpoint cannot see the second case
    at all, and at 548 endpoints it is the case a crash produces for every
    endpoint after the one it died on.

    See the comment in web/tests/fixtures/run-crashed-partway.nq for how the
    crashed run was produced.
    """
    return _loaded_store(
        tmp_path, "store-crashed-partway", RUN_WITH_SAMPLES, RUN_CRASHED_PARTWAY
    )

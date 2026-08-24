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
from pyoxigraph import RdfFormat, Store

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


def _loaded_store(tmp_path: Path, name: str, *fixtures: Path) -> Store:
    """One store holding every run file given, in the order given.

    More than one is the normal case for a real deployment: a store that has
    been swept twice holds two run graphs. Two fixtures in one store is how
    the "different runs answer different questions" cases below are built.
    """
    store = Store(str(tmp_path / name))
    for fixture in fixtures:
        store.load(fixture.read_bytes(), format=RdfFormat.N_QUADS)
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

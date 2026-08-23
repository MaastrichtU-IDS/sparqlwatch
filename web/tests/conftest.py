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


def _loaded_store(tmp_path: Path, name: str, fixture: Path) -> Store:
    store = Store(str(tmp_path / name))
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

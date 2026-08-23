"""Proves the fixture files are what they claim to be, and that an Oxigraph
store built from them persists across a reopen.

Fixture provenance:

- ``fixtures/run-with-samples.nq`` is a REAL sweep, copied byte-for-byte from
  a probe run captured at 2026-08-22T16:00:00Z (278 quads, one named graph).
  It carries content samples for two endpoints: data.kkg.kadaster.nl/query
  (59 sampled classes) and ontop.certain.ai.ustp.at/sparql (50 sampled
  classes). A third endpoint, qlever.dev/api/osm-planet, appears in the run
  with no content sample, because its class enumeration exceeded the request
  budget rather than being declined outright.

- ``fixtures/run-truncated.nq`` is SYNTHETIC: hand-built because no endpoint
  in the registry holds more than 200 classes, so a real
  ``sw:sampleTruncated true`` cannot be captured live. See the comment at the
  top of that file for the full reasoning.

- ``fixtures/run-two-sweeps.nq`` is SYNTHETIC and DERIVED from the real run:
  the real run plus a second copy of it with the run IRI rewritten and one
  kadaster class swapped, so that "the same endpoint sampled twice, with
  different values" is testable. One run cannot exercise most-recent-run
  logic on its own. See the comment at the top of that file for the exact
  construction.
"""

from pathlib import Path

from pyoxigraph import RdfFormat, Store

FIXTURE = Path(__file__).parent / "fixtures" / "run-with-samples.nq"
TRUNCATED_FIXTURE = Path(__file__).parent / "fixtures" / "run-truncated.nq"
TWO_SWEEPS_FIXTURE = Path(__file__).parent / "fixtures" / "run-two-sweeps.nq"


def test_the_fixture_loads_and_reopens(tmp_path):
    """A store must persist. An in-memory store would pass every query test in
    this suite and be useless to a web tier that opens the store in a
    different process."""
    store = Store(str(tmp_path / "s"))
    store.load(FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    loaded = len(store)
    assert loaded == 278, f"the fixture is 278 quads, got {loaded}"
    assert len(list(store.named_graphs())) == 1, "one run, one named graph"
    del store
    assert len(Store(str(tmp_path / "s"))) == loaded, "reopening must see the same quads"


def test_the_truncated_fixture_is_the_shape_it_claims(tmp_path):
    """run-truncated.nq is hand-built and synthetic (see the comment at the
    top of that file). Assert its exact quad count, not merely that it loads:
    a fixture that silently lost content would still pass every query test
    built on it, and quietly stop testing what it claims to test."""
    store = Store(str(tmp_path / "s"))
    store.load(TRUNCATED_FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    assert len(store) == 12, f"the truncated fixture is 12 quads, got {len(store)}"
    assert len(list(store.named_graphs())) == 1


def test_the_two_sweeps_fixture_is_the_shape_it_claims(tmp_path):
    """run-two-sweeps.nq is derived from the real run (see the comment at the
    top of that file): the real run plus a rewritten, altered copy of it, so
    two runs of the same endpoint coexist with different values."""
    store = Store(str(tmp_path / "s"))
    store.load(TWO_SWEEPS_FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    assert len(store) == 556, f"the two-sweeps fixture is 556 quads, got {len(store)}"
    assert len(list(store.named_graphs())) == 2, "two sweeps, two named graphs"

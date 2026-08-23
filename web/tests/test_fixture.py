"""Proves the fixture files are what they claim to be, and that an Oxigraph
store built from them persists across a reopen.

Fixture provenance:

- ``fixtures/run-with-samples.nq`` is a REAL sweep, copied byte-for-byte from
  a probe run captured at 2026-08-22T16:00:00Z (278 quads, one named graph).
  It carries content samples for two endpoints: data.kkg.kadaster.nl/query
  (59 sampled classes) and ontop.certain.ai.ustp.at/sparql (50 sampled
  classes). A third endpoint, qlever.dev/api/osm-planet, appears in the run
  with no content sample. The reason is a timeout and not a cost-ceiling
  decline: its metric:classes measurement in the same graph is dqv:value
  "indeterminate" with sw:elapsedMs "30003", the 30 second request budget
  running out. The run was captured with ``--max-cost expensive``, where
  nothing is declined, so it holds no ``sw:NotMeasured`` resource at all.

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

- ``fixtures/run-zero-classes.nq`` is SYNTHETIC: a sample reporting a size of
  0 and listing no values, which the prober deliberately never writes (it
  skips a sample that bound nothing, so that a size of 0 cannot be misread as
  "this endpoint holds no classes"). It exists to make the OPTIONAL around
  sw:sampledValue testable, so that "sampled and found nothing" stays
  distinguishable from "no run sampled this endpoint".

- ``fixtures/run-properties-sample.nq`` is SYNTHETIC: one sample carrying
  sw:sampledBy sw:metric:properties, a metric no captured sweep has yet run
  (spec stage 2b). It exists to make the content query's pin to
  sw:metric:classes testable, so that another metric's values can never be
  published as an endpoint's classes.

- ``fixtures/run-classes-absent.nq`` is SYNTHETIC: a run whose
  sw:metric:classes measurement reads dqv:value "absent", which no captured
  sweep holds, with no sample beside it. "absent" is one of the two
  assertive verdicts, so it is the one missing sample that is a finding
  rather than a gap, and the page has to say so.

- ``fixtures/run-later-sample-only.nq`` is SYNTHETIC: a run that publishes a
  class sample for data.kkg.kadaster.nl/query and measures nothing. Loaded
  beside the real 16:00 sweep it makes "the newest run that measured this
  endpoint is not the newest run that sampled it" testable in the direction
  where the sample is the newer of the two; run-with-samples.nq beside
  run-declined.nq gives the other direction, an older sample under a newer
  sweep that declined the metric.

- ``fixtures/run-hostile-literals.nq`` is SYNTHETIC: one dqv:value and one
  sw:notMeasuredReason carrying HTML markup, which no captured sweep holds.
  They are the literals, not the IRIs, because an IRI cannot contain '<',
  '>' or '"' at all and pyoxigraph rejects one that tries, so the literals
  are the values that can carry markup onto the page. Both reach the page
  verbatim, in a data- attribute and in the text a reader sees.
"""

from pathlib import Path

from pyoxigraph import RdfFormat, Store

FIXTURE = Path(__file__).parent / "fixtures" / "run-with-samples.nq"
TRUNCATED_FIXTURE = Path(__file__).parent / "fixtures" / "run-truncated.nq"
TWO_SWEEPS_FIXTURE = Path(__file__).parent / "fixtures" / "run-two-sweeps.nq"
ZERO_CLASSES_FIXTURE = Path(__file__).parent / "fixtures" / "run-zero-classes.nq"
PROPERTIES_FIXTURE = Path(__file__).parent / "fixtures" / "run-properties-sample.nq"
CLASSES_ABSENT_FIXTURE = Path(__file__).parent / "fixtures" / "run-classes-absent.nq"
LATER_SAMPLE_FIXTURE = (
    Path(__file__).parent / "fixtures" / "run-later-sample-only.nq"
)


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


def test_the_zero_classes_fixture_is_the_shape_it_claims(tmp_path):
    """run-zero-classes.nq is hand-built (see its header). Its whole point is a
    sample with a size and NO sw:sampledValue, so assert the absence too: a
    fixture that quietly gained a value would keep passing while no longer
    testing the OPTIONAL it exists for."""
    store = Store(str(tmp_path / "s"))
    store.load(ZERO_CLASSES_FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    assert len(store) == 9, f"the zero-classes fixture is 9 quads, got {len(store)}"
    assert len(list(store.named_graphs())) == 1
    assert not bool(store.query(
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledValue> ?v } }"
    )), "the sample must list no values at all"


def test_the_properties_fixture_samples_no_classes(tmp_path):
    """run-properties-sample.nq is hand-built (see its header). Assert that the
    metric really is properties and that nothing in it is sampled by
    metric:classes, which is the only reason the fixture tells the query's
    metric pin from its absence."""
    store = Store(str(tmp_path / "s"))
    store.load(PROPERTIES_FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    assert len(store) == 11, f"the properties fixture is 11 quads, got {len(store)}"
    assert bool(store.query(
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledBy> "
        "<urn:sparqlwatch:metric:properties> } }"
    )), "the sample must be sampledBy metric:properties"
    assert not bool(store.query(
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledBy> "
        "<urn:sparqlwatch:metric:classes> } }"
    )), "nothing here may be sampledBy metric:classes"


def test_the_classes_absent_fixture_measures_absent_and_samples_nothing(tmp_path):
    """run-classes-absent.nq is hand-built (see its header). Both halves are
    the point: the classes verdict is "absent", and there is no
    sw:ContentSample anywhere, because the prober writes no sample beside
    that verdict. A fixture that gained one would stop testing the case."""
    store = Store(str(tmp_path / "s"))
    store.load(CLASSES_ABSENT_FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    assert len(store) == 16, f"this fixture is 16 quads, got {len(store)}"
    assert len(list(store.named_graphs())) == 1
    assert bool(store.query(
        "ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#isMeasurementOf> "
        "<urn:sparqlwatch:metric:classes> ; "
        "<http://www.w3.org/ns/dqv#value> 'absent' } }"
    )), "the classes metric must read absent"
    assert not bool(store.query(
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledFrom> ?e } }"
    )), "there must be no content sample at all"


def test_the_later_sample_fixture_samples_without_measuring(tmp_path):
    """run-later-sample-only.nq is hand-built (see its header). It must hold a
    class sample for kadaster and NO measurement and no decline, because that
    is what makes the run invisible to endpoint_measurements.rq and so
    produces the two-run skew it exists for."""
    store = Store(str(tmp_path / "s"))
    store.load(LATER_SAMPLE_FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    assert len(store) == 12, f"this fixture is 12 quads, got {len(store)}"
    assert bool(store.query(
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledFrom> "
        "<https://data.kkg.kadaster.nl/query> ; "
        "<urn:sparqlwatch:sampledBy> <urn:sparqlwatch:metric:classes> } }"
    )), "the sample must be kadaster's classes"
    for predicate in (
        "<http://www.w3.org/ns/dqv#computedOn>",
        "<urn:sparqlwatch:notMeasuredOn>",
    ):
        assert not bool(store.query(
            f"ASK {{ GRAPH ?g {{ ?s {predicate} ?e }} }}"
        )), f"this run must record no {predicate}"

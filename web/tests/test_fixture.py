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

- ``fixtures/run-prober-failed.nq`` is SYNTHETIC, and it is the emitter's own
  output rather than a hand-written file: ``prober/src/emit.rs``'s
  ``emit_nquads`` was called with the eight metrics of ``prober/metrics.toml``
  against one endpoint, the seven cheap ones as
  ``NotMeasuredReason::ProberFailed`` and the one expensive one
  (``metric:classes``) as ``NotMeasuredReason::CostCeiling``. It cannot be a
  captured sweep: "prober-failed" is published only when the task probing an
  endpoint's host panicked or was cancelled. Both reasons in one run is the
  point, because they are opposite claims about who is responsible and a
  fixture carrying one reason cannot tell a page that renders one sentence for
  every decline from a page that reads the reason.

- ``fixtures/run-crashed-partway.nq`` is SYNTHETIC, and like
  run-prober-failed.nq it is the emitter's own output rather than a
  hand-written file: ``prober/src/emit.rs``'s ``emit_header`` was called once,
  then ``emit_endpoint`` once for data.kkg.kadaster.nl/query, and
  ``emit_footer`` was NOT called. That last absence is the whole fixture, and
  it cannot be a captured sweep, because producing one live would mean
  shipping a prober that dies halfway through a run. It is the file a crash
  leaves on disk and, because its last statement is a chunk's
  sw:completedEndpoint terminator, it is also exactly what web/load_run.py
  loads from that file. Loaded beside run-with-samples.nq it produces both of
  the read tier's two unfinished-run conditions in one store: kadaster's facts
  come from a run that did not finish, and qlever.dev/api/osm-planet's come
  from the 16:00 sweep with a newer, unfinished run in the store that never
  named it. See the comment at the top of that file for the exact inputs.

- ``fixtures/run-new-subjects.nq`` is SYNTHETIC and DERIVED from the real run,
  the same way ``run-two-sweeps.nq`` is: the full real run from
  run-with-samples.nq, with its run IRI and prov:generatedAtTime advanced
  from 2026-08-22T16:00:00Z to 2026-08-22T20:00:00Z, and every
  ``urn:sparqlwatch:measurement:<run>:<row index>`` and
  ``urn:sparqlwatch:content-sample:<run>:<row index>`` subject rewritten into
  the derived scheme prober/src/emit.rs's ``subject_iri`` produces after
  stage 1c-b3:
  ``urn:sparqlwatch:<kind>:<run>:<percent-encoded endpoint>:<metric id>``.
  It is the FIRST fixture in that derived scheme; every other fixture in this
  list, including run-with-samples.nq itself, still carries the old row-index
  shape, which is exactly the point: after stage 1c-b3 the prober can no
  longer produce that shape, so a production store holds both for as long as
  history is kept, and this is the only fixture pairing that puts both in one
  store (see ``conftest.py``'s ``store_new_subjects``). Two further hand
  edits on top of the mechanical rewrite, mirroring run-two-sweeps.nq's one
  hand edit: kadaster's sw:metric:classes dqv:value in this (20:00) run was
  changed from "verified" to "indeterminate", and one of its sampled classes
  was swapped from ``owl:Restriction`` to
  ``urn:sparqlwatch:test:new-scheme-only-class``, so that "which run's verdict
  and sample come back" is testable regardless of which subject scheme either
  run uses. See the comment at the top of that file for the exact
  construction.

- ``fixtures/run-registry-sample.nq`` is SYNTHETIC and DERIVED, and it is
  derived by CUTTING rather than by rewriting: it is a strict line subset of
  ``~/code/sparqlwatch-runs/run-2026-08-24T19-45-03Z-lod-cloud-543.nq``, the
  project's first registry-scale sweep, in that file's own order. Nothing was
  rewritten, so the run IRI, the prov:generatedAtTime, the verdicts and the
  elapsed times are the real sweep's and every quad here appears verbatim in
  it. What was dropped is 534 of its 543 endpoint chunks. The whole sweep is
  6.3 MB and is deliberately not in git (``~/code/sparqlwatch-runs/README.md``
  says why), and the index this fixture exists for needs nine rows rather than
  543 to be testable.

  The nine were chosen to cover every verdict value that sweep produced.
  Their availability verdicts, which is what the index groups by, are three
  "verified", four "indeterminate" and two "absent", and two of the sweep's
  only four "absent" endpoints are here on purpose: one of them is a .ttl file
  on raw.githubusercontent.com, so the host answered with something that was
  not a SPARQL result, which is what that verdict means and why it is not the
  same fact as "indeterminate". Their other metrics carry "verified",
  "undeclared-but-verified", "declared-but-wrong", "indeterminate" and
  "absent"; "declared-only" appears nowhere, in this file or in the 543 it was
  cut from. Every one of the nine also carries a "cost-ceiling" decline of
  sw:metric:classes, because the sweep ran at the cheap ceiling, so there is
  no content sample anywhere in it. See the comment at the top of the file for
  which nine and what each one is for.
"""

from pathlib import Path

from pyoxigraph import NamedNode, Store

from conftest import run_graph_names, run_graph_query, run_quad_count
from load_run import load_run

FIXTURE = Path(__file__).parent / "fixtures" / "run-with-samples.nq"
TRUNCATED_FIXTURE = Path(__file__).parent / "fixtures" / "run-truncated.nq"
TWO_SWEEPS_FIXTURE = Path(__file__).parent / "fixtures" / "run-two-sweeps.nq"
ZERO_CLASSES_FIXTURE = Path(__file__).parent / "fixtures" / "run-zero-classes.nq"
PROPERTIES_FIXTURE = Path(__file__).parent / "fixtures" / "run-properties-sample.nq"
CLASSES_ABSENT_FIXTURE = Path(__file__).parent / "fixtures" / "run-classes-absent.nq"
LATER_SAMPLE_FIXTURE = (
    Path(__file__).parent / "fixtures" / "run-later-sample-only.nq"
)
NEW_SUBJECTS_FIXTURE = Path(__file__).parent / "fixtures" / "run-new-subjects.nq"
PROBER_FAILED_FIXTURE = (
    Path(__file__).parent / "fixtures" / "run-prober-failed.nq"
)
CRASHED_PARTWAY_FIXTURE = (
    Path(__file__).parent / "fixtures" / "run-crashed-partway.nq"
)
REGISTRY_SAMPLE_FIXTURE = (
    Path(__file__).parent / "fixtures" / "run-registry-sample.nq"
)
NO_AVAILABILITY_FIXTURE = (
    Path(__file__).parent / "fixtures" / "run-no-availability.nq"
)


def test_the_fixture_loads_and_reopens(tmp_path):
    """A store must persist. An in-memory store would pass every query test in
    this suite and be useless to a web tier that opens the store in a
    different process."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    loaded = run_quad_count(store)
    assert loaded == 278, f"the fixture is 278 quads, got {loaded}"
    assert len(run_graph_names(store)) == 1, "one run, one named graph"
    del store
    reopened = Store(str(tmp_path / "s"))
    assert run_quad_count(reopened) == loaded, "reopening must see the same quads"
    assert reopened.contains_named_graph(NamedNode("urn:sparqlwatch:current")), (
        "and the derived graph the read queries read must persist too"
    )


def test_the_truncated_fixture_is_the_shape_it_claims(tmp_path):
    """run-truncated.nq is hand-built and synthetic (see the comment at the
    top of that file). Assert its exact quad count, not merely that it loads:
    a fixture that silently lost content would still pass every query test
    built on it, and quietly stop testing what it claims to test."""
    store = Store(str(tmp_path / "s"))
    load_run(store, TRUNCATED_FIXTURE.read_bytes())
    assert run_quad_count(store) == 12, f"the truncated fixture is 12 quads, got {run_quad_count(store)}"
    assert len(run_graph_names(store)) == 1


def test_the_two_sweeps_fixture_is_the_shape_it_claims(tmp_path):
    """run-two-sweeps.nq is derived from the real run (see the comment at the
    top of that file): the real run plus a rewritten, altered copy of it, so
    two runs of the same endpoint coexist with different values."""
    store = Store(str(tmp_path / "s"))
    load_run(store, TWO_SWEEPS_FIXTURE.read_bytes())
    assert run_quad_count(store) == 556, f"the two-sweeps fixture is 556 quads, got {run_quad_count(store)}"
    assert len(run_graph_names(store)) == 2, "two sweeps, two named graphs"


def test_the_zero_classes_fixture_is_the_shape_it_claims(tmp_path):
    """run-zero-classes.nq is hand-built (see its header). Its whole point is a
    sample with a size and NO sw:sampledValue, so assert the absence too: a
    fixture that quietly gained a value would keep passing while no longer
    testing the OPTIONAL it exists for."""
    store = Store(str(tmp_path / "s"))
    load_run(store, ZERO_CLASSES_FIXTURE.read_bytes())
    assert run_quad_count(store) == 9, f"the zero-classes fixture is 9 quads, got {run_quad_count(store)}"
    assert len(run_graph_names(store)) == 1
    assert not bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledValue> ?v } }"
    )), "the sample must list no values at all"


def test_the_properties_fixture_samples_no_classes(tmp_path):
    """run-properties-sample.nq is hand-built (see its header). Assert that the
    metric really is properties and that nothing in it is sampled by
    metric:classes, which is the only reason the fixture tells the query's
    metric pin from its absence."""
    store = Store(str(tmp_path / "s"))
    load_run(store, PROPERTIES_FIXTURE.read_bytes())
    assert run_quad_count(store) == 11, f"the properties fixture is 11 quads, got {run_quad_count(store)}"
    assert bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledBy> "
        "<urn:sparqlwatch:metric:properties> } }"
    )), "the sample must be sampledBy metric:properties"
    assert not bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledBy> "
        "<urn:sparqlwatch:metric:classes> } }"
    )), "nothing here may be sampledBy metric:classes"


def test_the_classes_absent_fixture_measures_absent_and_samples_nothing(tmp_path):
    """run-classes-absent.nq is hand-built (see its header). Both halves are
    the point: the classes verdict is "absent", and there is no
    sw:ContentSample anywhere, because the prober writes no sample beside
    that verdict. A fixture that gained one would stop testing the case."""
    store = Store(str(tmp_path / "s"))
    load_run(store, CLASSES_ABSENT_FIXTURE.read_bytes())
    assert run_quad_count(store) == 16, f"this fixture is 16 quads, got {run_quad_count(store)}"
    assert len(run_graph_names(store)) == 1
    assert bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#isMeasurementOf> "
        "<urn:sparqlwatch:metric:classes> ; "
        "<http://www.w3.org/ns/dqv#value> 'absent' } }"
    )), "the classes metric must read absent"
    assert not bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledFrom> ?e } }"
    )), "there must be no content sample at all"


def test_the_no_availability_fixture_measures_one_metric_and_not_availability(
    tmp_path,
):
    """run-no-availability.nq is hand-built (see its header), and the whole
    fixture is the ABSENCE of two quads rather than the presence of one.

    So both halves are asserted, and the second is the one that matters: there
    must be no availability fact of EITHER kind. A fixture that gained a decline
    would land in the index's final group for the ordinary reason and stop being
    the second way to get there; one that gained a measurement would leave the
    group altogether."""
    store = Store(str(tmp_path / "s"))
    load_run(store, NO_AVAILABILITY_FIXTURE.read_bytes())
    assert run_quad_count(store) == 10, (
        f"this fixture is 10 quads, got {run_quad_count(store)}"
    )
    assert len(run_graph_names(store)) == 1
    assert bool(run_graph_query(store,
        "ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#isMeasurementOf> "
        "<urn:sparqlwatch:metric:classes> ; "
        "<http://www.w3.org/ns/dqv#value> 'absent' } }"
    )), "one metric must be measured, or the run recorded nothing at all"
    assert not bool(run_graph_query(store,
        "ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#isMeasurementOf> "
        "<urn:sparqlwatch:metric:availability> } }"
    )), "availability must not be measured"
    assert not bool(run_graph_query(store,
        "ASK { GRAPH ?g { ?n <urn:sparqlwatch:notMeasuredMetric> "
        "<urn:sparqlwatch:metric:availability> } }"
    )), "and it must not be declined either: that is the whole fixture"


NEW_SUBJECTS_RUN = "2026-08-22T20:00:00Z"
PROBER_FAILED_RUN = "2026-08-23T02:00:00Z"
CRASHED_PARTWAY_RUN = "2026-08-23T04:00:00Z"

# The two facts each kind of subject is derived from, as (kind segment, the
# predicate naming the endpoint, the predicate naming the metric). Both pairs
# are read off the fact itself, which is the whole property the derived scheme
# claims: the subject says nothing the quads beside it do not already say.
_DERIVED_FROM = (
    (
        "measurement",
        "http://www.w3.org/ns/dqv#computedOn",
        "http://www.w3.org/ns/dqv#isMeasurementOf",
    ),
    (
        "content-sample",
        "urn:sparqlwatch:sampledFrom",
        "urn:sparqlwatch:sampledBy",
    ),
    (
        "not-measured",
        "urn:sparqlwatch:notMeasuredOn",
        "urn:sparqlwatch:notMeasuredMetric",
    ),
)


def _encode_unreserved(s: str) -> str:
    """Percent-encode keeping RFC 3986's unreserved set, uppercase hex.

    A deliberate second implementation of prober/src/emit.rs's
    ``encode_unreserved``, and the reason it is worth having twice is that
    the fixture is a committed static file: nothing else in this suite can
    notice it drifting away from the scheme its header claims. Kept to the
    one property that matters here, that the encoding is reversible and
    normalises nothing, so a fixture whose subject lowercased or stripped
    anything fails.
    """
    unreserved = set(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~"
    )
    out = []
    for byte in s.encode("utf-8"):
        char = chr(byte)
        out.append(char if char in unreserved else f"%{byte:02X}")
    return "".join(out)


def _derived_subjects(store, run: str) -> dict[str, str]:
    """Every run-scoped subject in ``store``, mapped to what the scheme says
    it should be, derived from that subject's own two facts."""
    expected = {}
    for kind, on, of in _DERIVED_FROM:
        rows = run_graph_query(store, 
            "SELECT ?s ?e ?m WHERE { GRAPH ?g { "
            f"?s <{on}> ?e ; <{of}> ?m" + " } }"
        )
        for row in rows:
            metric = row["m"].value.removeprefix("urn:sparqlwatch:metric:")
            expected[row["s"].value] = (
                f"urn:sparqlwatch:{kind}:{run}:"
                f"{_encode_unreserved(row['e'].value)}:{metric}"
            )
    return expected


def test_the_new_subjects_fixture_is_the_shape_it_claims(tmp_path):
    """run-new-subjects.nq is derived from the real run (see the comment at
    the top of that file): the same 278 quads as run-with-samples.nq, with
    the run IRI advanced and every subject rewritten into the derived scheme.
    Same quad count as the real run because the rewrite touches subjects and
    two values, never adds or drops a triple.

    Every subject is checked, not one of them: this fixture is what stands in
    for the derived scheme in the Python suite, and an earlier form of this
    test named the single subject ``...:0``, which a fixture that kept
    ``...:1`` through ``...:23`` would have passed.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, NEW_SUBJECTS_FIXTURE.read_bytes())
    assert run_quad_count(store) == 278, f"the new-subjects fixture is 278 quads, got {run_quad_count(store)}"
    assert len(run_graph_names(store)) == 1, "one run, one named graph"
    assert bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledValue> "
        "<urn:sparqlwatch:test:new-scheme-only-class> } }"
    )), "the hand-swapped class must be present"

    expected = _derived_subjects(store, NEW_SUBJECTS_RUN)
    assert len(expected) == 26, (
        f"the fixture holds 26 run-scoped facts, found {len(expected)}"
    )
    wrong = {s: want for s, want in expected.items() if s != want}
    assert not wrong, f"these subjects are not what the scheme derives: {wrong}"


def test_the_prober_failed_fixture_is_the_shape_it_claims(tmp_path):
    """run-prober-failed.nq is emit_nquads' own output (see its header).

    Both reasons are asserted by count, because a fixture that lost the
    prober-failed rows would still render a page and still pass every test
    that only asks for a decline. The absence of sw:declarationsRead is
    asserted too: nothing was ever fetched from this endpoint, so a fact
    saying whether its declarations were read would be an answer no request
    was made for.

    The three section terminators are asserted here because this is the only
    committed fixture that claims to be current emitter output. What that
    catches is this file going stale, which it has: it was regenerated
    mid-branch at 51 quads from a 48-quad version that had been passing. What
    it does NOT catch is the emitter renaming a terminator, because these bytes
    are a frozen copy and nothing in prober/ regenerates or diffs them, so a
    rename would move the emitter and leave this fixture, and both suites,
    agreeing on the old spelling. That seam is held by
    docs/design/section-terminators.md, which both sides read: see
    test_the_loader_recognises_exactly_the_documented_terminators in
    test_load_run.py and the emitter's own
    the_emitter_writes_the_terminators_the_shared_wire_format_names.

    Every other fixture is a captured historical sweep and carries no
    terminator on purpose.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, PROBER_FAILED_FIXTURE.read_bytes())
    assert run_quad_count(store) == 51, f"this fixture is 51 quads, got {run_quad_count(store)}"
    assert len(run_graph_names(store)) == 1, "one run, one named graph"

    # A complete run: the header's terminator, one chunk terminator naming the
    # one endpoint, and the footer's. A fixture missing any of the three would
    # be a run this emitter cannot produce, and the loader that has to
    # recognise all three would have nothing here to recognise.
    for ask, why in (
        ('?a <urn:sparqlwatch:emission> "incremental"',
         "the header says how the run is written"),
        ("?a <urn:sparqlwatch:completedEndpoint> "
         "<https://data.kkg.kadaster.nl/query>",
         "the chunk says which endpoint the run finished"),
        ('?a <urn:sparqlwatch:finalised> true',
         "the footer says the run finished"),
    ):
        assert bool(run_graph_query(store, f"ASK {{ GRAPH ?g {{ {ask} }} }}")), why
    markers = list(run_graph_query(store, 
        "SELECT ?e WHERE { GRAPH ?g { ?a "
        "<urn:sparqlwatch:completedEndpoint> ?e } }"
    ))
    assert len(markers) == 1, (
        f"one endpoint, so exactly one chunk terminator, got {len(markers)}"
    )

    reasons = [
        row["r"].value
        for row in run_graph_query(store, 
            "SELECT ?r WHERE { GRAPH ?g { ?s "
            "<urn:sparqlwatch:notMeasuredReason> ?r } }"
        )
    ]
    assert sorted(reasons) == ["cost-ceiling"] + ["prober-failed"] * 7

    for predicate in (
        "<urn:sparqlwatch:declarationsRead>",
        "<http://www.w3.org/ns/dqv#value>",
        "<urn:sparqlwatch:sampledFrom>",
    ):
        assert not bool(run_graph_query(store, 
            f"ASK {{ GRAPH ?g {{ ?s {predicate} ?o }} }}"
        )), f"a failed endpoint has no {predicate}"

    assert bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?a <urn:sparqlwatch:failedEndpoints> "
        '1 } }'
    )), "the run must say how many endpoints it failed on"

    # The `not-measured` arm of `_DERIVED_FROM` matches nothing in
    # run-new-subjects.nq, which is an expensive sweep with no declines, so
    # this is the only fixture that exercises it. Without this the arm is dead
    # and a not-measured subject could drift from the scheme unnoticed.
    expected = _derived_subjects(store, PROBER_FAILED_RUN)
    assert len(expected) == 8, (
        f"eight not-measured subjects to check, derived {len(expected)}"
    )
    subjects = {
        row["s"].value
        for row in run_graph_query(store, 
            "SELECT DISTINCT ?s WHERE { GRAPH ?g { ?s "
            "<urn:sparqlwatch:notMeasuredOn> ?e } }"
        )
    }
    assert subjects == set(expected.values()), (
        "a not-measured subject is not what the derived scheme says it is"
    )


def test_the_later_sample_fixture_samples_without_measuring(tmp_path):
    """run-later-sample-only.nq is hand-built (see its header). It must hold a
    class sample for kadaster and NO measurement and no decline, because that
    is what makes the run invisible to endpoint_measurements.rq and so
    produces the two-run skew it exists for."""
    store = Store(str(tmp_path / "s"))
    load_run(store, LATER_SAMPLE_FIXTURE.read_bytes())
    assert run_quad_count(store) == 12, f"this fixture is 12 quads, got {run_quad_count(store)}"
    assert bool(run_graph_query(store, 
        "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledFrom> "
        "<https://data.kkg.kadaster.nl/query> ; "
        "<urn:sparqlwatch:sampledBy> <urn:sparqlwatch:metric:classes> } }"
    )), "the sample must be kadaster's classes"
    for predicate in (
        "<http://www.w3.org/ns/dqv#computedOn>",
        "<urn:sparqlwatch:notMeasuredOn>",
    ):
        assert not bool(run_graph_query(store, 
            f"ASK {{ GRAPH ?g {{ ?s {predicate} ?e }} }}"
        )), f"this run must record no {predicate}"


def test_the_crashed_partway_fixture_stops_before_its_footer(tmp_path):
    """run-crashed-partway.nq is emit_header plus one emit_endpoint and no
    emit_footer (see its header).

    Every assertion here is about what the file does NOT hold, because that
    is what it exists for: no sw:finalised, and so no sw:failedEndpoints
    either, since both are footer facts. A fixture that gained a footer would
    still render a page and still pass every test that only asks for a
    verdict, while silently becoming a finished run.

    The two facts it does hold are asserted beside them, because "no
    sw:finalised" only means "this run did not finish" for a run that
    promised one: sw:emission is the promise, and the chunk terminator is
    what makes "the run got as far as this endpoint" a fact rather than an
    inference from the presence of its measurements.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, CRASHED_PARTWAY_FIXTURE.read_bytes())
    assert run_quad_count(store) == 67, f"this fixture is 67 quads, got {run_quad_count(store)}"
    assert len(run_graph_names(store)) == 1, "one run, one named graph"

    for ask, why in (
        ('?a <urn:sparqlwatch:emission> "incremental"',
         "the header says how the run is written"),
        ("?a <urn:sparqlwatch:completedEndpoint> "
         "<https://data.kkg.kadaster.nl/query>",
         "the one chunk says the run finished that endpoint"),
    ):
        assert bool(run_graph_query(store, f"ASK {{ GRAPH ?g {{ {ask} }} }}")), why

    for predicate in (
        "<urn:sparqlwatch:finalised>",
        "<urn:sparqlwatch:failedEndpoints>",
    ):
        assert not bool(run_graph_query(store, 
            f"ASK {{ GRAPH ?g {{ ?a {predicate} ?o }} }}"
        )), f"no footer was written, so there is no {predicate}"

    markers = list(run_graph_query(store, 
        "SELECT ?e WHERE { GRAPH ?g { ?a "
        "<urn:sparqlwatch:completedEndpoint> ?e } }"
    ))
    assert len(markers) == 1, (
        f"one chunk was written, so one terminator, got {len(markers)}"
    )

    # The file must be loadable as it stands, which is the claim that it is
    # what load_run.py would put in the store rather than merely what the
    # prober would have written: the last statement is a terminator, so the
    # loader's cut-back to the last terminator removes nothing.
    assert CRASHED_PARTWAY_FIXTURE.read_text().rstrip().endswith(
        "<urn:sparqlwatch:completedEndpoint> "
        "<https://data.kkg.kadaster.nl/query> "
        "<urn:sparqlwatch:run:2026-08-23T04:00:00Z> ."
    ), "a crashed run's file ends at its last whole section"

    # The subjects are the derived scheme, because this run's emitter is the
    # one that produces it. Same check as the two fixtures above, and it is
    # the only one that can notice a committed static file drifting away from
    # the scheme its header claims.
    expected = _derived_subjects(store, CRASHED_PARTWAY_RUN)
    assert len(expected) == 9, (
        f"eight measurements and one sample to check, derived {len(expected)}"
    )
    wrong = {s: want for s, want in expected.items() if s != want}
    assert not wrong, f"these subjects are not what the scheme derives: {wrong}"


def test_the_registry_sample_fixture_is_the_shape_it_claims(tmp_path):
    """run-registry-sample.nq is nine endpoint chunks cut out of the real
    543-endpoint sweep (see its header and the provenance list above).

    THE ENDPOINT COUNT ASSERTED IS THE FIXTURE'S OWN NINE, not the sweep's 543.
    Asserting 543 here would be a claim about a file that is not in git, and it
    would pass for as long as nobody looked: every test built on this fixture
    would then be quietly measuring nine rows while saying 543.

    The rest is the shape the index's tests depend on, and each part of it is
    something a careless re-cut would lose: three availability verdicts across
    the nine rather than one, the two "absent" endpoints that make the grouping
    more than a boolean, and the eight metrics per endpoint made of seven
    measurements and one cost-ceiling decline.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, REGISTRY_SAMPLE_FIXTURE.read_bytes())
    assert run_quad_count(store) == 462, (
        f"the registry sample is 462 quads, got {run_quad_count(store)}"
    )
    assert len(run_graph_names(store)) == 1, "one sweep, one named graph"

    endpoints = {
        row["endpoint"].value
        for row in run_graph_query(
            store,
            "SELECT DISTINCT ?endpoint WHERE { GRAPH ?g { "
            "?m <http://www.w3.org/ns/dqv#computedOn> ?endpoint } }",
        )
    }
    assert len(endpoints) == 9, f"nine endpoints, got {len(endpoints)}"

    availability = sorted(
        row["verdict"].value
        for row in run_graph_query(
            store,
            "SELECT ?verdict WHERE { GRAPH ?g { "
            "?m <http://www.w3.org/ns/dqv#isMeasurementOf> "
            "<urn:sparqlwatch:metric:availability> ; "
            "<http://www.w3.org/ns/dqv#value> ?verdict } }",
        )
    )
    assert availability == [
        "absent",
        "absent",
        "indeterminate",
        "indeterminate",
        "indeterminate",
        "indeterminate",
        "verified",
        "verified",
        "verified",
    ], availability

    verdicts = {
        row["verdict"].value
        for row in run_graph_query(
            store,
            "SELECT ?verdict WHERE { GRAPH ?g { "
            "?m <http://www.w3.org/ns/dqv#value> ?verdict } }",
        )
    }
    assert verdicts == {
        "absent",
        "declared-but-wrong",
        "indeterminate",
        "undeclared-but-verified",
        "verified",
    }, verdicts

    declines = sorted(
        row["reason"].value
        for row in run_graph_query(
            store,
            "SELECT ?reason WHERE { GRAPH ?g { "
            "?n <urn:sparqlwatch:notMeasuredReason> ?reason } }",
        )
    )
    assert declines == ["cost-ceiling"] * 9, declines
    assert not bool(
        run_graph_query(
            store, "ASK { GRAPH ?g { ?s <urn:sparqlwatch:sampledValue> ?v } }"
        )
    ), "the cheap ceiling declined the classes metric, so there is no sample"
    assert bool(
        run_graph_query(
            store,
            "ASK { GRAPH ?g { ?a <urn:sparqlwatch:finalised> ?f } }",
        )
    ), "the sweep finished, and its footer has to be here to say so"

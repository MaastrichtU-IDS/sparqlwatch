"""What is in this endpoint? The first question the UI's front door asks.

Every value asserted here is a real (or, for the synthetic fixtures, a
deliberately constructed) value of the committed run files under
web/tests/fixtures/. See each fixture's header comment for its provenance.
"""

import pytest
from pyoxigraph import NamedNode, RdfFormat, Store

from endpoint_content import endpoint_content
from load_run import check_current, load_run, rebuild_current

# run-truncated.nq: the only endpoint in that synthetic run.
TRUNCATED_ENDPOINT = "https://truncated.example/sparql"

# run-two-sweeps.nq: kadaster is sampled by both the current (16:00) and the
# stale (14:00) run, with one differing value each. The constants come from
# that fixture's header comment.
REPEATED_ENDPOINT = "https://data.kkg.kadaster.nl/query"
CURRENT_RUN = "urn:sparqlwatch:run:2026-08-22T16:00:00Z"
CURRENT_ONLY_CLASS = "http://www.w3.org/2002/07/owl#Restriction"
STALE_ONLY_CLASS = "urn:sparqlwatch:test:stale-only-class"

# run-zero-classes.nq and run-properties-sample.nq: the only endpoint in each
# of those synthetic runs. See each fixture's header comment.
ZERO_CLASSES_ENDPOINT = "https://zero-classes.example/sparql"
PROPERTIES_ENDPOINT = "https://properties-only.example/sparql"
TIED_ENDPOINT = "https://tied.example/sparql"


def test_kadaster_content(store):
    r = endpoint_content(store, "https://data.kkg.kadaster.nl/query")
    assert r.size == 59
    assert r.truncated is False
    assert len(r.classes) == 59, "size must match the values actually returned"
    assert "http://www.w3.org/2002/07/owl#Class" in r.classes


def test_ontop_content_is_its_own(store):
    """Two endpoints in one graph. A query that ignores its parameter returns 109
    values and passes any test that only checks a count is plausible."""
    r = endpoint_content(store, "https://ontop.certain.ai.ustp.at/sparql")
    assert r.size == 50
    assert len(r.classes) == 50
    assert "https://w3id.org/aidoc-ap#AISystemCapability" in r.classes
    assert "http://www.w3.org/2002/07/owl#Class" not in r.classes, "that is kadaster's"


def test_an_endpoint_with_no_sample_is_not_an_empty_endpoint(store):
    """qlever is in the fixture with no class sample, and the reason is a
    timeout, not a decline: the same graph carries its metric:classes
    measurement as dqv:value "indeterminate" with sw:elapsedMs "30003", the 30
    second request budget running out. The fixture holds no sw:NotMeasured
    resource at all (it was captured with --max-cost expensive). So the answer
    must be distinguishable from an endpoint that genuinely holds no classes:
    returning an empty list for both would publish a confident wrong answer in
    the UI. What sampled=False does NOT say is which of those reasons applied;
    this query never reads the measurement row that would tell it."""
    r = endpoint_content(store, "https://qlever.dev/api/osm-planet")
    assert r.sampled is False
    assert r.classes == []


def test_an_endpoint_absent_from_the_store_is_also_not_sampled(store):
    """Nothing in the store mentions this IRI at all. The caller gets the same
    honest 'no sample' answer rather than an exception or an empty class list
    that reads as 'this endpoint holds nothing'."""
    r = endpoint_content(store, "https://nowhere.example/sparql")
    assert r.sampled is False
    assert r.classes == []
    assert r.size is None
    assert r.truncated is None


def test_a_truncated_sample_is_reported_as_truncated(store_truncated):
    """An implementation that hardcodes truncated=False passes every other test
    here, because no real endpoint in the registry exceeds the 200 cap. A list a
    reader believes is complete when it is not is the failure the whole sampling
    design guards against, so it needs its own fixture."""
    r = endpoint_content(store_truncated, TRUNCATED_ENDPOINT)
    assert r.truncated is True


def test_only_the_most_recent_run_is_returned(store_two_sweeps):
    """The same endpoint sampled twice. A query that unions every run passes all
    the tests above and reports stale classes beside current ones, with no way for
    a reader to tell which is which. Assert the VALUES, not just the count: a
    union of two runs can coincidentally have a plausible size."""
    r = endpoint_content(store_two_sweeps, REPEATED_ENDPOINT)
    assert CURRENT_ONLY_CLASS in r.classes
    assert STALE_ONLY_CLASS not in r.classes, "a stale run's values must not appear"


def test_the_answer_names_the_run_it_came_from(store_two_sweeps):
    """A reader cannot judge how current an answer is without knowing which sweep
    produced it, and with two runs in the store the wrong one is a real option."""
    r = endpoint_content(store_two_sweeps, REPEATED_ENDPOINT)
    assert r.run == CURRENT_RUN
    assert r.generated_at == "2026-08-22T16:00:00Z"


def test_the_run_chosen_is_the_newest_that_sampled_THIS_endpoint(store_two_sweeps):
    """Not the newest run in the store. Derived in-test rather than as a fourth
    fixture: drop kadaster's sample from the newest run, leaving a store whose
    newest run sampled only the other endpoints.

    The recency this exercises is now decided once per load and written into
    urn:sparqlwatch:current, not decided per request, so this hand-edit is the
    one case the update rule cannot fix: a run graph that shrank after current
    was written. Both halves are asserted. The check must NOTICE it, because
    current is then attributing a sample to a run that no longer holds one, and
    a rebuild must repair it by choosing the 14:00 run.

    The property being pinned is unchanged and is why a rebuild cannot take a
    shortcut: recency is per endpoint. Picking the newest run in the store and
    then looking for this endpoint's sample in it returns NOTHING here, and an
    endpoint that stopped responding would silently lose the last answer anyone
    had about it.
    """
    current_sample = NamedNode("urn:sparqlwatch:content-sample:2026-08-22T16:00:00Z:0")
    removed = list(store_two_sweeps.quads_for_pattern(current_sample, None, None, None))
    assert removed, "fixture changed: the newest run's kadaster sample is gone"
    for quad in removed:
        store_two_sweeps.remove(quad)

    drifted = check_current(store_two_sweeps)
    assert REPEATED_ENDPOINT in drifted.drifted, (
        "current still attributes a sample to a run that no longer holds one"
    )

    rebuild_current(store_two_sweeps)
    r = endpoint_content(store_two_sweeps, REPEATED_ENDPOINT)
    assert r.run == "urn:sparqlwatch:run:2026-08-22T14:00:00Z"
    assert STALE_ONLY_CLASS in r.classes, "the 14:00 run is now the newest that sampled it"
    assert check_current(store_two_sweeps).ok, "and the rebuild leaves no drift"


def test_a_sample_that_found_nothing_still_reports_itself(store_zero_classes):
    """The distinction the sampled flag exists to carry, at its hardest point.
    This sample lists no values, so making sw:sampledValue required (dropping
    the OPTIONAL in the query) drops the whole solution and the caller is told
    sampled=False: the same answer as an endpoint no run has ever looked at.
    It has to say instead that somebody looked, found nothing, and was not cut
    short. The fixture is synthetic because the prober never writes this
    shape; see its header for why it is worth defending against anyway."""
    r = endpoint_content(store_zero_classes, ZERO_CLASSES_ENDPOINT)
    assert r.sampled is True, "a sample that found nothing is still a sample"
    assert r.classes == []
    assert r.size == 0, "the size the sample published, not the length of a list"
    assert r.truncated is False
    assert r.run == "urn:sparqlwatch:run:2026-08-24T00:00:00Z"


def test_another_metrics_sample_is_not_reported_as_classes(store_properties_sample):
    """This store holds exactly one sample for this endpoint and it is a
    PROPERTIES sample. Delete sw:sampledBy sw:metric:classes from the query and
    every other test in this suite still passes, because no other fixture holds
    a sample from a second metric, while this endpoint's sampled properties get
    published as its classes by a function documented as answering which
    classes are in there. Spec stage 2b samples properties for real."""
    r = endpoint_content(store_properties_sample, PROPERTIES_ENDPOINT)
    assert r.sampled is False, "no run sampled this endpoint's CLASSES"
    assert r.classes == [], "a property IRI is not a class IRI"
    assert r.size is None


def _tied_runs() -> bytes:
    """Two run graphs with distinct IRIs, one class sample of the same endpoint
    each, and the SAME prov:generatedAtTime. Built here rather than as a
    fixture file because the tie is the only thing that matters and it is one
    literal: the two runs are otherwise identical in shape."""
    lines = []
    for run, klass in (("a", "FromRunA"), ("b", "FromRunB")):
        graph = f"<urn:sparqlwatch:test:run:{run}>"
        activity = f"<urn:sparqlwatch:test:activity:{run}>"
        sample = f"<urn:sparqlwatch:test:content-sample:{run}>"
        lines += [
            f"{activity} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
            f"<http://www.w3.org/ns/prov#Activity> {graph} .",
            f"{activity} <http://www.w3.org/ns/prov#generatedAtTime> "
            f'"2026-08-22T16:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> {graph} .',
            f"{sample} <urn:sparqlwatch:sampledFrom> <{TIED_ENDPOINT}> {graph} .",
            f"{sample} <urn:sparqlwatch:sampledBy> <urn:sparqlwatch:metric:classes> {graph} .",
            f'{sample} <urn:sparqlwatch:sampleSize> "1"^^'
            f"<http://www.w3.org/2001/XMLSchema#integer> {graph} .",
            f'{sample} <urn:sparqlwatch:sampleTruncated> "false"^^'
            f"<http://www.w3.org/2001/XMLSchema#boolean> {graph} .",
            f"{sample} <urn:sparqlwatch:sampledValue> "
            f"<https://tied.example/vocab#{klass}> {graph} .",
        ]
    return ("\n".join(lines) + "\n").encode()


def test_two_runs_tied_as_most_recent_are_refused_not_blended(tmp_path):
    """Neither run is newer, so "the most recent run that sampled this endpoint"
    has no answer.

    The refusal now happens where the choice is made, which is the loader:
    current holds one sw:currentSampleRun per endpoint, so advancing it past a
    tie would decide by load order which of two sweeps the page attributes the
    sample to. That is a behaviour change nobody asked for, and silently
    resolving it is worse than the blend it replaces, because the blend was at
    least visible as two runs in one answer.

    Without the refusal the caller gets one run's IRI beside one run's classes,
    under the other run's size and truncation flag, depending on which file was
    loaded second: a single sweep's answer that is not any sweep's answer.
    Deleting the raise leaves every other test in this suite green, so it needs
    this test.
    """
    store = Store(str(tmp_path / "s"))
    with pytest.raises(ValueError, match="2 runs tied as most recent") as raised:
        load_run(store, _tied_runs())
    message = str(raised.value)
    assert "urn:sparqlwatch:test:run:a" in message, "name both runs, so the store is fixable"
    assert "urn:sparqlwatch:test:run:b" in message
    assert "--rebuild" in message, "and say what to do once one of them is dropped"


def test_a_current_graph_naming_two_sample_runs_is_refused_not_blended(tmp_path):
    """The reader's own guard, which the loader's refusal does not replace.

    load_run refuses to write two sw:currentSampleRun quads for one endpoint, so
    this state is unreachable through it, and the store here is built by hand
    for that reason: the two run graphs are inserted raw and the doubled pointer
    is written straight into current. That is not a hypothetical shape. current
    is a derived graph and an operator repairing one edits it, and a doubled
    pointer makes this query return two runs' samples in one result set, which
    a caller would publish as the union of both class lists under one run's size
    and truncation flag. So the reader still refuses, and this test is what
    keeps that guard from becoming dead code once the loader took over the
    choice.
    """
    store = Store(str(tmp_path / "s"))
    store.load(_tied_runs(), format=RdfFormat.N_QUADS)
    # ONE pointer resource carrying TWO runs, which is the new shape's version of
    # the doubled pointer this guards against: since 2026-09-03 a sample pointer
    # is a resource keyed on (endpoint, metric) rather than a triple on the
    # endpoint, so the corruption to hand-build is two sw:sampleRunIs values on
    # one of them.
    store.update(
        f"""
        INSERT DATA {{ GRAPH <urn:sparqlwatch:current> {{
          <urn:sparqlwatch:test:ptr> <urn:sparqlwatch:sampleRunFor>
            <{TIED_ENDPOINT}> ;
            <urn:sparqlwatch:sampleRunMetric> <urn:sparqlwatch:metric:classes> ;
            <urn:sparqlwatch:sampleRunIs>
              <urn:sparqlwatch:test:run:a> ,
              <urn:sparqlwatch:test:run:b> .
        }} }}"""
    )

    with pytest.raises(ValueError, match="2 runs tied as most recent") as raised:
        endpoint_content(store, TIED_ENDPOINT)
    message = str(raised.value)
    assert "urn:sparqlwatch:test:run:a" in message
    assert "urn:sparqlwatch:test:run:b" in message

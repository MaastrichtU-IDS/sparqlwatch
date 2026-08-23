"""What are this endpoint's verdicts? The second question a page for an
endpoint needs, right after "what is in it" (test_endpoint_content.py).

Every value asserted here is a real value of the committed fixtures under
web/tests/fixtures/. See each fixture's header comment (or, for
run-with-samples.nq, web/tests/test_fixture.py) for its provenance.
"""

from endpoint_measurements import endpoint_measurements

KADASTER = "https://data.kkg.kadaster.nl/query"
ONTOP = "https://ontop.certain.ai.ustp.at/sparql"

M = "urn:sparqlwatch:metric:"

# run-two-sweeps.nq: kadaster is measured in both the current (16:00) and the
# stale (14:00) run. Its metric:classes dqv:value differs between them: the
# hand edit recorded in that fixture's header comment.
CURRENT_RUN = "urn:sparqlwatch:run:2026-08-22T16:00:00Z"
STALE_RUN = "urn:sparqlwatch:run:2026-08-22T14:00:00Z"
CURRENT_CLASSES_VERDICT = "verified"
STALE_CLASSES_VERDICT = "indeterminate"


def _verdicts_by_metric(result):
    return {v.metric: v for v in result.verdicts}


def _declined_by_metric(result):
    return {d.metric: d for d in result.declined}


def test_kadaster_verdicts_are_returned_with_exact_values(store):
    """web/tests/fixtures/run-with-samples.nq, kadaster's 8 measurements
    (indices 8-15), each value read straight from the fixture."""
    r = endpoint_measurements(store, KADASTER)
    assert r.assessed is True
    assert len(r.verdicts) == 8
    assert r.declined == []

    v = _verdicts_by_metric(r)
    assert v[M + "availability"].verdict == "verified"
    assert v[M + "availability"].elapsed_ms == 77
    assert v[M + "cors"].verdict == "verified"
    assert v[M + "cors"].elapsed_ms == 41
    assert v[M + "cors-preflight"].verdict == "verified"
    assert v[M + "cors-preflight"].elapsed_ms == 26
    assert v[M + "geo-functions"].verdict == "undeclared-but-verified"
    assert v[M + "geo-functions"].elapsed_ms == 58
    assert v[M + "geo-data"].verdict == "verified"
    assert v[M + "geo-data"].elapsed_ms == 45
    assert v[M + "service-description"].verdict == "verified"
    assert v[M + "service-description"].elapsed_ms == 151
    assert v[M + "has-classes"].verdict == "verified"
    assert v[M + "has-classes"].elapsed_ms == 117
    assert v[M + "classes"].verdict == "verified"
    assert v[M + "classes"].elapsed_ms == 73


def test_ontops_verdicts_are_not_returned_when_asking_for_kadaster(store):
    """Two endpoints' measurements sit in one graph. A query that ignores its
    ?endpoint substitution returns all 24 rows in the store (8 per endpoint,
    3 endpoints) and passes a test that only checks a plausible count, so
    this asserts the exact set of metrics and a value that differs between
    the two endpoints."""
    r = endpoint_measurements(store, KADASTER)
    assert len(r.verdicts) == 8, "24 rows in the store; only kadaster's 8 belong here"

    v = _verdicts_by_metric(r)
    # ontop's service-description is "indeterminate" (run-with-samples.nq
    # measurement :21); kadaster's is "verified" (measurement :13). A leak
    # would make both values available under the same metric key.
    assert v[M + "service-description"].verdict == "verified"
    assert v[M + "service-description"].level == 1, "kadaster's level, not ontop's absent one"

    # ontop's own class sample vocabulary term must not appear as if it were
    # one of kadaster's metric ids or verdicts.
    assert all(not mv.metric.startswith("https://w3id.org/aidoc-ap") for mv in r.verdicts)


def test_the_most_recent_run_wins_for_measurements(store_two_sweeps):
    """web/tests/fixtures/run-two-sweeps.nq: kadaster's metric:classes verdict
    differs between the current (16:00, "verified") and stale (14:00,
    "indeterminate") run. A query that unions every run would let the stale
    value overwrite or coexist with the current one; assert on the value
    itself, not a row count, because a union of two runs can coincidentally
    have a plausible size (8 here either way, since both runs measure the
    same 8 metrics)."""
    r = endpoint_measurements(store_two_sweeps, KADASTER)
    assert r.run == CURRENT_RUN
    assert len(r.verdicts) == 8, "one run's 8 metrics, not both runs' 16 rows"

    v = _verdicts_by_metric(r)
    assert v[M + "classes"].verdict == CURRENT_CLASSES_VERDICT
    assert v[M + "classes"].verdict != STALE_CLASSES_VERDICT


def test_a_level_is_returned_where_the_graph_has_one_and_absent_where_not(store):
    """service-description carries a level for kadaster (measurement :13,
    sw:level "1") but not for ontop (measurement :21, no sw:level triple at
    all). Both endpoints measure the same metric, so this isolates the
    OPTIONAL rather than a metric-specific default."""
    kadaster = _verdicts_by_metric(endpoint_measurements(store, KADASTER))
    ontop = _verdicts_by_metric(endpoint_measurements(store, ONTOP))

    assert kadaster[M + "service-description"].level == 1
    assert ontop[M + "service-description"].level is None

    # A metric with no sw:level triple anywhere in the fixture must not
    # silently inherit some other metric's value.
    assert kadaster[M + "availability"].level is None
    assert kadaster[M + "classes"].level is None


def test_an_endpoint_never_measured_returns_nothing(store):
    """Nothing in the store mentions this IRI at all: no measurement, no
    decline. The caller gets an honest 'not assessed' answer, not an
    exception or an empty-looking result indistinguishable from a run that
    declined every metric (see the next test)."""
    r = endpoint_measurements(store, "https://nowhere.example/sparql")
    assert r.assessed is False
    assert r.verdicts == []
    assert r.declined == []
    assert r.run is None


def test_a_declined_metric_is_distinguishable_from_no_measurement_at_all(store_declined):
    """web/tests/fixtures/run-declined.nq: kadaster's metric:classes was
    declined (sw:NotMeasured, reason "cost-ceiling"), not measured. An
    implementation that returns declined rows as if they were ordinary
    measurements would put "classes" in r.verdicts with the reason string
    (or nothing at all) standing in for a verdict, and r.declined would stay
    empty: the exact shape this test rejects. The endpoint is still
    ``assessed`` (a run did look at it), unlike the truly-absent endpoint
    above."""
    r = endpoint_measurements(store_declined, KADASTER)
    assert r.assessed is True
    assert len(r.verdicts) == 7, "8 metrics minus the 1 declined"
    assert len(r.declined) == 1

    d = _declined_by_metric(r)
    assert d[M + "classes"].reason == "cost-ceiling"

    v = _verdicts_by_metric(r)
    assert (M + "classes") not in v, "a declined metric is not a verdict"

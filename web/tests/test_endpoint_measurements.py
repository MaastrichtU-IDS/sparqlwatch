"""What are this endpoint's verdicts? The second question a page for an
endpoint needs, right after "what is in it" (test_endpoint_content.py).

Every value asserted here is a real value of the committed fixtures under
web/tests/fixtures/. See each fixture's header comment (or, for
run-with-samples.nq, web/tests/test_fixture.py) for its provenance.
"""

from pyoxigraph import NamedNode

from endpoint_content import endpoint_content
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

# run-new-subjects.nq beside run-with-samples.nq: the same kadaster endpoint
# published once under the OLD row-index subject scheme (16:00) and once
# under the NEW derived subject scheme (20:00, the one prober/src/emit.rs's
# subject_iri produces after stage 1c-b3). The constants come from
# run-new-subjects.nq's header comment.
NEW_RUN = "urn:sparqlwatch:run:2026-08-22T20:00:00Z"
OLD_RUN = "urn:sparqlwatch:run:2026-08-22T16:00:00Z"
NEW_SCHEME_CLASSES_VERDICT = "indeterminate"
OLD_SCHEME_CLASSES_VERDICT = "verified"
NEW_SCHEME_ONLY_CLASS = "urn:sparqlwatch:test:new-scheme-only-class"
OLD_SCHEME_ONLY_CLASS = "http://www.w3.org/2002/07/owl#Restriction"


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


def test_the_old_and_new_subject_schemes_coexist_and_the_newer_wins(store_new_subjects):
    """web/tests/fixtures/run-new-subjects.nq (NEW derived subject scheme,
    20:00) loaded beside run-with-samples.nq (OLD row-index scheme, 16:00).
    Both name kadaster. web/queries/endpoint_measurements.rq and
    endpoint_content.rq match on predicates (dqv:computedOn, sw:sampledFrom,
    and so on), never on the shape of the subject IRI, so the most-recent-run
    selection must pick the newer run's verdict and sample regardless of
    which scheme produced its subjects. Assert the exact values, not a row
    count: both runs measure the same 8 metrics and sample the same-sized
    class list, so a query that blended or picked the wrong run would still
    look plausible by count alone."""
    m = endpoint_measurements(store_new_subjects, KADASTER)
    assert m.run == NEW_RUN
    assert len(m.verdicts) == 8

    v = _verdicts_by_metric(m)
    assert v[M + "classes"].verdict == NEW_SCHEME_CLASSES_VERDICT
    assert v[M + "classes"].verdict != OLD_SCHEME_CLASSES_VERDICT

    c = endpoint_content(store_new_subjects, KADASTER)
    assert c.run == NEW_RUN
    assert c.sampled is True
    assert c.size == 59, "the sample size run-new-subjects.nq's kadaster sample publishes"
    assert c.truncated is False
    assert len(c.classes) == 59
    assert NEW_SCHEME_ONLY_CLASS in c.classes, "the new run's swapped-in class"
    assert OLD_SCHEME_ONLY_CLASS not in c.classes, "the old run's class must not leak through"


def test_the_old_scheme_run_is_still_reachable_once_the_newer_one_is_gone(store_new_subjects):
    """Deleting the NEW-scheme run's graph must not make the OLD-scheme run
    (run-with-samples.nq, 16:00) unreachable. If either query secretly
    depended on the new derived subject shape rather than on predicates
    alone, the old row-index subjects would already be invisible to it and
    removing the newer run would surface nothing rather than the old run's
    real values."""
    store_new_subjects.remove_graph(NamedNode(NEW_RUN))

    m = endpoint_measurements(store_new_subjects, KADASTER)
    assert m.run == OLD_RUN
    v = _verdicts_by_metric(m)
    assert v[M + "classes"].verdict == OLD_SCHEME_CLASSES_VERDICT

    c = endpoint_content(store_new_subjects, KADASTER)
    assert c.run == OLD_RUN
    assert OLD_SCHEME_ONLY_CLASS in c.classes
    assert NEW_SCHEME_ONLY_CLASS not in c.classes


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


# ---------------------------------------------------------------------------
# Did the run whose facts these are actually finish?
# ---------------------------------------------------------------------------
# The three run-level facts stage 1c-b4 writes, and the two conditions the
# read tier derives from them. run-crashed-partway.nq beside
# run-with-samples.nq is one store answering both questions differently for
# two of its endpoints; see conftest's store_crashed_partway.
CRASHED_RUN = "urn:sparqlwatch:run:2026-08-23T04:00:00Z"
CRASHED_SWEEP = "2026-08-23T04:00:00Z"
FINISHED_RUN = "urn:sparqlwatch:run:2026-08-23T02:00:00Z"
QLEVER = "https://qlever.dev/api/osm-planet"


def test_a_finished_run_carries_both_terminators_and_derives_neither_condition(
    store_prober_failed,
):
    """run-prober-failed.nq is a whole run: sw:emission, one
    sw:completedEndpoint for its one endpoint, and sw:finalised.

    Both conditions must be false, and the second one for a reason worth
    naming: this run IS the newest run in the store, so there is no newer run
    to say anything about. A derivation that read "the newest run has no
    sw:completedEndpoint for this endpoint" without first checking that the
    newest run is a different run would fire on every finished run's own
    page.
    """
    r = endpoint_measurements(store_prober_failed, KADASTER)
    assert r.run == FINISHED_RUN
    assert r.emission == "incremental"
    assert r.finalised is True
    assert r.newest_run == FINISHED_RUN
    assert r.newest_completed_this_endpoint is True
    assert r.run_did_not_finish is False
    assert r.newer_run_did_not_reach_this_endpoint is False


def test_an_unfinished_run_showing_its_own_facts_is_derivable_as_such(
    store_crashed_partway,
):
    """Condition (a): the run whose facts are shown did not finish.

    The crashed run wrote kadaster's chunk, so it is the newest run that
    recorded anything for kadaster and its facts are the ones this endpoint
    answers with. The verdict is asserted too, because "which run's facts"
    is the claim the condition is about: availability reads "indeterminate"
    in the crashed run and "verified" in the 16:00 sweep.

    Condition (b) must NOT also fire here. The crashed run is the newest run
    in the store and it is also the run being shown, so there is no later
    sweep to report, and saying there is one would invent a sweep.
    """
    r = endpoint_measurements(store_crashed_partway, KADASTER)
    assert r.run == CRASHED_RUN
    assert _verdicts_by_metric(r)[M + "availability"].verdict == "indeterminate"
    assert r.emission == "incremental"
    assert r.finalised is False
    assert r.run_did_not_finish is True
    assert r.newest_run == CRASHED_RUN
    assert r.newer_run_did_not_reach_this_endpoint is False


def test_an_endpoint_a_crashed_newer_run_never_reached_is_derivable_as_such(
    store_crashed_partway,
):
    """Condition (b), the one a per-endpoint query cannot see.

    The crashed run never reached qlever, so it recorded nothing for it and
    endpoint_measurements.rq's per-endpoint selection falls back to the 16:00
    sweep. Every fact about qlever on this page is therefore true and current
    as far as this endpoint's own facts go, and the store still holds a newer
    run that died before getting here. Condition (a) is false, because the
    run being shown is a pre-1c-b4 sweep that promised nothing.
    """
    r = endpoint_measurements(store_crashed_partway, QLEVER)
    assert r.run == CURRENT_RUN
    assert r.emission is None
    assert r.finalised is False
    assert r.run_did_not_finish is False

    assert r.newest_run == CRASHED_RUN
    assert r.newest_generated_at == CRASHED_SWEEP
    assert r.newest_emission == "incremental"
    assert r.newest_finalised is False
    assert r.newest_completed_this_endpoint is False
    assert r.newer_run_did_not_reach_this_endpoint is True


def test_a_run_from_before_this_stage_derives_neither_condition(store):
    """A run that promised nothing must read exactly as it did before this
    stage existed.

    run-with-samples.nq carries no sw:emission, no sw:completedEndpoint and
    no sw:finalised, because it was captured before the section protocol
    existed. "No sw:finalised" is therefore not evidence that it did not
    finish: it is evidence that this run says nothing either way, and the
    only honest reading is silence. Deriving condition (a) from the absence
    of sw:finalised alone would mark every historical run in the store as
    crashed.
    """
    r = endpoint_measurements(store, KADASTER)
    assert r.run == CURRENT_RUN
    assert r.emission is None
    assert r.finalised is False
    assert r.newest_run == CURRENT_RUN
    assert r.newest_emission is None
    assert r.newest_finalised is False
    assert r.run_did_not_finish is False
    assert r.newer_run_did_not_reach_this_endpoint is False

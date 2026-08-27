"""What are this endpoint's verdicts? The second question a page for an
endpoint needs, right after "what is in it" (test_endpoint_content.py).

Every value asserted here is a real value of the committed fixtures under
web/tests/fixtures/. See each fixture's header comment (or, for
run-with-samples.nq, web/tests/test_fixture.py) for its provenance.
"""

from dataclasses import asdict

import pytest
from pyoxigraph import NamedNode, Store

from conftest import RUN_WITH_SAMPLES
from endpoint_content import endpoint_content
from endpoint_index import endpoint_index
from load_run import check_current, load_run, rebuild_current
from endpoint_measurements import EndpointMeasurements, endpoint_measurements

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

# run-later-sample-only.nq: the 22:00 sweep that sampled kadaster and measured
# nothing, so it records no measurement and no decline for any endpoint. See
# that fixture's header comment.
LATER_SAMPLE_RUN = "urn:sparqlwatch:run:2026-08-22T22:00:00Z"


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
    real values.

    Dropping a run graph is one of the two things urn:sparqlwatch:current
    cannot follow on its own, and the spec's whole reason for one graph per run
    is that a bad run can be dropped wholesale. So the drop is followed by the
    rebuild that is its documented repair, and the check is asserted on both
    sides of it: current names a graph that is gone, and afterwards it does
    not."""
    store_new_subjects.remove_graph(NamedNode(NEW_RUN))
    assert not check_current(store_new_subjects).ok, (
        "current still names the run that was dropped"
    )
    rebuild_current(store_new_subjects)
    assert check_current(store_new_subjects).ok

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


def test_a_newer_historical_run_is_not_a_crash(store_later_sample):
    """The steady state, and the one store shape the four tests above miss.

    Both runs here were captured before stage 1c-b4, so neither carries
    sw:emission, sw:finalised or sw:completedEndpoint. The 22:00 run only
    sampled kadaster, so the per-endpoint selection falls back to the 16:00
    sweep and the newest run in the store is a DIFFERENT run from the one
    being shown. That makes three of condition (b)'s five conjuncts true at
    once: the newest run is not this run, it carries no sw:finalised, and it
    marked no sw:completedEndpoint here. Only "the newest run said it was
    written incrementally" stops the page reporting a crash that never
    happened, and a store of two finished historical runs is what every
    production store holds most of.
    """
    r = endpoint_measurements(store_later_sample, KADASTER)
    assert r.run == CURRENT_RUN, "the 22:00 run measured nothing here"
    assert r.newest_run == LATER_SAMPLE_RUN
    assert r.newest_run != r.run
    assert r.newest_emission is None, "a pre-1c-b4 run promises nothing"
    assert r.newest_finalised is False
    assert r.newest_completed_this_endpoint is False
    assert r.newer_run_did_not_reach_this_endpoint is False


# ---------------------------------------------------------------------------
# Condition (b)'s five conjuncts, one at a time
# ---------------------------------------------------------------------------
# The tests above reach the property through the query, which is the right
# way round for the store shapes a run file can produce. Three of the five
# conjuncts cannot be reached that way: the query binds ?newestRun whenever it
# returns a row at all, and a chunk's sw:completedEndpoint is in the same
# chunk as its measurements, so "the newest run recorded facts here" and "the
# newest run marked this endpoint" cannot come apart in any file the emitter
# writes. Those conjuncts are guards against a store shape that would be a
# bug elsewhere, so they are pinned on the dataclass directly: each case below
# satisfies four conjuncts and violates exactly one, so it turns True the
# moment its own conjunct is deleted.


def _newest_is_a_crashed_later_run(**overrides):
    """An EndpointMeasurements where condition (b) is true, before overrides.

    The 16:00 sweep's facts, with a newer run that said it was written
    incrementally, never said it finished, and never marked this endpoint.
    """
    facts = dict(
        endpoint=KADASTER,
        assessed=True,
        run=CURRENT_RUN,
        generated_at="2026-08-22T16:00:00Z",
        newest_run=CRASHED_RUN,
        newest_generated_at=CRASHED_SWEEP,
        newest_emission="incremental",
        newest_finalised=False,
        newest_completed_this_endpoint=False,
    )
    facts.update(overrides)
    return EndpointMeasurements(**facts)


def test_the_reference_shape_for_the_conjuncts_below_does_derive_condition_b():
    """The control. Every test below changes one field of this shape, so if
    this one did not derive the condition none of them would be testing the
    conjunct it names."""
    assert (
        _newest_is_a_crashed_later_run().newer_run_did_not_reach_this_endpoint
        is True
    )


def test_no_newest_run_at_all_reports_nothing():
    """Conjunct 1: there has to BE a newest run.

    ``None != self.run`` is True, so without this conjunct an absent newest
    run reads as "a run other than this one" and the page names a later sweep
    the store does not hold. Only this conjunct stops that: the query binds
    ?newestRun and ?newestEmission in one OPTIONAL, so no store makes the
    other four conjuncts true with newest_run empty, and the conjunct that
    would otherwise catch it (conjunct 3) is satisfied here for exactly that
    reason. What is pinned is the reading, not a store shape: the five fields
    are read as one answer about one run, so an empty newest_run is "no newer
    run" and never "a different run".
    """
    r = _newest_is_a_crashed_later_run(newest_run=None, newest_generated_at=None)
    assert r.newer_run_did_not_reach_this_endpoint is False


def test_the_newest_run_being_this_run_reports_nothing():
    """Conjunct 2: the newest run has to be a DIFFERENT run.

    A run that crashed after writing this endpoint's chunk is both the run
    being shown and the newest run in the store. Its facts are on the page
    already, so "a later sweep never got here" would invent a second sweep.
    That case is condition (a)'s, and store_crashed_partway's kadaster page
    is where it is asserted end to end.
    """
    r = _newest_is_a_crashed_later_run(newest_run=CURRENT_RUN)
    assert r.newer_run_did_not_reach_this_endpoint is False


def test_a_newest_run_that_promised_nothing_reports_nothing():
    """Conjunct 3: the newest run has to have said it writes incrementally.

    Without sw:emission the missing sw:finalised is not evidence of anything,
    exactly as in run_did_not_finish. This is the conjunct
    test_a_newer_historical_run_is_not_a_crash reaches through a real store.
    """
    r = _newest_is_a_crashed_later_run(newest_emission=None)
    assert r.newer_run_did_not_reach_this_endpoint is False


def test_a_newest_run_that_finished_reports_nothing():
    """Conjunct 4: the newest run must not have recorded finishing.

    A finished sweep that recorded nothing for this endpoint is a different
    fact from a crash, and not one this sentence is about.
    """
    r = _newest_is_a_crashed_later_run(newest_finalised=True)
    assert r.newer_run_did_not_reach_this_endpoint is False


def test_a_newest_run_that_marked_this_endpoint_reports_nothing():
    """Conjunct 5: the newest run must not have marked this endpoint done.

    An endpoint the crashed run did finish is one whose facts are simply
    older than the crash, and saying the run never reached it would be false.
    """
    r = _newest_is_a_crashed_later_run(newest_completed_this_endpoint=True)
    assert r.newer_run_did_not_reach_this_endpoint is False


def _two_activities_tied_as_newest() -> bytes:
    """Two run graphs holding nothing but an activity, with distinct IRIs, no
    endpoint facts, and the SAME prov:generatedAtTime, later than any fixture's.

    Hand-built rather than a fixture pair, because --at is both the run IRI and
    the timestamp, so no two run files the prober writes can tie.

    It goes through load_run() like every other store in this suite. An
    earlier version of this comment said load_run refuses a file of this shape
    (activity metadata, no endpoint facts, no terminator) and inserted the
    bytes raw instead. That was wrong about these bytes: load_run's
    _holds_endpoint_facts test asks whether any subject falls outside the
    urn:sparqlwatch:activity: prefix, and these activities are
    urn:sparqlwatch:test:activity:a and :b, so the file reads as one that does
    hold endpoint facts and loads whole. The refusal it named is real for a
    file the prober wrote; it never applied here.

    Neither graph names an endpoint, so loading them advances no endpoint's
    pointer in urn:sparqlwatch:current and the tie stays where this test wants
    it: in the store-wide newest-run aggregate, which is still a query over
    the run graphs.
    """
    lines = []
    for run in ("a", "b"):
        graph = f"<urn:sparqlwatch:test:run:{run}>"
        activity = f"<urn:sparqlwatch:test:activity:{run}>"
        lines += [
            f"{activity} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
            f"<http://www.w3.org/ns/prov#Activity> {graph} .",
            f"{activity} <http://www.w3.org/ns/prov#generatedAtTime> "
            f'"2026-08-25T00:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> {graph} .',
        ]
    return ("\n".join(lines) + "\n").encode()


def test_two_runs_tied_as_the_newest_in_the_store_are_refused(tmp_path):
    """The second tie check, on the one selection in the query that is not
    per-endpoint.

    A different store from the tie above: neither of these two runs touched
    kadaster at all, so the per-endpoint selection is unambiguous and returns
    the 16:00 sweep's rows, while the newest-run subquery's MAX matches two
    graphs and multiplies every row. Resolving that silently would attribute
    one run's sw:emission and sw:finalised to a page that names the other, and
    the sentence the read tier derives from them would be about a run the
    reader cannot find. Refusing needs its own test: replacing the check with
    `if False` leaves the rest of this suite green.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, RUN_WITH_SAMPLES.read_bytes())
    load_run(store, _two_activities_tied_as_newest())

    with pytest.raises(ValueError, match="2 runs tie as the newest") as raised:
        endpoint_measurements(store, KADASTER)
    message = str(raised.value)
    assert "urn:sparqlwatch:test:run:a" in message, "name both runs, so the store is fixable"
    assert "urn:sparqlwatch:test:run:b" in message


# ---------------------------------------------------------------------------
# The newest sweep recorded nothing here, and whether it said why
# ---------------------------------------------------------------------------
# Condition (b) above is a crash. Its mirror is a DECISION: a newer sweep that
# finished and recorded nothing for this endpoint. Both leave the endpoint's
# own facts complete and current and both leave the page dating them to a
# sweep that is not the newest one, and newest_finalised is the single fact
# that tells them apart.
#
# Dormancy is one way the decision arises and the only one the store explains:
# prober/src/dormancy.rs's cadence declines to ask an endpoint that has proved
# expensive and silent, and the run graph publishes sw:dormantEndpoint,
# sw:dormancyReason and sw:dormantSince for it. The others (a url dropped from
# the registry, one added to registry/exclusions.toml, a deliberately narrowed
# sweep) publish nothing, which is why the property is not gated on dormancy.
#
# DORMANCY IS NOT A VERDICT. Nothing below reads it as one; what it decides is
# which sentence a page may print about the age of the verdicts it already has.
DECLINING_RUN = "urn:sparqlwatch:run:2026-08-27T10:00:00Z"
DECLINING_SWEEP = "2026-08-27T10:00:00Z"
PROMOTING_RUN = "urn:sparqlwatch:run:2026-08-27T11:00:00Z"
REGISTRY_RUN = "urn:sparqlwatch:run:2026-08-24T19:45:03Z"
PROMOTES_THE_DORMANT = (
    RUN_WITH_SAMPLES.parent / "run-promotes-the-dormant.nq"
)


def test_a_finished_sweep_that_recorded_nothing_is_not_a_crash(
    store_registry_and_failure,
):
    """The general shape, with no dormancy anywhere in the store.

    The registry sweep is the newest run here and it finished. It never
    mentions kadaster, because it is a nine-endpoint cut of a different sweep,
    which is exactly what a narrowed run looks like from an endpoint it left
    out. So kadaster's facts come from the older 02:00 run, the newest sweep
    recorded nothing for it, and nothing in the store says why.

    Condition (b) must stay false: that sweep finished, so "it stopped before
    it got here" would be false, and the two properties are mutually exclusive
    on exactly that fact.
    """
    r = endpoint_measurements(store_registry_and_failure, KADASTER)
    assert r.run == FINISHED_RUN
    assert r.newest_run == REGISTRY_RUN
    assert r.newest_finalised is True
    assert r.newest_completed_this_endpoint is False
    assert r.newest_sweep_recorded_nothing_for_this_endpoint is True
    assert r.newer_run_did_not_reach_this_endpoint is False
    assert r.newest_declared_this_endpoint_dormant is False
    assert r.newest_dormancy_reason is None, (
        "no run in this store declares anything dormant, so no reason may be "
        "invented for the silence"
    )


def test_a_finished_declining_sweep_publishes_the_reason_it_did_not_ask(
    store_dormant_newest,
):
    """The same silence, explained. run-with-dormancy.nq finished, measured
    the other two endpoints of the trio, and published one dormancy group for
    kadaster with sw:dormancyReason "operator-hold".

    The reason comes from inside the newest run's graph, which is where the
    prober writes it; load_run deliberately keeps no dormancy in the derived
    current graph, so there is nowhere else it could come from.
    """
    r = endpoint_measurements(store_dormant_newest, KADASTER)
    assert r.newest_run == DECLINING_RUN
    assert r.newest_sweep_recorded_nothing_for_this_endpoint is True
    assert r.newest_declared_this_endpoint_dormant is True
    assert r.newest_dormancy_reason == "operator-hold"
    assert r.newer_run_did_not_reach_this_endpoint is False


def test_the_verdicts_age_comes_from_the_endpoints_own_pointer(
    store_dormant_newest,
):
    """The trap this pair of fixtures exists to catch.

    The declining run is the NEWEST run in the store and is deliberately NOT
    kadaster's sw:currentRun. Code that dates a verdict by the store's
    greatest prov:generatedAtTime dates it to the sweep that refused to
    measure it, and the verdicts would then be reported as five days fresher
    than they are.
    """
    r = endpoint_measurements(store_dormant_newest, KADASTER)
    assert r.run == CURRENT_RUN
    assert r.generated_at == "2026-08-22T16:00:00Z"
    assert r.newest_generated_at == DECLINING_SWEEP
    assert r.generated_at != r.newest_generated_at, (
        "the two timestamps must stay distinguishable, or nothing here is "
        "being tested"
    )


def test_the_endpoints_the_declining_sweep_did_measure_report_no_dormancy(
    store_dormant_newest,
):
    """The same store, the other two endpoints. The declining sweep is their
    own run, so there is no older run to date and no dormancy to report, and a
    dormancy fact about kadaster may not leak onto their rows: the declaration
    names one endpoint and is a fact about that one."""
    for endpoint in (ONTOP, QLEVER):
        r = endpoint_measurements(store_dormant_newest, endpoint)
        assert r.run == DECLINING_RUN
        assert r.newest_run == DECLINING_RUN
        assert r.newest_declared_this_endpoint_dormant is False
        assert r.newest_dormancy_reason is None
        assert r.newest_sweep_recorded_nothing_for_this_endpoint is False


def test_a_crashed_newest_sweep_that_declared_dormancy_is_not_reported_as_a_crash(
    store_dormancy_then_crash,
):
    """The live defect, and the reason condition (b) is gated on dormancy.

    The dormancy section sits before the first chunk, so it survives every
    truncation that keeps the header. This store's newest run published its
    complete dormancy list and was then killed: it carries sw:emission, no
    sw:finalised and no sw:completedEndpoint at all, which satisfies every one
    of condition (b)'s five conjuncts for kadaster.

    Both sentences would be about the same sweep and only one of them is true.
    "It stopped before it got here" is a crash claim about an endpoint that
    sweep declined to ask and said so in the same run graph, so the crash
    claim is the one that must not be made.
    """
    r = endpoint_measurements(store_dormancy_then_crash, KADASTER)
    assert r.run == CURRENT_RUN
    assert r.newest_run == DECLINING_RUN
    assert r.newest_emission == "incremental"
    assert r.newest_finalised is False
    assert r.newest_completed_this_endpoint is False
    assert r.newest_declared_this_endpoint_dormant is True
    assert r.newest_dormancy_reason == "operator-hold"
    assert r.newer_run_did_not_reach_this_endpoint is False, (
        "the crash claim is false: that sweep never intended to reach this "
        "endpoint and published the reason"
    )
    assert r.newest_sweep_recorded_nothing_for_this_endpoint is False, (
        "and the decision claim needs a finished sweep, which this is not"
    )


def test_only_the_newest_runs_declaration_is_read(tmp_path):
    """WHICH declaring run, decided. After several weekly skips more than one
    run graph declares the same endpoint dormant, and the answer is: the
    newest run in the store, and no other.

    run-promotes-the-dormant.nq is the committed shape that makes the choice
    visible. Its 10:00 run declared kadaster dormant; its 11:00 run is the
    probe week and measured it. Reading "the newest run that declared it
    dormant" would report kadaster as dormant while its own verdicts come from
    a later sweep that asked, which is how dormancy would stop clearing
    itself. Reading only the newest run reports nothing, and nothing is right.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, PROMOTES_THE_DORMANT.read_bytes())

    r = endpoint_measurements(store, KADASTER)
    assert r.run == PROMOTING_RUN
    assert r.newest_run == PROMOTING_RUN
    assert r.newest_declared_this_endpoint_dormant is False
    assert r.newest_dormancy_reason is None, (
        "the 10:00 run's declaration is not the newest run's, so it is not "
        "this page's answer"
    )


def test_a_dormancy_with_no_reason_still_suppresses_the_crash_claim():
    """The gate is the DECLARATION, not the reason.

    Every dormancy group the prober writes carries an sw:dormancyReason, so
    this shape has no committed fixture, and it is pinned on the dataclass for
    the same reason condition (b)'s middle conjuncts are: a run graph that
    declared an endpoint dormant and recorded no reason has still said it
    declined to ask, and printing a crash claim over it would be as false as
    printing one over a run that gave its reason.
    """
    r = _newest_is_a_crashed_later_run(
        newest_declared_this_endpoint_dormant=True
    )
    assert r.newest_dormancy_reason is None
    assert r.newer_run_did_not_reach_this_endpoint is False


# ---------------------------------------------------------------------------
# The decision's four conjuncts, one at a time
# ---------------------------------------------------------------------------
def _newest_finished_and_recorded_nothing(**overrides):
    """An EndpointMeasurements where the decision is true, before overrides."""
    facts = dict(
        endpoint=KADASTER,
        assessed=True,
        run=CURRENT_RUN,
        generated_at="2026-08-22T16:00:00Z",
        newest_run=DECLINING_RUN,
        newest_generated_at=DECLINING_SWEEP,
        newest_emission="incremental",
        newest_finalised=True,
        newest_completed_this_endpoint=False,
    )
    facts.update(overrides)
    return EndpointMeasurements(**facts)


def test_the_reference_shape_for_the_four_conjuncts_does_derive_the_decision():
    """The control, as above: every test below changes one field of this
    shape, so if this one did not derive the property none of them would be
    testing the conjunct it names."""
    assert (
        _newest_finished_and_recorded_nothing()
        .newest_sweep_recorded_nothing_for_this_endpoint
        is True
    )


def test_no_newest_run_at_all_reports_no_decision():
    """Conjunct 1: there has to BE a newest run, for the same reason as
    condition (b)'s first conjunct: ``None != self.run`` is True, so an absent
    newest run would otherwise read as "a run other than this one"."""
    r = _newest_finished_and_recorded_nothing(
        newest_run=None, newest_generated_at=None
    )
    assert r.newest_sweep_recorded_nothing_for_this_endpoint is False


def test_the_newest_run_being_this_run_reports_no_decision():
    """Conjunct 2: a finished run on its own page is not a run that recorded
    nothing here. Its facts ARE what is on the page."""
    r = _newest_finished_and_recorded_nothing(newest_run=CURRENT_RUN)
    assert r.newest_sweep_recorded_nothing_for_this_endpoint is False


def test_an_unfinalised_newest_run_reports_no_decision():
    """Conjunct 3, the one that separates the decision from the crash. A run
    that never recorded finishing may yet have been on its way here, so
    "it recorded nothing" is not a decision it made."""
    r = _newest_finished_and_recorded_nothing(newest_finalised=False)
    assert r.newest_sweep_recorded_nothing_for_this_endpoint is False


def test_a_newest_run_that_marked_this_endpoint_reports_no_decision():
    """Conjunct 4: an endpoint the newest run finished is one it did record
    something for, whatever else is true, so the facts on the page are its
    own and not an older sweep's."""
    r = _newest_finished_and_recorded_nothing(
        newest_completed_this_endpoint=True
    )
    assert r.newest_sweep_recorded_nothing_for_this_endpoint is False


def test_the_index_and_the_endpoint_page_agree_about_dormancy(
    store_dormant_newest, store_dormancy_then_crash
):
    """The two queries, held to the one answer.

    web/queries/index.rq and web/queries/endpoint_measurements.rq ask the same
    question of the store and share one constructor, so a chip on the index
    cannot contradict the page it links to. That guarantee is only as good as
    the columns the two queries select: measurements_from_rows reads bindings
    by NAME, and pyoxigraph 0.5.9 returns None for a name a query does not
    project rather than raising, so an index.rq that had not gained the two new
    columns would report every endpoint in the registry as not dormant, in
    silence.

    The comparison is over every field of every endpoint, not over the two new
    ones, because the shapes that break the newest-run branch in this query
    break the rest of it too: the form this file's header argues against gives
    the dormant endpoint the right answer and every other endpoint a newest run
    with no timestamp and no sw:finalised.
    """
    for store in (store_dormant_newest, store_dormancy_then_crash):
        rows = endpoint_index(store)
        assert rows, "the index must list something to be compared"
        dormant = [r for r in rows if r.newest_declared_this_endpoint_dormant]
        assert [r.endpoint for r in dormant] == [KADASTER], (
            "one endpoint of the trio is dormant in both of these stores"
        )
        for row in rows:
            assert asdict(row) == asdict(
                endpoint_measurements(store, row.endpoint)
            ), f"the two queries disagree about {row.endpoint}"

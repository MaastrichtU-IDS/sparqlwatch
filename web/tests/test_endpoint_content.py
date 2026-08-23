"""What is in this endpoint? The first question the UI's front door asks.

Every value asserted here is a real (or, for the two synthetic fixtures, a
deliberately constructed) value of the committed run files under
web/tests/fixtures/. See each fixture's header comment for its provenance.
"""

from pyoxigraph import NamedNode

from endpoint_content import endpoint_content

# run-truncated.nq: the only endpoint in that synthetic run.
TRUNCATED_ENDPOINT = "https://truncated.example/sparql"

# run-two-sweeps.nq: kadaster is sampled by both the current (16:00) and the
# stale (14:00) run, with one differing value each. The constants come from
# that fixture's header comment.
REPEATED_ENDPOINT = "https://data.kkg.kadaster.nl/query"
CURRENT_RUN = "urn:sparqlwatch:run:2026-08-22T16:00:00Z"
CURRENT_ONLY_CLASS = "http://www.w3.org/2002/07/owl#Restriction"
STALE_ONLY_CLASS = "urn:sparqlwatch:test:stale-only-class"


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
    fixture: drop kadaster's sample from the current run, leaving a store whose
    newest run sampled only the other endpoints. The obvious implementation
    (ORDER BY DESC(?generatedAt) LIMIT 1 in a subquery) returns NOTHING here,
    because the substituted endpoint does not reach inside a subquery's own
    projection, so the subquery picks the newest run overall and then fails to
    join. An endpoint that stopped responding would silently lose the last
    answer anyone had about it."""
    current_sample = NamedNode("urn:sparqlwatch:content-sample:2026-08-22T16:00:00Z:0")
    removed = list(store_two_sweeps.quads_for_pattern(current_sample, None, None, None))
    assert removed, "fixture changed: the current run's kadaster sample is gone"
    for quad in removed:
        store_two_sweeps.remove(quad)

    r = endpoint_content(store_two_sweeps, REPEATED_ENDPOINT)
    assert r.run == "urn:sparqlwatch:run:2026-08-22T14:00:00Z"
    assert STALE_ONLY_CLASS in r.classes, "the 14:00 run is now the newest that sampled it"

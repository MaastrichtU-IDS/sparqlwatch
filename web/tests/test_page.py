"""The page a person actually looks at, and the encoding it draws with.

Three kinds of test live here, and they are here together on purpose.

The first kind is about the encoding itself, and it renders nothing: the seven
states of docs/design/verdict-encoding.md must have distinct (border, fill,
weight) triples, because that triple is everything a reader who cannot
separate the colours has left. That property is what broke twice in the
JavaScript viewer, so it is asserted rather than reasoned about, and the
implementation is checked against the canonical document itself so the two
cannot drift.

The second kind is about the page not overstating what the store holds. An
empty class list for an endpoint nobody sampled, or a truncated sample drawn
only as a dashed border, are both confident wrong answers, and both are
cheaper to prevent here than to explain later.

The third kind is that the chips and the legend draw the same thing. They are
the same CSS class by construction, and this asserts it on the rendered page
rather than trusting the construction, because the construction is what was
wrong before.

Every value asserted here is a real value of the committed fixtures under
web/tests/fixtures/; see each fixture's header comment for its provenance.
"""

from html.parser import HTMLParser
from pathlib import Path

import pytest
from starlette.testclient import TestClient

import verdict_encoding
from app import (
    COMPLETE_TEXT,
    ENDPOINT_PATH,
    TRUNCATED_TEXT,
    _sample,
    app,
    get_store,
)
from endpoint_content import EndpointContent
from endpoint_measurements import EndpointMeasurements, MetricVerdict

KADASTER = "https://data.kkg.kadaster.nl/query"
QLEVER = "https://qlever.dev/api/osm-planet"
TRUNCATED = "https://truncated.example/sparql"
NO_CLASSES = "https://no-classes.example/sparql"

RUN = "urn:sparqlwatch:run:"
# The three sweeps the two-run stores below are built from. Each is the
# prov:generatedAtTime of one committed fixture's single run graph.
SAMPLING_SWEEP = "2026-08-22T16:00:00Z"
DECLINING_SWEEP = "2026-08-22T18:00:00Z"
LATER_SAMPLING_SWEEP = "2026-08-22T22:00:00Z"

# run-later-sample-only.nq's two values, which appear in no other fixture.
LATER_SAMPLE_CLASSES = [
    "urn:sparqlwatch:test:later-sample-class-a",
    "urn:sparqlwatch:test:later-sample-class-b",
]

M = "urn:sparqlwatch:metric:"
OWL_CLASS = "http://www.w3.org/2002/07/owl#Class"

# run-with-samples.nq: kadaster's class sample holds 59 values and is not
# truncated. See the fixture's header for how it was captured.
KADASTER_CLASS_COUNT = 59

CANONICAL_DOC = (
    Path(__file__).resolve().parents[2] / "docs/design/verdict-encoding.md"
)


@pytest.fixture
def client_for():
    """A test client whose route reads the store handed to it."""
    clients = []

    def build(store):
        app.dependency_overrides[get_store] = lambda: store
        client = TestClient(app)
        clients.append(client)
        return client

    yield build
    app.dependency_overrides.clear()
    for client in clients:
        client.close()


def page(client, endpoint):
    response = client.get(
        ENDPOINT_PATH, params={"url": endpoint}, headers={"accept": "text/html"}
    )
    assert response.status_code == 200
    return response.text


class _Elements(HTMLParser):
    """Every element on the page, as (tag, attributes) pairs.

    Reading the page through a parser rather than with a substring search
    means these tests do not depend on tag names, nesting or attribute order,
    only on the attributes the page is contracted to carry.
    """

    def __init__(self):
        super().__init__()
        self.elements = []

    def handle_starttag(self, tag, attrs):
        self.elements.append((tag, dict(attrs)))


def elements(text):
    parser = _Elements()
    parser.feed(text)
    return parser.elements


def with_attribute(text, name):
    return [attrs for _, attrs in elements(text) if name in attrs]


class _Texts(HTMLParser):
    """The text inside each element carrying a given attribute.

    Needed because several claims on this page are sentences rather than
    attributes: which sweep a sample came from, and what a missing sample
    means. Asserting that a sentence is somewhere in the document would pass
    while the sentence sat in the legend or in a comment, so these tests read
    the text of the element that is contracted to carry it.
    """

    def __init__(self, attribute):
        super().__init__()
        self.attribute = attribute
        self.found = []
        self._depth = None
        self._buffer = []

    def handle_starttag(self, tag, attrs):
        if self._depth is not None:
            self._depth += 1
        elif self.attribute in dict(attrs):
            self._depth = 0
            self._buffer = []

    def handle_endtag(self, tag):
        if self._depth is None:
            return
        if self._depth == 0:
            self.found.append(" ".join("".join(self._buffer).split()))
            self._depth = None
        else:
            self._depth -= 1

    def handle_data(self, data):
        if self._depth is not None:
            self._buffer.append(data)


def texts_with(text, attribute):
    parser = _Texts(attribute)
    parser.feed(text)
    return parser.found


def row_for(text, metric):
    """One metric row's attributes, by metric id."""
    rows = [row for row, _ in chips(text) if row["data-metric"] == metric]
    assert len(rows) == 1, f"{metric} appears {len(rows)} times"
    return rows[0]


def classes_of(attrs):
    return set(attrs.get("class", "").split())


def encoding_classes(attrs):
    """The .enc- classes on one element: what state it is drawn as."""
    return {name for name in classes_of(attrs) if name.startswith("enc-")}


def _drawn_by(text, row_attribute, marker):
    """Pair each row carrying ``row_attribute`` with the state its mark is
    drawn in.

    The state's class sits on the chip or the swatch, which is a child of the
    row, so this walks the document in order and takes the first ``marker``
    element inside each row rather than looking on the row itself. Reading the
    class off the mark is the point: the mark is the thing a reader sees, and
    a row that claims one state while its chip draws another is exactly the
    defect these tests are for.
    """
    found = []
    pending = None
    for _, attrs in elements(text):
        if row_attribute in attrs:
            assert pending is None, f"a row before {attrs} carried no {marker}"
            pending = attrs
        elif pending is not None and marker in classes_of(attrs):
            found.append((pending, encoding_classes(attrs)))
            pending = None
    assert pending is None, f"the last row carried no {marker}"
    return found


def chips(text):
    """Every metric row, paired with the state its chip is drawn in."""
    return _drawn_by(text, "data-metric", "chip")


def swatches(text):
    """Every legend entry, paired with the state its swatch is drawn in."""
    return {
        row["data-state"]: drawn
        for row, drawn in _drawn_by(text, "data-state", "swatch")
    }


# ---------------------------------------------------------------------------
# The encoding
# ---------------------------------------------------------------------------
def test_no_two_states_share_a_border_fill_weight_triple():
    """The property the whole encoding rests on.

    Colour is the one channel a reader may not have. If two states agree on
    border style, fill and border weight, then for that reader they are the
    same state. The earlier encoding used border style alone and collided
    twice over: solid covered verified, declared-only and declared-but-wrong,
    and dashed covered undeclared-but-verified and indeterminate.
    """
    triples = {}
    for state in verdict_encoding.STATES:
        assert state.triple not in triples, (
            f"{state.slug} and {triples[state.triple]} are drawn identically "
            f"as {state.triple}, so they are the same state to a reader who "
            f"cannot separate their colours"
        )
        triples[state.triple] = state.slug
    assert len(triples) == len(verdict_encoding.STATES) == 7


def test_the_unrecognised_fallback_collides_with_nothing():
    """An unknown verdict must not be drawn as a known one.

    The JavaScript viewer's fallback reuses absent's presentation exactly, so
    a value it does not understand is drawn as the one verdict that claims a
    negative: "we have no idea" rendered as "we established nothing was
    there". This tier draws it in a style none of the seven uses.
    """
    assert verdict_encoding.UNRECOGNISED.triple not in {
        state.triple for state in verdict_encoding.STATES
    }


def _states_in_the_canonical_document():
    """The seven states as docs/design/verdict-encoding.md's table states them.

    Parsed from the document rather than copied out of it: the document is
    canonical, so a test that reads it is a test that the implementation still
    matches the thing it is supposed to implement.
    """
    wanted = ("border", "fill", "weight")
    parsed = {}
    header_seen = False
    for line in CANONICAL_DOC.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|"):
            continue
        cells = [cell.strip(" `*") for cell in line.strip("|").split("|")]
        if cells[:4] == ["state", *wanted]:
            header_seen = True
            continue
        if not header_seen or set(cells[0]) <= {"-", ":"}:
            continue
        slug = cells[0].replace(" ", "-")
        parsed[slug] = (cells[1], cells[2] == "filled", int(cells[3].rstrip("px")))
    return parsed


def test_the_implementation_equals_the_canonical_table():
    """No third copy of the encoding.

    docs/design/verdict-encoding.md is canonical and verdict_encoding.py is
    the implementation, which makes them two copies of one table. This is the
    only thing keeping them equal.
    """
    documented = _states_in_the_canonical_document()
    implemented = {
        state.slug: state.triple for state in verdict_encoding.STATES
    }
    assert documented == implemented


def test_every_state_has_exactly_one_generated_rule():
    """The stylesheet is generated from the table, so the rules and the states
    are the same set. A hand-written rule for a state that no longer exists,
    or a state with no rule, is how a chip ends up drawn by whatever it
    inherits."""
    rules = verdict_encoding.css_rules()
    for state in (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED):
        selector = "." + verdict_encoding.css_class(state.slug) + " "
        assert rules.count(selector) == 1
    assert rules.count("{") == len(verdict_encoding.STATES) + 1


def test_a_borderless_state_still_reserves_its_border():
    """absent has no border, and still occupies the same space as the others.

    Dropping the border rather than making it transparent would shift every
    chip beside it by a pixel or two, which reads as a layout bug and, worse,
    makes the absent chip a different size from the swatch that explains it.
    """
    absent = verdict_encoding.presentation("absent")
    rule = [
        line
        for line in verdict_encoding.css_rules().splitlines()
        if verdict_encoding.css_class("absent") in line
    ]
    assert len(rule) == 1
    assert absent.border == "none"
    assert "1px solid transparent" in rule[0]


# ---------------------------------------------------------------------------
# The page
# ---------------------------------------------------------------------------
def test_the_page_names_the_endpoint_and_draws_every_measured_metric(
    client_for, store
):
    """One chip per metric the run recorded, and the endpoint named in full.

    run-with-samples.nq measures eight metrics on kadaster. A page that drops
    one is a page that under-reports a sweep.
    """
    text = page(client_for(store), KADASTER)
    assert KADASTER in text

    rows = chips(text)
    assert len(rows) == 8
    assert {row["data-metric"] for row, _ in rows} == {
        M + name
        for name in (
            "availability",
            "classes",
            "cors",
            "cors-preflight",
            "geo-data",
            "geo-functions",
            "has-classes",
            "service-description",
        )
    }
    for _, drawn in rows:
        assert len(drawn) == 1, "every row is drawn in exactly one state"


def test_a_verdict_is_drawn_in_its_own_states_class(client_for, store):
    """The verdict in the graph decides the drawing.

    kadaster's geo-functions is undeclared-but-verified and everything else
    it measures except that is verified, so this catches a page that draws
    every row the same way while still labelling them differently.
    """
    text = page(client_for(store), KADASTER)
    drawn = {
        row["data-metric"]: state
        for row, state in chips(text)
        if "data-verdict" in row
    }
    assert drawn[M + "geo-functions"] == {
        verdict_encoding.css_class("undeclared-but-verified")
    }
    assert drawn[M + "availability"] == {verdict_encoding.css_class("verified")}
    assert drawn[M + "geo-functions"] != drawn[M + "availability"]


def test_each_state_is_written_out_beside_its_chip(client_for, store):
    """The state is in the text, not only in the drawing.

    A chip is an aid for a reader who can see it. Someone hearing this page
    read aloud gets nothing from a border style, so the state is spelled out
    as well: qlever's service-description is absent and its classes metric is
    indeterminate, and both words appear.
    """
    text = page(client_for(store), QLEVER)
    for label in ("absent", "indeterminate", "verified"):
        assert label in text


# ---------------------------------------------------------------------------
# Truncation
# ---------------------------------------------------------------------------
def test_a_truncated_sample_says_so_in_text(client_for, store_truncated):
    """The one thing on this page that must never be a border style alone.

    run-truncated.nq carries sw:sampleTruncated true. A reader who takes a
    cut-off list for the endpoint's whole vocabulary has been misled by this
    page, so the truncation is a sentence, which survives greyscale and a
    screen reader both.
    """
    text = page(client_for(store_truncated), TRUNCATED)
    assert TRUNCATED_TEXT in text
    assert TRUNCATED_TEXT == "truncated: more may exist beyond the limit"
    assert COMPLETE_TEXT not in text


def test_a_complete_sample_says_that_in_text_too(client_for, store):
    """kadaster's sample is not truncated, and the page says which it is
    rather than leaving the reader to notice an absence. "No warning" and "we
    checked and it is complete" are different claims."""
    text = page(client_for(store), KADASTER)
    assert COMPLETE_TEXT in text
    assert TRUNCATED_TEXT not in text


def test_a_sample_only_endpoint_still_gets_a_page(client_for, store_truncated):
    """run-truncated.nq holds no measurement at all, so the conformance
    section has nothing to show. It must say so rather than render an empty
    list, which would read as a sweep that found nothing wrong."""
    text = page(client_for(store_truncated), TRUNCATED)
    assert with_attribute(text, "data-metric") == []
    assert with_attribute(text, "data-no-measurements") != []


# ---------------------------------------------------------------------------
# An endpoint with no sample
# ---------------------------------------------------------------------------
def test_an_unsampled_endpoint_renders_no_class_list(client_for, store):
    """qlever has no class sample in run-with-samples.nq.

    Its classes metric read indeterminate after 30003 ms, the 30 second
    request budget running out. An empty class list here would tell a reader
    this endpoint holds no classes, which is a confident wrong answer about
    the largest endpoint in the fixture.
    """
    text = page(client_for(store), QLEVER)
    assert with_attribute(text, "data-class") == []
    absent = with_attribute(text, "data-sample")
    assert [row["data-sample"] for row in absent] == ["absent"]
    assert "classes sampled" not in text


def test_the_reason_for_a_missing_sample_is_read_from_the_graph(
    client_for, store, store_declined
):
    """Two endpoints have no sample for two different reasons, and the page
    reports each one's own.

    qlever's classes metric was measured and came back indeterminate.
    kadaster in run-declined.nq was never measured at all, on the run's cost
    ceiling. Naming either cause for the other would be a fabrication, and
    naming neither would leave the reader to assume the endpoint is empty.
    """
    timed_out = page(client_for(store), QLEVER)
    assert "indeterminate" in timed_out
    assert "30003" in timed_out
    assert "cost-ceiling" not in timed_out

    declined = page(client_for(store_declined), KADASTER)
    assert "cost-ceiling" in declined
    assert "30003" not in declined


def test_a_sample_of_zero_reports_its_size_rather_than_a_list(
    client_for, store_zero_classes
):
    """run-zero-classes.nq is a sample that states a size of 0 and lists
    nothing, which the prober deliberately never writes.

    This is the mirror image of the unsampled case: here a run really did
    sample, so the page states what it found, and it says in words that the
    size is the thing to read rather than leaving an empty list to be read as
    the answer.
    """
    text = page(client_for(store_zero_classes), "https://zero-classes.example/sparql")
    assert [row["data-sample"] for row in with_attribute(text, "data-sample")] == [
        "present"
    ]
    assert with_attribute(text, "data-class") == []
    assert with_attribute(text, "data-sample-empty") != []
    assert "0 classes sampled" in text


def test_a_run_that_never_looked_at_classes_borrows_no_reason():
    """The third way a sample can be missing: nothing about classes at all.

    A run that measured other metrics and never touched sw:metric:classes has
    no reason to report, and there is no committed fixture in that shape (the
    route 404s an endpoint with neither a measurement nor a class sample), so
    this asks _sample directly rather than inventing a store. What it pins is
    that the page borrows neither of the other two explanations, which are
    both statements about a probe that ran.
    """
    measured_something_else = EndpointMeasurements(
        endpoint="https://example.org/sparql",
        assessed=True,
        run="urn:sparqlwatch:run:x",
        generated_at="2026-08-22T16:00:00Z",
        verdicts=[MetricVerdict(metric=M + "availability", verdict="verified")],
    )
    sample = _sample(
        measured_something_else,
        EndpointContent(endpoint="https://example.org/sparql", sampled=False),
    )
    assert sample["present"] is False
    assert sample["classes"] == []
    assert "no measurement of the classes metric" in sample["this_run_text"]
    assert "cost-ceiling" not in sample["this_run_text"]
    assert "indeterminate" not in sample["this_run_text"]


# ---------------------------------------------------------------------------
# A decline is not a verdict
# ---------------------------------------------------------------------------
def test_a_declined_metric_is_drawn_as_not_measured(client_for, store_declined):
    """run-declined.nq declines sw:metric:classes for kadaster.

    "We chose not to look" is not a finding about the endpoint, so it is drawn
    in the one state that is not a verdict, and its row states no value.
    """
    text = page(client_for(store_declined), KADASTER)

    declined = [(row, state) for row, state in chips(text) if "data-declined" in row]
    assert len(declined) == 1
    row, state = declined[0]
    assert row["data-metric"] == M + "classes"
    assert row["data-declined"] == "cost-ceiling"
    assert "data-verdict" not in row
    assert state == {verdict_encoding.css_class(verdict_encoding.NOT_MEASURED)}


def test_a_decline_is_drawn_unlike_every_verdict(client_for, store_declined):
    """Distinguishable from a verdict, not merely labelled differently.

    The decline's drawing must differ from every verdict's drawing on the same
    page, otherwise a reader sees a metric that was measured.
    """
    text = page(client_for(store_declined), KADASTER)
    drawn = chips(text)
    declined = {
        frozenset(state) for row, state in drawn if "data-declined" in row
    }
    verdicts = {
        frozenset(state) for row, state in drawn if "data-verdict" in row
    }
    assert len(declined) == 1
    assert verdicts, "the same page must carry verdicts to be distinguished from"
    assert not (declined & verdicts)


# ---------------------------------------------------------------------------
# The class list
# ---------------------------------------------------------------------------
def test_the_class_list_holds_every_sampled_value(client_for, store):
    """kadaster's sample in run-with-samples.nq is 59 classes, one of them
    owl:Class. Both the count the page states and the list it renders have to
    match the sample, because a list shorter than its own stated count is a
    truncation the page is not admitting to."""
    text = page(client_for(store), KADASTER)

    listed = [row["data-class"] for row in with_attribute(text, "data-class")]
    assert len(listed) == KADASTER_CLASS_COUNT
    assert OWL_CLASS in listed
    assert f"{KADASTER_CLASS_COUNT} classes sampled" in text


def test_a_class_iri_is_escaped_not_interpolated(client_for, store):
    """Every value on this page is data from a third party. An endpoint that
    returns a class IRI containing markup must not be able to write markup
    into the page, so the rendered document holds no raw angle bracket from
    any sampled value."""
    text = page(client_for(store), KADASTER)
    listed = [row["data-class"] for row in with_attribute(text, "data-class")]
    assert all(value.startswith("http") for value in listed)
    assert "<http" not in text


# ---------------------------------------------------------------------------
# The legend and the chips draw the same thing
# ---------------------------------------------------------------------------
def test_the_legend_explains_all_seven_states(client_for, store):
    text = page(client_for(store), KADASTER)
    listed = [row["data-state"] for row in with_attribute(text, "data-state")]
    assert listed == [state.slug for state in verdict_encoding.STATES]


def test_a_legend_swatch_is_drawn_exactly_like_the_chip_it_explains(
    client_for, store
):
    """The defect this file exists to prevent.

    The JavaScript viewer's legend gave absent a fill the chips never had, so
    the one verdict that claims a negative was explained by a swatch that did
    not match it. Here the swatch and the chip are the same generated class,
    and this asserts it on the rendered page rather than trusting that.
    """
    text = page(client_for(store), KADASTER)

    explained = swatches(text)
    for state in verdict_encoding.STATES:
        assert explained[state.slug] == {
            verdict_encoding.css_class(state.slug)
        }, f"the legend draws {state.slug} as something else"

    drawn_on_swatches = set().union(*explained.values())
    for row, drawn in chips(text):
        assert drawn <= drawn_on_swatches, (
            f"{row['data-metric']} is drawn as {drawn}, which no legend "
            f"swatch explains"
        )


def test_the_page_carries_the_generated_rules_and_no_others(client_for, store):
    """The stylesheet the browser sees is the generated one.

    A second, hand-written .enc- rule further down the page would win on
    cascade order and quietly redraw a state, which is precisely how two
    copies of this encoding drifted before.
    """
    text = page(client_for(store), KADASTER)
    for state in (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED):
        selector = "." + verdict_encoding.css_class(state.slug)
        assert text.count(selector + " ") == 1
    assert verdict_encoding.css_rules() in text


# ---------------------------------------------------------------------------
# Two sweeps, two facts, two attributions
# ---------------------------------------------------------------------------
def test_a_page_from_one_sweep_attributes_everything_to_it(client_for, store):
    """The common case, and the baseline for the two skewed ones below.

    run-with-samples.nq is a single sweep, so the verdicts and the class
    sample really are one sweep's observations and the page says so plainly.
    Nothing here may hedge about a second sweep, because there is not one:
    a clause explaining which sweep saw what would be noise on every page
    of a freshly swept store.
    """
    text = page(client_for(store), KADASTER)

    assert (
        f"Everything below is what one probe sweep observed, at "
        f"{SAMPLING_SWEEP}." in text
    )
    head = with_attribute(text, "data-sample-generated-at")
    assert len(head) == 1
    assert head[0]["data-sample-generated-at"] == SAMPLING_SWEEP
    assert head[0]["data-sample-run"] == RUN + SAMPLING_SWEEP
    assert [row["data-run"] for row in with_attribute(text, "data-run")] == [
        RUN + SAMPLING_SWEEP
    ]
    assert with_attribute(text, "data-sample-provenance") == []
    assert with_attribute(text, "data-this-run-sample") == []
    assert "different sweep" not in text


def test_an_older_samples_own_sweep_is_named_beside_it(
    client_for, store_stale_sample
):
    """The defect this pair of fixtures exists to catch.

    The 18:00 sweep declined sw:metric:classes on its cost ceiling, which is
    the prober's default and therefore the intended steady state, so the
    newest run that MEASURED kadaster and the newest run that SAMPLED it are
    different runs. Both facts are true and both are kept, and each is
    attributed to the sweep that observed it: the page must not draw a
    59-class sample marked complete under a timestamp belonging to a sweep
    that declined to look at classes at all.
    """
    text = page(client_for(store_stale_sample), KADASTER)

    assert [row["data-run"] for row in with_attribute(text, "data-run")] == [
        RUN + DECLINING_SWEEP
    ]
    head = with_attribute(text, "data-sample-generated-at")
    assert len(head) == 1
    assert head[0]["data-sample-generated-at"] == SAMPLING_SWEEP
    assert head[0]["data-sample-run"] == RUN + SAMPLING_SWEEP

    provenance = texts_with(text, "data-sample-provenance")
    assert len(provenance) == 1, "the sample must say which sweep took it"
    assert SAMPLING_SWEEP in provenance[0]
    assert DECLINING_SWEEP in provenance[0]

    # The header sentence covered both facts and dated both to the newer
    # sweep. It must no longer claim anything about the sample.
    assert "Everything below is what one probe sweep observed" not in text
    assert f"Every verdict below is what one probe sweep observed, at {DECLINING_SWEEP}." in text

    # Both facts survive. The decline is still drawn as a decline...
    declined = row_for(text, M + "classes")
    assert declined["data-declined"] == "cost-ceiling"
    this_run = texts_with(text, "data-this-run-sample")
    assert len(this_run) == 1
    assert "cost-ceiling" in this_run[0]
    # ...and the older sweep's 59 classes are still published, not discarded.
    assert (
        len(with_attribute(text, "data-class")) == KADASTER_CLASS_COUNT
    )
    assert f"{KADASTER_CLASS_COUNT} classes sampled" in text
    assert COMPLETE_TEXT in text


def test_a_newer_samples_own_sweep_is_named_beside_it(
    client_for, store_later_sample
):
    """The same skew running the other way.

    A 22:00 run sampled kadaster and measured nothing, so the verdicts are
    the 16:00 sweep's and the sample is the 22:00 run's. The page dated
    everything to 16:00 while showing the 22:00 sample, truncation badge and
    all. Note what must NOT appear: the 16:00 sweep did publish a sample of
    its own (this store holds it), so nothing here may say that the sweep the
    verdicts came from produced no class sample.
    """
    text = page(client_for(store_later_sample), KADASTER)

    assert [row["data-run"] for row in with_attribute(text, "data-run")] == [
        RUN + SAMPLING_SWEEP
    ]
    head = with_attribute(text, "data-sample-generated-at")
    assert len(head) == 1
    assert head[0]["data-sample-generated-at"] == LATER_SAMPLING_SWEEP
    assert head[0]["data-sample-run"] == RUN + LATER_SAMPLING_SWEEP

    provenance = texts_with(text, "data-sample-provenance")
    assert len(provenance) == 1
    assert LATER_SAMPLING_SWEEP in provenance[0]
    assert SAMPLING_SWEEP in provenance[0]

    listed = [row["data-class"] for row in with_attribute(text, "data-class")]
    assert listed == LATER_SAMPLE_CLASSES
    assert OWL_CLASS not in listed, "that is the 16:00 sample's value"
    assert "2 classes sampled" in text
    assert TRUNCATED_TEXT in text

    # The 16:00 verdict for the same metric is still reported as a verdict.
    assert row_for(text, M + "classes")["data-verdict"] == "verified"
    assert texts_with(text, "data-this-run-sample") == []


# ---------------------------------------------------------------------------
# absent is a finding, not a shrug
# ---------------------------------------------------------------------------
def test_an_absent_classes_verdict_is_reported_as_an_established_negative(
    client_for, store_classes_absent, store
):
    """absent is one of the two assertive verdicts, and the page must not
    deny it.

    prober/src/resolve.rs records absent for a SelectIris probe only when the
    endpoint answered with a parsed SPARQL-JSON result that bound nothing,
    which is the evidence that establishes the negative, and it writes no
    sample beside it. Telling the reader that this is "not a report that the
    endpoint holds no classes" contradicts the store, and buries the one
    negative finding this metric can establish. indeterminate, on the same
    page shape, is the case where that sentence is true, so both are asserted
    here.
    """
    established = texts_with(page(client_for(store_classes_absent), NO_CLASSES), "data-sample")
    assert len(established) == 1
    assert "'absent'" in established[0]
    assert "42 ms" in established[0]
    assert "does report that the endpoint holds no classes" in established[0]
    assert (
        "is not a report that the endpoint holds no classes"
        not in established[0]
    )

    text = page(client_for(store_classes_absent), NO_CLASSES)
    assert row_for(text, M + "classes")["data-verdict"] == "absent"

    # qlever's classes probe ran out of its 30 second budget, so nothing was
    # established and the page says exactly that.
    nothing_established = texts_with(page(client_for(store), QLEVER), "data-sample")
    assert len(nothing_established) == 1
    assert "'indeterminate'" in nothing_established[0]
    assert (
        "is not a report that the endpoint holds no classes"
        in nothing_established[0]
    )

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

import re
from html.parser import HTMLParser
from pathlib import Path

import pytest
from starlette.testclient import TestClient

import verdict_encoding
from app import (
    COMPLETE_TEXT,
    ENDPOINT_PATH,
    TRUNCATED_TEXT,
    _rows,
    _sample,
    app,
    get_store,
)
from endpoint_content import EndpointContent
from endpoint_measurements import (
    DeclinedMetric,
    EndpointMeasurements,
    MetricVerdict,
)

KADASTER = "https://data.kkg.kadaster.nl/query"
QLEVER = "https://qlever.dev/api/osm-planet"
TRUNCATED = "https://truncated.example/sparql"
NO_CLASSES = "https://no-classes.example/sparql"
HOSTILE = "https://hostile-literals.example/sparql"

# run-hostile-literals.nq's two literals, verbatim. Each opens with a double
# quote to close whatever attribute it lands in, then opens an element.
HOSTILE_VERDICT = '"><script>alert(1)</script>'
HOSTILE_REASON = '"><img src=x onerror="alert(2)">'

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


class _RowCells(HTMLParser):
    """The text of each named cell inside each metric row, by metric id.

    The row's own text is every cell run together, so a test that read the
    row could not tell the state apart from the metric name, the detail or
    the elapsed time, and a test that searched the whole document would pass
    on the legend, which lists every state label on every page. These are the
    three cells the template contracts to carry a claim about one metric:
    ``m-state`` is the state written out in words beside the chip,
    ``m-detail`` the graded conformance level, and ``m-time`` the elapsed
    time. A cell the row does not render is absent from the mapping rather
    than empty, which is a different thing from a cell that rendered nothing.
    """

    CELLS = ("m-state", "m-detail", "m-time")

    def __init__(self):
        super().__init__()
        self.rows = {}
        self._metric = None
        self._cell = None
        self._buffer = []

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "data-metric" in attributes:
            self._metric = attributes["data-metric"]
            self.rows[self._metric] = {}
            return
        if self._metric is None:
            return
        named = classes_of(attributes) & set(self.CELLS)
        if named:
            assert len(named) == 1, f"one cell carries {named}"
            cell = named.pop()
            assert cell not in self.rows[self._metric], (
                f"{cell} appears twice in {self._metric}'s row"
            )
            self._cell = cell
            self._buffer = []

    def handle_endtag(self, tag):
        if self._cell is None:
            return
        self.rows[self._metric][self._cell] = " ".join(
            "".join(self._buffer).split()
        )
        self._cell = None

    def handle_data(self, data):
        if self._cell is not None:
            self._buffer.append(data)


def row_cells(text, metric):
    """One metric row's named cells, as the reader sees them."""
    parser = _RowCells()
    parser.feed(text)
    assert metric in parser.rows, f"{metric} has no row on this page"
    return parser.rows[metric]


def test_a_duplicated_row_cell_is_refused_by_the_parser():
    """The other half of the docstring's claim ("the three cells the template
    contracts to carry a claim about one metric"): a cell name must not
    appear twice in one row. Without this, ``handle_endtag`` assigning
    ``self.rows[self._metric][self._cell] = ...`` simply overwrites the first
    span with the second, so a template that renders
    ``<span class="m-state ...">verified</span>`` before the real state span
    would report "verified absent" nowhere, because the mapping keeps only
    the last one. See N4 in the re-review for the live consequence."""
    html = (
        '<li class="metric-row" data-metric="urn:sparqlwatch:metric:classes">'
        '<span class="m-state tok-good">verified</span>'
        '<span class="m-state tok-bad">indeterminate</span>'
        "</li>"
    )
    with pytest.raises(AssertionError, match="m-state"):
        row_cells(html, "urn:sparqlwatch:metric:classes")


class _LegendCells(HTMLParser):
    """Each legend entry's label and count, by state slug."""

    CELLS = ("l-label", "l-count")

    def __init__(self):
        super().__init__()
        self.entries = {}
        self._slug = None
        self._cell = None
        self._buffer = []

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "data-state" in attributes:
            self._slug = attributes["data-state"]
            self.entries[self._slug] = {}
            return
        if self._slug is None:
            return
        named = classes_of(attributes) & set(self.CELLS)
        if named:
            cell = named.pop()
            assert cell not in self.entries[self._slug], (
                f"{cell} appears twice in the {self._slug} legend entry"
            )
            self._cell = cell
            self._buffer = []

    def handle_endtag(self, tag):
        if self._cell is None:
            return
        self.entries[self._slug][self._cell] = " ".join(
            "".join(self._buffer).split()
        )
        self._cell = None

    def handle_data(self, data):
        if self._cell is not None:
            self._buffer.append(data)


def legend_cells(text):
    parser = _LegendCells()
    parser.feed(text)
    return parser.entries


def test_a_duplicated_legend_cell_is_refused_by_the_parser():
    """The same shape of gap as the row parser above, in ``_LegendCells``:
    two elements named ``l-label`` in one legend entry must not silently
    collapse to the last one."""
    html = (
        '<li data-state="verified">'
        '<span class="l-label">Verified</span>'
        '<span class="l-label">Something else</span>'
        '<span class="l-count">3</span>'
        "</li>"
    )
    with pytest.raises(AssertionError, match="l-label"):
        legend_cells(html)


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
    wanted = ("border", "fill", "weight", "means")
    parsed = {}
    header_seen = False
    for line in CANONICAL_DOC.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|"):
            continue
        cells = [cell.strip(" `*") for cell in line.strip("|").split("|")]
        if cells[:5] == ["state", *wanted]:
            header_seen = True
            continue
        if not header_seen or set(cells[0]) <= {"-", ":"}:
            continue
        slug = cells[0].replace(" ", "-")
        parsed[slug] = (
            cells[1],
            cells[2] == "filled",
            int(cells[3].rstrip("px")),
            cells[4],
        )
    return parsed


def test_the_implementation_equals_the_canonical_table():
    """No third copy of the encoding.

    docs/design/verdict-encoding.md is canonical and verdict_encoding.py is
    the implementation, which makes them two copies of one table. This is the
    only thing keeping them equal.
    """
    documented = _states_in_the_canonical_document()
    implemented = {
        state.slug: (*state.triple, state.meaning)
        for state in verdict_encoding.STATES
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


def _generated_declarations():
    """The emitted stylesheet, as class name -> declaration body.

    Read out of css_rules() rather than off the table. _declarations() is a
    separate function with its own mapping step from the three channels to
    CSS, so the table's injectivity does not reach the stylesheet by
    construction, and the stylesheet is what the reader sees.
    """
    bodies = {}
    for line in verdict_encoding.css_rules().splitlines():
        selector, brace, rest = line.strip().partition("{")
        assert brace, f"not a rule: {line!r}"
        bodies[selector.strip().lstrip(".")] = " ".join(rest.rstrip("}").split())
    return bodies


# A theme colour variable, and nothing else. "transparent" is deliberately
# NOT matched: absent's border is realised as "1px solid transparent", which
# is an absence of ink rather than a hue, so it is a channel value that
# survives greyscale and must stay in the comparison below. Erasing it would
# make absent and declared-only look like a collision that they are not.
_COLOUR_TOKEN = re.compile(r"var\(--[a-z0-9-]+\)")


def test_no_two_generated_rules_differ_only_by_colour():
    """The injectivity the encoding rests on, asserted on the CSS emitted.

    test_no_two_states_share_a_border_fill_weight_triple asserts it on the
    table. Nothing asserted it on the rules, and the emitter maps the table
    to CSS itself: collapsing its border-style mapping to a constant made
    verified and undeclared-but-verified byte-identical and left three more
    states differing only by colour token, with every other test in this file
    green. That is the drift this module exists to prevent, and it is the
    drift that happened in the JavaScript viewer, where a dotted style
    rendered as solid.

    Two states whose rules differ only in the colour they name are one state
    to a reader who cannot separate the colours, whatever the table says.
    """
    bodies = _generated_declarations()
    assert set(bodies) == {
        verdict_encoding.css_class(state.slug)
        for state in (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED)
    }

    colourless = {}
    for name, body in bodies.items():
        without_colour = _COLOUR_TOKEN.sub("COLOUR", body)
        assert without_colour not in colourless, (
            f"{name} and {colourless[without_colour]} emit rules that differ "
            f"only in the colour they name ({without_colour}), so they are "
            f"the same state to a reader who cannot separate the colours"
        )
        colourless[without_colour] = name
    assert len(colourless) == len(bodies) == len(verdict_encoding.STATES) + 1


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
    """The state is in the text of the row, not only in the drawing.

    A chip is an aid for a reader who can see it, and it carries
    aria-hidden="true" precisely because it is not the channel the state
    travels on. Someone hearing this page read aloud gets nothing from a
    border style, so the state is spelled out in the row itself.

    Asserted per named metric, with the value the graph holds. Searching the
    whole document for a state label cannot fail: the legend lists all seven
    on every page, so emptying every row's state text leaves such a test
    green while removing the entire textual channel.
    """
    text = page(client_for(store), QLEVER)
    assert row_cells(text, M + "service-description")["m-state"] == "absent"
    assert row_cells(text, M + "classes")["m-state"] == "indeterminate"
    assert row_cells(text, M + "availability")["m-state"] == "verified"
    assert row_cells(text, M + "geo-functions")["m-state"] == "indeterminate"

    # The same page, and the same channel, for the two states whose label is
    # not their slug: kadaster's geo-functions is undeclared-but-verified,
    # which reads "works, not declared".
    kadaster = page(client_for(store), KADASTER)
    assert (
        row_cells(kadaster, M + "geo-functions")["m-state"]
        == verdict_encoding.presentation("undeclared-but-verified").label
    )
    assert row_cells(kadaster, M + "classes")["m-state"] == "verified"


def test_a_declined_metric_writes_out_the_state_and_the_reason(
    client_for, store_declined
):
    """A decline is not a verdict, and its row says so in words as well.

    "not measured (cost-ceiling)" states no value about the endpoint and
    names why nobody looked. Drawn as a dotted border alone it would be
    indistinguishable, to a reader who cannot see it, from a measurement.
    """
    text = page(client_for(store_declined), KADASTER)
    assert (
        row_cells(text, M + "classes")["m-state"] == "not measured (cost-ceiling)"
    )


def test_a_graded_metric_states_the_level_the_graph_recorded(client_for, store):
    """The conformance level is the only graded value on the page.

    run-with-samples.nq carries sw:level on exactly two measurements, both
    for service-description: 1 for kadaster and 0 for qlever. Asserting the
    value rather than the presence of the word is what makes this catch a
    page that prints one level for every row, and asserting a metric with no
    level catches a page that invents one.
    """
    kadaster = page(client_for(store), KADASTER)
    assert (
        row_cells(kadaster, M + "service-description")["m-detail"]
        == "conformance level 1"
    )
    assert "m-detail" not in row_cells(kadaster, M + "has-classes")

    qlever = page(client_for(store), QLEVER)
    assert (
        row_cells(qlever, M + "service-description")["m-detail"]
        == "conformance level 0"
    )


def test_a_row_states_the_elapsed_time_the_graph_recorded(
    client_for, store, store_declined
):
    """The elapsed time is a measured value, so it comes from the graph.

    qlever's classes probe ran 30003 ms (the 30 second budget running out)
    and kadaster's service-description 151 ms, both recorded in
    run-with-samples.nq. A declined metric was never run, so its row states
    no time at all: printing a zero there would read as an instant answer.
    """
    qlever = page(client_for(store), QLEVER)
    assert row_cells(qlever, M + "classes")["m-time"] == "30003 ms"

    kadaster = page(client_for(store), KADASTER)
    assert (
        row_cells(kadaster, M + "service-description")["m-time"] == "151 ms"
    )
    assert row_cells(kadaster, M + "classes")["m-time"] == "73 ms"

    declined = page(client_for(store_declined), KADASTER)
    assert row_cells(declined, M + "classes")["m-time"] == ""


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


def test_each_decline_reason_gets_its_own_detail():
    """A declined row's detail is derived from the graph's reason.

    Two reasons exist today, and prober/src/emit.rs's
    NotMeasuredReason::slug is where both come from: "cost-ceiling", where
    the sweep looked at its budget and chose not to run the metric, and
    "prober-failed", where the task probing the endpoint's host panicked or
    was cancelled so nothing was ever asked. They are opposite claims about
    who is responsible, and reporting a crash as a cost decision points an
    operator at --max-cost instead of at the crash.

    The third case is the one this project will meet again, because it has
    added a reason once already: a reason from a prober this page has no
    sentence for. There the honest answer is to claim nothing and let the
    state text carry the value verbatim, which is how an unrecognised
    verdict is already handled.

    Asked of _rows directly rather than through a store, because the third
    case has no committed fixture and inventing one would pin a reason no
    prober emits.
    """
    rows = {
        row["metric"]: row
        for row in _rows(
            EndpointMeasurements(
                endpoint="https://example.org/sparql",
                assessed=True,
                run="urn:sparqlwatch:run:x",
                generated_at="2026-08-22T16:00:00Z",
                declined=[
                    DeclinedMetric(metric=M + "classes", reason="cost-ceiling"),
                    DeclinedMetric(metric=M + "cors", reason="prober-failed"),
                    DeclinedMetric(metric=M + "geo-data", reason="from-the-future"),
                ],
            )
        )
    }

    assert rows[M + "classes"]["detail"] == (
        "we declined to look, so this says nothing about the endpoint"
    )
    assert rows[M + "cors"]["detail"] == (
        "the prober failed on this endpoint, so this run observed nothing "
        "about it"
    )
    assert rows[M + "geo-data"]["detail"] == (
        "unrecognised reason, shown as the store recorded it"
    )

    # Whichever branch the detail came from, the row still names the reason
    # the store holds, so a reader is never left with only our sentence.
    for metric, reason in (
        (M + "classes", "cost-ceiling"),
        (M + "cors", "prober-failed"),
        (M + "geo-data", "from-the-future"),
    ):
        assert rows[metric]["state_text"] == f"not measured ({reason})"


def test_a_prober_failed_row_and_a_cost_ceiling_row_read_differently(
    client_for, store_prober_failed
):
    """The same page, both reasons, and neither row borrows the other's text.

    run-prober-failed.nq is the emitter's own output for an endpoint whose
    group failed under the default cost ceiling: seven metrics prober-failed
    and metric:classes cost-ceiling. An operator reads this page precisely
    when the prober has crashed, so a prober-failed row that says the metric
    was priced out of the run sends them to --max-cost instead of to the
    crash.
    """
    text = page(client_for(store_prober_failed), KADASTER)

    reasons = {
        row["data-metric"]: row["data-declined"]
        for row in with_attribute(text, "data-declined")
    }
    assert reasons[M + "classes"] == "cost-ceiling"
    assert reasons[M + "availability"] == "prober-failed"
    assert len(reasons) == 8, "every metric in this run was declined"

    assert row_cells(text, M + "availability")["m-detail"] == (
        "the prober failed on this endpoint, so this run observed nothing "
        "about it"
    )
    assert row_cells(text, M + "classes")["m-detail"] == (
        "we declined to look, so this says nothing about the endpoint"
    )

    # The legend is on this page too, and it explains a state rather than
    # either of the two reasons the rows carry.
    assert "cost ceiling" not in text


def test_the_not_measured_legend_entry_names_no_reason():
    """The legend explains a state, and the state is not one reason.

    templates/endpoint.html renders state.meaning into the legend for every
    page, whatever reasons that page's rows carry, so a meaning naming one
    of the two reasons tells half the readers of a prober-failed run
    something untrue. The reason belongs on the row, where it is read out of
    the graph.
    """
    state = verdict_encoding.presentation(verdict_encoding.NOT_MEASURED)
    assert state.meaning == "no measurement was taken; the row says why"
    for reason in ("cost", "ceiling", "prober", "failed", "declined"):
        assert reason not in state.meaning


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


def test_the_class_list_renders_bare_iris(client_for, store):
    """Each sampled value is rendered as the IRI it is, nothing around it.

    This is a claim about the rendering, not about escaping: an IRI cannot
    contain '<', '>' or '"' at all and pyoxigraph rejects one that tries, so
    a class IRI is a value that cannot carry markup onto this page whatever
    the template does with it. The escaping guarantee is asserted on the
    literals, which can, in
    test_a_hostile_literal_is_escaped_in_the_attribute_and_the_text below.

    Reads the rendered text of each data-class element rather than searching
    the raw HTML source: the N-Triples form of an IRI is `&lt;http`, not
    `<http`, so a source-text search for `<http` can only ever catch a
    template that writes a literal '<' of its own outside an attribute.
    """
    text = page(client_for(store), KADASTER)
    listed = [row["data-class"] for row in with_attribute(text, "data-class")]
    assert all(value.startswith("http") for value in listed)
    rendered = texts_with(text, "data-class")
    assert rendered == listed, "each value must render as the bare IRI, not wrapped in <>"


def test_a_hostile_literal_is_escaped_in_the_attribute_and_the_text(
    client_for, store_hostile_literals
):
    """Markup in a literal must not become markup on the page.

    The values that can carry it are the literals, because an IRI cannot hold
    an angle bracket. run-hostile-literals.nq carries two: a dqv:value the
    page shows verbatim (it is not one of the six verdicts, so it is drawn
    unrecognised and reported as recorded) and an sw:notMeasuredReason. Both
    reach the page through two channels, an attribute and the text a reader
    sees, so both are checked on both, and each is checked to have arrived
    intact as well as inert: escaping that mangled the value would hide what
    the store holds, which is the other half of the same requirement.
    """
    text = page(client_for(store_hostile_literals), HOSTILE)

    # Nothing either literal asked for became an element or a raw tag.
    tags = {tag for tag, _ in elements(text)}
    assert "script" not in tags
    assert "img" not in tags
    assert "<script" not in text
    assert "<img" not in text
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in text

    # The unrecognised verdict, on both channels, unchanged.
    assert row_for(text, M + "classes")["data-verdict"] == HOSTILE_VERDICT
    assert row_cells(text, M + "classes")["m-state"] == HOSTILE_VERDICT

    # The decline reason, on both channels, unchanged.
    assert row_for(text, M + "geo-data")["data-declined"] == HOSTILE_REASON
    assert (
        row_cells(text, M + "geo-data")["m-state"]
        == f"not measured ({HOSTILE_REASON})"
    )

    # And in the sentence explaining why there is no class sample, which
    # quotes the verdict it read.
    note = texts_with(text, "data-sample")
    assert len(note) == 1
    assert f"'{HOSTILE_VERDICT}'" in note[0]


def test_an_unrecognised_verdict_is_drawn_and_explained_as_unrecognised(
    client_for, store_hostile_literals, store
):
    """A value this build has no encoding for is shown, not dropped or
    relabelled.

    The legend's eighth entry exists only when a page needed it, so a page
    without such a value must not carry it: a key that changes shape between
    endpoints for no reason makes the reader compare two different keys. The
    JavaScript viewer drew an unknown value in absent's presentation, which
    renders "we have no idea what this means" as "we established that nothing
    was there".
    """
    text = page(client_for(store_hostile_literals), HOSTILE)

    drawn = {row["data-metric"]: state for row, state in chips(text)}
    assert drawn[M + "classes"] == {
        verdict_encoding.css_class(verdict_encoding.UNRECOGNISED.slug)
    }
    assert (
        row_cells(text, M + "classes")["m-detail"]
        == "unrecognised verdict, shown as the store recorded it"
    )

    legend = legend_cells(text)
    assert list(legend) == [
        state.slug for state in verdict_encoding.STATES
    ] + [verdict_encoding.UNRECOGNISED.slug]
    assert (
        legend[verdict_encoding.UNRECOGNISED.slug]["l-label"]
        == verdict_encoding.UNRECOGNISED.label
    )
    assert legend[verdict_encoding.UNRECOGNISED.slug]["l-count"] == "1"

    # kadaster's eight verdicts are all in the table, so its legend has seven.
    assert verdict_encoding.UNRECOGNISED.slug not in legend_cells(
        page(client_for(store), KADASTER)
    )


# ---------------------------------------------------------------------------
# The legend and the chips draw the same thing
# ---------------------------------------------------------------------------
def test_the_legend_explains_all_seven_states(client_for, store):
    text = page(client_for(store), KADASTER)
    listed = [row["data-state"] for row in with_attribute(text, "data-state")]
    assert listed == [state.slug for state in verdict_encoding.STATES]


def test_the_legend_labels_and_counts_the_states_on_this_page(
    client_for, store
):
    """The legend's count is a claim about this page, and it is checked.

    kadaster's eight measured metrics in run-with-samples.nq are seven
    verified and one undeclared-but-verified, so those are the only two
    non-zero counts, and every other state is listed at zero rather than
    dropped: the legend explains an encoding, not this endpoint. qlever's
    counts differ on the same fixture, which is what makes this catch a
    legend printing one number everywhere.
    """
    kadaster = legend_cells(page(client_for(store), KADASTER))
    assert {
        slug: entry["l-count"] for slug, entry in kadaster.items()
    } == {
        "verified": "7",
        "undeclared-but-verified": "1",
        "declared-only": "0",
        "declared-but-wrong": "0",
        "indeterminate": "0",
        "absent": "0",
        "not-measured": "0",
    }
    for state in verdict_encoding.STATES:
        assert kadaster[state.slug]["l-label"] == state.label

    qlever = legend_cells(page(client_for(store), QLEVER))
    assert {
        slug: entry["l-count"]
        for slug, entry in qlever.items()
        if entry["l-count"] != "0"
    } == {"verified": "5", "indeterminate": "2", "absent": "1"}


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


# ---------------------------------------------------------------------------
# Saying, in the place the run is named, when that run did not finish
# ---------------------------------------------------------------------------
# The four states the page has to distinguish. Three of them are new and the
# fourth is the guarantee that nothing else moved: a run from before stage
# 1c-b4 promised nothing about finishing, so its page must read exactly as it
# did before this stage existed.
CRASHED_SWEEP = "2026-08-23T04:00:00Z"
FINISHED_SWEEP = "2026-08-23T02:00:00Z"

UNFINISHED = "data-run-unfinished"
NEWER_UNFINISHED = "data-newer-run-unfinished"


def test_a_finished_run_says_nothing_about_not_finishing(
    client_for, store_prober_failed
):
    """State one of four: run-prober-failed.nq is a whole run.

    It carries sw:emission, one sw:completedEndpoint and sw:finalised, so
    both sentences would be false and neither may appear. This is also the
    only page state where the newest run in the store is the run being shown,
    which is what stops the second sentence from firing on every finished
    run.
    """
    text = page(client_for(store_prober_failed), KADASTER)
    assert with_attribute(text, UNFINISHED) == []
    assert with_attribute(text, NEWER_UNFINISHED) == []
    assert "did not finish" not in text


def test_an_unfinished_run_says_so_where_it_names_the_run(
    client_for, store_crashed_partway
):
    """State two of four: the facts shown come from a run that did not finish.

    The crashed run wrote kadaster's chunk, so kadaster's page is that run's
    page, and the sentence goes where the page already names the run its
    facts came from. Both halves are asserted: the sentence, verbatim, and
    that the page really is showing the crashed run's facts (availability
    reads "indeterminate" here and "verified" in the 16:00 sweep, and this
    run's own two-class sample replaces that sweep's 59).
    """
    text = page(client_for(store_crashed_partway), KADASTER)

    assert [row["data-run"] for row in with_attribute(text, "data-run")] == [
        RUN + CRASHED_SWEEP
    ]
    assert row_for(text, M + "availability")["data-verdict"] == "indeterminate"
    assert "2 classes sampled" in text

    said = texts_with(text, UNFINISHED)
    assert len(said) == 1, "one sentence, where the run is named"
    assert said[0] == (
        f"The sweep at {CRASHED_SWEEP} did not finish: it recorded that it "
        f"was being written one endpoint at a time and never recorded that "
        f"it was complete. What is above is what it had written for this "
        f"endpoint when it stopped."
    )
    # The other condition is about a sweep this page is NOT showing, and
    # there is no such sweep here: the crashed run is the newest in the store.
    assert with_attribute(text, NEWER_UNFINISHED) == []


def test_an_endpoint_a_crashed_newer_run_never_reached_says_so(
    client_for, store_crashed_partway
):
    """State three of four, and the whole reason the derivation has two
    conditions.

    The crashed run never reached qlever, so it recorded nothing for it and
    the page falls back to the 16:00 sweep. Every verdict shown is that
    sweep's and every one of them is the newest this store has for qlever, so
    the first sentence would be false. What is true, and what a reader
    checking on tonight's sweep needs, is that a later sweep exists, died,
    and never got here.

    Before this stage the prober published prober-failed declines for an
    endpoint its sweep failed on, so this page said "not measured" about
    tonight's run. Nothing says it now unless this sentence does.
    """
    text = page(client_for(store_crashed_partway), QLEVER)

    assert [row["data-run"] for row in with_attribute(text, "data-run")] == [
        RUN + SAMPLING_SWEEP
    ]
    assert row_for(text, M + "availability")["data-verdict"] == "verified"

    said = texts_with(text, NEWER_UNFINISHED)
    assert len(said) == 1, "one sentence, where the run is named"
    assert said[0] == (
        f"A later sweep, at {CRASHED_SWEEP}, did not finish and never "
        f"recorded finishing this endpoint, so nothing above comes from it. "
        f"What is above is the newest this store holds for this endpoint, "
        f"from the sweep at {SAMPLING_SWEEP}."
    )
    # The run being shown is a pre-1c-b4 sweep, which promised nothing about
    # finishing, so the first sentence has no basis and must not appear.
    assert with_attribute(text, UNFINISHED) == []


def test_a_run_from_before_this_stage_reads_exactly_as_it_did(client_for, store):
    """State four of four, and the one that is a guarantee rather than a
    feature.

    run-with-samples.nq carries none of the three facts stage 1c-b4 writes,
    because it was captured before they existed. Absence of sw:finalised is
    therefore not evidence of a crash, and a page that read it as one would
    stamp every historical run in the store as unfinished. The existing
    header sentence is asserted verbatim beside the two absences, so this
    test fails if either new sentence appears OR if the old one changed
    shape.
    """
    text = page(client_for(store), KADASTER)

    assert with_attribute(text, UNFINISHED) == []
    assert with_attribute(text, NEWER_UNFINISHED) == []
    assert "did not finish" not in text
    assert (
        f"Everything below is what one probe sweep observed, at "
        f"{SAMPLING_SWEEP}." in text
    )


def test_two_finished_historical_runs_say_nothing_about_not_finishing(
    client_for, store_later_sample
):
    """The fifth state, which is the fourth one on a store of more than one
    run, and the shape a production store is in most of the time.

    Both sweeps here predate stage 1c-b4 and both finished. The 22:00 one only
    sampled kadaster, so the page falls back to the 16:00 sweep's verdicts and
    the newest run in the store is neither the run being shown, nor finalised,
    nor a run that marked this endpoint. Three of the second sentence's five
    conditions are therefore met, and the page must still say nothing: a
    sentence here would assert a crash on every endpoint whose newest facts
    predate the store's newest run, which is most endpoints most nights.
    """
    text = page(client_for(store_later_sample), KADASTER)

    assert [row["data-run"] for row in with_attribute(text, "data-run")] == [
        RUN + SAMPLING_SWEEP
    ], "the 22:00 sweep measured nothing here, so the verdicts are 16:00's"
    assert with_attribute(text, UNFINISHED) == []
    assert with_attribute(text, NEWER_UNFINISHED) == []
    assert "did not finish" not in text

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
import urllib.parse
from html.parser import HTMLParser
from pathlib import Path

import pytest
from conftest import requires_repo_sources
from starlette.testclient import TestClient

import verdict_encoding
from app import (
    EXPLORE_PATH,
    ABOUT_PATH,
    DOCS_PATH,
    COMPLETE_TEXT,
    INDEX_PATH,
    LINKABLE_SCHEMES,
    STYLESHEET_PATH,
    _outward_link,
    ENDPOINT_PATH,
    TRUNCATED_TEXT,
    _DECLINE_DETAILS,
    _DORMANCY_REASONS,
    _newest_sweep_silence_text,
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

# The synthetic endpoint run-content-profiles.nq and run-sampled-profile.nq
# both describe. Task 8's rail states facts read out of void_summary, and
# void_summary reads a class-profiles pass (web/queries/endpoint_void.rq) --
# no fixture that predates the profile work has one, so this is the only
# endpoint in the committed fixtures whose page can carry both a vocabulary
# section and a rail with more than the two facts (last checked, classes
# sampled) that need no derived VoID document at all.
CONTENT_ENDPOINT = "http://127.0.0.1:9200/sparql"
ENDPOINT_URL = ENDPOINT_PATH + "?url=" + urllib.parse.quote(CONTENT_ENDPOINT, safe="")

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


@requires_repo_sources
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
    every = (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED)
    for state in every:
        assert rules.count("." + verdict_encoding.css_class(state.slug) + " ") == 1
        # The second family, added 2026-09-01 for the matrix header's labels.
        # Generated from the same table for the same reason: a state added with
        # no text colour would render its label in the default ink and silently
        # stop being marked.
        assert rules.count("." + verdict_encoding.text_class(state.slug) + " ") == 1
    assert rules.count("{") == 2 * len(every)


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
        name = selector.strip().lstrip(".")
        # The text family is EXCLUDED, and that is the point rather than a
        # convenience. Text colours are deliberately not injective: verified and
        # undeclared-but-verified are both `good`, absent and not-measured are
        # both `text-dim`. Injectivity is carried by border, fill and weight, so
        # the colour is what those pairs are allowed to share. Folding them in
        # here would report the design as a collision.
        if name.startswith("enc-text-"):
            continue
        bodies[name] = " ".join(rest.rstrip("}").split())
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
        EndpointContent(
            endpoint="https://example.org/sparql",
            # Required since 2026-09-03: the field has no default, so a
            # caller cannot build a sample without saying what it is of.
            metric="urn:sparqlwatch:metric:classes",
            sampled=False,
        ),
    )
    assert sample["present"] is False
    assert sample["classes"] == []
    # The wording stopped naming the classes metric on 2026-09-04, when that
    # metric was retired and the sampling one became class-profiles: this
    # branch is reachable for either, so it names neither.
    assert "no account of looking for one" in sample["this_run_text"]
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
    assert state.meaning == "not measured"
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
    #
    # The page carries ONE script of its own since 2026-09-06, the timeline's,
    # so "no script element at all" stopped being the check. What replaces it is
    # stricter about the thing that matters: the page's own script is counted,
    # so a literal cannot add a second, and its text is checked to be free of
    # anything a literal supplied. A literal that reached inside the script
    # would be inert to this page's markup checks and live to a browser.
    tags = [tag for tag, _ in elements(text)]
    assert "img" not in tags
    assert "<img" not in text

    # The page ships scripts of its own, so "no script element" stopped being
    # the check on 2026-09-06. Asserted on CONTENT instead, which is the thing
    # that matters and does not need revisiting each time the page gains one:
    # no script anywhere holds anything a literal supplied. A literal that
    # reached inside a script would be inert to every markup check here and
    # live to a browser.
    scripts = re.findall(r"<script[^>]*>(.*?)</script>", text, re.S)
    assert scripts, "this test is about the page's scripts; it has none"
    for body in scripts:
        assert "alert(1)" not in body, "a literal reached inside a script"
        assert HOSTILE_VERDICT not in body
        assert HOSTILE_REASON not in body

    # Every `<script` in the document opens one of those, and none was written
    # by a literal.
    assert text.count("<script") == len(scripts)
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
    """The stylesheet the browser applies is the generated one, with no
    private second copy anywhere this page could source rules from.

    A second, hand-written .enc- rule further down the page would win on
    cascade order and quietly redraw a state, which is precisely how two
    copies of this encoding drifted before.

    The rules moved out of this page's own markup on 2026-09-16, the same day
    the index's inline copy went, into the generated stylesheet at
    app.STYLESHEET_PATH (see app._STYLESHEET). This page does not link that
    stylesheet yet -- Task 8 moves it onto base.html -- so between this
    commit and that one its own verdict chips carry no rule at all; what this
    test still holds is that the page prints no PRIVATE second copy of them,
    and that exactly one true copy exists, in the generated sheet.
    """
    client = client_for(store)
    text = page(client, KADASTER)
    css = client.get(STYLESHEET_PATH).text
    for state in (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED):
        selector = "." + verdict_encoding.css_class(state.slug)
        assert selector + " " not in text, f"{selector} is a private copy on the page"
        assert css.count(selector + " ") == 1
    assert verdict_encoding.css_rules() not in text
    assert verdict_encoding.css_rules() in css


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


# ---------------------------------------------------------------------------
# The newest sweep did not ask this endpoint
# ---------------------------------------------------------------------------
# An absent qualifier on this page is a POSITIVE CLAIM: the header sentence
# dates every verdict below to one sweep, and saying nothing else asserts that
# the newest sweep in the store is that sweep. For an endpoint the newest sweep
# declined to ask, that is false, and the store holds the reason.
#
# DORMANCY IS NOT A VERDICT and nothing below lets it become one. The six
# verdict states and the not-measured state are the closed vocabulary of the
# chips; what these tests read is a sentence about the age of the verdicts the
# page already draws.
DECLINING_SWEEP_INSTANT = "2026-08-27T10:00:00Z"
REGISTRY_SWEEP_INSTANT = "2026-08-24T19:45:03Z"
FAILED_SWEEP_INSTANT = "2026-08-23T02:00:00Z"

SILENT = "data-newest-sweep-silent"
DORMANT = "data-newest-sweep-dormant"
CRASH = "data-newer-run-unfinished"


def test_the_endpoint_page_says_the_newest_sweep_did_not_ask(
    client_for, store_dormant_newest
):
    """store_dormant_newest's kadaster, which is the whole point of the task.

    The newest sweep finished, measured the other two endpoints of the trio
    and published one dormancy group naming this one. So the page carries one
    sentence, and it has to name three things: the sweep that did not ask, the
    reason it gives, and the sweep the verdicts above actually come from. It
    must NOT carry the crash sentence, which is about a sweep that stopped.
    """
    text = page(client_for(store_dormant_newest), KADASTER)

    said = texts_with(text, SILENT)
    assert len(said) == 1, "one sentence, in the element contracted to carry it"
    assert DECLINING_SWEEP_INSTANT in said[0], "name the sweep that did not ask"
    assert SAMPLING_SWEEP in said[0], "and the sweep the verdicts come from"
    assert "dormant" in said[0]
    assert "operator-hold" in said[0]

    marked = with_attribute(text, SILENT)[0]
    assert marked[DORMANT] == "true"
    assert marked["data-dormancy-reason"] == "operator-hold"
    assert texts_with(text, CRASH) == [], (
        "that sweep finished, so nothing about it stopped partway"
    )


def test_the_endpoint_page_links_to_the_docs_section(client_for, store):
    """The header's link became the docs tab on 2026-08-28.

    A dormant endpoint's page still links straight to /about inside the sentence
    that explains the marker, because the reader that sentence is written for
    came for that page rather than for a section, and making them find it
    through an index would be worse for exactly the reader who needs it most.
    """
    text = page(client_for(store), KADASTER)
    nav = with_attribute(text, "data-nav")
    # base.html's four-item nav replaced this page's own two-link header on
    # 2026-09-16, the same shell / , /explore and /docs already carry (see
    # test_index.py and test_explore.py's identical assertion). Still an exact
    # list: the header is small on purpose, and a link arriving in it without
    # a test changing is how a nav becomes a menu.
    assert [a["href"] for a in nav] == [INDEX_PATH, EXPLORE_PATH, DOCS_PATH, ABOUT_PATH]


def test_the_dormancy_sentence_is_not_drawn_as_a_verdict(
    client_for, store_dormant_newest
):
    """The closed vocabulary, pinned on the page that could break it.

    Dormancy carries no chip, no metric row and no legend entry: it is not a
    finding about the endpoint, and drawing it beside the verdicts would put
    an eighth state into an encoding whose whole value is that it has seven.
    The verdicts the 16:00 sweep did record are still drawn, unchanged.
    """
    text = page(client_for(store_dormant_newest), KADASTER)

    # BOTH spellings, and neither one alone. The vocabulary uses both,
    # sw:dormantEndpoint and sw:dormancyReason, so an eighth state could be
    # slugged either way, and neither word contains the other: "dormant" does
    # not appear in "dormancy". This assertion has been wrong in each direction
    # once, which is why it now names them both instead of picking the one that
    # looks more likely.
    for word in ("dormant", "dormancy"):
        assert word not in str(with_attribute(text, "data-metric"))
        assert word not in str(with_attribute(text, "data-state"))
    assert [row["data-metric"] for row, _ in chips(text)], (
        "the page must still draw the verdicts it has"
    )
    assert row_for(text, M + "availability")["data-verdict"] == "verified"


def test_a_crashed_declining_sweep_makes_no_crash_claim(
    client_for, store_dormancy_then_crash
):
    """The live defect this task fixes, end to end.

    The newest run here published its dormancy list and was then killed, so
    every conjunct of newer_run_did_not_reach_this_endpoint held and the page
    printed "A later sweep did not finish and never recorded finishing this
    endpoint" over an endpoint that sweep deliberately declined to ask, with
    the true reason bound in the same row and unused.
    """
    text = page(client_for(store_dormancy_then_crash), KADASTER)

    assert texts_with(text, CRASH) == [], (
        "the sweep never intended to reach this endpoint, so it did not stop "
        "short of it"
    )
    said = texts_with(text, SILENT)
    assert len(said) == 1
    assert DECLINING_SWEEP_INSTANT in said[0]
    assert "operator-hold" in said[0]
    assert SAMPLING_SWEEP in said[0]


def test_the_calibration_shape_qualifies_every_page_the_narrower_run_skipped(
    client_for, store_registry_and_failure
):
    """A finished, newer, NARROWER run, and no dormancy anywhere.

    This is the defect on the deployed store: a 54-endpoint calibration run
    minutes newer than the 543-endpoint sweep leaves 489 pages whose newest
    sweep recorded nothing for them, and every one of those pages said
    nothing at all about it. store_registry_and_failure is that shape at
    fixture size, the registry sweep being a nine-endpoint cut that never
    mentions kadaster.

    Nothing in the store says WHY, so the sentence must not guess: no
    dormancy attribute and no reason.
    """
    text = page(client_for(store_registry_and_failure), KADASTER)

    said = texts_with(text, SILENT)
    assert len(said) == 1
    assert REGISTRY_SWEEP_INSTANT in said[0]
    assert FAILED_SWEEP_INSTANT in said[0], "and the sweep the facts come from"
    assert "dormant" not in said[0]
    marked = with_attribute(text, SILENT)[0]
    assert DORMANT not in marked
    assert "data-dormancy-reason" not in marked
    assert texts_with(text, CRASH) == []


def test_a_page_no_later_sweep_passed_over_says_none_of_this(client_for, store):
    """The control. One sweep, and it is every endpoint's own run, so there is
    no later sweep to qualify anything and the page says nothing about one.

    Without this the tests above would pass over a page that printed the
    sentence unconditionally.
    """
    text = page(client_for(store), KADASTER)
    assert with_attribute(text, SILENT) == []
    assert with_attribute(text, CRASH) == []


def _silence_text(**overrides):
    """The sentence for one EndpointMeasurements, asked of app.py directly.

    The two cases below have no committed fixture: every dormancy group the
    prober writes carries a reason, and the reason it carries is one of
    prober/src/dormancy.rs's two slugs. Inventing fixtures for them would pin
    a run file no prober emits, which is the same call
    test_each_decline_reason_gets_its_own_detail makes.
    """
    facts = dict(
        endpoint="https://example.org/sparql",
        assessed=True,
        run="urn:sparqlwatch:run:2026-08-22T16:00:00Z",
        generated_at="2026-08-22T16:00:00Z",
        newest_run="urn:sparqlwatch:run:2026-08-27T10:00:00Z",
        newest_generated_at=DECLINING_SWEEP_INSTANT,
        newest_emission="incremental",
        newest_finalised=True,
        newest_declared_this_endpoint_dormant=True,
        newest_dormancy_reason="operator-hold",
    )
    facts.update(overrides)
    return _newest_sweep_silence_text(EndpointMeasurements(**facts))


def test_each_dormancy_reason_gets_its_own_sentence():
    """Every slug prober/src/dormancy.rs::SkipReason emits says something
    different, and no two of them share a sentence.

    "automatic" is this service's cost policy relegating an endpoint that proved
    expensive and silent; "operator-hold" is a person; "not-in-this-sweep" is
    neither, and is not a relegation at all. Reporting any of them as another
    tells a reader that somebody or something did what it did not: the machine
    did a person's work, or a decision was taken about their server that nobody
    took.

    Read off _DORMANCY_REASONS rather than listed here, so that a fourth slug
    fails this test rather than passing it three out of four.
    test_about.py's test_the_dormancy_reasons_the_pages_read_are_the_probers_own
    holds that map's keys to the Rust match arms.
    """
    said = {
        slug: _silence_text(newest_dormancy_reason=slug)
        for slug in _DORMANCY_REASONS
    }
    for slug, sentence in said.items():
        assert slug in sentence, f"{slug} is not named in its own sentence"
        for other in said:
            if other != slug:
                assert other not in sentence, f"{slug}'s sentence names {other}"
    assert len(set(said.values())) == len(said), "two reasons share a sentence"


def test_it_says_so_without_a_reason_when_no_reason_is_bound():
    """A declaration with no sw:dormancyReason beside it.

    The declaration is the fact that matters: the sweep said it declined to
    ask. So the sentence is still made, and it says the reason is not
    recorded rather than borrowing either of the two above.
    """
    text = _silence_text(newest_dormancy_reason=None)
    assert "dormant" in text
    assert DECLINING_SWEEP_INSTANT in text
    assert "no reason" in text
    assert "automatic" not in text and "operator-hold" not in text


def test_an_unrecognised_dormancy_reason_is_shown_verbatim():
    """A reason from a prober newer than this page.

    The same answer as an unrecognised verdict and an unrecognised decline
    reason: claim nothing about what it means, and carry the value the store
    holds so a reader can see which value that is. A reason has been added to
    a closed set in this project once already.
    """
    text = _silence_text(newest_dormancy_reason="hibernating-2027")
    assert "hibernating-2027" in text
    assert "automatic" not in text and "operator-hold" not in text


def test_a_dormancy_with_no_newer_sweep_to_attribute_it_to_says_nothing():
    """The sentence is about a newer sweep, so it needs one.

    Both of the property's own run guards, read through the sentence: an
    absent newest run and a newest run that is this endpoint's own run each
    leave nothing to say, and a sentence that fired anyway would name a sweep
    the store does not hold or invent a second one. WHICH run's declaration is
    read is a different question and
    test_only_the_newest_runs_declaration_is_read owns it.
    """
    assert _silence_text(newest_run=None, newest_generated_at=None) is None
    assert (
        _silence_text(newest_run="urn:sparqlwatch:run:2026-08-22T16:00:00Z")
        is None
    )

# ---------------------------------------------------------------------------
# The way home, and the way out
# ---------------------------------------------------------------------------


def test_the_logo_leads_home(client_for, store):
    """Every page's logo is a link to the index.

    A reader who lands on one endpoint from a search engine had no way back
    except the browser's own button until 2026-08-28.
    """
    text = page(client_for(store), KADASTER)
    logos = [a for a in with_attribute(text, "class") if a["class"] == "logo"]
    assert len(logos) == 1
    assert logos[0]["href"] == INDEX_PATH


def test_the_outward_link_opens_the_endpoint_in_a_new_tab(client_for, store):
    """And carries both halves of rel, for two different reasons.

    `noopener` is the security one. `noreferrer` is the courtesy one: without it
    the operator of that endpoint reads this page's url in their referer log, and
    somebody looking up their own server should not have to announce that they
    read us first.
    """
    text = page(client_for(store), KADASTER)
    links = with_attribute(text, "data-outward-link")
    assert len(links) == 1
    assert links[0]["href"] == KADASTER
    assert links[0]["target"] == "_blank"
    assert set(links[0]["rel"].split()) == {"noopener", "noreferrer"}


def test_the_outward_link_says_a_plain_visit_sends_no_query(client_for, store):
    """Because it does not, and the difference matters to a reader.

    A GET with no query is the request `declare.rs` makes, and what comes back is
    a form, an error, or a service description. A reader expecting results and
    meeting an error page would read that as the endpoint being broken, which is
    a conclusion this page has metrics for and this link does not support.
    """
    text = page(client_for(store), KADASTER)
    assert "sends no query" in text
    assert "a form, an" in text and "error, or a description of itself" in text


def test_only_a_scheme_a_browser_should_follow_becomes_a_link():
    """The guard that keeps a deferred allowlist from becoming a hole.

    Autoescaping does not help: `javascript:alert(1)` holds no character an HTML
    escaper touches, so it reaches the attribute unchanged and a browser runs it
    on click. The spec records that the scheme allowlist was found unnecessary
    FOR THE 2026-06-15 DUMP and explicitly not retired in general, stage 5
    accepts public submissions, and `load_run` loads whatever run file it is
    given. This link is the surface that would have paid for that.
    """
    assert _outward_link("https://a.example/sparql") == "https://a.example/sparql"
    assert _outward_link("http://a.example/sparql") == "http://a.example/sparql"
    assert _outward_link("HTTPS://A.example/sparql") == "HTTPS://A.example/sparql"
    for hostile in (
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "vbscript:msgbox(1)",
        "file:///etc/passwd",
        "urn:sparqlwatch:not-a-url",
    ):
        assert _outward_link(hostile) is None, hostile


def test_an_endpoint_with_an_unlinkable_scheme_keeps_its_page(store_hostile_literals):
    """No link, and everything else intact.

    Saying nothing is the right answer: this service has no opinion on such a
    url, and the page prints it in full as text where a reader can see it.
    """
    assert LINKABLE_SCHEMES == ("http://", "https://")


@requires_repo_sources
def test_every_decline_reason_the_prober_can_write_has_a_sentence():
    """The pin this table did not have, and the drift it just failed to catch.

    _DECLINE_DETAILS is keyed on NotMeasuredReason::slug, and nothing tied the
    two together: the enumeration-failed variant was added to the prober and
    the page said "unrecognised reason" for it, which is the fallback doing its
    job while the reader learns nothing. The fallback is still right for a
    store written by a NEWER prober than this page, which is why it stays; it
    is wrong as a description of a reason shipping in this same commit.

    Read out of slug()'s own match arms rather than the enum's variants,
    because the slug is the string the graph carries and the variant name is
    not.
    """
    source = (Path(__file__).resolve().parents[2] / "prober" / "src" / "emit.rs").read_text()
    body = source.split("pub fn slug(&self) -> &'static str {", 1)
    assert len(body) == 2, "NotMeasuredReason::slug moved; this pin needs its new shape"
    slugs = set(re.findall(r'NotMeasuredReason::\w+ => "([^"]+)"', body[1]))
    assert slugs, "no slug arms found, so this test would pass vacuously"
    missing = slugs - set(_DECLINE_DETAILS)
    assert not missing, (
        f"the prober can write {sorted(missing)} and this page has no sentence "
        f"for it, so a declined row would read 'unrecognised reason'"
    )


def test_a_run_declining_the_profile_pass_reports_that_decline():
    """The retirement's live case, and the one that was quietly wrong.

    Since 2026-09-04 the metric a cheap sweep declines is class-profiles, not
    the retired classes. _declined_classes keyed on the retired id, so a run
    that HAD declined the sampling metric and recorded why fell through to "this
    run recorded no measurement either" -- the sentence reserved for a run that
    never looked at all. Every committed fixture predates the retirement and
    declines the old id, so no existing test could see this.
    """
    declined_the_pass = EndpointMeasurements(
        endpoint="https://example.org/sparql",
        assessed=True,
        run="urn:sparqlwatch:run:x",
        generated_at="2026-09-04T16:00:00Z",
        verdicts=[MetricVerdict(metric=M + "availability", verdict="verified")],
        declined=[
            DeclinedMetric(metric=M + "class-profiles", reason="cost-ceiling")
        ],
    )
    sample = _sample(
        declined_the_pass,
        EndpointContent(
            endpoint="https://example.org/sparql",
            metric=M + "class-profiles",
            sampled=False,
        ),
    )
    assert sample["present"] is False
    assert "cost-ceiling" in sample["this_run_text"], (
        "the run said why it did not look, so the page must say so: "
        f"{sample['this_run_text']!r}"
    )
    assert "no measurement of the classes metric" not in sample["this_run_text"], (
        "that sentence is for a run that never looked at all"
    )


# ---------------------------------------------------------------------------
# The tab icon
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("path", ["/", "/about", "/docs", "/explore"])
def test_every_page_carries_the_tab_icon(client_for, store_content_profiles, path):
    """A pinned tab shows an icon or it shows nothing, per page.

    Parameterised over the routes rather than asserted on one, because the
    templates have no shared head: each declares its own, so an icon added to
    six of seven is a page that silently has none. /explore was exactly that
    when this was written, and worse, it had no <head> AT ALL.
    """
    body = client_for(store_content_profiles).get(path).text
    assert '<link rel="icon" href="/icon.svg"' in body, f"{path} has no tab icon"
    assert 'rel="mask-icon"' in body, f"{path} has no Safari pinned-tab mask"


@pytest.mark.parametrize("path", ["/", "/about", "/docs", "/explore"])
def test_every_page_is_a_document_and_says_what_it_is(client_for, store_content_profiles, path):
    """The other half of what a pinned tab needs: a name.

    /explore served a bare fragment starting at <style>, with no doctype, no
    <head>, no charset and no <title>, so a browser rendered it in quirks mode
    and the tab read as a URL. Found while adding the icon, since a page with no
    head has nowhere to put one.
    """
    body = client_for(store_content_profiles).get(path).text
    assert body.lstrip().lower().startswith("<!doctype html>"), f"{path} is not a document"
    assert "<title>" in body, f"{path} has no title, so its tab has no name"
    assert 'charset="utf-8"' in body, f"{path} declares no encoding"


def test_the_icon_is_served_and_is_an_svg(client_for, store_content_profiles):
    """The link points somewhere real, in both spellings, plus the /favicon.ico
    a browser asks for whether or not the head mentions it."""
    client = client_for(store_content_profiles)
    for path in ("/icon.svg", "/icon-mono.svg", "/favicon.ico"):
        r = client.get(path)
        assert r.status_code == 200, f"{path} is {r.status_code}"
        assert r.headers["content-type"].startswith("image/svg+xml"), path
        assert r.text.lstrip().startswith("<svg"), path


def test_the_pinned_tab_mask_is_monochrome(client_for, store_content_profiles):
    """Safari paints the mask itself with the colour on the <link>, so a mask
    carrying its own colours renders as a solid blob."""
    body = client_for(store_content_profiles).get("/icon-mono.svg").text
    assert "#81d4fa" not in body, "the mask must not carry the brand colour"
    assert "<rect" not in body, "a background rect would mask the whole square"


# ---------------------------------------------------------------------------
# A counting metric shows its numbers
# ---------------------------------------------------------------------------


def _counting_verdict(**kw):
    from endpoint_measurements import MetricVerdict

    base = dict(metric=M + "triple-count", verdict="verified")
    base.update(kw)
    return MetricVerdict(**base)


def test_a_counting_row_states_both_numbers():
    """The verdict is a grade of a claim and never the claim.

    `verified` says a description was right without saying what it said, and a
    reader asking how big an endpoint is wants the number. The prober publishes
    both beside the verdict, and this page did not read them until 2026-09-05.
    """
    from app import _detail

    d = _detail(_counting_verdict(declared_count=1000000, observed_count=1020000), True)
    assert d == "declares 1,000,000, counted 1,020,000", d


def test_a_count_with_no_claim_says_so_rather_than_showing_a_zero():
    """`undeclared-but-verified` on a count means the endpoint holds this much
    and says nothing. A zero in the declared slot would be a claim it made."""
    from app import _detail

    d = _detail(
        _counting_verdict(verdict="undeclared-but-verified", observed_count=12510784),
        True,
    )
    assert d == "counted 12,510,784, declared nothing", d


def test_a_claim_we_could_not_check_is_stated_as_a_claim():
    """`declared-only`: the number is the endpoint's word, not our finding, and
    the wording must not present it as measured."""
    from app import _detail

    d = _detail(_counting_verdict(verdict="declared-only", declared_count=500), True)
    assert d == "declares 500, not counted", d


def test_a_metric_with_no_counts_carries_no_count_clause():
    """Every other metric. A clause about numbers on an availability row would
    be a sentence about nothing."""
    from app import _detail

    assert _detail(_counting_verdict(metric=M + "availability"), True) is None


def test_a_conformance_level_still_wins_the_clause():
    """service-description carries a level and no counts, and the level is what
    its row has always said. Pinned so adding the counts did not displace it."""
    from app import _detail

    d = _detail(_counting_verdict(metric=M + "service-description", level=3), True)
    assert d == "conformance level 3", d


# ---------------------------------------------------------------------------
# The timeline
# ---------------------------------------------------------------------------


def test_a_single_run_store_draws_no_timeline(client_for, store):
    """One run is not a history, and a one-cell timeline would imply a trend
    from one observation. Every committed fixture is one or two runs, so this is
    the state most of this suite is in."""
    body = page(client_for(store), KADASTER)
    assert 'data-section="history"' not in body, "a single run must draw no timeline"


def test_the_timeline_draws_in_the_sites_own_encoding(client_for, store_two_sweeps):
    """A reading in a timeline is the same fact as a reading in a cell. A second
    visual language for it would be a second thing to learn, and /docs/states
    would stop describing half of them."""
    import verdict_encoding

    body = page(client_for(store_two_sweeps), KADASTER)
    assert 'data-section="history"' in body, "two runs is a history"
    drawn = set(re.findall(r'class="hcell (enc-[a-z-]+)"', body))
    assert drawn, "the timeline draws cells"
    known = {"enc-" + s.slug for s in verdict_encoding.STATES}
    known.add("enc-" + verdict_encoding.NOT_MEASURED)
    assert drawn <= known, f"{drawn - known} is not a state this site defines"


def test_every_timeline_cell_names_its_run_and_reading(client_for, store_two_sweeps):
    """A row of coloured boxes is unreadable without them: the encoding says
    WHAT was read and only the tooltip says when, and which."""
    body = page(client_for(store_two_sweeps), KADASTER)
    # Matched across the whole tag rather than requiring `title` to follow
    # `class`: attribute ORDER is not a contract, and the first version of this
    # broke when the cell gained data-at and data-reading between the two.
    cells = [
        re.search(r'title="([^"]*)"', tag).group(1)
        for tag in re.findall(r'<span class="hcell[^>]*>', body)
        if 'title="' in tag
    ]
    assert cells, "the timeline draws cells"
    for title in cells:
        assert re.match(r"^\d{4}-\d{2}-\d{2}T", title), f"no run instant in {title!r}"
        assert ": " in title, f"no reading in {title!r}"


def test_a_run_that_said_nothing_is_a_gap_and_says_so(client_for, store_two_sweeps):
    """The reading a gap must carry, in words, because "this run recorded
    nothing" is not a verdict about the endpoint and an empty box in a row of
    boxes reads as one."""
    body = page(client_for(store_two_sweeps), KADASTER)
    gaps = re.findall(r'class="hcell hgap" title="([^"]*)"', body)
    for title in gaps:
        assert "recorded nothing" in title, title


def test_the_endpoint_page_lists_metrics_in_the_index_order(client_for, store):
    """One order for both pages.

    They disagreed until 2026-09-05: the index drew columns in reading order
    and this page sorted by metric id, so a reader moving between them had to
    find each metric twice.
    """
    from app import _column_rank, _index_metrics, _rows
    from endpoint_measurements import endpoint_measurements

    m = endpoint_measurements(store, KADASTER)
    page_order = [r["metric"] for r in _rows(m)]
    assert page_order == sorted(page_order, key=_column_rank)


def test_a_metric_with_no_verdict_gets_no_row_either(client_for, store_declined):
    """The index drops those columns; this page dropped them too on 2026-09-05,
    found by reading a real timeline.

    A class-profiles row drew `not measured` on every day its pass was declined
    and a GAP on the day it SUCCEEDED, because a successful pass publishes
    neither a measurement nor a decline. A gap means "this run recorded
    nothing", so the one day the pass worked was the one day the row looked
    like nothing had happened.

    Nothing is lost: the class sample section states this run's account of the
    pass in words, and the `content` link says whether a profile exists.
    """
    from app import METRIC_DOCS, _rows
    from endpoint_measurements import endpoint_measurements

    verdictless = {
        "urn:sparqlwatch:metric:" + name
        for name, facts in METRIC_DOCS.items()
        if not facts.get("yields_measurement", True)
    }
    assert verdictless, "METRIC_DOCS must mark at least one metric as verdictless"
    for endpoint in (KADASTER, QLEVER):
        rows = _rows(endpoint_measurements(store_declined, endpoint))
        drawn = {r["metric"] for r in rows}
        assert not (drawn & verdictless), f"{drawn & verdictless} has no verdict to draw"


# ---------------------------------------------------------------------------
# The contract the page's scripts read
# ---------------------------------------------------------------------------
#
# There is no harness in this project that drives inline script, so what these
# pin is the half that CAN be checked: the attributes the scripts read, written
# by the template. A script reading an attribute the template stopped writing
# fails silently in a browser and passes every test here, so the attributes are
# asserted by name.


def test_every_timeline_cell_carries_the_column_and_instant_the_script_reads(
    client_for, store_two_sweeps
):
    body = page(client_for(store_two_sweeps), KADASTER)
    cells = re.findall(r"<span class=\"hcell[^>]*>", body)
    assert cells, "the timeline draws cells"
    for tag in cells:
        assert 'data-at="' in tag, tag
        assert 'data-reading="' in tag, tag
    # The crosshair groups by column, so every cell and header needs one.
    assert re.search(r'<td data-col="\d+">', body), "cells carry their column"
    assert re.search(r'class="h-run" data-col="\d+"', body), "headers carry theirs"
    assert 'id="h-readout"' in body, "the readout the script writes into"
    assert 'data-empty="' in body, "and what it says before anything is pointed at"


def test_every_vocabulary_row_carries_a_prebuilt_haystack(
    client_for, store_content_profiles
):
    """The search reads `data-hay` rather than the row's text, so a keystroke
    does not walk the DOM for every row. It has to contain what a reader would
    type: the local name, the prefix, and the IRI, lowercased."""
    body = page(client_for(store_content_profiles), "http://127.0.0.1:9200/sparql")
    rows = re.findall(r"<li data-kind=\"[^\"]*\"[^>]*>", body)
    assert rows, "the panel lists terms"
    for tag in rows:
        hay = re.search(r'data-hay="([^"]*)"', tag)
        assert hay, tag
        assert hay.group(1) == hay.group(1).lower(), "the needle is lowercased too"
    total = re.search(r'class="vocab-count"[^>]*data-total="(\d+)"', body)
    assert total, "the count carries its denominator for the script to restore"
    assert int(total.group(1)) == len(rows), "and it is the number of rows"


def test_the_vocabulary_states_are_the_sites_own(client_for, store_content_profiles):
    """A term's state IS a verdict here, drawn in the same encoding as every
    other reading on the site."""
    import verdict_encoding

    body = page(client_for(store_content_profiles), "http://127.0.0.1:9200/sparql")
    drawn = set(re.findall(r'class="v-chip (enc-[a-z-]+)"', body))
    assert drawn, "the panel draws chips"
    known = {"enc-" + s.slug for s in verdict_encoding.STATES}
    assert drawn <= known, f"{drawn - known} is not a state this site defines"


def test_an_endpoint_with_no_profile_gets_no_vocabulary_panel(client_for, store):
    """An empty searchable list would say this endpoint has no vocabulary, when
    what happened is that nobody profiled it."""
    body = page(client_for(store), KADASTER)
    assert 'data-section="vocabulary"' not in body


def test_every_page_footer_shows_the_version_the_prober_sends(client_for, store):
    """The footer's number is the number in the User-Agent, on every page.

    A version in a footer is only worth putting there if it IS the version. A
    reader matching a verdict to the build that produced it, or an operator
    matching a line in their logs to this service, is misled by a stale one
    rather than merely unhelped.

    `app.VERSION` is read out of PROBER_USER_AGENT, and
    test_about.py::test_the_user_agent_shown_is_the_one_the_prober_sends
    already pins that string to prober/Cargo.toml. Asserting the footer carries
    `app.VERSION` therefore closes the chain: a version bump that forgets a
    file reds one of the two.
    """
    import urllib.parse

    import app as app_module

    version = app_module.VERSION
    assert version and version[0].isdigit(), f"VERSION looks unparsed: {version!r}"

    client = client_for(store)
    endpoint = urllib.parse.quote(KADASTER, safe="")
    for path in (
        "/",
        "/about",
        "/docs",
        "/docs/metrics",
        "/docs/states",
        "/explore",
        f"/endpoint?url={endpoint}",
    ):
        body = client.get(path, headers={"accept": "text/html"}).text
        assert '<footer class="site-foot">' in body, f"{path} has no footer"
        assert version in body, f"{path} does not show version {version}"


# ---------------------------------------------------------------------------
# Task 8: the column and the rail
# ---------------------------------------------------------------------------
#
# The six sections stay six sections; what moves is the legend (into a rail,
# beside the marks it explains) and a handful of identity facts (out of prose
# and into it). See templates/endpoint.html's own header comment.


def test_the_page_keeps_all_six_sections(client_for, store_content_profiles):
    """Converting the page to a column and a rail must not fold any section
    into another.

    Deliberately run against store_content_profiles rather than the plain
    `store` fixture: `store`'s only endpoints (kadaster, qlever) carry no
    content profile, so their pages never render a vocabulary section at all
    -- asserting "vocabulary" present against one of them would not exercise
    the thing this test exists to pin.
    """
    body = client_for(store_content_profiles).get(
        ENDPOINT_URL, headers={"accept": "text/html"}
    ).text
    sections = [s["data-section"] for s in with_attribute(body, "data-section")]
    assert "vocabulary" in sections
    assert "sample" in sections, (
        "the classes sample reports a different metric from the vocabulary and "
        "renders for endpoints that have no vocabulary at all; it keeps its own "
        "section"
    )


def test_the_sample_survives_an_endpoint_with_no_vocabulary(client_for, store):
    """The regression this task exists to avoid.

    Folding the sample into a section wrapped in {% if vocabulary %} would make
    it vanish for every endpoint without a content profile.

    NOT store_sampled_profile: that fixture's endpoint carries a class-profiles
    pass, so its page DOES render a vocabulary section (data-section =
    conformance, sample, vocabulary, void) -- against it, folding the sample
    into {% if vocabulary %} would still show a sample, and this test would
    keep passing while missing the exact regression it is written to catch.
    `store`'s kadaster has no content profile at all (data-section =
    conformance, sample only), which is the shape this guard needs, so the
    premise -- no vocabulary section here -- is asserted as well: a fixture
    swap that quietly grew a vocabulary section would disarm the guard again
    without anything here going red.
    """
    body = page(client_for(store), KADASTER)
    assert 'data-section="vocabulary"' not in body, (
        "this test's premise: the endpoint must have no vocabulary section, "
        "or it cannot tell folding-into-vocabulary apart from working code"
    )
    assert 'data-sample="present"' in body or 'data-sample="absent"' in body


def test_the_legend_sits_beside_the_marks(client_for, store_content_profiles):
    """The legend moved into the rail so it is adjacent to what it explains,
    and the rail states identity facts already in this page's context.

    Also run against store_content_profiles rather than plain `store`: the
    rail's void-derived facts (classes described/reported, whether the
    description is provably complete, the sampling ladder) all come from
    void_summary, which is None for every fixture with no class-profiles
    pass -- including `store`'s kadaster and qlever. Against those, the rail
    can only ever carry two facts (last checked, classes sampled), which is
    a true statement about this page's design but does not exercise the
    >= 3 this test pins.
    """
    body = client_for(store_content_profiles).get(
        ENDPOINT_URL, headers={"accept": "text/html"}
    ).text
    assert 'data-rail="legend"' in body, (
        "the legend moved into the rail so it is adjacent to what it explains"
    )
    assert len(with_attribute(body, "data-rail-fact")) >= 3, (
        "the rail states what the endpoint is, from facts the context already "
        "holds -- classes described and reported, whether the description is "
        "provably complete, when it was last checked"
    )


def test_the_endpoint_page_carries_no_stylesheet_of_its_own(client_for, store):
    """The regression Task 8 closes: between Task 5 and this commit this page
    kept its own dark-only :root with no --fill, so every .enc- chip on it
    drew with no fill and no border. Pinned the way test_docs.py and
    test_explore.py already pin it for their pages: no inline token
    declaration, and the one generated stylesheet linked."""
    text = page(client_for(store), KADASTER)
    assert "--accent:" not in text, (
        "design tokens belong in web/static/site.css, not in this template"
    )
    assert STYLESHEET_PATH in text, "the page must link the one stylesheet"

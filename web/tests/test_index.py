"""The index: every endpoint this service knows, on one page.

Until this stage the service had exactly one route, and reaching it meant
knowing an endpoint URL and percent-encoding it by hand. This page is the way
in, and the whole difficulty of it is honesty at scale: 543 rows is 543
chances to state something the store does not hold.

Three claims are asserted here rather than reasoned about, because each of
them is a mistake this project has already made or has already written down as
the mistake to avoid.

The first is that every chip is the verdict the store holds for that endpoint
and that metric. The measured registry sweep is 57 endpoints whose
availability reads "verified", 482 "indeterminate" and 4 "absent", and
"absent" is common on other metrics of endpoints that never answered at all
(cors 19, cors-preflight 78, geo-data 52, service-description 9). So a row is
routinely a MIX of indeterminate and absent chips, and any substitute value,
any collapsing of the two, or any drawing of a verdict as a boolean would pass
a test that only counted chips. These tests read the page's chips back and
compare them, one by one, against a query of the same store.

The second is the grouping. The groups are the availability verdict's own
values, one per value present, in the encoding table's order, plus one final
group for endpoints whose newest run recorded no availability verdict at all.
A heading like "did not answer" over the last three would be a determined
negative built out of two facts that are not one: "absent" means the host
answered with something that was not a SPARQL result (one of the four is a
.ttl file on raw.githubusercontent.com), while "indeterminate" covers a
timeout, a transport error, a DNS failure and an HTML front end.

The third is that a row whose facts are not current says so. The endpoint page
already says when the run it shows did not finish and when a newer unfinished
run never reached that endpoint; an index of 543 rows of those same facts
needs the same qualification, or it presents them as current.

Every value asserted here is a real value of the committed fixtures under
web/tests/fixtures/; see each fixture's header comment for its provenance.
"""

import re
from pathlib import Path
from html.parser import HTMLParser

import pytest
from pyoxigraph import NamedNode, RdfFormat, Store, parse
from starlette.testclient import TestClient

import verdict_encoding
from endpoint_measurements import endpoint_measurements, EndpointMeasurements, MetricVerdict
import app as app_module
from app import (
    _index_metrics,
    _index_rows,
    _metric_state_matrix,
    EXPLORE_PATH,
    ABOUT_PATH,
    DOCS_PATH,
    METRIC_DESCRIPTIONS,
    DORMANCY,
    _index_html,
    _legend,
    _matches_facet,
    _matches_query,
    _state_facets,
    ENDPOINT_PATH,
    INDEX_PATH,
    ROW_DORMANT_TEXT,
    _ROW_DORMANCY_REASONS,
    _row_dormancy_text,
    app,
    get_store,
)

# The generic HTML readers, imported from the endpoint page's tests rather than
# written twice. They depend on nothing about that page: ``with_attribute``
# returns the attributes of every element carrying a name, and ``texts_with``
# the text of each element carrying one. A second copy here would be a second
# thing to keep right.
from test_page import texts_with, with_attribute

DQV = "http://www.w3.org/ns/dqv#"
M = "urn:sparqlwatch:metric:"

# The nine endpoints of run-registry-sample.nq. Named here so that a test
# mentioning one says which it is without the reader opening the fixture; the
# fixture's header says what each one's verdicts are and why it was chosen.
FOODIE = "https://www.foodie-cloud.org/sparql"
UNIPROT = "https://sparql.uniprot.org/sparql"
EPO = "https://data.epo.org/linked-data/query"
VISUALDATAWEB = "http://visualdataweb.infor.uva.es/sparql"
ASCDC = "https://data.ascdc.tw/en/sparql.php"
DBPEDIA_PAGE = "https://dbpedia.org/page/Summer_Olympic_Games"
CALIGRAPH = "http://caligraph.org/sparql"
WHONTO = "https://purl.org/whonto/onto"
BOOKKEEPING = (
    "https://raw.githubusercontent.com/GVogeler/bookkeeping/master/"
    "bookkeeping.ttl"
)

# The count the fixture actually has, which is not the sweep's 543: this file
# is nine of that sweep's endpoints, cut from it line by line.
REGISTRY_SAMPLE_ENDPOINTS = 9

# run-prober-failed.nq's one endpoint, which has no availability verdict at
# all: every metric it applies was declined.
KADASTER = "https://data.kkg.kadaster.nl/query"

# Two of the three endpoints of run-with-samples.nq. Through
# store_crashed_partway, kadaster's facts come from a run that did not finish
# and these two are endpoints that run never reached.
QLEVER = "https://qlever.dev/api/osm-planet"
ONTOP = "https://ontop.certain.ai.ustp.at/sparql"

# The instant of the run that died partway, off run-crashed-partway.nq. It is
# the newest activity in store_crashed_partway, so it is the sweep the store
# level note is about.
CRASHED_SWEEP = "2026-08-23T04:00:00Z"

# run-hostile-literals.nq's one endpoint and its dqv:value, verbatim. The value
# opens with a double quote to close whatever attribute it lands in, then opens
# an element.
HOSTILE = "https://hostile-literals.example/sparql"
HOSTILE_VERDICT = '"><script>alert(1)</script>'

# run-classes-absent.nq's one endpoint. Its run recorded two metrics and not
# eight, which is what makes it the fixture that catches a hard-coded metric
# list.
NO_CLASSES = "https://no-classes.example/sparql"

# run-no-availability.nq's one endpoint. Its run measured sw:metric:classes and
# recorded no availability fact of either kind, which is the second way into the
# index's final group.
NO_AVAILABILITY = "https://no-availability.example/sparql"


@pytest.fixture
def client_for():
    """A test client whose routes read the store handed to it."""
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


def index(client, accept="text/html"):
    response = client.get(INDEX_PATH, headers={"accept": accept})
    assert response.status_code == 200, response.text
    return response.text


# ---------------------------------------------------------------------------
# Reading the page back
# ---------------------------------------------------------------------------
#
# The page's contract with these tests is a handful of attributes, and the
# readers below depend on nothing else: not on tag names, not on how deeply
# anything is nested, not on the order attributes are written in.
#
#   data-endpoint         one per row, the endpoint URL the row is about
#   data-run-unfinished   on the row, when its facts come from a sweep that
#                         stopped before writing its footer
#   data-newer-run-unfinished
#                         on the row, when a later sweep stopped and never
#                         recorded reaching this endpoint
#   data-verdict          one per measured chip, the verdict value verbatim
#   data-declined         one per declined chip, the decline reason verbatim
#   data-gap              on a cell for a metric this row has no fact about
#
# A CHIP DOES NOT NAME ITS METRIC IN AN ATTRIBUTE. Its text is the metric's
# abbreviation, and the metric key maps each abbreviation to the metric's name,
# so these tests resolve a chip the way a reader does: by looking its letters up
# in the key. Carrying the metric in an attribute as well cost 165 KB of a 610 KB
# page for a second spelling of one fact, which the size budget did not have. The
# consequence for a test is that it must not assume a chip's column: nothing here
# reads a cell by position.
#   data-availability     one per group, the availability value it groups by,
#                         empty for the group of endpoints with no verdict
#   data-group-label      the group's label, from verdict_encoding.py
#   data-group-count      the group's count, with data-group-of beside it
#   data-group-heading    the heading a reader sees, counted and denominated
#   data-metric-column    one per metric in the key, with data-abbr beside it


class _Rows(HTMLParser):
    """Every row, in page order, with what is inside it.

    A chip and a link are attributed to the row they are nested in rather than
    to the nearest preceding row, so anything that escaped its row is lost here
    instead of being silently credited to the row above it.
    """

    def __init__(self):
        super().__init__()
        self.rows = []
        self._depth = None
        self._text = []

    def __init__(self):
        super().__init__()
        self.rows = []
        self._depth = None
        self._text = []
        self._chip = None

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "data-endpoint" in attributes:
            assert self._depth is None, "a row is nested inside another row"
            self.rows.append(
                {
                    "endpoint": attributes["data-endpoint"],
                    "attributes": attributes,
                    "chips": [],
                    "gaps": 0,
                    "href": None,
                    "text": "",
                }
            )
            self._depth = 0
            self._text = []
            return
        if self._depth is None:
            return
        self._depth += 1
        if "data-verdict" in attributes or "data-declined" in attributes:
            # The chip's abbreviation is its text, so it is collected as the
            # parser walks over it rather than read out of an attribute.
            self._chip = dict(attributes, abbr="")
            self.rows[-1]["chips"].append(self._chip)
        if "data-gap" in attributes:
            self.rows[-1]["gaps"] += 1
        if tag == "a" and "href" in attributes:
            assert self.rows[-1]["href"] is None, "a row holds two links"
            self.rows[-1]["href"] = attributes["href"]

    def handle_endtag(self, tag):
        if self._depth is None:
            return
        self._chip = None
        if self._depth == 0:
            self.rows[-1]["text"] = " ".join("".join(self._text).split())
            self._depth = None
        else:
            self._depth -= 1

    def handle_data(self, data):
        if self._depth is not None:
            self._text.append(data)
        if self._chip is not None:
            self._chip["abbr"] += data


def rows(text):
    """Every row on the page, in page order."""
    parser = _Rows()
    parser.feed(text)
    return parser.rows


def listed(text):
    return [row["endpoint"] for row in rows(text)]


def row_for(text, endpoint):
    matching = [row for row in rows(text) if row["endpoint"] == endpoint]
    assert len(matching) == 1, f"{endpoint} has {len(matching)} rows"
    return matching[0]


def chip_verdicts(text, row):
    """One row's chips as {metric: verdict}, declines excluded.

    The metric comes from the page's own key, which is how a reader resolves a
    chip's two letters, so a page whose key disagreed with its chips could not
    satisfy this and a test cannot silently start reading cells by position.

    A declined chip is not a verdict and must never be read as one, so it is
    absent from this mapping rather than present with its reason as a value.
    """
    metrics = {abbr: metric for metric, abbr in metric_key(text).items()}
    return {
        metrics[chip["abbr"]]: chip["data-verdict"]
        for chip in row["chips"]
        if "data-verdict" in chip
    }


class _Groups(HTMLParser):
    """Each group's availability value, label, heading, count and denominator."""

    def __init__(self):
        super().__init__()
        self.groups = []
        self._depth = None
        self._heading = None
        self._buffer = []

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "data-availability" in attributes:
            assert self._depth is None, "a group is nested inside another group"
            self.groups.append(
                {
                    "availability": attributes["data-availability"],
                    "label": None,
                    "heading": None,
                    "count": None,
                    "of": None,
                }
            )
            self._depth = 0
            return
        if self._depth is None:
            return
        self._depth += 1
        if "data-group-label" in attributes:
            self.groups[-1]["label"] = attributes["data-group-label"]
        if "data-group-count" in attributes:
            self.groups[-1]["count"] = attributes["data-group-count"]
            self.groups[-1]["of"] = attributes["data-group-of"]
        if "data-group-heading" in attributes:
            self._heading = self._depth
            self._buffer = []

    def handle_endtag(self, tag):
        if self._depth is None:
            return
        if self._heading is not None and self._depth == self._heading:
            self.groups[-1]["heading"] = " ".join(
                "".join(self._buffer).split()
            )
            self._heading = None
        if self._depth == 0:
            self._depth = None
        else:
            self._depth -= 1

    def handle_data(self, data):
        if self._heading is not None:
            self._buffer.append(data)


def groups(text):
    parser = _Groups()
    parser.feed(text)
    return parser.groups


def group_note(text, availability):
    """One group's explanatory sentence, by the availability value it is keyed on.

    Read off the element the template contracts to carry it rather than searched
    for in the document, for the reason test_page's _Texts gives: a sentence
    asserted to be "somewhere in the page" passes while it sits in the legend.
    """
    found = [
        note
        for attributes, note in zip(
            with_attribute(text, "data-group-note"),
            texts_with(text, "data-group-note"),
        )
        if attributes["data-group-note"] == availability
    ]
    assert len(found) == 1, f"{availability!r} has {len(found)} group notes"
    return found[0]


def metric_key(text):
    """{metric local name: abbreviation} as the page's metric key states it."""
    return {
        attributes["data-metric-column"]: attributes["data-abbr"]
        for attributes in with_attribute(text, "data-metric-column")
    }


def stored_verdicts(store):
    """{(endpoint, metric local name): verdict} straight out of the store.

    Asked of the store rather than of the reader module, so a page built from a
    broken reader cannot agree with it.
    """
    query = """
    PREFIX dqv: <http://www.w3.org/ns/dqv#>
    PREFIX sw: <urn:sparqlwatch:>
    SELECT ?endpoint ?metric ?verdict WHERE {
      GRAPH sw:current {
        ?endpoint sw:currentRun ?run .
        ?m dqv:computedOn ?endpoint ;
           dqv:isMeasurementOf ?metric ;
           dqv:value ?verdict .
      }
    }
    """
    return {
        (row["endpoint"].value, row["metric"].value.removeprefix(M)): row[
            "verdict"
        ].value
        for row in store.query(query)
    }


def stored_endpoints(store):
    query = """
    PREFIX sw: <urn:sparqlwatch:>
    SELECT ?endpoint WHERE {
      GRAPH sw:current { ?endpoint sw:currentRun ?run }
    }
    """
    return {row["endpoint"].value for row in store.query(query)}


# ---------------------------------------------------------------------------
# What the page lists
# ---------------------------------------------------------------------------
def test_the_index_lists_every_endpoint_current_knows(
    client_for, store_registry_sample
):
    """Every endpoint, and no other, with no pagination in the way.

    The count asserted is the fixture's own nine and not the sweep's 543: this
    fixture is nine of that sweep's endpoints, and asserting 543 against nine
    rows would be a claim about a file that is not in git. What is asserted
    against the store is the identity of all of them, so a page that dropped
    one or invented one fails whatever the number is.
    """
    page = index(client_for(store_registry_sample))

    assert len(listed(page)) == REGISTRY_SAMPLE_ENDPOINTS, (
        f"{len(listed(page))} rows for {REGISTRY_SAMPLE_ENDPOINTS} endpoints"
    )
    assert len(set(listed(page))) == len(listed(page)), "an endpoint has two rows"
    assert set(listed(page)) == stored_endpoints(store_registry_sample)


def test_each_chip_matches_the_stored_verdict_for_that_endpoint_and_metric(
    client_for, store_registry_sample
):
    """Two named endpoints, one whose availability is indeterminate and one of
    the four the sweep recorded as absent, so a substitute verdict cannot pass.

    Both rows are read whole rather than by their availability chip alone,
    because the mix is the point: bookkeeping.ttl never answered a query and
    still carries a verified cors chip, and ascdc.tw carries an absent
    cors-preflight chip beside five indeterminate ones.
    """
    page = index(client_for(store_registry_sample))

    assert chip_verdicts(page, row_for(page, ASCDC)) == {
        "availability": "indeterminate",
        "cors": "indeterminate",
        "cors-preflight": "absent",
        "geo-data": "indeterminate",
        "geo-functions": "indeterminate",
        "has-classes": "indeterminate",
        "service-description": "indeterminate",
    }
    assert chip_verdicts(page, row_for(page, BOOKKEEPING)) == {
        "availability": "absent",
        "cors": "verified",
        "cors-preflight": "indeterminate",
        "geo-data": "indeterminate",
        "geo-functions": "indeterminate",
        "has-classes": "indeterminate",
        "service-description": "indeterminate",
    }


def test_no_row_displays_a_verdict_the_store_does_not_hold(
    client_for, store_registry_sample
):
    """Every chip on the page against every measurement in the store, both
    ways, so neither an invented verdict nor a dropped one passes."""
    page = index(client_for(store_registry_sample))
    drawn = {
        (row["endpoint"], metric): verdict
        for row in rows(page)
        for metric, verdict in chip_verdicts(page, row).items()
    }

    assert drawn == stored_verdicts(store_registry_sample)


def test_a_declined_metric_is_not_drawn_as_a_verdict(
    client_for, store_registry_sample
):
    """The classes metric was declined for all nine on the cost ceiling, so
    every row carries a classes chip that is a decline and not a verdict.

    Drawing it with a dqv:value the store does not hold is what this guards,
    and it is the same thing the endpoint page's tests guard there: "we chose
    not to look" is not a finding about the endpoint.
    """
    page = index(client_for(store_registry_sample))
    classes_abbr = metric_key(page)["classes"]

    for row in rows(page):
        classes = [c for c in row["chips"] if c["abbr"] == classes_abbr]
        assert len(classes) == 1, (
            f"{row['endpoint']} has {len(classes)} classes chips"
        )
        assert "data-verdict" not in classes[0], (
            f"{row['endpoint']}'s declined classes chip carries a verdict"
        )
        assert classes[0]["data-declined"] == "cost-ceiling"


def test_each_group_states_its_count_with_the_denominator(
    client_for, store_registry_and_failure
):
    """Each count says what it is a count of, in the heading a reader sees.

    Stage 1d-a published a wrong number by conflating three quantities that
    were each true of something else, and the correction was to state each
    distinctly. A heading reading "verified 3" invites exactly that: three of
    what?
    """
    page = index(client_for(store_registry_and_failure))
    total = REGISTRY_SAMPLE_ENDPOINTS + 1

    for group in groups(page):
        assert group["of"] == str(total), (
            f"{group['label']} is counted out of {group['of']}"
        )
        assert re.search(
            rf"\b{group['count']} of {total} endpoints\b", group["heading"] or ""
        ), f"{group['label']}'s heading reads {group['heading']!r}"


def test_a_finished_sweep_qualifies_no_row(client_for, store_registry_sample):
    """The registry sweep finished, so nothing on its index is qualified.

    Without this the test above passes over a page that marks every row, which
    says of a complete sweep that it stopped.
    """
    page = index(client_for(store_registry_sample))

    assert with_attribute(page, "data-run-unfinished") == []
    assert with_attribute(page, "data-newer-run-unfinished") == []


def test_a_store_whose_newest_sweep_did_not_finish_says_so_once(
    client_for, store_crashed_partway
):
    """The one store-level claim this page makes, pinned.

    It is said once, at the top, because it is a fact about the store rather
    than 543 facts about endpoints, and the rows below say which of them it
    leaves qualified. Both halves are asserted: that the note is there, and that
    it names the sweep it is about, since a sentence naming no instant leaves a
    reader unable to tell which sweep stopped.
    """
    page = index(client_for(store_crashed_partway))
    noted = texts_with(page, "data-newest-sweep-unfinished")

    assert len(noted) == 1, (
        f"one store, one note about the store, got {len(noted)}"
    )
    assert CRASHED_SWEEP in noted[0], noted[0]
    assert "did not finish" in noted[0], noted[0]


def test_a_store_whose_newest_sweep_finished_says_nothing_about_it(
    client_for, store_registry_sample
):
    """The other direction, which is the half that catches a note always drawn.

    The registry sweep recorded sw:finalised, so there is nothing to qualify
    about it, and a page that carried the sentence anyway would be telling a
    reader that a completed sweep stopped. Both halves are needed for the same
    reason the row-level pair beside them is: the test above passes just as well
    over a page that says this of every store.
    """
    page = index(client_for(store_registry_sample))

    assert with_attribute(page, "data-newest-sweep-unfinished") == []
    assert "did not finish" not in page


# ---------------------------------------------------------------------------
# The link, the encoding, the metric list
# ---------------------------------------------------------------------------
def test_a_row_links_to_the_percent_encoded_endpoint_page(
    client_for, store_registry_sample
):
    """The link a reader follows, and it has to be encoded rather than pasted.

    The endpoint resource names its endpoint in a url query parameter (see
    web/app.py on why not a path segment), and a query string is unquoted
    exactly once, so the value has to arrive encoded. bookkeeping.ttl is the
    row that shows it: its URL holds slashes and a colon that must not be left
    bare, and the page it links to must come back with that endpoint's own
    facts rather than a 404 or somebody else's row.
    """
    page = index(client_for(store_registry_sample))
    links = {row["endpoint"]: row["href"] for row in rows(page)}
    assert set(links) == stored_endpoints(store_registry_sample)
    assert None not in links.values()

    assert links[BOOKKEEPING] == (
        ENDPOINT_PATH
        + "?url=https%3A%2F%2Fraw.githubusercontent.com%2FGVogeler"
        "%2Fbookkeeping%2Fmaster%2Fbookkeeping.ttl"
    )

    followed = client_for(store_registry_sample).get(
        links[BOOKKEEPING], headers={"accept": "text/html"}
    )
    assert followed.status_code == 200
    assert BOOKKEEPING in followed.text


def test_the_chips_come_from_the_one_encoding_table(
    client_for, store_registry_sample
):
    """Every chip is drawn by a class verdict_encoding.py generates, and it is
    the class that table gives that chip's verdict.

    A second copy of the encoding is how the JavaScript viewer's legend came to
    give "absent" a fill its chips never had. The index has its own chips and
    its own group headings, and both are read out of that module, so a private
    table in the template fails here rather than drifting.

    The rules themselves moved out of this page's own markup and into the
    generated stylesheet on 2026-09-16 (see app.STYLESHEET_PATH), so this
    reads them from there instead of from the page: the page no longer prints
    them at all, which is what test_the_index_declares_no_tokens_and_no_inline_verdict_css
    pins.
    """
    client = client_for(store_registry_sample)
    page = index(client)
    css = client.get(app_module.STYLESHEET_PATH).text
    generated = set(re.findall(r"\.(enc-[a-z-]+)\s*\{", css))
    # Both families since 2026-09-01: the chip rule and the text colour the
    # matrix header's labels wear. Still asserted as an exact set, so a private
    # table in the template still fails here.
    assert {g for g in generated if g.startswith("enc-text-")} == {
        verdict_encoding.text_class(state.slug)
        for state in (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED)
    }
    generated = {g for g in generated if not g.startswith("enc-text-")}
    assert generated == {
        verdict_encoding.css_class(state.slug)
        for state in (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED)
    }

    for row in rows(page):
        for chip in row["chips"]:
            drawn = {
                name
                for name in chip.get("class", "").split()
                if name.startswith("enc-")
            }
            expected = (
                verdict_encoding.css_class(chip["data-verdict"])
                if "data-verdict" in chip
                else verdict_encoding.css_class(verdict_encoding.NOT_MEASURED)
            )
            assert drawn == {expected}, f"{row['endpoint']} {chip}"


def test_an_unrecognised_verdict_is_drawn_as_unrecognised_and_shown_verbatim(
    client_for, store_hostile_literals
):
    """A dqv:value this build has no encoding for, and its endpoint has no
    availability verdict either.

    Two things at once out of one committed fixture. The value reaches the page
    verbatim in data-verdict, escaped, because relabelling it would hide which
    value the store holds; and it is drawn in the fallback class, which uses a
    border style none of the seven uses, rather than in absent's, which would
    draw "we have no idea what this means" as "we established nothing was
    there".
    """
    page = index(client_for(store_hostile_literals))
    row = row_for(page, HOSTILE)

    assert list(chip_verdicts(page, row).values()) == [HOSTILE_VERDICT]
    assert HOSTILE_VERDICT not in page, "the hostile literal reached the page raw"
    drawn = [c for c in row["chips"] if c.get("data-verdict") == HOSTILE_VERDICT]
    assert verdict_encoding.css_class(
        verdict_encoding.UNRECOGNISED.slug
    ) in drawn[0]["class"].split()

    # One listing since 2026-08-28, so there is no group whose availability
    # value could be read. What matters is on the row: it is on the page and its
    # chip carries the unrecognised encoding.
    assert any(r["endpoint"] == row["endpoint"] for r in grouped_rows(page))


def test_the_metric_columns_come_from_the_run_rather_than_a_fixed_list(
    client_for, store_classes_absent, store_registry_sample
):
    """The metric list is what the run recorded, not the eight of today.

    run-classes-absent.nq records two metrics, availability and classes, and
    nothing else. A page built around a hard-coded eight draws six chips no
    measurement stands behind, which is the same wrong answer as a substitute
    verdict, and it breaks the first time prober/metrics.toml changes. The
    registry sample is asserted beside it so that "two columns" cannot be the
    answer to every store either.
    """
    narrow = index(client_for(store_classes_absent))
    assert set(metric_key(narrow)) == {"availability", "classes"}
    assert {
        chip["abbr"] for row in rows(narrow) for chip in row["chips"]
    } == set(metric_key(narrow).values())

    wide = index(client_for(store_registry_sample))
    assert set(metric_key(wide)) == {
        "availability",
        "classes",
        "cors",
        "cors-preflight",
        "geo-data",
        "geo-functions",
        "has-classes",
        "service-description",
    }


def test_every_chip_abbreviation_is_unique_and_written_out_in_the_key(
    client_for, store_registry_sample
):
    """A chip carries an abbreviation, so the page has to explain it.

    The endpoint page's chips carry no text because the metric is named beside
    them; here 543 rows of full metric names is markup the size budget does not
    have, so the chip is abbreviated the way design/Main.dc.html abbreviates
    it. That is only honest if each abbreviation is unique and is written out in
    full somewhere on the page, which is what the metric key is for.
    """
    page = index(client_for(store_registry_sample))
    key = metric_key(page)
    assert len(set(key.values())) == len(key), f"abbreviations collide: {key}"

    written = texts_with(page, "data-metric-column")
    assert len(written) == len(key)
    for entry, (metric, abbr) in zip(written, key.items()):
        assert metric in entry, f"{metric} is not written out in {entry!r}"
        assert abbr in entry

    for row in rows(page):
        for chip in row["chips"]:
            assert chip["abbr"] in set(key.values()), (
                f"{row['endpoint']} carries a chip reading {chip['abbr']!r}, "
                f"which the key does not explain"
            )


# ---------------------------------------------------------------------------
# Agreement, negotiation, and the page standing on its own
# ---------------------------------------------------------------------------
def test_the_index_and_the_endpoint_page_agree_about_a_verdict(
    client_for, store_registry_sample
):
    """One endpoint's chips on the index against its own page's rows.

    They are two renderings of one question, and the index reaching the store
    through its own query is exactly how they could come to disagree. epo is
    the row asserted because it carries declared-but-wrong, the verdict that
    asserts an error, beside two absent chips.
    """
    client = client_for(store_registry_sample)
    page = index(client)
    on_the_index = chip_verdicts(page, row_for(page, EPO))

    own = client.get(
        ENDPOINT_PATH, params={"url": EPO}, headers={"accept": "text/html"}
    )
    assert own.status_code == 200
    on_its_page = {
        attributes["data-metric"].removeprefix(M): attributes["data-verdict"]
        for attributes in with_attribute(own.text, "data-metric")
        if "data-verdict" in attributes
    }

    assert on_the_index == on_its_page
    assert on_the_index["geo-functions"] == "declared-but-wrong"


def test_the_index_negotiates_like_every_other_resource(
    client_for, store_registry_sample
):
    """HTML, RDF, and a refusal, on one URL.

    The design spec requires content negotiation of every resource, so the
    index is one resource in several representations rather than an HTML page
    with a data feed beside it. The fallbacks are the endpoint resource's, for
    the reasons written there: `*/*` and a missing Accept both get the legible
    representation, and an Accept nothing offered can satisfy gets a 406 rather
    than something the client said it could not read.
    """
    client = client_for(store_registry_sample)

    html = client.get(INDEX_PATH, headers={"accept": "text/html"})
    assert html.status_code == 200
    assert html.headers["content-type"].startswith("text/html")

    assert client.get(INDEX_PATH, headers={"accept": "*/*"}).headers[
        "content-type"
    ].startswith("text/html")

    request = client.build_request("GET", INDEX_PATH)
    del request.headers["accept"]
    assert client.send(request).headers["content-type"].startswith("text/html")

    turtle = client.get(INDEX_PATH, headers={"accept": "text/turtle"})
    assert turtle.status_code == 200
    assert turtle.headers["content-type"].startswith("text/turtle")
    assert list(parse(turtle.content, format=RdfFormat.TURTLE))

    assert (
        client.get(INDEX_PATH, headers={"accept": "application/pdf"}).status_code
        == 406
    )


def test_the_rdf_index_carries_every_endpoints_verdicts(
    client_for, store_registry_sample
):
    """The machine representation says what the page says, for all nine.

    Built by a CONSTRUCT out of the store rather than from the rows the page
    was built from, for the reason web/queries/endpoint_description.rq's header
    gives: two representations re-derived from one flattened copy of the facts
    is where they start to disagree.
    """
    client = client_for(store_registry_sample)
    turtle = client.get(INDEX_PATH, headers={"accept": "text/turtle"})
    graph = Store()
    graph.extend(list(parse(turtle.content, format=RdfFormat.TURTLE)))

    served = {}
    for quad in graph.quads_for_pattern(
        None, NamedNode(DQV + "computedOn"), None
    ):
        metric = {
            q.object.value
            for q in graph.quads_for_pattern(
                quad.subject, NamedNode(DQV + "isMeasurementOf"), None
            )
        }
        verdict = {
            q.object.value
            for q in graph.quads_for_pattern(
                quad.subject, NamedNode(DQV + "value"), None
            )
        }
        assert len(metric) == 1 and len(verdict) == 1, quad.subject
        served[(quad.object.value, metric.pop().removeprefix(M))] = verdict.pop()

    assert served == stored_verdicts(store_registry_sample)


def test_the_page_holds_every_row_without_the_filter_script_running(
    client_for, store_registry_sample
):
    """The filter is an inline script, and nothing depends on it running.

    With JavaScript off every row is in the document and only the filter is
    missing, so the page is a document first and a filtered view second. A page
    that rendered its rows from the script would be empty to a reader who
    blocks it, and the spec's own risk table flags the content security policy
    that may block exactly this script.
    """
    page = index(client_for(store_registry_sample))
    scripts = re.findall(r"<script\b[^>]*>(.*?)</script>", page, re.S)
    assert len(scripts) == 1, f"{len(scripts)} scripts on the page"

    without = re.sub(r"<script\b[^>]*>.*?</script>", "", page, flags=re.S)
    assert listed(without) == listed(page)
    assert len(rows(without)) == REGISTRY_SAMPLE_ENDPOINTS


def test_the_rdf_index_carries_every_endpoint_a_crashed_run_left_behind(
    client_for, store_crashed_partway
):
    """Three endpoints, one completion marker, and all three in the document.

    What is pinned is that reaching a run-level fact THROUGH a marker can never
    remove an endpoint from the document. The marker is per endpoint and most
    endpoints do not have one: the run that crashed here recorded one, and the
    older run that holds the other two endpoints' facts recorded none at all,
    because it predates the scheme. A pattern that required the marker, or an
    OPTIONAL group that bound ?endpoint beside the run's timestamp, would take
    those endpoints out of the RDF while the HTML still listed them, and the two
    representations of one resource would then disagree about which endpoints
    exist. The SELECT beside this had exactly that shape and it inverted the "a
    later sweep never got here" marker on the two endpoints that needed it; on a
    CONSTRUCT the same mistake is quieter and no better.

    What the document has to carry is the INPUTS a consumer draws the two
    conclusions from, never the conclusions: sw:emission on the crashed run, no
    sw:finalised on it, and its sw:completedEndpoint for the one endpoint it
    finished and for no other.
    """
    client = client_for(store_crashed_partway)
    turtle = client.get(INDEX_PATH, headers={"accept": "text/turtle"})
    graph = Store()
    graph.extend(list(parse(turtle.content, format=RdfFormat.TURTLE)))

    described = {
        quad.object.value
        for predicate in ("computedOn",)
        for quad in graph.quads_for_pattern(
            None, NamedNode(DQV + predicate), None
        )
    }
    assert described == {KADASTER, QLEVER, ONTOP}

    sw = "urn:sparqlwatch:"
    completed = {
        quad.object.value
        for quad in graph.quads_for_pattern(
            None, NamedNode(sw + "completedEndpoint"), None
        )
    }
    assert completed == {KADASTER}
    assert list(
        graph.quads_for_pattern(None, NamedNode(sw + "emission"), None)
    ), "the crashed run's sw:emission is not in the document"
    assert not list(
        graph.quads_for_pattern(None, NamedNode(sw + "finalised"), None)
    ), "a run that did not finish is served as finalised"


def test_a_metric_this_rows_run_never_recorded_is_not_drawn_as_a_verdict(
    client_for, store_two_metric_sets
):
    """Two runs, two metric sets, and one row with six cells behind no fact.

    run-classes-absent.nq recorded availability and classes for its one
    endpoint and nothing else, so beside the registry sweep's nine that row is
    six columns short. Those six cells hold a dot and carry no verdict, no
    decline and no encoding class, because every one of the seven states is a
    finding and "this run recorded nothing here" is not one of them: drawing it
    as absent would say nothing was there, and drawing it as not measured would
    say the run declined it, which it never did.

    Without this the union of the metric sets is untested, and the union is the
    normal shape of a store the first time prober/metrics.toml changes.
    """
    page = index(client_for(store_two_metric_sets))
    key = metric_key(page)
    assert len(key) == 8, f"the columns are {sorted(key)}"

    narrow = row_for(page, NO_CLASSES)
    assert set(chip_verdicts(page, narrow)) == {"availability", "classes"}
    assert narrow["gaps"] == 6, f"{narrow['gaps']} cells stand for no fact"
    assert len(narrow["chips"]) + narrow["gaps"] == len(key)

    # And the gap is not counted as a state: the legend counts chips.
    for wide in (row_for(page, EPO), row_for(page, ASCDC)):
        assert wide["gaps"] == 0
        assert len(wide["chips"]) == len(key)


# ---------------------------------------------------------------------------
# Rows the newest sweep did not ask, and rows it did not measure
# ---------------------------------------------------------------------------
#
# The defect these close is one sentence in the header: "newest sweep
# <instant>". It is one fact about the store, and until now nothing on a row
# said whether that instant had anything to do with the verdicts beside it. On
# this site an absent qualifier is a positive claim, so 543 rows of a week-old
# sweep's verdicts under a header dated today claimed 543 times that they were
# measured today.
#
# Two claims, and they are not the same claim. WHICH SWEEP MEASURED THIS ROW is
# provenance and is true of any row whose facts are not the newest sweep's, for
# any reason at all. THE NEWEST SWEEP DID NOT ASK is a fact about this service's
# rotation, published by the sweep itself with a reason.
#
# Both are worded against the sweep and never as an age from today. Nothing here
# runs on a schedule, so "six days ago" is a claim the data does not support: the
# store holds two instants and no cadence between them, and _provenance's rule
# is that the two timestamps are named and never ordered.


def stale_rows(page):
    """{endpoint: the instant its row names} for every row that names one."""
    return {
        attributes["data-endpoint"]: attributes["data-measured-by-sweep"]
        for attributes in with_attribute(page, "data-measured-by-sweep")
    }


def dormant_rows(page):
    """{endpoint: the reason its row carries, or None} for every dormant row."""
    return {
        attributes["data-endpoint"]: attributes.get("data-dormancy-reason")
        for attributes in with_attribute(page, "data-newest-sweep-dormant")
    }


def test_a_row_the_newest_sweep_declined_to_ask_says_so_in_words(
    client_for, store_dormant_newest
):
    """The row for the endpoint the 10:00 sweep published as dormant.

    Its verdicts are the 16:00 sweep's, five days older than the header's
    instant, and the newest sweep did not ask it: it said so, and it said why.
    The marker is words a reader sees and an attribute a machine reads, and the
    reason travels verbatim so a consumer sees the value the store holds rather
    than this build's reading of it.

    The other two endpoints of the same trio were measured by that same 10:00
    sweep, so nothing about them is qualified. Both halves are asserted for the
    reason the pair beside them is: a page that marked every row would say of a
    measured endpoint that nobody asked it.
    """
    page = index(client_for(store_dormant_newest))
    assert set(listed(page)) == {KADASTER, QLEVER, ONTOP}

    assert dormant_rows(page) == {KADASTER: "operator-hold"}

    text = row_for(page, KADASTER)["text"]
    assert text.strip() != KADASTER, "the marker attribute carries no words"
    assert "did not ask" in text, text
    for endpoint in (QLEVER, ONTOP):
        assert "did not ask" not in row_for(page, endpoint)["text"]


def test_a_dormant_rows_words_name_the_reason_the_store_holds(
    client_for, store_dormant_newest, store_dormant_automatic
):
    """Two of the reasons, and the words differ between them.

    An operator's hold and a machine relegation are different facts about this
    service and a row that read the same for both would tell a reader nothing
    the attribute did not already say. The automatic case is the one a stranger
    meets in production: a hold is one person's decision about one endpoint.
    """
    held = index(client_for(store_dormant_newest))
    automatic = index(client_for(store_dormant_automatic))

    assert dormant_rows(held) == {KADASTER: "operator-hold"}
    assert dormant_rows(automatic) == {KADASTER: "automatic"}

    assert row_for(held, KADASTER)["text"] != row_for(automatic, KADASTER)["text"]
    assert "hand" in row_for(held, KADASTER)["text"]
    assert "cost" in row_for(automatic, KADASTER)["text"]


def test_a_dormant_row_keeps_the_verdict_its_last_probe_produced(
    client_for, store_dormant_newest
):
    """A group note, not a group of its own, and not a chip either.

    Dormancy is not a verdict: the six-verdict vocabulary is closed and this is
    a fact about this service's rotation. So the row stays in the group its last
    real probe put it in, with every chip that probe produced, and moving it to
    a group of its own would misfile a measured endpoint under a heading about
    us. The group it sits in says what the marker means and how many of its rows
    carry one.
    """
    page = index(client_for(store_dormant_newest))

    # Dormancy was never a group and is not one now: the row keeps the verdict
    # its last real probe produced, and since 2026-08-28 there are no groups at
    # all, so the only place the marker can appear is on the row.
    assert not with_attribute(page, "data-group-heading")
    assert chip_verdicts(page, row_for(page, KADASTER))["availability"] == "verified"
    assert (
        chip_verdicts(page, row_for(page, KADASTER))
        == {
            metric: verdict
            for (endpoint, metric), verdict in stored_verdicts(
                store_dormant_newest
            ).items()
            if endpoint == KADASTER
        }
    )


def test_a_group_no_dormant_row_is_in_carries_no_dormancy_note(
    client_for, store_registry_sample
):
    """The other half, which is what catches a note drawn on every group.

    The registry sweep declared nothing dormant, so a note saying some of these
    rows were not asked would be false of all nine of them.
    """
    page = index(client_for(store_registry_sample))
    assert with_attribute(page, "data-group-dormant") == []


def test_a_row_whose_facts_are_older_than_the_newest_sweep_names_its_own(
    client_for, store_dormant_newest
):
    """Which sweep measured this row, on the row, whenever it is not the header's.

    The header states one instant for the store. This row's verdicts come from
    another sweep, and the gap is exactly what the marker above is about, so the
    row carries the instant of the sweep that DID measure it. Not an age: the
    store holds two instants and nothing that says how long is expected between
    two sweeps.
    """
    page = index(client_for(store_dormant_newest))

    assert stale_rows(page) == {KADASTER: "2026-08-22T16:00:00Z"}
    assert "2026-08-22T16:00:00Z" in row_for(page, KADASTER)["text"]
    assert "2026-08-27T10:00:00Z" in page, "the header's own instant is gone"


def test_no_row_states_an_age_or_orders_the_two_instants(
    client_for, store_dormant_newest
):
    """The wording rule, asserted rather than trusted.

    _provenance's rule is that the two timestamps are named and never ordered.
    Nothing in this service runs on a schedule, so "5 days ago", "stale for a
    week" and "out of date" are all claims the store cannot support: it holds
    two instants and no cadence between them.
    """
    page = index(client_for(store_dormant_newest)).lower()
    for forbidden in ("ago", "days old", "out of date", "stale", "weekly"):
        assert forbidden not in page, forbidden


def test_a_store_of_one_sweep_names_no_sweep_on_any_row(
    client_for, store_registry_sample
):
    """The half that catches an instant printed on every row.

    Every row of this store's nine was measured by the only sweep in it, which
    is the sweep the header names, so a row repeating that instant would be
    qualifying a fact that needs no qualification.
    """
    page = index(client_for(store_registry_sample))
    assert stale_rows(page) == {}
    assert dormant_rows(page) == {}


def test_a_row_older_than_the_newest_sweep_names_it_with_no_other_marker(
    client_for, store_later_sample
):
    """The case only the widest condition catches, and no other marker fires.

    This store's newest run is the 22:00 one, which SAMPLED one endpoint and
    measured nothing at all. So no row here is from a sweep that stopped, no
    later sweep crashed before reaching one, and no run in the store recorded
    finishing while saying nothing about one: all three of the endpoint page's
    qualifications are false of all three rows, and all three rows still carry
    the 16:00 sweep's verdicts under a header dated 22:00.

    That is why the instant is asked as its own question rather than hung off
    one of the other three. Without it these three rows are the defect with no
    marker at all on the page.
    """
    page = index(client_for(store_later_sample))

    assert stale_rows(page) == {
        KADASTER: "2026-08-22T16:00:00Z",
        QLEVER: "2026-08-22T16:00:00Z",
        ONTOP: "2026-08-22T16:00:00Z",
    }
    assert with_attribute(page, "data-run-unfinished") == []
    assert with_attribute(page, "data-newer-run-unfinished") == []
    assert dormant_rows(page) == {}


def test_the_rows_a_crashed_sweep_never_reached_name_the_sweep_that_measured_them(
    client_for, store_crashed_partway
):
    """The claim that was already on those rows, now with the instant beside it.

    "a later sweep never got here" says which sweep did NOT produce the row. It
    never said which one did, and the header names a third instant, so a reader
    had the two facts the marker is about and neither of the two dates.
    """
    page = index(client_for(store_crashed_partway))

    assert stale_rows(page) == {
        QLEVER: "2026-08-22T16:00:00Z",
        ONTOP: "2026-08-22T16:00:00Z",
    }
    # kadaster's own facts ARE the newest run's, unfinished though it is, so
    # there is no second sweep to name for it.
    assert KADASTER not in stale_rows(page)
    assert row_for(page, KADASTER)["attributes"].get("data-run-unfinished") == "true"


def test_a_crashed_declining_sweep_says_it_declined_and_not_that_it_crashed(
    client_for, store_dormancy_then_crash
):
    """Two true facts about one sweep, and only one of them may be said.

    The newest run in this store promised to write incrementally, never recorded
    finishing, and published a complete account of the one endpoint it declined
    to ask before it died. Both "the sweep stopped before it got here" and "the
    sweep never intended to ask" are supported by those bytes; only the second is
    true. The store-level note still says the sweep did not finish, because it
    did not, and that is a fact about the store rather than about this row.
    """
    page = index(client_for(store_dormancy_then_crash))

    assert dormant_rows(page) == {KADASTER: "operator-hold"}
    never_reached = {
        attributes["data-endpoint"]
        for attributes in with_attribute(page, "data-newer-run-unfinished")
    }
    assert KADASTER not in never_reached, (
        "a sweep that published why it skipped this endpoint is reported as "
        "having crashed before reaching it"
    )
    assert "did not ask" not in row_for(page, QLEVER)["text"]
    # And the other two rows keep the crash claim, which is true of them: that
    # sweep declined one endpoint and died before reaching either of these. Both
    # claims are on the page at once, about different rows, off one run graph.
    assert never_reached == {QLEVER, ONTOP}
    assert len(texts_with(page, "data-newest-sweep-unfinished")) == 1


def test_an_endpoint_this_store_knows_only_as_dormant_has_no_row(
    client_for, store_dormancy_alone
):
    """Nothing measured it, so there is nothing to date and no row to qualify.

    Both read queries build rows from sw:currentRun, which such an endpoint has
    none of, so it is absent from the index and its page 404s. A row drawn from
    the declaration alone would carry no verdict, no sweep and no chip, and
    would be this service's own rotation presented as an endpoint's record.
    """
    page = index(client_for(store_dormancy_alone))

    assert set(listed(page)) == {QLEVER, ONTOP}
    assert dormant_rows(page) == {}
    assert stale_rows(page) == {}


def test_dormancy_is_not_added_to_the_legend(client_for, store_dormant_newest):
    """The seven states, and dormancy is not an eighth.

    The legend counts chip states from the closed table in verdict_encoding and
    is built by the same function as the endpoint page's, so an entry added here
    would appear there too and would describe a drawing neither page uses.
    Dormancy has no chip: it is a row qualifier, explained in the panel that
    explains the other two.
    """
    page = index(client_for(store_dormant_newest))
    states = [attributes["data-state"] for attributes in with_attribute(page, "data-state")]

    assert states == [state.slug for state in verdict_encoding.STATES]
    assert "dormant" not in states


def test_the_row_markers_are_explained_in_the_docs_and_not_on_the_index(
    client_for, store_dormant_newest
):
    """The panel that explained the four row markers went on 2026-08-28, and
    the header now leads to the docs section rather than straight to /about.

    The markers still render, so a reader still meets "the newest sweep did not
    ask" on a row. What the index no longer does is explain it, and the way to
    the page that does is two clicks instead of one: docs, then Monitoring.
    """
    page = index(client_for(store_dormant_newest))
    assert not with_attribute(page, "data-row-marker")
    marked = texts_with(page, "data-newest-sweep-dormant")
    assert marked and any("did not ask" in text for text in marked)
    nav = with_attribute(page, "data-nav")
    # base.html's four-item nav replaced this page's own two-link header on
    # 2026-09-16, the same shell /explore, /docs and /about already carry.
    # Still an exact list: this header is deliberately small, and a link
    # appearing in it without a test changing is how a nav turns into a menu.
    assert [a["href"] for a in nav] == [INDEX_PATH, EXPLORE_PATH, DOCS_PATH, ABOUT_PATH]


def test_the_index_carries_one_nav_link_and_never_one_per_row(
    client_for, store_dormant_newest
):
    """A constant handful of links, in the header and the footer, and never
    one per row.

    It pointed at /about until 2026-08-28 and points at /docs now. The page
    moved onto base.html's shared shell on 2026-09-16, whose footer repeats
    the docs link ("how we measure") beside the header nav's own copy: a
    second CONSTANT occurrence and not a second one per row. The invariant
    that matters at 543 rows is unchanged: a link repeated per row would
    spend about 1,200 bytes as 24,000, and web/README.md's table is the
    record of how little headroom that leaves.
    """
    text = index(client_for(store_dormant_newest))
    rows = len(listed(text))
    # DOCS_PATH twice -- the header nav and the shared footer both carry it --
    # EXPLORE_PATH once, since the footer links only docs and VoID.
    for path, expected in ((DOCS_PATH, 2), (EXPLORE_PATH, 1)):
        occurrences = text.count(f'href="{path}"')
        assert occurrences == expected, f"{occurrences} links to {path}"
        # The assertion that survives a redesign: whatever the header and
        # footer hold between them, it must not scale with the listing.
        assert occurrences < rows, f"{path} appears per row"


def test_the_row_marker_reads_every_reason_a_run_graph_can_carry():
    """Every named reason and both unnamed branches, asked of the function
    rather than of a store.

    Two of the branches have no committed fixture and cannot get one from the
    prober: it writes a reason with every declaration, and the only file
    carrying an unreadable spelling would be a hand-edited one. They are
    reachable all the same, from a prober newer or older than this page and from
    a hand-edited run file, and each is a sentence a reader would act on, so each
    is pinned here the way test_page.py pins the endpoint page's longer version
    of the same set.

    An unrecognised reason travels VERBATIM. Relabelling it would hide which
    value the store holds, and dropping it would leave the row claiming the run
    gave no reason when it gave one this build cannot read.
    """
    assert _row_dormancy_text("operator-hold") == f"{ROW_DORMANT_TEXT}, by hand"
    assert _row_dormancy_text("automatic") == (
        f"{ROW_DORMANT_TEXT}, on cost and silence"
    )
    assert _row_dormancy_text("not-in-this-sweep") == (
        f"{ROW_DORMANT_TEXT}, on a replay of an earlier sweep"
    )
    # And every reason the map holds, so a fourth slug is a failure here rather
    # than a row that quietly reads "for a reason this page cannot read". The
    # words are spelled out above because a reader acts on them; the coverage is
    # read off the map because its keys belong to the prober.
    for slug, (clause, _) in _ROW_DORMANCY_REASONS.items():
        assert _row_dormancy_text(slug) == f"{ROW_DORMANT_TEXT}, {clause}", slug

    # A declaration with no reason beside it. The declaration is the fact the
    # page turns on, so the marker still appears and says the reason is missing
    # rather than guessing at one.
    missing = _row_dormancy_text(None)
    assert missing.startswith(ROW_DORMANT_TEXT)
    assert "no reason" in missing

    # A reason from a prober this build does not know. Both halves: the value is
    # there, and nothing is claimed about what it means.
    unknown = _row_dormancy_text("hibernating-2027")
    assert "hibernating-2027" in unknown
    assert "cannot read" in unknown
    readings = [clause for clause, _ in _ROW_DORMANCY_REASONS.values()]
    for reading in [*readings, "no reason"]:
        assert reading not in unknown, unknown


# ---------------------------------------------------------------------------
# The three facet groups
# ---------------------------------------------------------------------------
# Added 2026-08-27 with the facets. THE IMPLEMENTATION WENT IN FIRST AND THESE
# TESTS FOLLOWED IT, which inverts the order every task in the dormancy stage
# held to, and it showed: the two sentences that change removed turned out to
# be asserted by nothing, so the suite could not have told me whether removing
# them broke anything. The helper `group_note` above looks like cover for them
# and is not: its one caller asks for the empty-string group, which still has
# its note.
#
# Rewritten 2026-08-28 after a review of that commit returned SPEC: FAIL. Nine
# of these tests fed hand-built group dicts to the three builders and NOTHING
# connected any builder to the rows the page renders, which is how a legend
# entry that printed no count at all survived a green suite. The test that
# closes that is test_every_chip_count_is_the_rows_the_page_renders below, and
# it is the one to keep working: it reads the rendered page, applies the
# script's own predicate to the rows in it, and compares the answer with the
# number each chip prints.


def facets(text, name):
    """{facet value: full button text} for one facet group, off the buttons."""
    return {
        attributes["data-facet-value"]: label
        for attributes, label in zip(
            with_attribute(text, "data-facet-value"),
            texts_with(text, "data-facet-value"),
        )
        if attributes.get("data-facet") == name
    }


class _FacetChips(HTMLParser):
    """Every facet chip: its tag, its attributes, and what its count span says.

    The count is read off the span rather than out of the button's whole text,
    because a state chip's text carries TWO numbers, the chips count and the
    rows count, and picking one out of a run-together sentence would pass while
    they were swapped. texts_with cannot do this: it records the outermost
    element carrying an attribute, and every count span is inside a button that
    carries a class of its own.

    The tag name is collected too, which no other reader here does, because what
    a chip IS is part of the claim being tested.
    """

    def __init__(self):
        super().__init__()
        self.chips = []
        self._depth = None
        self._count = None
        self._buffer = []

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "data-facet" in attributes:
            assert self._depth is None, "a facet chip is nested inside another"
            self.chips.append(
                {"tag": tag, "attributes": attributes, "count": None}
            )
            self._depth = 0
            return
        if self._depth is None:
            return
        self._depth += 1
        if "f-count" in (attributes.get("class") or "").split():
            assert self.chips[-1]["count"] is None, "a chip holds two counts"
            self._count = self._depth
            self._buffer = []

    def handle_endtag(self, tag):
        if self._depth is None:
            return
        if self._count is not None and self._depth == self._count:
            self.chips[-1]["count"] = " ".join("".join(self._buffer).split())
            self._count = None
        if self._depth == 0:
            self._depth = None
        else:
            self._depth -= 1

    def handle_data(self, data):
        if self._count is not None:
            self._buffer.append(data)


def facet_chips(text):
    """Every facet chip on the page, in page order."""
    parser = _FacetChips()
    parser.feed(text)
    return parser.chips


def facet_counts(text):
    """{(group, value): the number that chip prints}.

    A span that prints no digits at all fails here, and says so, rather than
    turning into a KeyError somewhere later. That was the shape of the defect
    this reader exists for: the eighth legend entry rendered a blank where the
    rows count belongs, and on these pages an absent qualifier is a positive
    claim.
    """
    counts = {}
    for chip in facet_chips(text):
        attributes = chip["attributes"]
        shown = chip["count"]
        value = attributes["data-facet-value"]
        assert shown is not None, f"chip {value!r} carries no count span"
        head = shown.split()[:1]
        assert head and head[0].isdigit(), (
            f"chip {value!r} prints {shown!r}, which states no count"
        )
        counts[(attributes["data-facet"], value)] = int(head[0])
    return counts


class _GroupedRows(HTMLParser):
    """Every rendered row and its chips.

    Tracked each row's enclosing availability group until 2026-08-28, when the
    three sections became one listing and there was no group to track. What it
    still refuses to fake is the row set: every assertion about a count on this
    page is checked against the rows the page actually renders, which is the
    only reading that catches a builder and a script disagreeing.
    """

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.rows = []
        self._row = None

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "data-endpoint" in attributes:
            self._row = {"endpoint": attributes["data-endpoint"], "chips": []}
            self.rows.append(self._row)
        elif self._row is not None and "class" in attributes:
            if attributes["class"].startswith("enc-"):
                self._chip = dict(attributes)
                self._row["chips"].append(self._chip)

    def handle_data(self, data):
        if self._row is not None and self._row["chips"]:
            chip = self._row["chips"][-1]
            if "abbr" not in chip and data.strip():
                chip["abbr"] = data.strip()

    def handle_endtag(self, tag):
        if tag == "li":
            self._row = None


def grouped_rows(text):
    """Every rendered row, with its group's availability value and its chips."""
    parser = _GroupedRows()
    parser.feed(text)
    return parser.rows


def encoding_class(chip):
    """The one enc- token on a cell, asserted to be the only class it carries.

    The script turns a cell into a state with
    ``cell.className.replace(/^enc-/, "")``, which returns "verified other" the
    moment a second class is added and then matches no state at all, silently,
    on every row. So the shape is pinned from pytest rather than trusted.
    """
    tokens = chip["class"].split()
    assert len(tokens) == 1, f"a cell carries {tokens}, not one class"
    assert tokens[0].startswith("enc-"), f"a cell carries {tokens[0]!r}"
    return tokens[0][len("enc-") :]


def group_of(_ignored, *cell_maps):
    """Rows that need not be alike, in the shape _index_rows returns.

    One list of rows and no group since 2026-08-28. The first parameter was the
    group's availability value and is ignored, kept so the call sites still read
    as descriptions of endpoints whose availability reads that value.
    """
    return [
        {
            "cells": [
                {
                    "present": True,
                    "name": name,
                    "abbr": name[:2].upper(),
                    "verdict": value,
                    "reason": None if value else "cost-ceiling",
                    "slug": (
                        value
                        if value in {state.slug for state in verdict_encoding.STATES}
                        else (
                            verdict_encoding.NOT_MEASURED
                            if value is None
                            else verdict_encoding.UNRECOGNISED.slug
                        )
                    ),
                }
                for name, value in cells.items()
            ]
        }
        for cells in cell_maps
    ]


def rows_of(_ignored, count, /, **cells):
    """`count` identical rows in the shape _index_rows returns.

    The first parameter was the group's availability value and is ignored since
    2026-08-28: the builders take one flat list of rows and no longer see a
    grouping. Kept in the signature so the call sites read the same, because
    what each one is describing is still an endpoint whose availability reads
    that value.

    `cells` maps a metric name to either a verdict string or None for a
    decline, which is the distinction every builder turns on.
    """
    built = [
        {
            "present": True,
            "name": name,
            "abbr": name[:2].upper(),
            "verdict": value,
            "reason": None if value else "cost-ceiling",
            "slug": value if value else verdict_encoding.NOT_MEASURED,
        }
        for name, value in cells.items()
    ]
    return [{"cells": built} for _ in range(count)]


def test_a_state_chip_counts_the_rows_carrying_at_least_one_of_that_state():
    """At least one chip in that state, not every chip, and declines count.

    Changed on 2026-08-28 and the measured numbers are the argument. Over the
    2026-08-24 sweep the old uniform reading gave `indeterminate` 402 and every
    other state 0, because no endpoint on this registry is uniform in anything
    else: six of the seven chips were dead and the seventh answered a question
    nobody asks. At least one gives verified 84, undeclared-but-verified 18,
    declared-but-wrong 7, absent 115, indeterminate 532, not-measured 543, and
    declared-only 0, which is genuinely none rather than an artefact.

    A row with three different chips counts once towards each of the three, and
    a declined chip counts too: that is what makes the "we did not look" chip
    mean the endpoints with a metric no sweep has run.
    """
    built = rows_of("indeterminate", 4, availability="indeterminate",
                    cors="indeterminate", classes=None) + rows_of(
        "verified", 2, availability="verified", cors="absent", classes=None)
    by_slug = {f["slug"]: f["count"] for f in _state_facets(built)}
    assert by_slug["indeterminate"] == 4
    assert by_slug["verified"] == 2, "the row has a verified chip, whatever else it has"
    assert by_slug["absent"] == 2, "and an absent one, so it counts towards both"
    assert by_slug[verdict_encoding.NOT_MEASURED] == 6, "every row declined classes"
    assert by_slug["declared-only"] == 0, "no row carries one, which is a true 0"


def test_the_state_chips_are_the_states_the_legend_lists_and_no_others():
    """The two sequences the template pairs by slug, asserted to be one set.

    `_legend` lists an EIGHTH state whenever this page drew a verdict this build
    has no encoding for. The template reads each legend entry's rows count out
    of this builder's result, so a builder that stopped at the closed seven
    leaves that eighth row stating no count at all, and then revealing a row
    when it is pressed. Keying the mapping by slug does not prevent that;
    emitting the same slugs on the same condition is what prevents it.
    """
    seven = [state.slug for state in verdict_encoding.STATES]
    eight = seven + [verdict_encoding.UNRECOGNISED.slug]
    uniform = group_of("verified", {"availability": "sometime-in-2031"})
    mixed = group_of(
        "verified", {"availability": "verified", "cors": "sometime-in-2031"}
    )
    known = group_of("verified", {"availability": "verified"})

    # A row uniform in the unrecognised state: the count is 1 and it is stated.
    assert [f["slug"] for f in _state_facets(uniform)] == eight
    assert {f["slug"]: f["count"] for f in _state_facets(uniform)}[
        "unrecognised"
    ] == 1
    # And the case a condition on uniformity alone would miss: a row that DREW
    # an unrecognised verdict without being uniform in it. The legend lists the
    # eighth state because a chip on the page is in it, so the mapping carries
    # it too, at the count that is true, which is no rows.
    assert [f["slug"] for f in _state_facets(mixed)] == eight
    # 1 and not 0: the row carries an unrecognised chip beside a verified one,
    # and at-least-one counts it. Under the uniform reading this was 0, and the
    # point of the case is unchanged: the eighth entry is emitted because a cell
    # was DRAWN in that state, which is the same condition _legend lists it on,
    # so neither sequence can grow an entry the other lacks.
    assert {f["slug"]: f["count"] for f in _state_facets(mixed)}[
        "unrecognised"
    ] == 1
    # The other direction, which is what catches an eighth entry always drawn.
    assert [f["slug"] for f in _state_facets(known)] == seven
    # And the condition is the legend's own, over the same rows.
    for case in (uniform, mixed, known):
        drawn = [
            {"slug": cell["slug"]}
            for row in case
            for cell in row["cells"]
            if cell["present"]
        ]
        assert [f["slug"] for f in _state_facets(case)] == [
            entry["slug"] for entry in _legend(drawn)
        ]


def test_the_grid_gains_a_column_for_a_verdict_it_has_no_encoding_for(
    client_for, store_hostile_literals
):
    """The eighth column, on the same condition the legend listed an eighth row.

    A store holding a value this build has no encoding for used to give the
    legend a row with a blank count. The grid inherits the case and the fix: the
    column appears because a cell was DRAWN in that state, and every cell in it
    carries a number.
    """
    page = index(client_for(store_hostile_literals))
    headers = [a["data-facet-value"] for a in with_attribute(page, "data-state")]
    assert "unrecognised" in headers
    for cell in texts_with(page, "data-matrix-cell"):
        assert cell.strip().isdigit()


def test_every_cell_in_the_grid_carries_a_number_or_is_not_pressable(
    client_for, store_registry_sample
):
    """The grid's own version of the blank-count defect.

    A cell either holds a count and is a button, or holds nothing and is not:
    35 of the 56 are empty on the measured sweep, and a button that selects no
    rows is a thing a reader presses twice before believing it.
    """
    page = index(client_for(store_registry_sample))
    for cell in texts_with(page, "data-matrix-cell"):
        assert cell.strip().isdigit() and int(cell) > 0
    for attributes in with_attribute(page, "data-matrix-empty"):
        assert "aria-pressed" not in attributes


def test_the_page_states_no_endpoint_or_metric_count_in_prose(
    client_for, store_registry_and_failure
):
    """Both sentences are gone and both numbers are still machine readable.

    They were removed because every chip now carries its own count and every
    group heading its denominator, so the sentence repeated in words what the
    page states in numbers. The attributes stay: that is where these tests and
    any other reader find them.

    Both halves are read off the elements the template contracts to carry them
    rather than searched for in the document. The first version of this test
    asserted that the substring "endpoints, and" is absent from the whole page,
    which is the anti-pattern test_page's _Texts docstring warns about and which
    says nothing about where the removed sentence was; and it never asserted the
    metric-count sentence was gone at all, nor that the metric count was
    readable, both of which its own docstring promised.
    """
    page = index(client_for(store_registry_and_failure))
    attributes = with_attribute(page, "data-endpoint-count")
    assert len(attributes) == 1
    endpoints = attributes[0]["data-endpoint-count"]
    metrics = attributes[0]["data-metric-count"]
    assert endpoints.isdigit() and metrics.isdigit()
    assert int(endpoints) == len(listed(page))

    # The paragraph that used to open with both counts, read as an element.
    said = texts_with(page, "data-endpoint-count")
    assert len(said) == 1
    assert "measured across" not in said[0], said[0]
    for number in (endpoints, metrics):
        assert number not in said[0], f"{number} is still in prose: {said[0]}"

    # The three group notes, of which the one keyed on NO verdict at all is the
    # only one kept: it says which two cases are in that group, which its
    # heading cannot. A group keyed ON a verdict has a heading that already
    # names that verdict and its denominator, so its note said nothing the
    # heading and the chips above the rows do not.
    # No group carries a note because no group is drawn: the three sections
    # became one listing on 2026-08-28. The two ways into the final group, a
    # DECLINED availability metric and a run that recorded nothing for it, are
    # both still on the row, where one draws a decline chip and the other a gap.
    assert not with_attribute(page, "data-group-note")
    assert not with_attribute(page, "data-group-heading")


def test_every_facet_chip_is_an_unpressed_button(client_for, store_registry_sample):
    """A button, because it changes this page and names no other resource, and
    unpressed on arrival, because the page is correct with no filter applied.

    Two of the four chip groups went on 2026-08-28: the availability panel, whose
    question the grid's availability row answers per state instead of merged into
    two words, and the metric row headers, which became labels. What is left is
    the grid: a column header per state and a cell per pair that holds anything.
    """
    page = index(client_for(store_registry_sample))
    for attributes in with_attribute(page, "data-facet"):
        assert attributes["aria-pressed"] == "false"
    assert not facets(page, "availability")
    assert not facets(page, "metric")
    assert len(facets(page, "state")) >= len(verdict_encoding.STATES)
    assert facets(page, "matrix")


def test_a_metric_name_is_a_label_and_not_a_filter(
    client_for, store_registry_sample
):
    """Removed by the plan owner on 2026-08-28, and pinned as removed.

    Every cell in a metric's row already filters on that metric, so a chip on
    the name could only select the union of its own row: it widened what the
    cells beside it narrow. The abbreviation and the name stay, and so does
    data-metric-column, which is how a reader of this HTML learns which
    abbreviation belongs to which metric.
    """
    page = index(client_for(store_registry_sample))
    key = metric_key(page)
    assert len(key) == 8, key
    for attributes in with_attribute(page, "data-metric-column"):
        assert "aria-pressed" not in attributes
        assert "data-facet" not in attributes


def test_a_grid_cell_is_a_bare_count_and_a_header_is_bare_words(
    client_for, store_registry_sample
):
    """The grid replaced the two chip strips, and with them the run-together
    accessible name they had ("not available486").

    A cell's whole text is its number and the row and column headers supply the
    words, which a table announces from the headers rather than from the cell.
    The availability chips keep the space, because they are still a label with a
    number after it.
    """
    page = index(client_for(store_registry_sample))
    for cell in texts_with(page, "data-matrix-cell"):
        assert cell.strip().isdigit()
    for label in facets(page, "availability").values():
        assert "  " not in label
        digits = "".join(c for c in label if c.isdigit())
        assert not digits or f" {digits}" in label


def test_the_count_contract_paragraph_says_nothing_false(
    client_for, store_registry_sample
):
    """Emptied by the plan owner on 2026-08-28, and the element left in place.

    It used to state the contract the counts keep: any within a group, all
    across groups, each count conditioned on the other groups' filters. What is
    gone is the only place the page explained why a count moves when another
    chip is pressed. The element stays because nothing reads it and an empty
    note breaks nothing.
    """
    page = index(client_for(store_registry_sample))
    said = texts_with(page, "data-facet-contract")
    assert len(said) <= 1
    if said:
        assert said[0].strip() == ""
    assert "what pressing it filters to" not in page


def test_the_grid_is_the_only_facet_panel(client_for, store_registry_sample):
    """One panel where there were three.

    The metric strip and the state legend became the grid's headers, and the
    availability panel went because the grid's availability row says the same
    thing per state instead of merging indeterminate and absent into one word.
    """
    page = index(client_for(store_registry_sample))
    order = [a["data-facet-group"] for a in with_attribute(page, "data-facet-group")]
    assert order == ["matrix"]
    assert "<h2>Availability</h2>" not in page
    assert "<h2>Metrics and states</h2>" in page


def test_a_filter_that_matches_nothing_has_a_sentence_ready(
    client_for, store_registry_sample
):
    """Hidden on arrival and present in the document, so the script never has to
    build markup: an empty page under a header naming 543 endpoints would leave
    a reader guessing whether the page broke.

    THE HIDDEN ATTRIBUTE IS THE ASSERTION. Remove it and every reader of a page
    of 543 rows sees "No endpoint in this store matches every filter selected
    above" standing permanently above them, which the first version of this
    test, under this same docstring, could not tell: it asserted that the id and
    the sentence are in the document and nothing more.
    """
    page = index(client_for(store_registry_sample))
    attributes = with_attribute(page, "data-facet-empty")
    said = texts_with(page, "data-facet-empty")

    assert len(attributes) == 1 and len(said) == 1
    assert "hidden" in attributes[0], attributes[0]
    assert attributes[0]["id"] == "nothing", "the script looks it up by id"
    assert "matches every filter selected above" in said[0]


def test_a_store_with_no_endpoints_offers_no_chip_to_press():
    """No rows, so no facet, and nothing implying a filter is hiding them.

    The availability and metric panels were guarded on the endpoint count and
    the state panel was not, so this page rendered seven pressable chips reading
    "0 chips 0 rows"; pressing one said "No endpoint in this store matches every
    filter selected above ... clear one to widen it", which implies clearing it
    would reveal something, on a store the paragraph above has just said holds
    nothing at all.

    Asked of the renderer rather than through a client, because a store holding
    no quads is one _opened_store refuses: this page is reachable only for a
    store whose runs measured nothing, and it has to be right there too.
    """
    # An empty store, not just an empty entry list: the renderer now asks the
    # store which endpoints the explorer has vocabulary for, so a page built
    # from no entries still needs one to ask.
    page = _index_html([], Store())

    assert with_attribute(page, "data-facet") == []
    assert with_attribute(page, "data-state") == []
    assert with_attribute(page, "data-facet-group") == []
    assert with_attribute(page, "data-facet-contract") == []
    assert "clear one to widen it" not in page
    assert "nothing to list" in page


def test_every_cell_carries_exactly_one_encoding_class(
    client_for, store_registry_sample, store_hostile_literals, store_dormant_newest
):
    """One enc- token per cell, because the script reads the state off the class.

    ``cell.className.replace(/^enc-/, "")`` is the whole of how a state chip
    knows what a row is in. A second class on a cell makes that return
    "verified something-else", which matches no state, on every row, with no
    error anywhere: the state chips would all read 0 and the page would look
    like a sweep that measured nothing.
    """
    for store in (store_registry_sample, store_hostile_literals, store_dormant_newest):
        page = index(client_for(store))
        seen = 0
        for row in grouped_rows(page):
            for chip in row["chips"]:
                encoding_class(chip)
                seen += 1
        assert seen, "no cell on this page carries a class at all"


def test_every_chip_count_is_the_rows_the_page_renders(
    client_for,
    store_registry_sample,
    store_registry_and_failure,
    store_hostile_literals,
    store_dormant_newest,
):
    """THE TEST THAT CONNECTS THE THREE BUILDERS TO THE PAGE.

    Every other test of the builders hands them a group dict written by hand, so
    all of them would pass over a page that rendered different rows, a different
    legend or a different set of chips. This one reads the rendered page, groups
    its cells by row and by encoding class, applies the predicate the script at
    the foot of the page applies, and asserts that each chip's printed count is
    the number of rendered rows that predicate selects.

    It needs no browser. What it cannot see is the recount the script does when
    a filter is pressed; what it does see is the number every reader arrives at,
    which is the number the page's own sentence about pressing a chip is about.

    Four stores, and each adds a shape: the nine-endpoint registry sample is
    heterogeneous in every metric, registry-and-failure adds the group keyed on
    no availability verdict at all, hostile-literals adds the eighth legend
    entry, and the dormant store adds rows the newest sweep declined to ask,
    which keep the verdicts their last real probe produced.
    """
    positive = ("verified", "undeclared-but-verified")
    for store in (
        store_registry_sample,
        store_registry_and_failure,
        store_hostile_literals,
        store_dormant_newest,
    ):
        page = index(client_for(store))
        rendered = grouped_rows(page)
        printed = facet_counts(page)
        abbreviations = set(metric_key(page).values())

        # Every row on the page is inside a group element, which is what the
        # script depends on and what this reader refuses to fake.
        assert [row["endpoint"] for row in rendered] == listed(page)
        # No metric group and no availability group since 2026-08-28: the row
        # headers are labels and the availability panel is gone. What the grid
        # projects is a column per state and a cell per pair that holds
        # anything, and every cell's abbreviation must be one of the page's own.
        assert not {group for group, _ in printed} - {"state", "matrix"}
        assert {
            value.split("|")[0] for group, value in printed if group == "matrix"
        } <= abbreviations

        expected = {key: 0 for key in printed}
        for row in rendered:
            # EVERY drawn chip, a declined one included, because the state
            # filter counts a row that carries the state anywhere. The metric
            # set above is the one that skips a decline.
            states = {encoding_class(chip) for chip in row["chips"]}
            for slug in states:
                expected[("state", slug)] += 1
            # And the grid: this metric in this state, keyed the way the cell
            # is. Derived from the row's own chips rather than from the builder,
            # which is the whole point of this test.
            for chip in row["chips"]:
                key = ("matrix", f"{chip['abbr'].strip()}|{encoding_class(chip)}")
                if key in expected:
                    expected[key] += 1

        assert printed == expected

# ---------------------------------------------------------------------------
# One listing, and the tooltips that replaced the prose
# ---------------------------------------------------------------------------


def test_the_page_is_one_listing_with_no_headings(
    client_for, store_registry_and_failure
):
    """Three sections became one on 2026-08-28.

    What the headings said, "availability verified: 57 of 543 endpoints" and two
    more, the grid's availability row now says per state: 57, 482 and 4, keeping
    apart the two that an available-or-not reading merged.
    """
    page = index(client_for(store_registry_and_failure))
    assert not with_attribute(page, "data-group-heading")
    assert not with_attribute(page, "data-availability")
    assert len(with_attribute(page, "data-listing")) == 1
    assert grouped_rows(page), "one listing, and it holds the rows"


def test_the_listing_is_alphabetical_and_ranks_nothing(
    client_for, store_registry_and_failure
):
    """The order the removal forces as a decision.

    Rows came out in the encoding table's order while the groups were labelled,
    and app.py's own comment said that order was "not any notion of better or
    worse" because "ranking the groups would be this service's opinion about the
    endpoints". Unlabelled, that order is an unexplained ranking with the
    sentence that excused it deleted, so the listing is the endpoint's own url.
    """
    page = index(client_for(store_registry_and_failure))
    listed_urls = [row["endpoint"] for row in grouped_rows(page)]
    assert listed_urls == sorted(listed_urls)


def test_every_metric_name_carries_what_the_metric_asks(
    client_for, store_registry_sample
):
    """A tooltip on the row label, from METRIC_DESCRIPTIONS."""
    page = index(client_for(store_registry_sample))
    labelled = with_attribute(page, "data-metric-column")
    assert labelled
    for attributes in labelled:
        name = attributes["data-metric-column"]
        if name in METRIC_DESCRIPTIONS:
            assert attributes["title"] == METRIC_DESCRIPTIONS[name]


# Kinds in prober/metrics.toml that publish no measurement row. Kept as a set
# rather than a single string so that adding another non-measuring kind is one
# edit here, and mirrors ProbeKind::yields_measurement returning false.
_KINDS_WITHOUT_A_COLUMN = {"ClassProfile"}


def test_the_metric_descriptions_are_the_probers_own_labels():
    """The pin that keeps the two from drifting.

    prober/metrics.toml is the source and the run graphs do not carry it: a
    measurement names its metric by IRI and nothing publishes a label for that
    IRI, so the web tier cannot read these out of the store the way it reads
    everything else it says. Same shape as test_about.py's politeness numbers,
    and the same answer: keep the words in app.py and fail here when they drift.
    """
    metrics = (
        Path(__file__).resolve().parents[2] / "prober" / "metrics.toml"
    ).read_text()
    labels = {}
    for block in metrics.split("[[metric]]")[1:]:
        found_id = re.search(r'^id = "([^"]+)"', block, re.M)
        found_label = re.search(r'^label = "([^"]+)"', block, re.M)
        found_kind = re.search(r'^kind = "([^"]+)"', block, re.M)
        # A kind that produces no measurement produces no column, so it has
        # nothing for this table to describe. `class-profiles` is the case:
        # its pass publishes content samples and profiles instead of a verdict,
        # so the index never derives a row label for it and a description here
        # would be a tooltip on nothing. The prober says the same thing in
        # ProbeKind::yields_measurement, which is the other half of this rule.
        if found_kind and found_kind.group(1) in _KINDS_WITHOUT_A_COLUMN:
            continue
        if found_id and found_label:
            labels[found_id.group(1)] = found_label.group(1)
    assert labels, "no metric in prober/metrics.toml carries a label"
    # Every metric the prober measures must be described, and a description may
    # outlive the probe: see test_docs.py's retirement rule, which is where the
    # marking is enforced. The store keeps publishing a retired metric's
    # measurements and the index keeps deriving a column from them.
    for metric, label in labels.items():
        assert METRIC_DESCRIPTIONS.get(metric) == label, (
            f"{metric}: the page says {METRIC_DESCRIPTIONS.get(metric)!r}, "
            f"prober/metrics.toml says {label!r}"
        )


def test_every_state_column_carries_its_meaning(client_for, store_registry_sample):
    """A tooltip on the column header, from verdict_encoding.

    These were printed as prose under the panel until the plan owner removed it,
    so the meanings did not go: they moved onto the thing they describe.
    """
    page = index(client_for(store_registry_sample))
    headers = with_attribute(page, "data-state")
    assert headers
    for attributes in headers:
        state = verdict_encoding.presentation(attributes["data-state"])
        assert attributes["title"] == state.meaning


def test_no_prose_stands_under_the_grid(client_for, store_registry_sample):
    """Removed by the plan owner on 2026-08-28.

    Both paragraphs went: the one explaining what a count is and the one
    explaining what the borders mean. The second one's content is now on the
    column headers, one state at a time. The first one's is not anywhere, which
    is the cost: nothing on the page now says a count is conditioned on the
    other filters, and the counts still are.
    """
    page = index(client_for(store_registry_sample))
    assert "A cell is the endpoints whose newest sweep" not in page
    assert "A filled chip means" not in page


def test_the_state_labels_wear_their_own_encoding(client_for, store_registry_sample):
    """Each state label carries its own colour and line style.

    The header held a swatch beside words in the default ink until 2026-09-01,
    which put the marker next to the thing it marked instead of on it. Both
    classes are generated from verdict_encoding's one table, so a state added
    without a text colour fails here rather than rendering unmarked.
    """
    page = index(client_for(store_registry_sample))
    labels = re.findall(r'class="f-head-label ([^"]+)"', page)
    assert labels, "no state labels in the matrix header"
    for classes in labels:
        names = classes.split()
        chip = [n for n in names if n.startswith("enc-") and not n.startswith("enc-text-")]
        text = [n for n in names if n.startswith("enc-text-")]
        assert len(chip) == 1 and len(text) == 1, classes
        # Same state on both, or the label would wear one state's colour and
        # another's border.
        assert text[0] == "enc-text-" + chip[0][len("enc-"):], classes


def test_the_state_labels_are_not_rotated(client_for, store_registry_sample):
    """Horizontal since 2026-09-01, and allowed to wrap.

    They were rotated with writing-mode so seven long labels could sit over
    narrow count columns. Asserted because the rule is easy to reintroduce while
    tidying CSS, and rotated text is what this change exists to remove.
    """
    page = index(client_for(store_registry_sample))
    # The RULE, not the string. A first cut searched the page for
    # "writing-mode" and matched the comment that explains its removal, which is
    # a test that fails on its own documentation.
    rule = re.search(r"\.f-head-label\s*\{([^}]*)\}", page)
    assert rule, "no .f-head-label rule"
    body = rule.group(1)
    assert "writing-mode" not in body, body
    assert "transform" not in body, body
    assert "white-space: normal" in body, body


def test_absent_sits_before_indeterminate(client_for, store_registry_sample):
    """The order the plan owner asked for on 2026-09-01.

    It carries no ranking, but it does group: absent ends the run of states that
    are ANSWERS about the endpoint, and indeterminate begins the two that say we
    do not know. Asserted against verdict_encoding rather than as a literal list,
    so the canonical table stays the only place the order is decided.
    """
    page = index(client_for(store_registry_sample))
    shown = re.findall(r'data-state="([a-z-]+)"', page)
    assert shown == [state.slug for state in verdict_encoding.STATES]
    assert shown.index("absent") < shown.index("indeterminate")


def test_each_states_count_sits_below_its_label(client_for, store_registry_sample):
    """The count is under the label, not beside it, as of 2026-09-01.

    A metric total under the metric name was added and removed the same day: it
    was the wrong axis. The counts that belong under something are the states',
    because each heads the column of cells it totals.

    Asserted on the ORDER inside the button and on the rule that stacks them,
    since either alone would pass on a layout that looks nothing like this: the
    markup order is the same whether the button is a row or a column.
    """
    page = index(client_for(store_registry_sample))
    button = re.search(r'<button[^>]*class="facet facet-head".*?</button>', page, re.S)
    assert button, "no state header button"
    inner = button.group(0)
    assert inner.index("f-head-label") < inner.index("f-count"), (
        "the label must come before the count"
    )
    rule = re.search(r"\.facet-head\s*\{([^}]*)\}", page)
    assert rule and "flex-direction: column" in rule.group(1), rule and rule.group(1)


def test_the_states_count_is_dressed_exactly_like_a_cell(client_for, store_registry_sample):
    """The header's count wears the state's own encoding, like the cells under it.

    It was neutral for one commit, on the argument that a column total is not a
    measurement and should not look like one. The plan owner asked for it to match
    the column, and consistency wins: a reader scanning a column sees one shape
    from the header to the last row.

    The class must be the state's OWN, or the header would head its column in
    another state's colours.
    """
    page = index(client_for(store_registry_sample))
    heads = re.findall(r'<button[^>]*class="facet facet-head".*?</button>', page, re.S)
    assert heads, "no state header buttons"
    for inner in heads:
        slug = re.search(r'data-state="([a-z-]+)"', inner).group(1)
        count = re.search(r'<span class="([^"]*f-count[^"]*)"', inner)
        assert count, inner[:120]
        classes = count.group(1).split()
        assert verdict_encoding.css_class(slug) in classes, (slug, classes)


def test_the_state_columns_are_centred(client_for, store_registry_sample):
    """Label, count and cells on one axis.

    The column's width is set by the label, which is wider than a count, so
    without centring the counts sat left of the cells they head.
    """
    page = index(client_for(store_registry_sample))
    head = re.search(r"\.facet-head\s*\{([^}]*)\}", page)
    assert head and "align-items: center" in head.group(1), head and head.group(1)
    assert re.search(
        r"table\.matrix thead th:not\(:first-child\)\s*\{[^}]*text-align: center",
        page,
    ), "the state headers must centre over their columns"


def test_no_metric_total_chip_remains(client_for, store_registry_sample):
    """Added and removed on 2026-09-01. Asserted so it does not come back by
    someone reading the commit that added it and not the one that took it out."""
    page = index(client_for(store_registry_sample))
    assert "mtotal" not in page
    assert "data-metric-total" not in page


def _endpoints_of(store):
    rows = store.query(
        "PREFIX dqv: <http://www.w3.org/ns/dqv#> "
        "SELECT DISTINCT ?e WHERE { GRAPH ?g { ?m dqv:computedOn ?e } }"
    )
    return {r["e"].value for r in rows}


# ---------------------------------------------------------------------------
# A metric with no verdict has no column
# ---------------------------------------------------------------------------


def test_a_metric_that_publishes_no_verdict_gets_no_column(store_content_profiles):
    """The matrix is a grid of VERDICTS, and class-profiles publishes none.

    Found by reading the live page on 2026-09-05. The column existed, because
    _index_metrics unions every metric carrying a verdict OR a decline and one
    endpoint's profile pass had failed, contributing a decline. So the column
    was drawn with exactly one filled cell: the FAILURE. The two endpoints
    whose passes succeeded, with 5 and 199 class profiles between them, drew a
    gap, whose documented meaning is that the run "recorded nothing at all
    about that metric, neither a measurement nor a decline".

    That is the opposite of what happened, stated by the page as a positive
    claim, which is the specific failure this project exists to prevent. There
    is no verdict to draw instead: Ruling 2 says a profile is a description and
    the six-verdict vocabulary is for what was observed. So the honest fix is no
    column, and the profile's presence is carried by the row's `content` link,
    which is already right for all three endpoints.
    """
    from app import _index_metrics
    from endpoint_index import endpoint_index
    from endpoint_measurements import DeclinedMetric

    entries = endpoint_index(store_content_profiles)
    assert entries, "the fixture describes at least one endpoint"

    # The decline is added HERE rather than taken from the fixture, because in
    # that run the profile pass SUCCEEDED, so no decline exists and the test
    # would pass without exercising anything. This is the shape the live store
    # had on 2026-09-05: one endpoint's pass failed and opened the column.
    entries[0].declined.append(
        DeclinedMetric(
            metric="urn:sparqlwatch:metric:class-profiles",
            reason="enumeration-failed",
        )
    )
    metrics = {m["metric"] for m in _index_metrics(entries)}
    assert "urn:sparqlwatch:metric:class-profiles" not in metrics, (
        f"a verdictless metric must not be a column in a verdict grid: {sorted(metrics)}"
    )
    assert metrics, "the other metrics still have columns"


def test_a_declined_verdictless_metric_does_not_conjure_a_column(store_declined):
    """The route the column got in by, closed.

    A decline alone used to be enough to add a column, so a metric that can
    never produce a verdict appeared the moment one endpoint failed to run it.
    Asked against a store whose declines are of ORDINARY metrics, so this also
    pins that a decline still opens a column for a metric that does measure.
    """
    from app import METRIC_DOCS, _index_metrics
    from endpoint_index import endpoint_index

    metrics = {m["metric"] for m in _index_metrics(endpoint_index(store_declined))}
    verdictless = {
        "urn:sparqlwatch:metric:" + name
        for name, facts in METRIC_DOCS.items()
        if not facts.get("yields_measurement", True)
    }
    assert verdictless, "METRIC_DOCS must mark at least one metric as verdictless"
    assert not (metrics & verdictless), f"{sorted(metrics & verdictless)} have no verdict to draw"


# ---------------------------------------------------------------------------
# Column order, and what a row says about size
# ---------------------------------------------------------------------------


def test_the_columns_are_in_reading_order_not_alphabetical():
    """Alphabetical put class-count second and cors third: a reading order
    decided by spelling. This groups by the question a reader is asking."""
    from app import METRIC_COLUMN_ORDER, _column_rank

    M = "urn:sparqlwatch:metric:"
    shuffled = [M + n for n in sorted(METRIC_COLUMN_ORDER)]
    assert [m.removeprefix(M) for m in sorted(shuffled, key=_column_rank)] == list(
        METRIC_COLUMN_ORDER
    )


def test_a_metric_this_build_does_not_know_still_gets_a_column():
    """A newer prober's metric must not vanish from the grid. An unexplained
    column at the end is the lesser error, and the same call _yields_measurement
    makes about an unknown kind."""
    from app import METRIC_COLUMN_ORDER, _column_rank

    M = "urn:sparqlwatch:metric:"
    order = sorted([M + "availability", M + "zz-from-the-future"], key=_column_rank)
    assert order[-1].endswith("zz-from-the-future"), "unknown metrics sort last"
    assert _column_rank(M + "zz-from-the-future")[0] == len(METRIC_COLUMN_ORDER)


def _entry_with(**counts):
    from endpoint_measurements import EndpointMeasurements, MetricVerdict

    M = "urn:sparqlwatch:metric:"
    return EndpointMeasurements(
        endpoint="https://e.example/sparql",
        assessed=True,
        run="urn:sparqlwatch:run:x",
        generated_at="2026-09-05T00:00:00Z",
        verdicts=[
            MetricVerdict(metric=M + k, verdict="undeclared-but-verified",
                          observed_count=v)
            for k, v in counts.items()
        ],
    )


def test_a_row_states_what_was_counted_largest_unit_first():
    from app import _row_size

    got = _row_size(_entry_with(**{
        "class-count": 205, "triple-count": 12_510_784, "graph-count": 46,
    }))
    assert [(s["n"], s["unit"]) for s in got] == [
        (12_510_784, "triples"), (46, "graphs"), (205, "classes")
    ]


def test_a_row_shows_the_counted_number_and_never_the_declared_one():
    """A row is this service's own reading. Showing an endpoint's CLAIM in the
    listing, beside a verdict saying the claim is wrong, would put the wrong
    number where a reader actually looks."""
    from app import _row_size
    from endpoint_measurements import EndpointMeasurements, MetricVerdict

    M = "urn:sparqlwatch:metric:"
    entry = EndpointMeasurements(
        endpoint="https://e.example/sparql", assessed=True,
        run="urn:sparqlwatch:run:x", generated_at="2026-09-05T00:00:00Z",
        verdicts=[MetricVerdict(metric=M + "triple-count", verdict="declared-but-wrong",
                                declared_count=1_000_000, observed_count=12_500_000)],
    )
    assert _row_size(entry) == [{"n": 12_500_000, "unit": "triples"}]


def test_a_row_nobody_counted_says_nothing_rather_than_zero():
    """At the default cheap ceiling nothing counts anything, so this is every
    row. "0 triples" for an endpoint nobody counted is the confident false
    negative this project exists to prevent."""
    from app import _row_size
    from endpoint_measurements import EndpointMeasurements, MetricVerdict

    M = "urn:sparqlwatch:metric:"
    entry = EndpointMeasurements(
        endpoint="https://e.example/sparql", assessed=True,
        run="urn:sparqlwatch:run:x", generated_at="2026-09-05T00:00:00Z",
        verdicts=[MetricVerdict(metric=M + "triple-count", verdict="indeterminate")],
    )
    assert _row_size(entry) == []


# ---------------------------------------------------------------------------
# The overview grid
# ---------------------------------------------------------------------------


def test_the_overview_grid_does_not_look_like_an_endpoint_row(client_for, store_two_sweeps):
    """The grid's rows carry their own attribute, not data-endpoint.

    They shared it when the grid was written, so the page counted eleven
    endpoints as twenty-two and the facet filters, which select
    [data-endpoint] in three places, would have hidden grid rows along with
    listing rows.
    """
    body = client_for(store_two_sweeps).get("/").text
    listed = re.findall(r'<li data-endpoint="([^"]+)"', body)
    grid = re.findall(r'data-fleet-endpoint="([^"]+)"', body)
    assert len(re.findall(r'data-endpoint="', body)) == len(listed), (
        "only listing rows may carry data-endpoint"
    )
    if grid:
        assert set(grid) <= set(listed), "the grid shows endpoints the listing has"


def test_the_overview_draws_in_the_sites_own_encoding(client_for, store_two_sweeps):
    """A state in the grid is the same fact as a state in the matrix below."""
    import verdict_encoding

    body = client_for(store_two_sweeps).get("/").text
    drawn = set(re.findall(r'class="fcell (enc-[a-z-]+)"', body))
    if not drawn:
        pytest.skip("this fixture has one sweep, so no grid is drawn")
    known = {"enc-" + s.slug for s in verdict_encoding.STATES}
    assert drawn <= known, f"{drawn - known} is not a state this site defines"


def test_a_filtered_grid_names_no_endpoint_the_filtered_rows_do_not(
    client_for, store_dormant_newest
):
    """The regression this test is written against: the grid was built from
    `history.changed`, read straight from the store and never passed through
    `_matches_query`, so a `?q=` that narrowed the listing left the grid
    showing -- and linking, via `row.href` -- an endpoint the same page's
    other representation (and its own rows) had just dropped.

    store_dormant_newest is the closest committed fixture to what this
    wants. Checked directly (by loading every multi-run fixture combination
    this suite has against `fleet_history`): none of them has more than ONE
    endpoint that ever reads differently between sweeps. This store's one is
    https://ontop.certain.ai.ustp.at/sparql. A single changed endpoint cannot
    show a query narrowing the grid from two rows to one, so this asks the
    two queries that together still pin the bug down: one that excludes the
    endpoint that changed (the grid must come up EMPTY, not still name it --
    this is the exact shape of the bug this was measured against, `?q=
    kadaster` naming ontop) and one that matches it (the grid must still draw
    it, ruling out the wrong fix of always emptying the grid under a
    filter).
    """
    client = client_for(store_dormant_newest)

    excluding = client.get("/?q=kadaster", headers={"accept": "text/html"}).text
    rows = set(re.findall(r'data-endpoint="([^"]+)"', excluding))
    grid = set(re.findall(r'data-fleet-endpoint="([^"]+)"', excluding))
    assert rows == {"https://data.kkg.kadaster.nl/query"}, rows
    assert grid <= rows, f"the grid names {grid - rows}, which the rows do not"
    assert grid == set(), (
        "the one endpoint that ever changed was filtered out of the rows; "
        "the grid must not still draw it"
    )

    matching = client.get("/?q=ontop", headers={"accept": "text/html"}).text
    rows2 = set(re.findall(r'data-endpoint="([^"]+)"', matching))
    grid2 = set(re.findall(r'data-fleet-endpoint="([^"]+)"', matching))
    assert grid2 <= rows2, f"the grid names {grid2 - rows2}, which the rows do not"
    assert grid2 == {"https://ontop.certain.ai.ustp.at/sparql"}, grid2


def test_a_domain_only_filter_states_the_fleet_ledes_denominator(
    client_for, store_dormant_newest, monkeypatch
):
    """Blocker-4 of the whole-branch review: the fleet lede's two "of N"
    clauses guarded on `{% if query %}` alone, so `?domain=` or `?facet=`
    narrowing the fleet left "All 2 read the same way every time." on the
    page -- correct English for the whole, unfiltered fleet, and printed on
    a page that was not it. `summary.matching` and `_no_match_description`
    were both already widened to `q or domain or facet`; this pins the same
    rule in the template.

    qlever and kadaster (this fixture's two STEADY endpoints -- ontop is the
    one that changed, per test_a_filtered_grid_names_no_endpoint_the_
    filtered_rows_do_not above) are given a shared domain here so `?domain=`
    narrows the fleet to them alone, dropping ontop and, with it, every
    "changed" row -- exactly the shape that renders the "All N read the same
    way" branch this bug was found in.
    """
    from registry_names import Name

    monkeypatch.setattr(
        app_module,
        "_NAMES",
        {
            "https://qlever.dev/api/osm-planet": Name(
                title=None, domain="test_domain", datasets=None,
                host="qlever.dev",
            ),
            "https://data.kkg.kadaster.nl/query": Name(
                title=None, domain="test_domain", datasets=None,
                host="data.kkg.kadaster.nl",
            ),
        },
    )
    body = client_for(store_dormant_newest).get(
        "/?domain=test_domain", headers={"accept": "text/html"}
    ).text
    lede = re.search(r'<p class="lede">(.*?)</p>', body, re.DOTALL).group(1)
    normalized = " ".join(lede.split())
    assert "All 2 of 3 read the same way every time." in normalized, normalized


def test_a_single_sweep_store_draws_no_overview(client_for, store):
    """One sweep is not a history, and a one-column grid implies a trend from
    one observation."""
    body = client_for(store).get("/").text
    assert 'data-section="overview"' not in body


def test_the_index_leads_with_the_fleet_in_four_figures(client_for, store):
    body = client_for(store).get("/", headers={"accept": "text/html"}).text
    figures = texts_with(body, "data-figure")
    assert len(figures) == 4, (
        f"the strip states endpoints, answering, not answering and freshness; "
        f"got {figures}"
    )


def test_the_strips_figures_are_coloured_by_data_figure_not_by_a_borrowed_verdict_class(
    client_for, store
):
    """The two counted figures must actually be coloured, and not by
    reusing the verdict encoding's enc-text- classes.

    They were coloured with `enc-text-verified` / `enc-text-declared-but-wrong`
    until this fix: `.strip .n`'s specificity (0,2,0) beats an enc-text- class's
    (0,1,0), so both classes were dead weight and both figures rendered in
    --text-bright regardless of value. A CSS-string assertion would not have
    caught that -- the bug was in the cascade, not in whether a rule existed --
    so this reads the markup two ways: the spans must not carry an enc-text-
    class at all (that is the regression that reintroduces the conflict, and
    it is also the wrong axis: enc-text- names a per-metric VERDICT and this
    strip states fleet-wide LIVENESS), and the page must declare the
    data-figure-scoped rules that do win.
    """
    body = client_for(store).get("/", headers={"accept": "text/html"}).text
    figures = {
        attrs["data-figure"]: attrs.get("class", "")
        for attrs in with_attribute(body, "data-figure")
    }
    for name in ("answering", "not-answering"):
        classes = figures[name].split()
        assert not any(c.startswith("enc-text-") for c in classes), (
            f"{name} still borrows a verdict class: {classes}"
        )
    assert '.strip .n[data-figure="answering"] { color: var(--good); }' in body
    assert (
        '.strip .n[data-figure="not-answering"] { color: var(--crit); }' in body
    )


def test_the_index_declares_no_tokens_and_no_inline_verdict_css(client_for, store):
    body = client_for(store).get("/", headers={"accept": "text/html"}).text
    assert "--accent:" not in body
    assert ".enc-verified" not in body, (
        "the verdict rules are generated into the cached stylesheet now; "
        "inline they would be re-sent on every page view"
    )
    assert app_module.STYLESHEET_PATH in body


# ---------------------------------------------------------------------------
# ?q= : a server-side filter that lives in the URL
# ---------------------------------------------------------------------------


def endpoints_shown(body):
    """Each rendered row's endpoint URL, read off `data-endpoint`.

    Named differently from the `rows_of` above: that helper already exists in
    this file and builds SYNTHETIC row dicts for `_state_facets` (see
    test_the_state_facets_count_every_chip_a_row_carries above), a completely
    different job. Reusing its name for "rows on a rendered page" would shadow
    it.
    """
    return [attrs["data-endpoint"] for attrs in with_attribute(body, "data-endpoint")]


def test_a_query_narrows_the_rows_without_javascript(client_for, store_registry_sample):
    client = client_for(store_registry_sample)
    everything = endpoints_shown(client.get("/", headers={"accept": "text/html"}).text)
    narrowed = endpoints_shown(
        client.get("/?q=uniprot", headers={"accept": "text/html"}).text
    )
    assert 0 < len(narrowed) < len(everything), (
        "?q= must be a server-side filter; a fixture yielding all or none "
        "proves nothing"
    )
    assert all("uniprot" in r.lower() for r in narrowed)


def test_a_filtered_page_states_its_denominator(client_for, store_registry_sample):
    """"18 endpoints" on a filtered page would misreport the fleet.

    The reader is looking at a subset. Every figure describes the subset, and
    every figure says what it is a subset of.
    """
    body = client_for(store_registry_sample).get(
        "/?q=uniprot", headers={"accept": "text/html"}
    ).text
    total = texts_with(body, "data-figure")[0]
    assert " of " in total, f"the count reads {total!r}, with no denominator"


def test_an_unfiltered_page_states_no_denominator(client_for, store_registry_sample):
    """The flip side: a page nobody filtered should not read "9 of 9"."""
    body = client_for(store_registry_sample).get(
        "/", headers={"accept": "text/html"}
    ).text
    total = texts_with(body, "data-figure")[0]
    assert " of " not in total, f"the count reads {total!r} with no filter applied"


def test_the_query_survives_in_the_input(client_for, store_registry_sample):
    """Otherwise the client-side enhancement re-filters with an empty needle
    and instantly widens the list the server just narrowed."""
    body = client_for(store_registry_sample).get(
        "/?q=uniprot", headers={"accept": "text/html"}
    ).text
    assert 'value="uniprot"' in body


def test_matches_query_is_case_insensitive_and_permissive_when_empty():
    assert _matches_query("https://sparql.UniProt.org/sparql", "uniprot")
    assert _matches_query("https://sparql.uniprot.org/sparql", "UNIPROT")
    assert not _matches_query("https://sparql.uniprot.org/sparql", "wikidata")
    assert _matches_query("https://sparql.uniprot.org/sparql", None)
    assert _matches_query("https://sparql.uniprot.org/sparql", "")
    assert _matches_query("https://sparql.uniprot.org/sparql", "  uniprot  ")


def test_matches_facet_reads_the_vocabulary_described_verdict():
    """No committed fixture carries a `vocabulary-described` verdict (it is
    the one `exhaustive`-tier metric none of the cheap-ceiling sweeps these
    fixtures were cut from ever ran), so this exercises `_matches_facet`'s
    "void" branch directly against a hand-built entry rather than through a
    store. Permissive when facet is falsy, same as `_matches_query`."""
    verified = EndpointMeasurements(
        endpoint="https://example.org/sparql",
        assessed=True,
        verdicts=[
            MetricVerdict(
                metric="urn:sparqlwatch:metric:vocabulary-described",
                verdict="verified",
            )
        ],
    )
    silent = EndpointMeasurements(endpoint="https://other.example/sparql", assessed=True)
    assert _matches_facet(verified, "void")
    assert not _matches_facet(silent, "void")
    assert _matches_facet(verified, None)
    assert _matches_facet(silent, None)
    assert _matches_facet(verified, "")
    assert not _matches_facet(verified, "not-a-real-facet")


def test_no_title_is_published_as_rdf(client_for, store_registry_sample):
    """A catalogue's title is not a measurement and does not enter the graph."""
    rdf = client_for(store_registry_sample).get(
        "/", headers={"accept": "text/turtle"}
    ).text
    assert "dcterms:title" not in rdf
    assert "http://purl.org/dc/terms/title" not in rdf


# ---------------------------------------------------------------------------
# ?q= matching nothing: the gap the coordinator's fix-round-1 measurement
# found. Filtering `entries` before anything else derives from them (see
# `_index_context`) meant `endpoint_count` -- guarding both the page's own
# one-sentence description of itself at index.html:334 and the facet-empty
# note at index.html:549 -- followed the filter too. A ?q= that matched
# nothing then hid the sentence explaining what this page is, on the one page
# where a reader most needs it, and left a near-empty page with no stated
# reason. `endpoint_count` now stays the unfiltered count (a fact about the
# store), and a dedicated message names the query and offers a way back.
# ---------------------------------------------------------------------------


DESCRIPTION_SENTENCE = "measured rather than asserted"


def test_the_sites_description_survives_a_query_that_matches_nothing(
    client_for, store_registry_sample
):
    body = client_for(store_registry_sample).get(
        "/?q=zzzznomatch", headers={"accept": "text/html"}
    ).text
    assert DESCRIPTION_SENTENCE in body, (
        "a query matching no endpoint must not hide the page's own "
        "description of itself"
    )


def test_a_query_matching_nothing_renders_its_own_message(
    client_for, store_registry_sample
):
    body = client_for(store_registry_sample).get(
        "/?q=zzzznomatch", headers={"accept": "text/html"}
    ).text
    messages = texts_with(body, "data-no-match")
    assert len(messages) == 1, "the no-match message must render exactly once"
    assert "zzzznomatch" in messages[0], (
        f"the message does not name the query that matched nothing: "
        f"{messages[0]!r}"
    )


def test_only_a_no_match_query_renders_the_no_match_message(
    client_for, store_registry_sample
):
    client = client_for(store_registry_sample)
    unfiltered = client.get("/", headers={"accept": "text/html"}).text
    narrowed = client.get("/?q=uniprot", headers={"accept": "text/html"}).text
    assert not with_attribute(unfiltered, "data-no-match")
    assert not with_attribute(narrowed, "data-no-match")


def test_a_domain_matching_nothing_renders_its_own_message(
    client_for, store_registry_sample
):
    """Fix-round-1's finding: ?domain= is a server-side filter exactly like
    ?q= and can narrow a page to zero rows the same way, so a domain that
    matches nothing must get the same explanation -- and it must name the
    DOMAIN, not a query string that was never set. Before the fix this
    message was keyed to `query` alone and read "matches &ldquo;&rdquo;" on
    exactly this page."""
    body = client_for(store_registry_sample).get(
        "/?domain=zzzznodomain", headers={"accept": "text/html"}
    ).text
    messages = texts_with(body, "data-no-match")
    assert len(messages) == 1, "the no-match message must render exactly once"
    assert "zzzznodomain" in messages[0], (
        f"the message does not name the domain that matched nothing: "
        f"{messages[0]!r}"
    )


def test_a_domain_that_matches_something_renders_no_message(
    client_for, store_registry_sample
):
    """The flip side, and the fixture this coordinates with: "government" now
    matches epo and visualdataweb once registry_names.load_names merges per
    field instead of keying on title alone (fix-round-1, finding 1)."""
    body = client_for(store_registry_sample).get(
        "/?domain=government", headers={"accept": "text/html"}
    ).text
    assert not with_attribute(body, "data-no-match")
    assert len(endpoints_shown(body)) == 2


def test_a_no_match_page_states_its_denominator_as_zero_of_the_fleet(
    client_for, store_registry_sample
):
    """Confirms the strip reads "0 of 9" and not "0 of 0": `total` is derived
    from the count BEFORE filtering, so it must not also collapse to the
    filtered (zero) count the way `endpoint_count` once did."""
    body = client_for(store_registry_sample).get(
        "/?q=zzzznomatch", headers={"accept": "text/html"}
    ).text
    total = texts_with(body, "data-figure")[0]
    assert total == "0 of 9", f"the count reads {total!r}"


# ---------------------------------------------------------------------------
# ?q= and the inline client-side script: fix round 2 (coordinator finding).
#
# The script at index.html's body_end block was written under the invariant
# stated in its own former comment -- "every row is in the document already"
# -- which ?q= broke without anyone updating the script. Three symptoms:
# `total` counted rendered (server-filtered) rows instead of the fleet; a page
# that loaded with the box pre-filled ran apply() on load and reported a
# client-side filter result nobody asked for ("showing 1 of 1" beside a strip
# reading "1 of 9"); and a no-match `?q=` unhid #nothing, showing two
# differently-worded "nothing found" notices at once.
#
# This suite has no JavaScript harness, so it asserts what markup can carry:
# the fleet total travels to the script as a `data-fleet-total` attribute
# rather than being countable from rendered rows, and it differs from the
# rendered row count exactly when a filter narrowed the page. The
# interaction-gating and #nothing-suppression behaviour is JS and was
# measured by hand against a running server (see the task-6 report).
# ---------------------------------------------------------------------------


def test_the_fleet_total_travels_to_the_script_as_an_attribute(
    client_for, store_registry_sample
):
    body = client_for(store_registry_sample).get(
        "/", headers={"accept": "text/html"}
    ).text
    attributes = with_attribute(body, "data-fleet-total")
    assert len(attributes) == 1, "the fleet total must be carried exactly once"
    assert attributes[0]["data-fleet-total"] == "9"


def test_a_filtered_pages_fleet_total_still_names_the_whole_fleet(
    client_for, store_registry_sample
):
    """The bug this guards against: `total` computed by counting
    `[data-endpoint]` rows in the document reads 1 on a page ?q= narrowed to
    one row, not 9. The attribute must stay the unfiltered count regardless,
    so a filtered page's fleet total DIFFERS from its rendered row count."""
    body = client_for(store_registry_sample).get(
        "/?q=uniprot", headers={"accept": "text/html"}
    ).text
    fleet_total = int(with_attribute(body, "data-fleet-total")[0]["data-fleet-total"])
    rendered_rows = len(endpoints_shown(body))
    assert rendered_rows == 1
    assert fleet_total == 9
    assert fleet_total != rendered_rows


def test_a_no_match_pages_fleet_total_is_still_the_fleet(
    client_for, store_registry_sample
):
    body = client_for(store_registry_sample).get(
        "/?q=zzzznomatch", headers={"accept": "text/html"}
    ).text
    fleet_total = int(with_attribute(body, "data-fleet-total")[0]["data-fleet-total"])
    assert fleet_total == 9
    assert endpoints_shown(body) == []


# ---------------------------------------------------------------------------
# Task 5: the row leads with a name.
# ---------------------------------------------------------------------------


def test_a_row_leads_with_the_name_and_keeps_the_url(client_for, store_registry_sample):
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    names = texts_with(body, "data-row-name")
    urls = texts_with(body, "data-row-url")
    assert names and len(names) == len(urls), "every row names itself and shows its URL"


def test_a_row_for_a_multi_dataset_endpoint_shows_a_count(client_for, store_many_datasets):
    """The rule: the host, and how many things it serves, never one of their names."""
    body = client_for(store_many_datasets).get("/", headers={"accept": "text/html"}).text
    assert "42 datasets" in body
    # The count alone does not pin the rule this test exists to enforce: a row
    # naming any of the 42 datasets beside "42 datasets" would also make this
    # string true. The name itself must be the host.
    names = texts_with(body, "data-row-name")
    assert "www.foodie-cloud.org" in names, (
        f"the row must lead with the host, not one of the 42 dataset titles: {names}"
    )


def test_the_page_says_where_names_come_from(client_for, store_registry_sample):
    """A borrowed title is attributed, or the registry is asserting it."""
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    assert "data-name-provenance" in body


# ---------------------------------------------------------------------------
# Task 6: facet pills, and the matrix moves down.
# ---------------------------------------------------------------------------


def test_the_pills_carry_their_counts(client_for, store_registry_sample):
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    pills = with_attribute(body, "data-pill")
    assert pills, "the registry offers no facets"
    for p in pills:
        assert p["data-pill-count"].isdigit(), f"{p['data-pill']} has no count"


def test_a_pill_is_a_link_that_works_without_javascript(client_for, store_registry_sample):
    """A faceted view must be shareable, like ?q=."""
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    hrefs = [a["href"] for a in with_attribute(body, "data-pill")]
    assert all(h.startswith("/?") for h in hrefs), f"pills are not links: {hrefs}"


def test_a_domain_pill_narrows_the_rows(client_for, store_registry_sample):
    """life_sciences: two of this fixture's nine endpoints
    (sparql.uniprot.org and www.foodie-cloud.org) carry it, which is enough
    on its own to prove `?domain=` narrows without pinning this test to a
    domain that used to be this fixture's only option.

    It once was: registry_names.load_names merged on title alone, so
    calibration-sample.toml's bare, title-less listings of data.epo.org and
    visualdataweb.infor.uva.es (read before lod-cloud.toml, alphabetically)
    silently won over lod-cloud.toml's richer "government" entries for the
    same two URLs, and "government" matched nothing on this fixture.
    Fix-round-1 corrected the merge to keep title, domain and datasets
    independently, and "government" now matches those same two endpoints
    (see test_a_domain_that_matches_something_renders_no_message below).
    life_sciences is kept here regardless, since this test only needs SOME
    domain that narrows, not a specific one."""
    client = client_for(store_registry_sample)
    everything = endpoints_shown(client.get("/", headers={"accept": "text/html"}).text)
    narrowed = endpoints_shown(
        client.get("/?domain=life_sciences", headers={"accept": "text/html"}).text
    )
    assert 0 < len(narrowed) < len(everything)


def test_a_pills_href_carries_the_active_query(client_for, store_registry_sample):
    """Blocker-5 of the whole-branch review: a pill's href used to be built
    from only its own `{param}={value}`, so on `/?q=uniprot` the
    life_sciences pill's COUNT was computed over the query-narrowed page (1
    endpoint) but its LINK, `/?domain=life_sciences`, dropped `q` and landed
    on a page of 2. A pill's link must carry every sibling filter its count
    was computed under.
    """
    body = client_for(store_registry_sample).get(
        "/?q=uniprot", headers={"accept": "text/html"}
    ).text
    pills = with_attribute(body, "data-pill")
    life_sciences = next(p for p in pills if p["data-pill"] == "life_sciences")
    assert life_sciences["data-pill-count"] == "1", life_sciences
    href = life_sciences["href"]
    assert "q=uniprot" in href and "domain=life_sciences" in href, href


def test_an_active_pills_href_clears_only_itself(client_for, store_registry_sample):
    """Pressing an already-active pill narrows nothing further -- it clears
    that one filter and keeps any other active one, per the same fix."""
    body = client_for(store_registry_sample).get(
        "/?domain=life_sciences", headers={"accept": "text/html"}
    ).text
    pills = with_attribute(body, "data-pill")
    life_sciences = next(p for p in pills if p["data-pill"] == "life_sciences")
    assert "on" in life_sciences.get("class", "")
    assert "domain=life_sciences" not in life_sciences["href"]


def test_the_matrix_sits_below_the_rows(client_for, store_registry_sample):
    """Demoted, not removed: it is the only way to ask a precise question."""
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    assert body.index('data-facet-group="matrix"') > body.index("data-endpoint=")


def test_a_facet_pill_narrows_the_rows(client_for, store_dormant_newest):
    """The non-domain pills are real filters too, not decoration: see
    _matches_facet. store_dormant_newest is three endpoints, one of them
    (kadaster) dormant -- the newest sweep declined to ask it at all -- so
    `?facet=answering` is the one of the two remaining named facets
    (`answering`, `void`) with a fixture on hand that is neither all nor
    none of a store: it must drop exactly the dormant one."""
    client = client_for(store_dormant_newest)
    everything = endpoints_shown(client.get("/", headers={"accept": "text/html"}).text)
    narrowed = endpoints_shown(
        client.get("/?facet=answering", headers={"accept": "text/html"}).text
    )
    assert 0 < len(narrowed) < len(everything)
    assert "https://data.kkg.kadaster.nl/query" not in narrowed


def test_no_pill_offers_a_filter_that_would_empty_the_page(client_for, store_registry_sample):
    """A control that promises a narrower view and delivers an empty one.

    `vocabulary-described` is an `exhaustive` metric (prober/metrics.toml), so
    only the nightly profile pass ever records it; on any store built from
    cheap sweeps its pill reads 0 permanently. Measured on the deployed dev
    site before this change: "Describes its own vocabulary 0", linking to a
    page with no rows. A pill that always says zero teaches a reader the
    filter is broken.
    """
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    counts = [a["data-pill-count"] for a in with_attribute(body, "data-pill-count")]
    assert counts, "the registry offers no pills at all"
    assert "0" not in counts, f"a pill offers an empty page: {counts}"


def test_a_domain_pill_reads_as_words_and_filters_by_its_slug(client_for, store_registry_sample):
    """The page exists to stop leading with jargon.

    The registry's domains are database keys -- `cross_domain`,
    `life_sciences`, `user_generated`. The label a person reads is spaced; the
    value in the URL stays the slug, because that is what `?domain=` matches.
    """
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    pills = with_attribute(body, "data-pill")
    domain_pills = [p for p in pills if "_" in p["data-pill"]]
    if not domain_pills:
        pytest.skip("this fixture holds no multi-word domain")
    for p in domain_pills:
        assert p["href"].count(p["data-pill"]) == 1, "the href must carry the slug"
    labels = texts_with(body, "data-pill")
    assert not any("_" in t for t in labels), f"a pill shows a raw slug: {labels}"

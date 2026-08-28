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
from html.parser import HTMLParser

import pytest
from pyoxigraph import NamedNode, RdfFormat, Store, parse
from starlette.testclient import TestClient

import verdict_encoding
from app import (
    ABOUT_PATH,
    DORMANCY,
    _index_html,
    _legend,
    _state_facets,
    ENDPOINT_PATH,
    INDEX_PATH,
    ROW_DORMANT_TEXT,
    _ROW_DORMANCY_REASONS,
    _group_dormancy_note,
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


# ---------------------------------------------------------------------------
# The grouping
# ---------------------------------------------------------------------------
def test_groups_are_the_availability_values_present_in_encoding_order(
    client_for, store_registry_sample
):
    """The order and the labels, not the membership.

    Three things are being pinned. The groups are the availability verdict's
    own values, so there are three of them here and not two. Their labels are
    the labels verdict_encoding.py gives those states. And the order is that
    table's order, which is neither alphabetical (absent, indeterminate,
    verified) nor by size (indeterminate 4, verified 3, absent 2), so a page
    that sorted by either would fail.
    """
    page = index(client_for(store_registry_sample))

    assert [group["availability"] for group in groups(page)] == [
        "verified",
        "indeterminate",
        "absent",
    ]
    assert [group["label"] for group in groups(page)] == [
        verdict_encoding.presentation(slug).label
        for slug in ("verified", "indeterminate", "absent")
    ]


def test_an_endpoint_with_no_availability_verdict_gets_its_own_group(
    client_for, store_registry_and_failure
):
    """run-prober-failed.nq beside the nine, and its endpoint merged with
    nothing.

    That run declined every metric it applied, so it recorded no availability
    verdict at all, and stage 1c-b3 makes this the normal outcome for a host
    group whose probe task panicked. Its group comes last, is labelled "not
    measured", and holds that endpoint alone: folding it into "absent" would
    turn "nobody looked" into "we established nothing was there", and folding
    it into "indeterminate" would claim a measurement that was never taken.
    """
    page = index(client_for(store_registry_and_failure))

    assert [group["availability"] for group in groups(page)] == [
        "verified",
        "indeterminate",
        "absent",
        "",
    ]
    assert groups(page)[-1]["label"] == "not measured"
    assert groups(page)[-1]["label"] == verdict_encoding.presentation(
        verdict_encoding.NOT_MEASURED
    ).label
    assert [group["count"] for group in groups(page)] == ["3", "4", "2", "1"]

    assert len(rows(page)) == REGISTRY_SAMPLE_ENDPOINTS + 1
    assert KADASTER in listed(page)
    assert chip_verdicts(page, row_for(page, KADASTER)) == {}


def test_the_final_groups_sentence_is_true_of_both_ways_into_it(
    client_for, store_no_availability_two_ways
):
    """The group is keyed on the ABSENCE of an availability verdict, and there
    are two opposite ways to have one.

    run-prober-failed.nq's endpoint was declined availability: a run recorded an
    sw:NotMeasured fact naming the metric and a reason. run-no-availability.nq's
    endpoint has no availability fact in either direction, while a metric of its
    own was measured. The group used to say "every metric it applied to them was
    declined rather than measured", which is false of the second on both counts,
    and it contradicted EMPTY_CELL_TEXT fifty lines above it in app.py, which
    says a metric a run recorded nothing about "is a gap in what this service
    holds, not a verdict about the endpoint".

    So the sentence must state the criterion the grouping actually uses and send
    a reader to the row for which of the two it is, and it must not claim a
    decline of either endpoint.
    """
    page = index(client_for(store_no_availability_two_ways))
    final = groups(page)[-1]

    assert final["availability"] == "", "the group keyed on no verdict"
    assert final["count"] == "2", "one endpoint of each kind is in it"
    assert set(listed(page)) == {KADASTER, NO_AVAILABILITY}

    said = group_note(page, "")
    assert "declined" in said, "the declining case must still be named"
    assert re.search(r"recorded nothing|nothing about it|no availability fact", said), (
        f"the other case must be named too, said {said!r}"
    )
    assert "every metric" not in said, (
        f"one of these endpoints had a metric measured, said {said!r}"
    )

    # And the rows are where a reader tells the two apart, which is what the
    # sentence sends them to: the declined endpoint carries a chip for
    # availability, the other carries a gap.
    kadaster = row_for(page, KADASTER)
    assert any(
        chip.get("data-declined") for chip in kadaster["chips"]
    ), "the declined endpoint's row must carry its decline"
    assert chip_verdicts(page, row_for(page, NO_AVAILABILITY)) == {
        "classes": "absent"
    }, "and the other endpoint's row must carry the metric that WAS measured"


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


def test_no_group_is_drawn_for_a_value_no_endpoint_carries(
    client_for, store_registry_sample
):
    """One group per value PRESENT, so the states this fixture has no endpoint
    for get no heading.

    A page listing all seven headings would read as a claim that the fixture
    holds an endpoint whose availability is declared-but-wrong. The legend is
    the place that lists every state whatever the page holds, and it says so
    about the encoding rather than about these endpoints.
    """
    page = index(client_for(store_registry_sample))
    drawn = {group["availability"] for group in groups(page)}

    assert drawn == {"verified", "indeterminate", "absent"}
    for slug in ("undeclared-but-verified", "declared-only", "declared-but-wrong"):
        assert slug not in drawn


# ---------------------------------------------------------------------------
# Rows whose facts are not current
# ---------------------------------------------------------------------------
def test_a_row_whose_run_did_not_finish_is_qualified(
    client_for, store_crashed_partway
):
    """The two conditions the endpoint page states, on the index's rows.

    kadaster's facts come from the run that died partway: it recorded
    sw:emission and never recorded sw:finalised, so what is shown is a partial
    sweep's. The other two endpoints' facts are complete and current, and what
    is true of them is a different claim: a later sweep exists, stopped, and
    never recorded reaching them, so their rows are not a report on that sweep.
    Two claims, so two markers, and a row carrying neither would present a
    crashed sweep's facts as a finished one's.

    Both markers are also words a reader sees, not attributes alone: a marker
    that only a test can read qualifies nothing.
    """
    page = index(client_for(store_crashed_partway))
    assert set(listed(page)) == {KADASTER, QLEVER, ONTOP}

    unfinished = {
        attributes["data-endpoint"]
        for attributes in with_attribute(page, "data-run-unfinished")
    }
    never_reached = {
        attributes["data-endpoint"]
        for attributes in with_attribute(page, "data-newer-run-unfinished")
    }
    assert unfinished == {KADASTER}
    assert never_reached == {QLEVER, ONTOP}

    assert row_for(page, KADASTER)["text"] != row_for(page, KADASTER)["endpoint"]
    for endpoint in (KADASTER, QLEVER, ONTOP):
        row = row_for(page, endpoint)
        assert row["text"].strip() not in ("", endpoint), (
            f"{endpoint}'s row carries the marker attribute and no words"
        )


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
    """
    page = index(client_for(store_registry_sample))
    generated = set(re.findall(r"\.(enc-[a-z-]+)\s*\{", page))
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

    assert [group["availability"] for group in groups(page)] == [""]


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

    groups_by_value = {group["availability"]: group for group in groups(page)}
    assert "dormant" not in groups_by_value, "dormancy was drawn as a group"
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

    note = texts_with(page, "data-group-dormant")
    assert len(note) == 1, f"{len(note)} groups carry a dormancy note"
    assert "1" in note[0], note[0]
    assert "did not ask" in note[0], note[0]


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


def test_the_row_markers_are_explained_on_about_and_not_on_the_index(
    client_for, store_dormant_newest
):
    """The panel that explained the four row markers was commented out by the
    plan owner on 2026-08-28, so this pins where the explanation now lives.

    The markers themselves still render, so a reader still meets "the newest
    sweep did not ask" on a row. What changed is that the index no longer says
    what that means, what it does NOT mean, or who changes it. /about does say
    all three, and the header links there from every page, which is why this is
    a relocation rather than a loss: the sentence a reader needs is one click
    away instead of one scroll away. The row's own wording still carries the
    reason, which is what stops the marker being bare.
    """
    page = index(client_for(store_dormant_newest))
    assert not with_attribute(page, "data-row-marker")
    # The marker is still on the row, and still says why.
    marked = texts_with(page, "data-newest-sweep-dormant")
    assert marked and any("did not ask" in text for text in marked)
    # And the way to the page that explains it is in the header.
    assert f'href="{ABOUT_PATH}"' in page


def test_the_index_links_to_about_once_and_not_once_per_row(
    client_for, store_dormant_newest
):
    """One occurrence, in the page header, and never one per row.

    Was two until 2026-08-28: the header's and one inside the panel that
    explained the dormancy marker, which the plan owner commented out. The
    invariant this test exists for is unchanged and is the one that matters at
    543 rows: a link repeated per row would spend 1,227 bytes as 24,000, and
    web/README.md's table is the record of how little headroom that leaves.
    """
    href = f'href="{ABOUT_PATH}"'
    text = index(client_for(store_dormant_newest))
    assert text.count(href) == 1, (
        f"the header's link and no more: {text.count(href)}"
    )
    # The real invariant: not once per row, whatever the row count.
    assert text.count(href) < len(listed(text))


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


def test_the_group_note_agrees_with_itself_in_number():
    """One marked row is "1 of these 2 rows carries", not "carry".

    Four words in the sentence move with the count, and a note reading "1 of
    these 2 rows carry the marker ... published them as endpoints" is a sentence
    a reader trips over on a page whose whole argument is that it says exactly
    what it means. Asserted on the function, because reaching the singular case
    through a store means a fixture with exactly one dormant row in its group
    and reaching the plural case means another one.
    """
    one = _group_dormancy_note(1, 2, 1)
    assert "1 of these 2 rows carries" in one, one
    for plural in (" carry ", " them as endpoints", "They are in", "their last"):
        assert plural not in one, f"{plural!r} is in the singular note: {one!r}"

    many = _group_dormancy_note(4, 9, 4)
    assert "4 of these 9 rows carry" in many, many
    assert "them as endpoints" in many, many
    for singular in ("carries", " it as an endpoint", "It is in", " its last"):
        assert singular not in many, f"{singular!r} is in the plural note: {many!r}"


def test_the_group_note_claims_a_reason_only_for_the_rows_that_carry_one():
    """"And said why" is counted off the rows, not asserted of the marker.

    The note used to end "declined to ask, and said why" whatever the rows said,
    and _row_dormancy_text renders "and gave no reason" for a declaration with no
    sw:dormancyReason beside it. A store whose newest run declares an endpoint
    dormant and records no reason therefore printed the group note "... declined
    to ask, and said why" directly above the row "... and gave no reason", which
    is the page contradicting itself inside one group.

    Unreachable from prober output, because the prober always writes a reason.
    The branch exists because _row_dormancy_text and _NO_DORMANCY_REASON both
    decline to assume one, and on this page a qualifier stated with no condition
    is a claim: the fix is to count, not to drop the clause, so the common case
    still reads as the strong sentence it is.
    """
    assert "and said why" in _group_dormancy_note(3, 9, 3)

    none_at_all = _group_dormancy_note(3, 9, 0)
    assert "and recorded no reason for any of them" in none_at_all, none_at_all
    assert "said why" not in none_at_all, none_at_all

    one_of_one = _group_dormancy_note(1, 4, 0)
    assert "and recorded no reason." in one_of_one, one_of_one
    assert "any of them" not in one_of_one, one_of_one

    some = _group_dormancy_note(3, 9, 2)
    assert "and said why for 2 of them" in some, some

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
        if attributes.get("class") == "f-count":
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
    """Every row with the group element it is NESTED INSIDE, and its chips.

    The nesting is the point. The script reads a row's availability off the
    group element the row is inside, so a reader that credited each row to the
    nearest PRECEDING group heading would agree with the page even on markup
    where a row had escaped its group, which is the arrangement that would make
    the script read null and file every row on the page under "not available".
    This tracks the open elements, so a row outside every group is not
    attributed to one.
    """

    def __init__(self):
        super().__init__()
        self.rows = []
        self._availability = None
        self._depth = None
        self._row_depth = None
        self._chip = None

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "data-availability" in attributes:
            assert self._depth is None, "a group is nested inside another group"
            self._availability = attributes["data-availability"]
            self._depth = 0
            return
        if self._depth is None:
            return
        self._depth += 1
        if "data-endpoint" in attributes:
            assert self._row_depth is None, "a row is nested inside another row"
            self._row_depth = self._depth
            self.rows.append(
                {
                    "availability": self._availability,
                    "endpoint": attributes["data-endpoint"],
                    "chips": [],
                }
            )
            return
        if self._row_depth is not None and (
            "data-verdict" in attributes or "data-declined" in attributes
        ):
            self._chip = dict(attributes, abbr="")
            self.rows[-1]["chips"].append(self._chip)

    def handle_endtag(self, tag):
        if self._depth is None:
            return
        self._chip = None
        if self._row_depth is not None and self._depth == self._row_depth:
            self._row_depth = None
        if self._depth == 0:
            self._depth = None
            self._availability = None
        else:
            self._depth -= 1

    def handle_data(self, data):
        if self._chip is not None:
            self._chip["abbr"] += data


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


def group_of(group_value, *rows):
    """One group of rows that need not be alike, in _index_groups' shape.

    Each row is a mapping from a metric name to either a verdict string or None
    for a decline, which is the distinction all three facet builders turn on.
    The slug is taken through verdict_encoding.presentation, exactly as
    _index_row takes it, so a value this build has no encoding for arrives here
    drawn the way the page draws it rather than under its own name.
    """
    return {
        "availability": group_value,
        "rows": [
            {
                "cells": [
                    {
                        "present": True,
                        "name": name,
                        "abbr": name[:2].upper(),
                        "verdict": value,
                        "reason": None if value else "cost-ceiling",
                        "slug": (
                            verdict_encoding.presentation(value).slug
                            if value
                            else verdict_encoding.NOT_MEASURED
                        ),
                    }
                    for name, value in cells.items()
                ]
            }
            for cells in rows
        ],
    }


def rows_of(group_value, count, /, **cells):
    """One group of `count` rows, all alike, in the shape _index_groups returns.

    Positional-only first parameter, because one of the metrics a caller names
    is `availability` and a keyword of that name would collide with the group's
    own value.

    Each row gets its OWN dict, through group_of. The first version of this
    built one row and aliased it `count` times, which was harmless because all
    three builders are read-only over rows; harmless-until is not a property
    worth keeping in a fixture, since a builder that ever annotated a row would
    have annotated 543 of them.
    """
    return group_of(group_value, *([cells] * count))


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
    groups = [
        rows_of("indeterminate", 4, availability="indeterminate",
                cors="indeterminate", classes=None),
        rows_of("verified", 2, availability="verified", cors="absent", classes=None),
    ]
    by_slug = {f["slug"]: f["count"] for f in _state_facets(groups)}
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
    assert [f["slug"] for f in _state_facets([uniform])] == eight
    assert {f["slug"]: f["count"] for f in _state_facets([uniform])}[
        "unrecognised"
    ] == 1
    # And the case a condition on uniformity alone would miss: a row that DREW
    # an unrecognised verdict without being uniform in it. The legend lists the
    # eighth state because a chip on the page is in it, so the mapping carries
    # it too, at the count that is true, which is no rows.
    assert [f["slug"] for f in _state_facets([mixed])] == eight
    # 1 and not 0: the row carries an unrecognised chip beside a verified one,
    # and at-least-one counts it. Under the uniform reading this was 0, and the
    # point of the case is unchanged: the eighth entry is emitted because a cell
    # was DRAWN in that state, which is the same condition _legend lists it on,
    # so neither sequence can grow an entry the other lacks.
    assert {f["slug"]: f["count"] for f in _state_facets([mixed])}[
        "unrecognised"
    ] == 1
    # The other direction, which is what catches an eighth entry always drawn.
    assert [f["slug"] for f in _state_facets([known])] == seven
    # And the condition is the legend's own, over the same rows.
    for group in (uniform, mixed, known):
        drawn = [
            {"slug": cell["slug"]}
            for row in group["rows"]
            for cell in row["cells"]
            if cell["present"]
        ]
        assert [f["slug"] for f in _state_facets([group])] == [
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
    keyed = [
        attributes["data-group-note"]
        for attributes in with_attribute(page, "data-group-note")
    ]
    assert keyed == [""], keyed
    assert "recorded no availability verdict at all" in group_note(page, "")


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
    page = _index_html([])

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

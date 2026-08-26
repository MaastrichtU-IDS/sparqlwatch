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
from app import ENDPOINT_PATH, INDEX_PATH, app, get_store

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

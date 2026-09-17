"""The availability grid, and the sweeps it lays out.

This stood on the registry until 2026-09-17 and these two tests came with
it -- each pins a bug that was real on the page they were written against,
and neither is about the registry any more. See their own docstrings.
"""

import re

from pyoxigraph import RdfFormat, parse

import pytest
from fastapi.testclient import TestClient

import app as app_module
from app import HISTORY_PATH, INDEX_PATH, app, get_store
from test_page import texts_with, with_attribute



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

    def rows_and_grid(query):
        """The registry's rows and the history grid, for one query.

        Two pages since 2026-09-17, one invariant: the grid may never name an
        endpoint the registry's own listing drops under the same `?q=`. Before
        the split both were on one page and one fetch saw both; the invariant
        is the same and now it takes two.
        """
        listing = client.get(f"{INDEX_PATH}?q={query}", headers={"accept": "text/html"}).text
        history = client.get(f"{HISTORY_PATH}?q={query}", headers={"accept": "text/html"}).text
        return (
            set(re.findall(r'data-endpoint="([^"]+)"', listing)),
            set(re.findall(r'data-fleet-endpoint="([^"]+)"', history)),
        )

    rows, grid = rows_and_grid("kadaster")
    assert rows == {"https://data.kkg.kadaster.nl/query"}, rows
    assert grid <= rows, f"the grid names {grid - rows}, which the rows do not"
    assert grid == set(), (
        "the one endpoint that ever changed was filtered out of the rows; "
        "the grid must not still draw it"
    )

    rows2, grid2 = rows_and_grid("ontop")
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
        f"{HISTORY_PATH}?domain=test_domain", headers={"accept": "text/html"}
    ).text
    lede = re.search(r'<p class="lede">(.*?)</p>', body, re.DOTALL).group(1)
    normalized = " ".join(lede.split())
    assert "All 2 of 3 read the same way every time." in normalized, normalized


def test_the_page_serves_both_representations(client_for, store_dormant_newest):
    """Every resource on this site is content-negotiated, and a new one that
    was not would be the single page a machine could not read."""
    client = client_for(store_dormant_newest)
    html = client.get(HISTORY_PATH, headers={"accept": "text/html"})
    assert html.status_code == 200
    assert html.headers["content-type"].startswith("text/html")
    assert "data-fleet-endpoint" in html.text

    rdf = client.get(HISTORY_PATH, headers={"accept": "text/turtle"})
    assert rdf.status_code == 200
    assert rdf.headers["content-type"].startswith("text/turtle")

    unservable = client.get(HISTORY_PATH, headers={"accept": "application/pdf"})
    assert unservable.status_code == 406


def test_its_rdf_is_the_registrys_own(client_for, store_dormant_newest):
    """The grid draws no fact the index does not already publish -- it lays the
    same availability verdicts out by sweep instead of by endpoint. Serving a
    second, narrower CONSTRUCT here would be a second chance to disagree with
    the first about one set of measurements."""
    client = client_for(store_dormant_newest)

    def triples(path):
        """Parsed, not compared as text: a CONSTRUCT's solution order is not
        specified and the Turtle writer groups by subject, so two serialisations
        of one graph differ as strings while naming the same facts."""
        body = client.get(path, headers={"accept": "text/turtle"}).text
        return {
            (str(t.subject), str(t.predicate), str(t.object))
            for t in parse(body.encode(), format=RdfFormat.TURTLE)
        }

    assert triples(HISTORY_PATH) == triples(INDEX_PATH)


def test_the_registry_no_longer_carries_the_grid(client_for, store_dormant_newest):
    """It moved on 2026-09-17. Left on both pages it would be two renderings
    of one fact, and the one nobody edited would go stale."""
    body = client_for(store_dormant_newest).get(INDEX_PATH, headers={"accept": "text/html"}).text
    assert "data-fleet-endpoint" not in body
    assert 'data-section="overview"' not in body


def test_the_nav_marks_this_page_as_the_one_you_are_on(client_for, store_dormant_newest):
    """It renders the registry's context, which names the registry as `here`.
    Left alone, the header marks Registry while you are looking at History."""
    body = client_for(store_dormant_newest).get(HISTORY_PATH, headers={"accept": "text/html"}).text
    current = re.findall(r'<a href="([^"]+)"[^>]*aria-current="page"', body)
    assert current == [HISTORY_PATH], f"nav marks {current} as current, not {HISTORY_PATH}"

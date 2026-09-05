"""The vocabulary explorer, as a route on this site rather than a second server.

It was a standalone page served by its own http.server on port 8731 until
2026-09-01. These tests exist because nothing else would notice if the route
regressed: the page's behaviour lives in an inline script, and the payload is a
committed file that no other test reads.
"""
from pathlib import Path

import json
import re
from urllib.parse import quote, unquote

import pytest
from starlette.testclient import TestClient

from app import EXPLORE_PATH, app, explore_endpoints, get_store

from test_page import with_attribute


@pytest.fixture
def client(store_content_profiles):
    """A client whose /explore reads a store holding content profiles.

    This fixture supplied NO store until 2026-09-05, deliberately: the route
    read a committed payload file and never touched one, and a fixture that
    handed it a store would have hidden it if that stopped being true. It has
    now stopped being true on purpose, so the guard is spent and its inversion
    is the change. run-content-profiles.nq is the only committed run carrying a
    profile, which is why every test here reads that one.
    """
    app.dependency_overrides[get_store] = lambda: store_content_profiles
    with TestClient(app) as built:
        yield built
    app.dependency_overrides.clear()


@pytest.fixture
def client_for():
    """A client whose routes read the store handed to it, for the tests below
    that fetch the INDEX. Same shape as test_index.py's."""
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


def test_the_route_answers_and_carries_its_payload(client):
    body = client.get(EXPLORE_PATH).text
    assert "id=\"data\"" in body
    payload = json.loads(body.split('id="data">', 1)[1].split("</script>", 1)[0])
    assert payload["terms"], "the page ships no vocabulary"
    # One, and it is the fixture's, rather than the prototype payload's fixed
    # two: the page describes what the store holds now.
    assert [e["url"] for e in payload["endpoints"]] == ["http://127.0.0.1:9200/sparql"]


def test_the_payload_is_what_the_store_holds(client, store_content_profiles):
    """Replaces a test that compared the page against a committed file.

    That file was read at import and the assertion kept the two from drifting,
    which mattered while nobody could rebuild it. It is computed now, so the
    thing that can drift is the page against the STORE, and that is what this
    asks instead.
    """
    from explore_payload import build_payload_json

    body = client.get(EXPLORE_PATH).text
    served = body.split('id="data">', 1)[1].split("</script>", 1)[0]
    assert served == build_payload_json(store_content_profiles)


def test_it_is_part_of_this_site_and_not_a_page_beside_it(client):
    """The whole point of folding it in. Its header must lead back to the index
    and carry the same nav as every other page, or it is still a separate site
    that happens to share a port."""
    body = client.get(EXPLORE_PATH).text
    assert 'class="logo" href="/"' in body, "the logo must lead home"
    nav = [a["href"] for a in with_attribute(body, "data-nav")]
    assert nav == [EXPLORE_PATH, "/docs"], f"nav is {nav}"


def test_the_page_says_what_it_is_a_reading_of(client):
    """One endpoint is not the registry, and the page has to say so.

    It said the literal word "prototype" while a static file backed it. A
    hardcoded provenance note outlives the data it describes and this one had:
    it still claimed two endpoints after the registry was replaced with three.
    So the note is counted now, and what this asserts is that the count is
    present and true rather than that a particular word is.
    """
    body = client.get(EXPLORE_PATH).text
    assert "1 endpoint with a content profile" in body, body[:0] or "note missing"
    assert "2 endpoints" not in body, "the stale hardcoded note is gone"


def test_the_states_it_draws_are_the_sites_own_vocabulary(client):
    """A term's evidence state IS a verdict here: used and declared is
    `verified`, used but not declared is `undeclared-but-verified`. If this page
    invented its own classes it would be teaching a second language for one
    fact, and /docs/states would stop describing it."""
    import verdict_encoding

    body = client.get(EXPLORE_PATH).text
    for state in ("verified", "undeclared-but-verified", "declared-only", "indeterminate"):
        assert f"enc-{state}" in body, f"missing enc-{state}"
        assert state in {s.slug for s in verdict_encoding.STATES}, f"{state} is not a real verdict"


def test_the_generator_can_still_rebuild_the_payload():
    """The recovery of 2026-09-01 turned a file nobody could rebuild into a file
    with a generator. This asserts that stayed true."""
    tools = Path(__file__).resolve().parents[2] / "tools" / "explorer"
    assert (tools / "build.py").is_file()
    assert (tools / "template.html").is_file()
    assert "__PAYLOAD__" in (tools / "template.html").read_text()


def test_the_index_links_only_the_rows_the_explorer_can_show(client_for, store):
    """Two of 543 today.

    A `content` link on an endpoint the explorer holds nothing for would open a
    page with an empty listing, and a reader would take that for "this endpoint
    has no vocabulary" when what happened is that nobody looked. On these pages
    an absent qualifier is a positive claim, and so is a link that leads
    somewhere empty.
    """
    body = client_for(store).get("/").text
    listed = set(re.findall(r'data-endpoint="([^"]+)"', body))
    linked = {unquote(u) for u in re.findall(r'href="/explore\?endpoint=([^"]+)"', body)}

    # The invariant, stated against whatever this store happens to hold rather
    # than against a fixed pair: a row is linked exactly when the explorer has
    # that endpoint. This fixture carries no content profile, so `linked` is
    # empty and that is the correct answer rather than a missing link.
    explorable = set(explore_endpoints(store))
    assert linked == listed & explorable, (
        f"linked {sorted(linked)}, expected {sorted(listed & explorable)}"
    )
    assert linked <= listed, "a link for an endpoint this page does not list"


def test_the_decision_itself_both_ways(store_content_profiles):
    """The positive case, asked of the function that makes the decision.

    Written against `_index_row` because the negative half needs an endpoint the
    store has no profile for, and a fixture cannot hold one of those and the
    positive case at once. It used to read the static payload's endpoint list;
    now the store supplies both sides, so this proves the link appears rather
    than only that it is withheld.
    """
    from app import _index_metrics, _index_row
    from endpoint_measurements import endpoint_measurements

    store = store_content_profiles
    explorable = explore_endpoints(store)
    known = sorted(explorable)[0]
    entries = [endpoint_measurements(store, e) for e in _endpoints_in(store)]
    metrics = _index_metrics(entries)
    sample = entries[0]

    object.__setattr__(sample, "endpoint", known)
    assert _index_row(sample, metrics, explorable)["content_href"] == (
        "/explore?endpoint=" + quote(known, safe="")
    )

    object.__setattr__(sample, "endpoint", "http://example.org/not-probed")
    assert _index_row(sample, metrics, explorable)["content_href"] is None


def _endpoints_in(store):
    rows = store.query(
        "PREFIX dqv: <http://www.w3.org/ns/dqv#> "
        "SELECT DISTINCT ?e WHERE { GRAPH ?g { ?m dqv:computedOn ?e } }"
    )
    return sorted(r["e"].value for r in rows)


def test_the_link_carries_the_endpoint_encoded(client_for, store):
    """The endpoint is a URL inside a URL. Unencoded, its own ?url= and & would
    be read as this page's parameters."""
    body = client_for(store).get("/").text
    for target in re.findall(r'href="/explore\?endpoint=([^"]+)"', body):
        assert "://" not in target, f"{target} is not encoded"
        assert unquote(target).startswith("http"), target


def test_a_linked_endpoint_is_one_the_payload_actually_holds(client_for, store):
    """The link and the data must agree, or the page opens with a chip selected
    that matches nothing and shows an empty listing: the exact failure the
    filtering above exists to prevent."""
    held = set(explore_endpoints(store))
    body = client_for(store).get("/").text
    for target in re.findall(r'href="/explore\?endpoint=([^"]+)"', body):
        assert unquote(target) in held, f"{unquote(target)} is linked but not in the payload"

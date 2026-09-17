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

import app as app_module
from app import (
    ABOUT_PATH,
    DOCS_PATH,
    EXPLORE_PATH,
    HISTORY_PATH,
    INDEX_PATH,
    app,
    get_store,
)

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
    that happens to share a port.

    Four items and not two: explore.html carried its own two-link header
    (vocabulary, docs) until it moved onto base.html's shared shell on
    2026-09-16, at which point its nav became the same fixed, four-item list
    every other page renders. This stays a full equality and not a subset or
    substring check, because its intent is that the nav is a fixed list and a
    page never grows one nav link per row.
    """
    body = client.get(EXPLORE_PATH).text
    assert 'class="logo" href="/"' in body, "the logo must lead home"
    nav = [a["href"] for a in with_attribute(body, "data-nav")]
    assert nav == [INDEX_PATH, EXPLORE_PATH, HISTORY_PATH, DOCS_PATH, ABOUT_PATH], f"nav is {nav}"


def test_the_page_carries_no_stylesheet_of_its_own(client):
    body = client.get(EXPLORE_PATH, headers={"accept": "text/html"}).text
    assert "--accent:" not in body, (
        "design tokens belong in web/static/site.css, not in this template"
    )
    assert app_module.STYLESHEET_PATH in body


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
    fact, and /docs/states would stop describing it.

    This used to assert `f"enc-{state}" in body`, which passed for the wrong
    reason: the only place those literal class names appeared in the served
    HTML was the nine hand-copied `.enc-` rules this page's own <style> block
    carried, not the chips themselves -- those build their class at runtime
    in the inline script (`"enc-" + s.slug`). Removing that private copy (see
    test_static.py's all-routes check) would have silently emptied this
    assertion's premise while it kept passing, because `body` still
    contained the string `"enc-verified"` for a completely different reason:
    JavaScript source text, not a drawn state.

    So this reads the two things that actually decide what gets drawn: the
    JS array the filter chips are generated from (`the script's slug list`),
    and the served stylesheet, which is now the ONLY place a rule for any of
    these classes exists at all.
    """
    import re

    import verdict_encoding

    body = client.get(EXPLORE_PATH).text
    script = body.split("var STATES = [", 1)[1].split("];", 1)[0]
    js_states = re.findall(r'slug:\s*"([a-z-]+)"', script)

    expected = ("verified", "undeclared-but-verified", "declared-only", "indeterminate")
    assert js_states == list(expected), f"the page's own filter chips are {js_states}"

    real_slugs = {s.slug for s in verdict_encoding.STATES}
    css = client.get(app_module.STYLESHEET_PATH).text
    for state in js_states:
        assert state in real_slugs, f"{state} is not a real verdict"
        # Not "somewhere in body": the generated stylesheet is where a rule
        # for this class must live now that explore.html carries no private
        # copy of its own.
        assert f".enc-{state} " in css, f"the stylesheet defines no rule for enc-{state}"


def test_the_generator_can_still_rebuild_the_payload():
    """The recovery of 2026-09-01 turned a file nobody could rebuild into a file
    with a generator. This asserts that stayed true."""
    tools = Path(__file__).resolve().parents[2] / "tools" / "explorer"
    assert (tools / "build.py").is_file()
    assert (tools / "template.html").is_file()
    assert "__PAYLOAD__" in (tools / "template.html").read_text()


def _endpoints_in(store):
    rows = store.query(
        "PREFIX dqv: <http://www.w3.org/ns/dqv#> "
        "SELECT DISTINCT ?e WHERE { GRAPH ?g { ?m dqv:computedOn ?e } }"
    )
    return sorted(r["e"].value for r in rows)


def test_the_index_draws_no_per_row_explorer_link(client_for, store_content_profiles):
    """Four tests stood here until 2026-09-17 and three had stopped asserting
    anything.

    They covered the [content] link: that it appeared exactly for the rows the
    explorer holds something for, that its target was percent-encoded, and that
    every linked endpoint was really in the payload. The owner removed the link
    -- the endpoint name beside it reaches the same page -- and
    `explore_endpoints`, the function that made the decision, went with its last
    caller.

    Three of the four looped over `/explore?endpoint=` links found in the page.
    With no such link emitted, those loops ran zero times and passed for the
    wrong reason, which is the failure this suite has been finding all day. One
    statement replaces them, and it is the one that is true now. Run against a
    fixture WITH content profiles, so the assertion has something to be wrong
    about: against a store the explorer holds nothing for, "no links" is true
    either way and the test would prove nothing again.
    """
    body = client_for(store_content_profiles).get("/").text
    assert re.search(r'data-endpoint="', body), "this fixture lists no rows"
    assert not re.findall(r'href="/explore\?endpoint=', body), (
        "the index draws a per-row explorer link again"
    )
    # The header nav's own /explore entry is a different thing and stays.
    assert 'href="/explore"' in body


def test_the_explorer_payload_is_built_once_per_render(
    client_for, store_content_profiles, monkeypatch
):
    """One render, one build.

    /explore used to call `build_payload` twice: once for the JSON it embeds
    and once for the note that counts what the JSON holds. On the live store on
    2026-09-14 that was the difference between a 19 second page and a 48 second
    one, and the second build could only ever produce what the first already
    had.

    Asserted by counting rather than by timing, because a timing assertion on a
    fixture this small would pass whatever the page did.
    """
    import app as app_module

    calls = []
    real = app_module.build_payload

    def counting(store):
        calls.append(store)
        return real(store)

    monkeypatch.setattr(app_module, "build_payload", counting)
    response = client_for(store_content_profiles).get("/explore")
    assert response.status_code == 200
    assert len(calls) == 1, f"the payload was built {len(calls)} times for one render"

"""The vocabulary explorer, as a route on this site rather than a second server.

It was a standalone page served by its own http.server on port 8731 until
2026-09-01. These tests exist because nothing else would notice if the route
regressed: the page's behaviour lives in an inline script, and the payload is a
committed file that no other test reads.
"""
from pathlib import Path

import json
import pytest
from starlette.testclient import TestClient

from app import EXPLORE_PATH, EXPLORE_PAYLOAD_FILE, app

from test_page import with_attribute


@pytest.fixture
def client():
    with TestClient(app) as built:
        yield built


def test_the_route_answers_and_carries_its_payload(client):
    body = client.get(EXPLORE_PATH).text
    assert "id=\"data\"" in body
    payload = json.loads(body.split('id="data">', 1)[1].split("</script>", 1)[0])
    assert payload["terms"], "the page ships no vocabulary"
    assert len(payload["endpoints"]) == 2


def test_the_payload_on_disk_is_what_the_page_serves(client):
    """One file, read at import. A page built from a different payload than the
    one committed would be a page nobody could rebuild, which is the state this
    route was recovered from."""
    body = client.get(EXPLORE_PATH).text
    served = body.split('id="data">', 1)[1].split("</script>", 1)[0]
    assert served == EXPLORE_PAYLOAD_FILE.read_text()


def test_it_is_part_of_this_site_and_not_a_page_beside_it(client):
    """The whole point of folding it in. Its header must lead back to the index
    and carry the same nav as every other page, or it is still a separate site
    that happens to share a port."""
    body = client.get(EXPLORE_PATH).text
    assert 'class="logo" href="/"' in body, "the logo must lead home"
    nav = [a["href"] for a in with_attribute(body, "data-nav")]
    assert nav == [EXPLORE_PATH, "/docs"], f"nav is {nav}"


def test_the_page_says_it_is_a_prototype(client):
    """Two endpoints is not the registry. A reader who assumed otherwise would
    draw conclusions about coverage the data does not support."""
    body = client.get(EXPLORE_PATH).text
    assert "prototype" in body.lower()


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

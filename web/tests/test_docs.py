"""Tests for the /docs section: three pages, and what each of them may claim.

The section exists so that a reader meeting a two-letter chip or a dashed border
has somewhere to look, and the two pages added with it carry facts that live
somewhere else: prober/metrics.toml decides what a metric is, and
verdict_encoding.py decides what a state looks like. So most of what is worth
testing here is not the prose. It is that the page and the source agree, and
that a page which cannot say something honestly does not say it.
"""

import re
from pathlib import Path

import pytest
from conftest import requires_repo_sources
from starlette.testclient import TestClient

import app as app_module
import verdict_encoding
from app import (
    ABOUT_PATH,
    DOCS_METRICS_PATH,
    DOCS_PATH,
    DOCS_STATES_PATH,
    DOCS_VOID_PATH,
    INDEX_PATH,
    METRIC_DOCS,
    OFFERED_MEDIA_TYPES,
    app,
)

from test_page import texts_with, with_attribute

PROBER = Path(__file__).resolve().parents[2] / "prober"
RDF_TYPES = [t for t in OFFERED_MEDIA_TYPES if t != "text/html"]


@pytest.fixture
def client():
    """No store override: none of these three routes reads one.

    That absence is the point of testing it. A documentation page that needed a
    measurement to render would be a page this service could not serve before
    its first sweep, and the one page in this section a stranger arrives at
    unprompted is the one they reach from a log line.
    """
    with TestClient(app) as built:
        yield built


def html(client, path):
    response = client.get(path, headers={"accept": "text/html"})
    assert response.status_code == 200, path
    return response.text


def metrics_toml():
    """{id: {label, dimension, cost}} as prober/metrics.toml states it."""
    found = {}
    for block in (PROBER / "metrics.toml").read_text().split("[[metric]]")[1:]:
        read = lambda key: re.search(rf'^{key} = "([^"]+)"', block, re.M)
        if read("id"):
            found[read("id").group(1)] = {
                "label": read("label").group(1),
                "dimension": read("dimension").group(1),
                "cost": read("cost").group(1),
            }
    return found


# ---------------------------------------------------------------------------
# The pin: what the docs claim against what decides it
# ---------------------------------------------------------------------------


def test_every_metric_fact_is_the_probers_own():
    """The label, the dimension and the cost class, all three of them.

    The run graphs carry none of this. A measurement names its metric by IRI and
    nothing publishes a label, a dimension or a cost for that IRI, so the web
    tier cannot read these out of the store the way it reads everything else it
    says. This test is what makes the copy safe: it fails the day
    prober/metrics.toml and app.py disagree, which is the same answer
    test_about.py gives for the politeness numbers.
    """
    stated = metrics_toml()
    assert stated, "no metric in prober/metrics.toml carries the three fields"

    # A metric this page describes is either one the prober measures now, and
    # then the three facts must match its file, or one it has RETIRED, and then
    # the file must not mention it. The second case is not an escape hatch: the
    # store still holds measurements a retired metric produced, the index still
    # derives a column from them, and a column a reader cannot look up would be
    # worse than a description marked out of date. What the marking buys is that
    # nobody reads it as something a new sweep will produce.
    described = set(METRIC_DOCS)
    retired = {m for m, facts in METRIC_DOCS.items() if facts.get("retired")}
    current = described - retired

    assert current == set(stated), (
        "the docs and prober/metrics.toml disagree about which metrics are "
        f"measured: docs say {sorted(current)}, the file says {sorted(stated)}"
    )
    for metric in retired:
        assert metric not in stated, (
            f"{metric} is marked retired and prober/metrics.toml still defines it"
        )
    for metric, facts in stated.items():
        for field in ("label", "dimension", "cost"):
            assert METRIC_DOCS[metric][field] == facts[field], (
                f"{metric}.{field}: docs say {METRIC_DOCS[metric][field]!r}, "
                f"prober/metrics.toml says {facts[field]!r}"
            )


def test_a_retired_metric_is_described_and_marked(client):
    """Because its measurements are still published and still true.

    has-classes was removed from prober/metrics.toml on 2026-08-28 after 543
    endpoints had been measured for it. Those measurements are a record of what
    that sweep observed, nothing here rewrites a run graph, and the index
    derives its columns from the store rather than from the prober's file, so
    the column outlives the probe. A reader who meets it needs to be able to
    look it up AND to be told no new sweep will produce it.
    """
    page = html(client, DOCS_METRICS_PATH)
    retired = [m for m, facts in METRIC_DOCS.items() if facts.get("retired")]
    assert retired, "this test needs at least one retired metric to be about"
    for metric in retired:
        assert metric in {
            a["data-metric-doc"] for a in with_attribute(page, "data-metric-doc")
        }
        marked = [
            a["data-metric-retired"]
            for a in with_attribute(page, "data-metric-retired")
        ]
        assert metric in marked, f"{metric} is not marked retired on the page"


def test_the_metrics_page_renders_every_metric_and_its_facts(client):
    page = html(client, DOCS_METRICS_PATH)
    stated = metrics_toml()
    # Every metric the docs describe, measured or retired: the page is the place
    # a reader looks up a column, and the store outlives the prober's file.
    assert {a["data-metric-doc"] for a in with_attribute(page, "data-metric-doc")} == (
        set(METRIC_DOCS)
    )
    assert set(stated) <= set(METRIC_DOCS)
    for metric, facts in stated.items():
        for field, attribute in (
            ("label", "data-metric-label"),
            ("dimension", "data-metric-dimension"),
            ("cost", "data-metric-cost"),
        ):
            said = [
                text
                for attributes, text in zip(
                    with_attribute(page, attribute), texts_with(page, attribute)
                )
                if attributes[attribute] == metric
            ]
            assert len(said) == 1, f"{metric} has {len(said)} {attribute} elements"
            assert facts[field] in said[0], f"{metric} {field}: {said[0]!r}"


def test_every_state_is_described_from_the_encoding_table(client):
    """Nothing about a state is written twice.

    The label, the meaning and all three drawing channels come from
    verdict_encoding, which docs/design/verdict-encoding.md is canonical for and
    test_page.py compares against. So this page cannot drift from the chips it
    describes without that comparison failing first.
    """
    page = html(client, DOCS_STATES_PATH)
    drawn = {a["data-state-doc"] for a in with_attribute(page, "data-state-doc")}
    assert drawn == {state.slug for state in verdict_encoding.STATES}
    for state in verdict_encoding.STATES:
        meaning = [
            text
            for attributes, text in zip(
                with_attribute(page, "data-state-meaning"),
                texts_with(page, "data-state-meaning"),
            )
            if attributes["data-state-meaning"] == state.slug
        ]
        assert meaning == [state.meaning], f"{state.slug}: {meaning}"


def test_the_states_page_publishes_all_three_channels(client):
    """Because the encoding does not rest on colour, and saying so is not enough.

    A reader is told the border style, the fill and the weight for every state,
    which is what lets them check the claim rather than take it. The triples are
    injective over those three, and test_page.py is what pins that; this page is
    where a person can see it.
    """
    page = html(client, DOCS_STATES_PATH)
    for state in verdict_encoding.STATES:
        for attribute, expected in (
            ("data-state-border", state.border),
            ("data-state-fill", "filled" if state.fill else "empty"),
            ("data-state-weight", f"{state.weight}px"),
        ):
            said = [
                text
                for attributes, text in zip(
                    with_attribute(page, attribute), texts_with(page, attribute)
                )
                if attributes[attribute] == state.slug
            ]
            assert len(said) == 1 and expected in said[0], (
                f"{state.slug} {attribute}: {said}"
            )


# ---------------------------------------------------------------------------
# The section, and the one page in it that is not under /docs
# ---------------------------------------------------------------------------


def test_the_index_lists_every_page_and_monitoring_keeps_its_own_url(client):
    """Monitoring is listed with the rest and lives at /about.

    Not a tidiness lapse. Every request this prober makes to a stranger's server
    carries `+https://<host>/about` in its User-Agent, so that url is a promise
    printed in traffic already sent, and it is the one address on this site that
    is not ours to move. The index names it and links to it.

    ASSERTED AS A RULE RATHER THAN A LIST, since 2026-09-15. This named three
    pages by hand and went red when a fourth was added, which is a test failing
    for the one reason it should not: the section growing is the thing
    `_docs_context`'s table was built to make easy. What must hold is that
    every page the table declares is listed, in its order, and that monitoring
    is among them under its own url.
    """
    import app

    page = html(client, DOCS_PATH)
    listed = [a["data-doc-page"] for a in with_attribute(page, "data-doc-page")]
    assert listed == [entry["path"] for entry in app._docs_context()["pages"]]
    assert ABOUT_PATH in listed, "monitoring is not listed"
    assert "Monitoring" in page


@requires_repo_sources
def test_the_user_agents_url_still_answers(client):
    """The promise itself, tested rather than assumed.

    If this ever fails, a stranger who found this service in their logs and
    followed the url gets a 404 from the one page written for them.
    """
    assert client.get(ABOUT_PATH, headers={"accept": "text/html"}).status_code == 200
    agent = (PROBER / "src" / "client.rs").read_text()
    assert ABOUT_PATH + ")" in agent, "the User-Agent no longer points at /about"


def test_every_docs_page_leads_home_and_back_to_the_section(client):
    for path in (DOCS_PATH, DOCS_METRICS_PATH, DOCS_STATES_PATH, ABOUT_PATH):
        page = html(client, path)
        logos = [a for a in with_attribute(page, "class") if a["class"] == "logo"]
        assert len(logos) == 1 and logos[0]["href"] == INDEX_PATH, path


# ---------------------------------------------------------------------------
# Negotiation, which the spec requires of every resource
# ---------------------------------------------------------------------------


def test_all_three_pages_negotiate(client):
    """HTML for people, RDF for machines, on every resource this site has.

    A metric and a state are vocabulary: what this service measures and what its
    verdicts mean are the things a client integrating with it needs without
    parsing English.
    """
    for path in (DOCS_PATH, DOCS_METRICS_PATH, DOCS_STATES_PATH):
        for media_type in RDF_TYPES:
            response = client.get(path, headers={"accept": media_type})
            assert response.status_code == 200, (path, media_type)
            assert response.headers["content-type"].startswith(media_type)
            assert response.content, (path, media_type)


def test_a_media_type_this_site_does_not_serve_is_refused(client):
    for path in (DOCS_PATH, DOCS_METRICS_PATH, DOCS_STATES_PATH):
        response = client.get(path, headers={"accept": "image/png"})
        assert response.status_code == 406, path
        assert "image/png" not in response.text


def test_the_rdf_carries_the_facts_and_not_the_prose(client):
    """An explanation is for a reader.

    Putting a paragraph in the data would invite a consumer to parse English,
    which is the thing negotiation exists to spare them. So the machine-readable
    metric page states the label, the dimension and the cost, and says nothing
    about what a metric establishes.
    """
    turtle = client.get(
        DOCS_METRICS_PATH, headers={"accept": "text/turtle"}
    ).text
    for metric, facts in METRIC_DOCS.items():
        assert facts["label"] in turtle, metric
        assert facts["cost"] in turtle, metric
        assert facts["explains"][:40] not in turtle, f"{metric}'s prose reached the RDF"


def test_the_state_rdf_carries_the_drawing(client):
    """A client rendering these verdicts itself needs the channels.

    Which is the point of publishing them: that the difference between verified
    and declared-but-wrong is a border weight rather than a colour is a fact
    about the encoding, and a consumer that redrew it in colour alone would lose
    the property the encoding exists for.
    """
    turtle = client.get(DOCS_STATES_PATH, headers={"accept": "text/turtle"}).text
    for state in verdict_encoding.STATES:
        assert state.slug in turtle
        assert state.meaning in turtle
    assert "border-weight-px" in turtle


# ---------------------------------------------------------------------------
# The shared shell: base.html, one stylesheet, one nav table
# ---------------------------------------------------------------------------


def test_the_docs_pages_carry_no_stylesheet_of_their_own(client):
    """The tokens must live in one place.

    They were copy-pasted into all eight templates until 2026-09-15, which is
    why re-tinting the palette was eight edits that could disagree. This test is
    the ratchet that keeps them from coming back.
    """
    for path in ("/docs", "/docs/metrics", "/docs/states", "/docs/void"):
        body = client.get(path, headers={"accept": "text/html"}).text
        assert "--accent:" not in body, (
            f"{path} declares a design token inline; tokens belong in "
            f"web/static/site.css. Page-specific RULES belong in head_extra."
        )
        assert app_module.STYLESHEET_PATH in body, (
            f"{path} must link the one stylesheet"
        )


def test_every_page_offers_the_same_header_nav(client):
    for path in ("/docs", "/docs/metrics", "/docs/states", "/docs/void"):
        body = client.get(path, headers={"accept": "text/html"}).text
        nav = [a["href"] for a in with_attribute(body, "data-nav")]
        assert nav == ["/", "/explore", "/history", "/docs", "/about"], f"{path} nav is {nav}"


def test_no_docs_page_ships_an_href_jinja_could_not_resolve(client):
    """void_path reached every docs page through _nav_context on 2026-09-15.

    Before that it was set in two of eight page contexts, and Jinja's default
    Undefined renders a missing value as an empty string rather than raising,
    so the other six pages would have shipped href="" with nothing to catch
    it. That is the failure this guards, and it is not specific to void_path:
    ANY path key a context forgets renders the same silent empty href.

    It was written as a check on the footer's VoID link, which is what the
    missing key showed up as. The owner removed that link on 2026-09-17, so
    the test now asks the question the bug was actually about, of every link
    on every docs page rather than of one.
    """
    for path in (DOCS_PATH, DOCS_METRICS_PATH, DOCS_STATES_PATH, DOCS_VOID_PATH):
        body = client.get(path, headers={"accept": "text/html"}).text
        hrefs = [a["href"] for a in with_attribute(body, "href")]
        assert hrefs, f"{path} has no links at all"
        assert "" not in hrefs, f"{path} has an href Jinja could not resolve"

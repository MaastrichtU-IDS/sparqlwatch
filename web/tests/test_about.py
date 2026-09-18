"""`/about`: the page a stranger reaches from our User-Agent.

Every request the prober makes carries
`sparqlwatch/<version> (+https://.../about)`, so this is the one page whose
reader did not come looking for us. They found an unfamiliar agent in a
server log and followed the URL to find out who is querying their endpoint
and how to make it stop. Three things follow from that, and they are what
this file asserts.

**It must answer when nothing else does.** `get_store` raises on a missing
store, an empty one, and (since stage 3-1 Task 1) one holding run graphs and
no derived `current` graph. The index and the endpoint page take that
dependency and are right to. This page must not: a stranger whose logs point
here is owed an answer whether or not this deployment has a store behind it.
`test_about_answers_with_no_store_configured_at_all` is that claim, and the
test beside it shows the same condition really does stop the index, so the
first is not passing by accident.

**Its numbers are the prober's, or they are wrong.** The politeness figures
on the page are a promise made to somebody's server. A page that says "two
seconds" while `DEFAULT_MIN_GAP` says something else is a confident wrong
answer about this project's own behaviour, which is the defect class this
project exists to avoid. So the numbers are read out of `prober/src/` here
and compared against the rendered page, and `prober/src/main.rs` is checked
to still take its flag defaults FROM those constants: a constant nothing
defaults to would pin nothing.

**Its promise is exactly as strong as the mechanism behind it.** Task 3
built the exclusion list and wrote down six limits it has. The page states
all six, because a courtesy channel described more generously than it works
is worse than none: the reader stops watching their logs and we keep
probing.

Every number asserted here is read from the file that decides it. Nothing in
this file is a remembered value.
"""

import re
from pathlib import Path

import pytest
from conftest import requires_repo_sources
from pyoxigraph import NamedNode, RdfFormat, Store, parse
from starlette.testclient import TestClient

import app as app_module
from app import (
    ABOUT_PATH,
    SAME_HOST_DIFFERENT_PORTS,
    INDEX_PATH,
    OFFERED_MEDIA_TYPES,
    RDF_MEDIA_TYPES,
    STORE_PATH_VARIABLE,
    app,
)

# The generic HTML readers, imported rather than written a third time. See
# test_index.py's note: they depend on nothing about the page they read.
from test_page import texts_with, with_attribute

REPO = Path(__file__).resolve().parents[2]
PROBER = REPO / "prober"

# The address the user supplied for this purpose, and the only one this page
# may carry.
CONTACT = "michel.dumontier@maastrichtuniversity.nl"

SW = "urn:sparqlwatch:"
SERVICE = NamedNode(SW + "service")
ABOUT = SW + "about:"
XSD_INTEGER = NamedNode("http://www.w3.org/2001/XMLSchema#integer")

# An email address, loosely enough to catch one this page should not carry.
EMAIL = re.compile(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+")


# ---------------------------------------------------------------------------
# What the prober's own source says
# ---------------------------------------------------------------------------
def rust(name: str) -> str:
    return (PROBER / "src" / name).read_text()


def const_secs(source: str, name: str) -> int:
    """`pub const NAME: Duration = Duration::from_secs(N);`, as N."""
    found = re.search(
        r"const\s+" + name + r"\s*:\s*Duration\s*=\s*Duration::from_secs\((\d+)\)",
        source,
    )
    assert found, f"{name} is not a Duration::from_secs constant any more"
    return int(found.group(1))


def const_nonzero(source: str, name: str) -> int:
    """`const NAME: NonZeroUsize = NonZeroUsize::new(N).unwrap();`, as N."""
    found = re.search(
        r"const\s+" + name + r"\s*:\s*NonZeroUsize\s*=\s*NonZeroUsize::new\((\d+)\)",
        source,
    )
    assert found, f"{name} is not a NonZeroUsize constant any more"
    return int(found.group(1))


def budget_secs(field: str) -> int:
    """One field of `Budget`'s `Default`, in seconds."""
    source = rust("budget.rs")
    body = source[source.index("impl Default for Budget") :]
    found = re.search(field + r":\s*Duration::from_secs\((\d+)\)", body)
    assert found, f"Budget::default() no longer sets {field} in whole seconds"
    return int(found.group(1))


def prober_defaults() -> dict[str, int]:
    """Every number this page states about how the prober behaves, read from
    the prober.

    `DEFAULT_MIN_GAP` and `DEFAULT_RETRY_AFTER_CAP` are defined in
    `politeness.rs` and `DEFAULT_CONCURRENCY` in `main.rs`; all three are
    named by `main.rs`'s `default_value_t` attributes, which is what
    `test_the_flag_defaults_still_come_from_those_constants` checks. Reading
    the constant is therefore reading the default.
    """
    politeness = rust("politeness.rs")
    main = rust("main.rs")
    min_gap = const_secs(politeness, "DEFAULT_MIN_GAP")
    hosts = const_nonzero(main, "DEFAULT_CONCURRENCY")
    return {
        "min-gap-seconds": min_gap,
        "hosts-in-flight": hosts,
        "requests-per-second": hosts // min_gap,
        "retry-after-cap-seconds": const_secs(politeness, "DEFAULT_RETRY_AFTER_CAP"),
        "request-budget-seconds": budget_secs("request"),
        "metric-budget-seconds": budget_secs("metric"),
        "endpoint-budget-seconds": budget_secs("endpoint"),
        # One request per metric measurable at the default cost ceiling, which
        # is `Cost::Cheap`: `main.rs` declares
        # `#[arg(long, value_enum, default_value_t = Cost::Cheap)]`.
        #
        # MINUS the derived ones, which is not a nicety. A derived metric is
        # cheap because it sends NOTHING: `vocabulary-described` grades the
        # profile pass's results against the description already fetched. This
        # counted every cheap metric until 2026-09-05, and adding that metric
        # made the page promise operators one more request per endpoint than it
        # sends, on the very page that exists to tell them what we do to their
        # server. Mirrors ProbeKind::dispatched_per_metric.
        "requests-per-endpoint": _cheap_metrics_that_send_a_request(),
    }


# Kinds in prober/metrics.toml that send no request. Kept beside the count they
# correct, as a set so a second derived kind is one edit.
_DERIVED_KINDS = {"VocabularyDescribed"}


def _cheap_metrics_that_send_a_request() -> int:
    """How many requests one endpoint receives at the default cost ceiling."""
    import re

    text = (PROBER / "metrics.toml").read_text()
    n = 0
    for block in text.split("[[metric]]")[1:]:
        if not re.search(r'^cost = "cheap"', block, re.M):
            continue
        kind = re.search(r'^kind = "([^"]+)"', block, re.M)
        if kind and kind.group(1) in _DERIVED_KINDS:
            continue
        n += 1
    return n


def const_int(source: str, name: str) -> int:
    """`pub const NAME: u64 = N;`, as N, with Rust's digit separators dropped.

    The dormancy thresholds are bare `u64` and `u32` constants rather than
    `Duration`s or `NonZero`s, so neither reader above matches them:
    `const_secs` wants `Duration::from_secs` and `const_nonzero` wants
    `NonZeroUsize::new`. The type is required in the pattern so that a constant
    that changed type, and therefore changed unit, reds here instead of being
    read as the same number.
    """
    found = re.search(
        r"const\s+" + name + r"\s*:\s*u(?:32|64)\s*=\s*([\d_]+)\s*;", source
    )
    assert found, f"{name} is not a bare u32/u64 integer constant any more"
    return int(found.group(1).replace("_", ""))


def dormancy_defaults() -> dict[str, int]:
    """The four numbers the admission policy is calibrated against, read from
    `prober/src/dormancy.rs`.

    THE UNIT CONVERSION IS HERE AND IT IS THE POINT OF THIS READER.
    `DEFAULT_COST_MS` is 60_000, in milliseconds, because the flag that carries
    it (`--dormant-cost-ms`) takes milliseconds; the honest number for a
    stranger reading a sentence about their own server is 60 seconds. So the
    page states seconds and this divides, and a constant that stops being a
    whole number of seconds fails here rather than being rounded onto the page.

    `DEFAULT_GRACE_DAYS` is the odd one of the four: it is not a flag on the
    sweeper. `main.rs` fills it in from the constant and nothing in that binary
    reads it, because its one reader is `dormancy::wake`, whose flag lives on
    the `dormancy` binary. See `thresholds_from`'s doc comment, which says why a
    `--dormant-grace-days` on the sweeper was removed rather than left to
    mislead. It is on the page because it is the number a person who asked to be
    left alone gets: a hand wake is immune from automatic relegation for that
    many days.
    """
    dormancy = rust("dormancy.rs")
    cost_ms = const_int(dormancy, "DEFAULT_COST_MS")
    assert cost_ms % 1000 == 0, (
        f"DEFAULT_COST_MS is {cost_ms} ms, which is not whole seconds, so the "
        "page cannot state it in seconds"
    )
    return {
        "dormant-cost-seconds": cost_ms // 1000,
        "dormant-strikes": const_int(dormancy, "DEFAULT_STRIKES"),
        "dormant-cadence-days": const_int(dormancy, "DEFAULT_CADENCE_DAYS"),
        "dormant-wake-grace-days": const_int(dormancy, "DEFAULT_GRACE_DAYS"),
    }


def skip_reason_slugs() -> set[str]:
    """`SkipReason::slug`'s match arms, as the strings a run graph can carry.

    The prober is the source of truth for these two words: it writes them into
    every dormancy group, `web/load_run.py` carries them through, and both HTML
    pages render them. Read out of the `match` rather than out of the enum's
    variant names, because the slug is what crosses the wire and a variant can
    be renamed without changing it.
    """
    source = rust("dormancy.rs")
    body = source[source.index("pub fn slug(&self)") :]
    body = body[: body.index("\n    }")]
    found = re.findall(r'SkipReason::\w+\s*=>\s*"([^"]+)"', body)
    assert found, "SkipReason::slug is no longer a match over string literals"
    return set(found)


def user_agent() -> str:
    """The User-Agent the prober really sends, rebuilt from `client.rs`'s
    `concat!` and the crate version `env!("CARGO_PKG_VERSION")` expands to."""
    source = rust("client.rs")
    found = re.search(
        r'\.user_agent\(concat!\(\s*"([^"]*)",\s*env!\("CARGO_PKG_VERSION"\),\s*'
        r'"([^"]*)"\s*\)\)',
        source,
    )
    assert found, "client.rs no longer builds its User-Agent from a concat!"
    version = re.search(
        r'^version\s*=\s*"([^"]+)"',
        (PROBER / "Cargo.toml").read_text(),
        re.M,
    )
    assert version, "prober/Cargo.toml has no version"
    return found.group(1) + version.group(1) + found.group(2)


def exclusions_path() -> str:
    found = re.search(
        r'pub const DEFAULT_EXCLUSIONS:\s*&str\s*=\s*"([^"]+)"', rust("registry.rs")
    )
    assert found, "registry.rs no longer has a DEFAULT_EXCLUSIONS constant"
    return found.group(1)


def provenance() -> dict[str, str]:
    """`registry/lod-cloud.provenance.toml` as a flat key/value mapping. The
    seeder writes it, so this is where the registry's origin is recorded."""
    text = (PROBER / "registry" / "lod-cloud.provenance.toml").read_text()
    return dict(
        re.findall(r'^(\w+)\s*=\s*"?([^"\n]+?)"?\s*$', text, re.M)
    )


def registry_size() -> int:
    """How many endpoints `registry/lod-cloud.toml` actually lists.

    Parses TOML natively to handle both bare-string and table formats.
    This is an independent cross-check of the hand-maintained constant in
    app.py:3212, so it does not import from registry_names even though both
    parse the same file — sharing would make the test circular.
    """
    import tomllib
    text = (PROBER / "registry" / "lod-cloud.toml").read_text()
    raw = tomllib.loads(text)
    return len(raw.get("endpoint", []))


def registry_endpoints() -> list[str]:
    """Every endpoint `registry/lod-cloud.toml` lists, as written.

    Parses TOML natively to handle both bare-string and table formats.
    This is an independent cross-check of the hand-maintained constant in
    app.py:3212, so it does not import from registry_names even though both
    parse the same file — sharing would make the test circular.
    """
    import tomllib
    text = (PROBER / "registry" / "lod-cloud.toml").read_text()
    raw = tomllib.loads(text)
    endpoints = []
    for entry in raw.get("endpoint", []):
        if isinstance(entry, str):
            endpoints.append(entry)
        else:
            url = entry.get("url", "")
            if url:
                endpoints.append(url)
    return endpoints


def _bare_host(url: str) -> str:
    """A URL's host with its port dropped, which is what "one machine" means to
    the operator reading this page. `politeness::host_key` deliberately keeps a
    non-default port, so this is the coarser thing the key is NOT."""
    authority = url.split("//", 1)[1].split("/", 1)[0]
    return authority.rsplit(":", 1)[0] if ":" in authority else authority


def _port_of(url: str) -> str:
    authority = url.split("//", 1)[1].split("/", 1)[0]
    return authority.rsplit(":", 1)[1] if ":" in authority else ""


# ---------------------------------------------------------------------------
# Fetching the page
# ---------------------------------------------------------------------------
@pytest.fixture
def client():
    """A client with NO store behind it and no dependency override.

    Deliberately built the way no other test in this suite is: every other
    client is handed a fixture store, and this page's whole contract is that
    it does not need one. The environment variable is cleared here as well as
    in the test that names it, so a machine that happens to have a store
    configured cannot make these tests pass for the wrong reason.
    """
    with pytest.MonkeyPatch.context() as patch:
        patch.delenv(STORE_PATH_VARIABLE, raising=False)
        with TestClient(app) as built:
            yield built


def get(client, accept="*/*"):
    """GET /about. ``accept=None`` sends no Accept header at all, which httpx
    will not do by default."""
    request = client.build_request("GET", ABOUT_PATH)
    if accept is None:
        del request.headers["accept"]
    else:
        request.headers["accept"] = accept
    return client.send(request)


def page(client) -> str:
    response = get(client, "text/html")
    assert response.status_code == 200
    return response.text


def graph_of(response) -> Store:
    """Parse a response body as RDF. Parsing is the assertion: pyoxigraph
    raises SyntaxError on a body that is not the media type it claimed."""
    store = Store()
    store.extend(
        quad
        for quad in parse(
            response.content,
            format=RdfFormat.from_media_type(
                response.headers["content-type"].split(";")[0].strip()
            ),
        )
    )
    return store


def one_object(store: Store, predicate: str):
    objects = {
        quad.object
        for quad in store.quads_for_pattern(SERVICE, NamedNode(predicate), None)
    }
    assert len(objects) == 1, f"{predicate} has {len(objects)} objects, wanted 1"
    return objects.pop()


def attributes_by(text: str, name: str) -> dict[str, dict]:
    """Every element carrying ``name``, keyed by that attribute's value."""
    found = {}
    for attrs in with_attribute(text, name):
        key = attrs[name]
        assert key not in found, f"{name}={key!r} appears more than once"
        found[key] = attrs
    return found


# ---------------------------------------------------------------------------
# It answers when the store does not
# ---------------------------------------------------------------------------
def test_about_answers_with_no_store_configured_at_all(client):
    """The one page a stranger reaches, with nothing behind it.

    No store path in the environment, no dependency override, no fixture. A
    reader who followed our User-Agent out of their own logs gets the answer
    whatever state this deployment's data is in.
    """
    response = get(client, "text/html")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/html")
    assert CONTACT in response.text


def test_the_index_does_not_answer_in_that_same_condition(client):
    """Proof the test above is not passing for a boring reason.

    `get_store` really does raise with no store configured, so if /about took
    that dependency it would fail exactly here. This test is what makes
    "/about does not take the store dependency" an observed difference
    between two routes rather than a claim about a function signature.
    """
    with pytest.raises(RuntimeError, match=STORE_PATH_VARIABLE):
        client.get(INDEX_PATH)


# ---------------------------------------------------------------------------
# It negotiates
# ---------------------------------------------------------------------------
def test_about_serves_html_to_a_browser_and_to_a_bare_curl(client):
    for accept in ("text/html", "*/*", None, "text/html,application/xhtml+xml"):
        response = get(client, accept)
        assert response.status_code == 200, accept
        assert response.headers["content-type"].startswith("text/html"), accept


def test_about_serves_every_rdf_representation_it_offers(client):
    """The design spec requires content negotiation of every resource, and
    this is a resource: what a machine wants from it is the contact address
    and the rate we promise, without parsing prose to find them."""
    for media_type in RDF_MEDIA_TYPES:
        response = get(client, media_type)
        assert response.status_code == 200, media_type
        assert response.headers["content-type"].startswith(media_type), media_type
        graph = graph_of(response)
        assert len(graph) > 0, media_type


def test_about_refuses_a_representation_it_cannot_serve(client):
    response = get(client, "application/pdf")
    assert response.status_code == 406
    for offered in OFFERED_MEDIA_TYPES:
        assert offered in response.text


# ---------------------------------------------------------------------------
# It is on the shared shell
# ---------------------------------------------------------------------------
def test_the_page_carries_no_stylesheet_of_its_own(client):
    body = page(client)
    assert "--accent:" not in body, (
        "design tokens belong in web/static/site.css, not in this template"
    )
    assert app_module.STYLESHEET_PATH in body


def test_the_rdf_and_the_html_state_the_same_numbers_and_the_same_address(client):
    """Two representations of one resource, and they have to agree.

    The endpoint page's RDF cannot disagree with its HTML because a CONSTRUCT
    serialises the store. Here there is no store, so the RDF is assembled in
    Python from the same constants the template renders. That is exactly the
    shape that can drift, so the agreement is asserted rather than reasoned
    about.
    """
    html = page(client)
    graph = graph_of(get(client, "text/turtle"))

    assert one_object(graph, ABOUT + "contact") == NamedNode("mailto:" + CONTACT)
    assert texts_with(html, "data-summary") == [one_object(graph, ABOUT + "summary").value]

    shown = attributes_by(html, "data-politeness")
    for key, attrs in shown.items():
        stated = one_object(graph, ABOUT + key)
        assert stated.datatype == XSD_INTEGER, key
        assert stated.value == attrs["data-value"], key


# ---------------------------------------------------------------------------
# The numbers are the prober's
# ---------------------------------------------------------------------------
@requires_repo_sources
def test_the_politeness_numbers_on_the_page_are_the_prober_defaults(client):
    """Read from `prober/src/`, compared against the page.

    Both halves of each claim are checked: the machine-readable
    ``data-value``, and the digits inside the sentence a person reads. A page
    whose attribute was right and whose prose said something else would be
    worse than one that was simply wrong, because the wrong half is the half
    the sysadmin acts on.
    """
    html = page(client)
    shown = attributes_by(html, "data-politeness")
    # BOTH dicts, and the page states their union or it states a different set.
    # The dormancy thresholds are politeness figures in exactly the sense this
    # section is about: they are how many requests somebody's server gets. They
    # are read from a different file, in a different unit, by a different
    # reader, so they are a second dict rather than four more entries in the
    # first, and the page has to carry all of them.
    expected = prober_defaults() | dormancy_defaults()

    assert set(shown) == set(expected), "the page states a different set of numbers"
    for key, value in expected.items():
        assert shown[key]["data-value"] == str(value), key

    texts = {
        attrs["data-politeness"]: text
        for attrs, text in zip(
            with_attribute(html, "data-politeness"),
            texts_with(html, "data-politeness"),
        )
    }
    for key, value in expected.items():
        assert re.search(rf"\b{value}\b", texts[key]), (
            f"{key}: the words a person reads do not contain {value}: {texts[key]!r}"
        )


@requires_repo_sources
def test_the_flag_defaults_still_come_from_those_constants():
    """The constants the test above reads are the values the flags default to.

    Without this, `DEFAULT_MIN_GAP` could keep saying two seconds while
    `--min-gap-ms` defaulted to something else, and the page would be pinned
    to a number no sweep uses.
    """
    main = rust("main.rs")
    for attribute in (
        "default_value_t = DEFAULT_MIN_GAP.as_millis() as u64",
        "default_value_t = DEFAULT_RETRY_AFTER_CAP.as_secs()",
        "default_value_t = DEFAULT_CONCURRENCY",
    ):
        assert attribute in main, attribute


def test_the_page_says_concurrency_counts_hosts_and_not_endpoints(client):
    """The honest and the reassuring statement are the same one here.

    `--concurrency` bounds how many HOST GROUPS run at once; within one host
    the prober holds a gate so one request is in flight at a time, with the
    gap measured from the release of the previous one. A page that said "four
    requests at once" would understate the politeness and overstate the load
    a single operator sees.
    """
    text = " ".join(texts_with(page(client), "data-politeness")).lower()
    assert "host" in text
    assert "one endpoint" in text or "one request" in text


def test_the_page_says_the_gap_and_the_count_are_per_host_and_port(client):
    """The politeness promise, pinned against the gate's actual key.

    The gate does not key on the host. `politeness::host_key` folds a
    scheme-default port and keeps every other one, its own unit tests assert
    that two ports are two keys, and `lib.rs` groups endpoints on that same key,
    so both the two-second gap and the four-at-once bound are per host AND port.
    The page used to say "one host", full stop, and "if you run several of the
    endpoints on the list, they are probed one after another, not together",
    which is false for exactly the reader it was written for: an operator
    running two engines on one machine behind two ports.

    The keying is deliberate and it is not what changed. What is pinned here is
    that the page says what the key is, that the pair it names to make the
    statement checkable is really on the shipped list, and that the two really
    are one machine and two ports.
    """
    html = page(client)
    numbers = {
        attrs["data-politeness"]: text
        for attrs, text in zip(
            with_attribute(html, "data-politeness"),
            texts_with(html, "data-politeness"),
        )
    }
    for key in ("min-gap-seconds", "hosts-in-flight"):
        assert "port" in numbers[key], (
            f"{key} is keyed on a port and the sentence does not say so: "
            f"{numbers[key]!r}"
        )
    assert "several of the endpoints on the list, they are probed one after" not in (
        " ".join(numbers.values())
    ), "the unconditional promise is the false one"

    caveat = texts_with(html, "data-politeness-caveat")
    assert len(caveat) == 1, f"one caveat, got {len(caveat)}"
    assert "at the same time" in caveat[0], caveat[0]

    named = [
        attrs["data-caveat-endpoint"] for attrs in with_attribute(html, "data-caveat-endpoint")
    ]
    assert named == ["1", "2"], "the page names two endpoints, in order"
    first, second = SAME_HOST_DIFFERENT_PORTS
    assert first in caveat[0] and second in caveat[0], caveat[0]

    listed = registry_endpoints()
    for endpoint in (first, second):
        assert endpoint in listed, (
            f"{endpoint} is not on registry/lod-cloud.toml any more, so the "
            f"page names a case a reader cannot check"
        )
    assert _bare_host(first) == _bare_host(second), "they must be one machine"
    assert _port_of(first) != _port_of(second), "and two ports"


@requires_repo_sources
def test_the_gate_really_keys_on_the_port_the_page_names(client):
    """The other side of the sentence above: the prober's own pins.

    Two of them, and they are different claims. `host_key`'s unit tests assert
    that two ports are two keys, which is where the behaviour is decided;
    `tests/politeness.rs` drives the real pair from the shipped registry through
    a real gate and asserts the two run together, which is where the page's
    sentence is checked end to end. Read from the source here for the same
    reason every other number on this page is: nothing in this process can run
    Rust, and a page pinned to a constant nothing acts on pins nothing.
    """
    politeness = rust("politeness.rs")
    assert re.search(
        r'assert_ne!\(\s*host_key\("http://example\.org:7878/x"\),\s*'
        r'host_key\("http://example\.org:7879/x"\)\s*\)',
        politeness,
    ), "host_key no longer asserts that two ports are two keys"
    assert "host_key(ep)" in rust("lib.rs"), (
        "the sweep no longer groups endpoints on host_key, so the page's "
        "concurrency sentence is about something else"
    )
    driven = (PROBER / "tests" / "politeness.rs").read_text()
    assert (
        "two_shipped_endpoints_on_one_machine_behind_different_ports_run_together"
        in driven
    ), "the end-to-end pin behind this page's paragraph is gone"


@requires_repo_sources
def test_the_user_agent_shown_is_the_one_the_prober_sends(client):
    """The string the reader searched their logs for, exactly.

    Rebuilt from `client.rs`'s `concat!` and `prober/Cargo.toml`'s version,
    so a version bump reds this test and the page has to move with it. That
    is the intended cost: the page quotes a version, so the quote is a claim.
    """
    shown = texts_with(page(client), "data-user-agent")
    assert shown == [user_agent()]


# ---------------------------------------------------------------------------
# Where the list came from, and how often we come back
# ---------------------------------------------------------------------------
def test_the_page_says_where_the_endpoint_list_came_from(client):
    """A reader's first question after "who are you" is "why me".

    Nobody on this list opted in: it is third-party metadata from a public
    dump, which is the whole reason the exclusion mechanism has to exist.
    """
    shown = attributes_by(page(client), "data-registry")
    prov = provenance()

    assert shown["endpoint-count"]["data-value"] == str(registry_size())
    assert shown["endpoint-count"]["data-value"] == prov["seeded"]
    assert shown["dump"]["href"] == prov["source"]
    assert prov["version"] in shown["dump-version"]["data-value"]
    assert shown["entries"]["data-value"] == prov["entries"]
    assert shown["distinct"]["data-value"] == prov["distinct"]


@requires_repo_sources
def test_the_page_says_nothing_is_on_a_schedule_and_nothing_is(client):
    """What a reader can expect in their logs, today.

    There is no deployment and no scheduled job in this repository: CI runs
    on push and pull request and nothing else, and there is no manifest
    declaring a periodic sweep. So a sweep is something a person starts, and
    saying otherwise would promise a cadence that does not exist. When stage
    4 adds the schedule this test reds, and the page is what has to change.
    """
    workflows = list((REPO / ".github" / "workflows").glob("*.y*ml"))
    assert workflows
    for workflow in workflows:
        assert "schedule:" not in workflow.read_text(), workflow
    manifests = [
        path
        for path in REPO.glob("*/*.y*ml")
        if path.parent.name not in {"workflows"}
    ]
    assert manifests == [], f"a deployment manifest exists now: {manifests}"

    html = page(client)
    text = " ".join(texts_with(html, "data-cadence")).lower()
    assert "schedule" in text
    assert "by hand" in text or "by a person" in text

    # The one measured full sweep, quoted from where it is recorded rather
    # than rounded into the page. prober/README.md's own Sweep cost section
    # says every other figure there is an estimate, so this is the only
    # duration this page may state.
    duration = re.search(r"\b\d+h\d+m\d+s\b", text)
    assert duration, f"no full-sweep duration on the page: {text!r}"
    assert duration.group(0) in (PROBER / "README.md").read_text()


@requires_repo_sources
def test_the_page_says_which_kind_of_block_changes_anything_and_which_does_not(
    client,
):
    """The reader's own first instinct, answered honestly now that half of the
    old answer is false.

    The page used to say a firewall rule "costs you the requests and gains you
    nothing, because nothing yet drops an endpoint from the list for failing".
    The admission policy drops one, so the second half is gone, and what
    replaced it is the distinction that decides which half of the old sentence
    still holds: WHICH KIND OF RULE.

    A fast refusal, a reset or an HTTP error, costs this project nothing, so it
    is never a strike and the endpoint is asked again on the next sweep at the
    same rate. Silently dropping the packets is exactly what the policy measures,
    because holding the connections open until each probe is cancelled is what
    made 57 endpoints 90% of one sweep's cost, so a blackhole earns dormancy and
    buys at most one probe in every seven days instead of one per sweep. Saying
    "blocking gains you nothing" would now be wrong for one of the two and
    saying "blocking works" would be wrong for the other.

    `prober/README.md` carried the same claim under "Not yet operable on a
    daily cadence" and this test holds both to the new one.
    """
    # Whitespace-collapsed, because the sentence is wrapped in the README and
    # a line break is not a change of claim.
    readme = " ".join((PROBER / "README.md").read_text().split())
    assert "nothing yet stops the dead being re-probed on every sweep" not in readme, (
        "the README still says the dead are re-probed on every sweep, which the "
        "admission policy is what changed"
    )
    assert "the dead are no longer re-probed on every sweep" in readme
    assert "a fast refusal costs nothing and so earns no relegation" in readme

    text = " ".join(texts_with(page(client), "data-blocking")).lower()
    assert "block" in text
    assert "next sweep" in text
    # The two kinds, and the page may not describe one without the other: a
    # reader deciding what rule to write acts on exactly this distinction.
    assert "refus" in text, text
    assert "drop" in text, text
    assert "dormant" in text, text
    # The old promise, which is now false of one of the two kinds.
    assert "gains you nothing" not in text, text


# ---------------------------------------------------------------------------
# The way out, described exactly as strongly as it works
# ---------------------------------------------------------------------------
# The six limits Task 3 recorded in `without_excluded`'s doc comment, in
# `registry.rs`'s module doc and in `registry/exclusions.toml`'s own header.
# Each is a way the mechanism is weaker than "email us and you are removed",
# and each is on the page for that reason.
EXCLUSION_LIMITS = {
    # Nothing watches the mailbox. A person has to add the entry.
    "by-hand": ("person", "hand"),
    # Once added, no rebuild and no redeploy stands in the way.
    "next-sweep": ("sweep",),
    # An exclusion stops future probing; it does not unpublish the past.
    "no-retraction": ("publish", "past", "remov", "retract"),
    # Whole host, not a suffix: example.org does not cover sub.example.org.
    "whole-host": ("host",),
    # Nothing is resolved: an alias is a second entry.
    "no-resolution": ("name", "alias", "resolv"),
    # The entry itself is public.
    "published": ("public", "publish"),
    # An IPv6 literal cannot be written as an entry: parse_exclusions refuses
    # any host containing a colon, and an IPv6 literal is full of them. Latent
    # rather than live (no endpoint on the shipped list is written that way),
    # and on the page because "one entry covers one whole host" is the claim it
    # is the exception to.
    "no-ipv6-literal": ("ipv6",),
}


def test_the_page_describes_the_exclusion_mechanism_and_every_limit_of_it(client):
    """All six, each in the element contracted to carry it.

    A promise is a claim, and this project's rule about confident wrong
    answers covers claims about itself. Asserting the words are somewhere in
    the document would pass while the sentence sat in an unrelated
    paragraph, so each limit is read out of its own element.
    """
    html = page(client)
    shown = attributes_by(html, "data-exclusion-limit")
    assert set(shown) == set(EXCLUSION_LIMITS), "the page states a different set"

    texts = {
        attrs["data-exclusion-limit"]: text.lower()
        for attrs, text in zip(
            with_attribute(html, "data-exclusion-limit"),
            texts_with(html, "data-exclusion-limit"),
        )
    }
    for slug, wanted in EXCLUSION_LIMITS.items():
        assert any(word in texts[slug] for word in wanted), (
            f"{slug}: {texts[slug]!r} says none of {wanted}"
        )


@requires_repo_sources
def test_the_page_names_the_file_an_exclusion_lands_in(client):
    """The path both binaries read at every run, as `registry.rs` spells it.

    Named because it is checkable: a reader can see whether their host is on
    the list, and that is the only way this promise is verifiable from
    outside.
    """
    assert exclusions_path() in page(client)


@requires_repo_sources
def test_the_ipv6_limit_the_page_states_is_the_one_the_parser_has(client):
    """The seventh limit, read off the parser rather than remembered.

    `parse_exclusions` refuses any host containing a colon, so an IPv6 literal
    has no writable entry at all: `[2001:db8::1]` is refused as "names a URL
    rather than a host". The page says so, and this test fails if the refusal
    is ever widened to accept a bracketed literal, at which point the page has
    to change rather than the caveat quietly becoming false in the other
    direction.
    """
    registry = rust("registry.rs")
    assert "host.contains(['/', ':', '@', ' '])" in registry, (
        "parse_exclusions no longer refuses a host containing a colon, so the "
        "page's IPv6 caveat may no longer be true"
    )
    limits = {
        attrs["data-exclusion-limit"]: text.lower()
        for attrs, text in zip(
            with_attribute(page(client), "data-exclusion-limit"),
            texts_with(page(client), "data-exclusion-limit"),
        )
    }
    assert "colon" in limits["no-ipv6-literal"], limits["no-ipv6-literal"]


@requires_repo_sources
def test_the_redirect_sentence_states_the_hop_limit_the_client_enforces(client):
    """"follows a redirect from it if there is one, and stops" understated it.

    `client.rs`'s MAX_REDIRECT_HOPS is 5, and a chain of up to five gated hops
    is followed, so a log can show five requests where the page implied two. The
    intent it was written for ("it does not crawl") is unchanged and still on the
    page; the number is now the client's own.
    """
    found = re.search(r"const MAX_REDIRECT_HOPS:\s*usize\s*=\s*(\d+)", rust("client.rs"))
    assert found, "client.rs no longer has a MAX_REDIRECT_HOPS constant"
    shown = texts_with(page(client), "data-redirects")
    assert shown == [found.group(1)], (
        f"the page states {shown} redirect hops and the client enforces "
        f"{found.group(1)}"
    )


@requires_repo_sources
def test_the_unroutable_rule_is_described_as_the_seeder_only_rule_it_is(client):
    """`without_unroutable_hosts` is not wired into `load_endpoints`.

    The page said the private-and-loopback rule was one "this project applies to
    any list it is given", and it is not: it runs in the seeder, on the way to
    writing `registry/lod-cloud.toml`, and wiring it into `load_endpoints` was
    tried and reverted. One of the five refusals recorded for this dump came from
    it, so the rule is real and its scope is not what the page said.
    """
    registry = rust("registry.rs")
    body = registry[registry.index("pub fn load_endpoints") :]
    body = body[: body.index("\n}")]
    assert "without_unroutable_hosts" not in body, (
        "load_endpoints now applies the unroutable rule too, so the page's "
        "sentence about where it runs has to change back"
    )
    assert "pub fn without_unroutable_hosts" in registry, "the rule still exists"

    said = " ".join(texts_with(page(client), "data-registry"))
    whole = page(client)
    assert "loopback" in whole
    # The claim the page may no longer make: that this rule applies to any list.
    assert "rules this project applies to any list it is given" not in whole


def test_the_page_carries_one_contact_address_and_invents_no_other_channel(client):
    """One address, the one supplied, and nothing that does not exist.

    No form, no ticket queue, no alias, no second mailbox: each would be a
    channel a reader would use and nobody would read. The address is
    published deliberately, so it is also asserted to be reachable as a
    mailto rather than only printed.
    """
    html = page(client)
    assert set(EMAIL.findall(html)) == {CONTACT}
    assert set(re.findall(r"mailto:([^\"'>\s]+)", html)) == {CONTACT}
    lowered = html.lower()
    for absent in ("<form", "<input", "ticket", "helpdesk", "service desk"):
        assert absent not in lowered, absent
    contacts = attributes_by(html, "data-contact")
    assert set(contacts) == {CONTACT}
    assert contacts[CONTACT]["href"] == "mailto:" + CONTACT


# ---------------------------------------------------------------------------
# What the site does not do
# ---------------------------------------------------------------------------
# web/README.md's "What does not exist" list, which exists for the same
# reason this section does: a reader who cannot find a feature should learn
# that it is absent rather than conclude they cannot navigate.
NOT_BUILT = {
    "leaderboard",
    "per-metric-pages",
    "history",
    "charts",
    "faceted-search",
    "query-editor",
    "public-sparql-endpoint",
}


def test_the_page_says_what_this_site_cannot_do(client):
    shown = attributes_by(page(client), "data-not-built")
    assert NOT_BUILT <= set(shown), f"missing {NOT_BUILT - set(shown)}"


def test_the_content_facets_are_recorded_as_blocked_rather_than_unbuilt(client):
    """A different and more useful statement than "not built".

    The vocabulary and class facets the design calls for need content data
    the prober does not yet gather; that is stage 2b. "Blocked on the data"
    tells a reader why asking again later might work, which "not built" does
    not.
    """
    html = page(client)
    shown = attributes_by(html, "data-blocked-on")
    assert "stage-2b" in shown
    text = " ".join(texts_with(html, "data-blocked-on")).lower()
    assert "vocabular" in text or "class" in text


# ---------------------------------------------------------------------------
# What dormant means, for the reader who finds their own endpoint marked
# ---------------------------------------------------------------------------
#
# This is the second reason a stranger arrives here. The first is the User-Agent
# in their logs; the second is following a row on the index that says the newest
# sweep did not ask their endpoint, which became a route a reader can take when
# the two pages grew a link to this one: test_index.py's
# test_the_index_links_to_about_once_and_not_once_per_row and test_page.py's
# test_the_endpoint_page_links_to_about. What they need is what that word
# claims, what it does not claim, and who changes it.
#
# The wording rules are not stylistic. "Weekly" is the one word this section may
# not use, because `test_the_page_says_nothing_is_on_a_schedule_and_nothing_is`
# pins that no sweep runs on a timer: a cadence measured in days is a bound on
# how often a dormant endpoint is asked WHEN somebody runs a sweep, and calling
# it weekly promises a sweep every week. And "unresponsive" is the word the
# prober's own module header refuses, for the reason the six-verdict vocabulary
# exists: what was observed is seven cancelled probes, not a broken server.
DORMANCY_CLAIMS = {
    # What it is: a place in this service's rotation, bounded in days. The
    # bound itself is asserted against the constant in
    # test_the_cadence_the_pages_state_is_the_constant_and_not_a_word, and not
    # spelled out here, because a number written twice is a number that can
    # drift.
    "what-it-is": ("rotation",),
    # What it is not: a verdict, and not a claim about the server.
    "not-a-verdict": ("not a verdict",),
    # WHICH of them put the endpoint here, said before any is described. This
    # section described the automatic case alone and presented it as what the
    # word means, on the one surface a stranger is sent to, while the index
    # panel and the endpoint page both read the slug the run graph carries. The
    # slugs themselves are asserted rather than glossed, because the reader
    # arrives holding one of them, and all of them are asserted because a page
    # naming two of three is the same false claim in a smaller size.
    "which-of-them": ("automatic", "operator-hold", "not-in-this-sweep"),
    # What was actually observed, in the terms the probe was run in, and whose
    # case that is.
    "what-was-measured": ("cancel", "set aside automatically"),
    # And the hand case, where the answer to "what was observed" is nothing.
    # Three claims this section used to make of every dormant endpoint are false
    # of a hold: that checks were sent and cancelled, that it is asked once in
    # the cadence, and that answering puts it back. prober/src/dormancy.rs skips
    # a Dormant hold before it sends anything, keeps it out of every sweep until
    # a person wakes it, and suppresses the promotion regardless, and the only
    # real prober output in this repository carries operator-hold.
    "set-aside-by-hand": (
        "nothing at all was observed",
        "never asked",
        "comes off by hand",
    ),
    # And the third reason, which is not a relegation in either direction. Rule 3
    # of the admission policy published "automatic" for endpoints a replayed
    # `--at` merely did not reach, so this page and both others reported a
    # decision about somebody's server that nobody took. What the reader with
    # this slug needs to be told is that none of the four thresholds is theirs.
    "not-a-relegation": (
        "not a relegation",
        "nothing was observed",
        "none of the four numbers above applies",
    ),
    # Who changes it, and the same honesty about the mailbox as the exclusion
    # list gets: nothing watches it.
    "how-to-change-it": ("person", "hand"),
}


def test_the_page_says_what_dormant_means_and_what_it_does_not(client):
    """One claim per element, because a reader who finds their own endpoint
    marked will read this section and nothing else.

    Seven of them now, and the three added ones are the fix for a section that
    was true of one reason and false of the others on all three of its
    substantive claims.
    """
    html = page(client)
    shown = attributes_by(html, "data-dormancy")
    assert set(shown) == set(DORMANCY_CLAIMS), "the page states a different set"

    texts = {
        attrs["data-dormancy"]: text.lower()
        for attrs, text in zip(
            with_attribute(html, "data-dormancy"),
            texts_with(html, "data-dormancy"),
        )
    }
    for slug, wanted in DORMANCY_CLAIMS.items():
        for word in wanted:
            assert word in texts[slug], f"{slug}: {texts[slug]!r} does not say {word!r}"


@requires_repo_sources
def test_the_dormant_cadence_is_stated_as_a_bound_and_never_as_a_schedule(client):
    """"At most one sweep in every seven days, and only when a person starts
    one", and never "weekly".

    Nothing here runs on a timer, which the cadence paragraph says and
    `test_the_page_says_nothing_is_on_a_schedule_and_nothing_is` pins against
    this repository's own workflows. "Weekly" would contradict it on the one
    page a server operator reads to decide whether to expect us, and it would
    overstate a cadence in the direction that gets somebody's server probed
    more often than they were told.
    """
    html = page(client)
    lowered = html.lower()
    assert "weekly" not in lowered, "the page calls the dormant cadence weekly"
    assert "every week" not in lowered
    # The bound, whole, in one element rather than assembled by a reader out of
    # two sentences in different sections. The number comes from the constant
    # for the reason test_the_cadence_the_pages_state_is_the_constant_and_not_a_word
    # gives; what this test owns is that the bound is stated as a bound at all.
    cadence = dormancy_defaults()["dormant-cadence-days"]
    said = " ".join(texts_with(html, "data-dormancy")).lower()
    assert f"at most one sweep in every {cadence} days" in said, said
    assert "only when a person starts one" in said, said


@requires_repo_sources
def test_the_page_does_not_call_a_dormant_endpoint_unresponsive(client):
    """The word `prober/src/dormancy.rs` refuses, refused here too.

    Its module header says why: what is known is that seven probes, each
    cancelled at 30 s, went unanswered. "Dormant" describes where the endpoint
    sits in our rotation, which is a fact about us; "unresponsive", "broken",
    "dead" and "down" are all claims about somebody else's server that this
    service did not establish. The prober's own source is read here so the two
    cannot drift apart.
    """
    header = rust("dormancy.rs")
    assert "`dormant`, not `unresponsive`" in header, (
        "dormancy.rs no longer argues for the word, so this test is pinning "
        "the page against something that moved"
    )
    lowered = page(client).lower()
    for overclaim in ("unresponsive", "broken", "is dead", "is down"):
        assert overclaim not in lowered, overclaim


@requires_repo_sources
def test_the_dormancy_numbers_come_from_the_flags_that_carry_them():
    """The four constants the reader above reads are the flag defaults.

    The same claim `test_the_flag_defaults_still_come_from_those_constants`
    makes about the politeness figures, and it is needed for the same reason: a
    constant nothing defaults to pins nothing, and the page would then be quoting
    a number no sweep uses.

    `DEFAULT_GRACE_DAYS` is checked on the OTHER binary, because that is where
    its only reader's flag is: `dormancy wake --grace-days`. A grace flag on the
    sweeper was removed rather than left to be parsed and ignored.
    """
    main = rust("main.rs")
    for attribute in (
        "default_value_t = DEFAULT_COST_MS",
        "default_value_t = DEFAULT_DORMANT_STRIKES",
        "default_value_t = DEFAULT_DORMANT_CADENCE",
    ):
        assert attribute in main, attribute
    assert "NonZeroU32::new(DEFAULT_STRIKES)" in main
    assert "NonZeroU64::new(DEFAULT_CADENCE_DAYS)" in main

    wake = (PROBER / "src" / "bin" / "dormancy.rs").read_text()
    assert "default_value_t = DEFAULT_GRACE" in wake
    assert "dormancy::DEFAULT_GRACE_DAYS" in wake


@requires_repo_sources
def test_the_dormancy_reasons_the_pages_read_are_the_probers_own():
    """The row reason map in `web/app.py`, against `SkipReason::slug`.

    Nothing else holds these two words together. The prober pins them on its
    own side (`the_two_reason_slugs_are_stable`), and until this test the Python
    side pinned nothing: renaming `operator-hold` in Rust left every row telling
    the truth through the verbatim fallback while the index panel went on
    asserting, in prose, that there are two reasons and naming a value no run
    graph could carry. That is a positive false claim on pages whose whole
    doctrine is that a stale qualifier is a claim, and it passed 304 tests.

    Set equality in both directions, so a slug the prober adds fails here as
    loudly as one it renames: a third reason with no reading would fall through
    to "a reason this page cannot read", which is honest about the row and
    silently wrong in the panel that says there are two.

    The endpoint page used to keep a second map of its own and is no longer in
    this test: since the dormancy sentence was cut back to `Dormant (<slug>).`
    it prints the store's value and glosses nothing, so it has no vocabulary
    left to drift.

    `not-in-this-sweep` is that third reason, and this test is what made the two
    sides land together. Rule 3 of the admission policy published `automatic`
    for endpoints a replayed `--at` merely did not reach, and both pages
    rendered that as a relegation on cost and silence that never happened. The
    Python maps carried sentences for the new slug before `SkipReason` carried
    the variant, and this assertion was red for exactly that window, which is
    the direction it was written for.
    """
    from app import _ROW_DORMANCY_REASONS

    slugs = skip_reason_slugs()
    assert slugs == {"automatic", "operator-hold", "not-in-this-sweep"}, (
        "the prober's reason slugs changed; the prose has to change with them, "
        "which is what the assertion below is about"
    )
    assert set(_ROW_DORMANCY_REASONS) == slugs, (
        "the index row's reason clauses and the prober's slugs differ"
    )


@requires_repo_sources
def test_the_numbers_in_the_prose_are_the_constants_and_not_words(client):
    """Every place the prose states the cadence or the strike count.

    The `<li>` items in the numbers list render `DORMANCY` and self-correct. The
    prose beside them spelled both numbers out in words, so changing
    `DEFAULT_CADENCE_DAYS` to 10 left three sentences promising seven days,
    green, on the one page a server operator reads to decide what to expect, and
    changing `DEFAULT_STRIKES` left two more describing a policy nobody runs.
    This asserts the prose against the same constants the list renders.
    """
    defaults = dormancy_defaults()
    cadence = defaults["dormant-cadence-days"]
    strikes = defaults["dormant-strikes"]
    html = page(client)

    said = " ".join(texts_with(html, "data-dormancy")).lower()
    assert f"at most one sweep in every {cadence} days" in said, said
    assert "only when a person starts one" in said, said
    assert f"in each of {strikes} sweeps in a row" in said, said

    blocking = " ".join(texts_with(html, "data-blocking")).lower()
    assert f"{cadence} days" in blocking, blocking
    assert f"after {strikes} such sweeps" in blocking, blocking


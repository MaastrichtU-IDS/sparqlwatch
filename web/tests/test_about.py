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
from pyoxigraph import NamedNode, RdfFormat, Store, parse
from starlette.testclient import TestClient

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
        # One request per metric measurable at the default cost ceiling,
        # which is `Cost::Cheap`: `main.rs` declares
        # `#[arg(long, value_enum, default_value_t = Cost::Cheap)]`.
        "requests-per-endpoint": (PROBER / "metrics.toml")
        .read_text()
        .count('cost = "cheap"'),
    }


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
    """How many endpoints `registry/lod-cloud.toml` actually lists."""
    text = (PROBER / "registry" / "lod-cloud.toml").read_text()
    return len(re.findall(r'^\s*"', text, re.M))


def registry_endpoints() -> list[str]:
    """Every endpoint `registry/lod-cloud.toml` lists, as written."""
    text = (PROBER / "registry" / "lod-cloud.toml").read_text()
    return re.findall(r'^\s*"([^"]+)",?\s*$', text, re.M)


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
    expected = prober_defaults()

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


def test_the_page_says_that_blocking_us_does_not_stop_the_requests(client):
    """The reader's own first instinct, answered honestly.

    Nothing yet drops an endpoint from the list for failing: an admission
    policy is a later slice, and until it exists a blocked endpoint is probed
    again at the same rate on the next sweep. `prober/README.md` records that
    under "Not yet operable on a daily cadence", and this test holds the page
    to it: when the dead stop being re-probed, this claim stops being true and
    the page has to change.
    """
    # Whitespace-collapsed, because the sentence is wrapped in the README and
    # a line break is not a change of claim.
    readme = " ".join((PROBER / "README.md").read_text().split())
    assert "nothing yet stops the dead being re-probed on every sweep" in readme

    text = " ".join(texts_with(page(client), "data-blocking")).lower()
    assert "block" in text
    assert "next sweep" in text


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


def test_the_page_names_the_file_an_exclusion_lands_in(client):
    """The path both binaries read at every run, as `registry.rs` spells it.

    Named because it is checkable: a reader can see whether their host is on
    the list, and that is the only way this promise is verifiable from
    outside.
    """
    assert exclusions_path() in page(client)


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

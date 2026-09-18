"""The response cache and the validator it puts on every page.

Both rest on one fact, which `app.py` argues where the cache is defined: this
process's store cannot change under it. It is opened with `Store.read_only`,
whose snapshot never sees a later write, and the sweep publishes by restarting
the Deployment rather than by writing into the store a running site holds. No
route reads a clock. So a GET is reproducible for the process's life, and the
second identical one can be answered with the first one's bytes.

That is the claim; these are the ways it could be false.
"""

import pytest
from fastapi.testclient import TestClient
from pyoxigraph import Store

import app as app_module
from app import ENDPOINT_PATH, INDEX_PATH, STYLESHEET_PATH, app, get_store

KADASTER = "https://data.kadaster.nl/sparql"
HTML = {"accept": "text/html"}
TURTLE = {"accept": "text/turtle"}


@pytest.fixture(autouse=True)
def empty_cache():
    """Each test starts with nothing held.

    The cache lives on the middleware instance, which lives on the app, which
    is a module-level object shared by every test in the run. Without this a
    test would be reading whatever the file before it happened to leave.
    """
    for middleware in _snapshot_caches():
        middleware.entries.clear()
    yield
    for middleware in _snapshot_caches():
        middleware.entries.clear()


def _snapshot_caches():
    """Every live SnapshotCache instance in the built middleware stack.

    Reached through the built stack rather than through `app.user_middleware`,
    because that list holds the CLASS and its arguments; the instance holding
    the entries is made when the stack is built.
    """
    found = []
    seen = getattr(app, "middleware_stack", None)
    while seen is not None:
        if isinstance(seen, app_module.SnapshotCache):
            found.append(seen)
        seen = getattr(seen, "app", None)
    return found


@pytest.fixture
def client_for():
    def build(store):
        app.dependency_overrides[get_store] = lambda: store
        return TestClient(app)

    yield build
    app.dependency_overrides.clear()


def test_two_stores_are_never_served_each_others_answers(client_for, store, store_declined):
    """THE bug the first version of this cache shipped, and the reason the
    store handle is in the key.

    Two stores, the same URL, one process. The first version keyed on host,
    path, query and the two negotiation headers, and answered the second store
    with the first store's page: fifty tests failed at once, all of them
    reading a page about a store they had not installed.

    The service would meet this the first time it served two stores. It does
    not serve two today -- `get_store` is `lru_cache`d onto one handle -- so
    nothing in the running site would have caught it, and it would have been a
    cache that silently answers for the wrong database.
    """
    first = client_for(store).get(INDEX_PATH, headers=HTML)
    second = client_for(store_declined).get(INDEX_PATH, headers=HTML)
    assert first.status_code == second.status_code == 200
    assert first.text != second.text, (
        "two different stores returned one page, so the second store was "
        "served the first store's answer"
    )

    # And each is still the answer it was, asked again.
    again = client_for(store).get(INDEX_PATH, headers=HTML)
    assert again.text == first.text


def test_the_same_request_twice_is_the_same_bytes(client_for, store):
    """The cache's whole purpose, stated as the property a reader cares about:
    asking twice cannot change the answer.
    """
    client = client_for(store)
    first = client.get(INDEX_PATH, headers=HTML)
    second = client.get(INDEX_PATH, headers=HTML)
    assert first.status_code == second.status_code == 200
    assert first.content == second.content
    assert first.headers["etag"] == second.headers["etag"]


def test_a_second_request_is_actually_served_from_the_cache(client_for, store):
    """The test above passes on a service with no cache at all, because the
    answer is reproducible either way. This one fails on one.

    The handler is taken away between the two requests. A service that renders
    on every request cannot answer the second; one that kept the first one's
    bytes answers it without asking the handler anything, which is the whole
    claim.
    """
    client = client_for(store)
    first = client.get(INDEX_PATH, headers=HTML)
    assert first.status_code == 200

    index = app_module.endpoint_index
    app_module.endpoint_index = _refuse
    try:
        second = client.get(INDEX_PATH, headers=HTML)
    finally:
        app_module.endpoint_index = index
    assert second.status_code == 200
    assert second.content == first.content


def _refuse(*_args, **_kwargs):
    raise AssertionError("the handler ran, so this response was not cached")


def test_the_two_representations_are_not_served_for_each_other(client_for, store):
    """Accept is in the key because one URL is four documents here.

    A cache that dropped it would serve Turtle to a browser, or a page to a
    script, depending only on which asked first.
    """
    client = client_for(store)
    html = client.get(INDEX_PATH, headers=HTML)
    turtle = client.get(INDEX_PATH, headers=TURTLE)
    assert html.headers["content-type"].startswith("text/html")
    assert turtle.headers["content-type"].startswith("text/turtle")
    assert html.content != turtle.content
    assert html.headers["etag"] != turtle.headers["etag"]


def test_a_query_that_filters_is_not_served_the_unfiltered_page(client_for, store):
    """The query string selects, so it is in the key."""
    client = client_for(store)
    everything = client.get(INDEX_PATH, headers=HTML)
    narrowed = client.get(INDEX_PATH, params={"q": "kadaster"}, headers=HTML)
    assert everything.status_code == narrowed.status_code == 200
    assert everything.content != narrowed.content


def test_a_reader_holding_the_page_is_told_it_is_unchanged(client_for, store):
    """The ETag's point: revalidate cheaply rather than re-send.

    `Cache-Control: no-cache` means the browser asks every time, so it can
    never show a page from a sweep ago; the 304 means asking costs one round
    trip and no body. Before this the service sent no validator at all and
    every revisit re-sent the whole page.
    """
    client = client_for(store)
    first = client.get(INDEX_PATH, headers=HTML)
    etag = first.headers["etag"]
    assert etag.startswith('"') and etag.endswith('"')
    assert first.headers["cache-control"] == "no-cache"

    again = client.get(INDEX_PATH, headers={**HTML, "if-none-match": etag})
    assert again.status_code == 304
    assert again.content == b""
    assert again.headers["etag"] == etag


def test_a_reader_holding_a_stale_page_is_sent_the_new_one(client_for, store):
    """The other half. An If-None-Match that does not match this entity is a
    reader holding something else, and they get the document.
    """
    client = client_for(store)
    first = client.get(INDEX_PATH, headers=HTML)
    again = client.get(
        INDEX_PATH, headers={**HTML, "if-none-match": '"not-this-page"'}
    )
    assert again.status_code == 200
    assert again.content == first.content


def test_a_weak_validator_still_matches(client_for, store):
    """`W/"x"` and `"x"` are the same entity for deciding whether to re-send,
    and a proxy is entitled to weaken a validator in transit.
    """
    client = client_for(store)
    etag = client.get(INDEX_PATH, headers=HTML).headers["etag"]
    again = client.get(INDEX_PATH, headers={**HTML, "if-none-match": f"W/{etag}"})
    assert again.status_code == 304


def test_the_immutable_stylesheet_keeps_its_own_caching(client_for, store):
    """The one route that already said how long it keeps, and must go on
    saying it.

    The hashed stylesheet is `immutable`: its URL changes when its bytes do, so
    a browser holding it never asks again. An earlier version of this
    middleware replaced every Cache-Control with `no-cache` on a cache hit,
    which would have turned a file that is never re-fetched into one
    revalidated on every page load -- and only on the SECOND request, so the
    test that pins the header passed.
    """
    client = client_for(store)
    first = client.get(STYLESHEET_PATH)
    second = client.get(STYLESHEET_PATH)
    immutable = "public, max-age=31536000, immutable"
    assert first.headers["cache-control"] == immutable
    assert second.headers["cache-control"] == immutable, (
        "the second request is the cached one, and it lost the header"
    )


def test_a_refusal_is_not_pinned_by_the_reader_who_caused_it(client_for, store):
    """Only 200s are kept.

    A 404 for an endpoint this store does not hold is cheap to produce and is
    exactly the answer that must not be held: nothing else in this file's
    argument covers it, because a refusal is about the request rather than
    about the store.
    """
    client = client_for(store)
    missing = client.get(
        ENDPOINT_PATH, params={"url": "https://nothing.example/sparql"}, headers=HTML
    )
    assert missing.status_code == 404
    for cache in _snapshot_caches():
        assert not cache.entries, "a non-200 was kept"


def test_the_cache_does_not_grow_without_bound(client_for, store):
    """A free-text `?q=` is reader input, so the number of distinct URLs this
    service answers is not bounded by anything this project controls.
    """
    client = client_for(store)
    caches = _snapshot_caches()
    assert caches, "no SnapshotCache in the stack, so this test proves nothing"
    limit = max(cache.max_entries for cache in caches)
    for n in range(limit + 20):
        client.get(INDEX_PATH, params={"q": f"nothing-matches-{n}"}, headers=HTML)
    for cache in caches:
        assert len(cache.entries) <= cache.max_entries


def test_a_head_or_post_is_left_alone(client_for, store):
    """Only GET. Anything else goes straight through, because this middleware
    reasons about a body it has no business holding for another method.
    """
    client = client_for(store)
    client.head(INDEX_PATH, headers=HTML)
    for cache in _snapshot_caches():
        assert not cache.entries


def test_the_negotiated_pages_declare_that_they_vary_on_accept(client_for, store):
    """What this service owed a shared cache before it had one of its own.

    Most resources here are four documents behind one URL, chosen by Accept,
    and nothing in the response said so. A cache between this service and a
    reader -- the ingress, a company proxy -- was entitled to hand a script's
    Turtle to the next browser asking for the same URL. It matters more now
    that every response carries an ETag, which is exactly the handle such a
    cache keys on.

    Accept-Encoding has to survive the merge: GZipMiddleware put it there and
    replacing it would tell that same cache it may serve gzip to a client that
    cannot read it.
    """
    client = client_for(store)
    for accept in (HTML, TURTLE):
        vary = client.get(INDEX_PATH, headers=accept).headers["vary"]
        fields = {field.strip().lower() for field in vary.split(",")}
        assert "accept" in fields, vary
        assert "accept-encoding" in fields, vary


def test_the_immutable_stylesheet_does_not_fragment_on_accept(client_for, store):
    """The one route that negotiates nothing keeps its Vary alone.

    It is content-addressed and serves one document; splitting its year-long
    cache entry by a header it never reads would cost something for a
    distinction it does not make.
    """
    client = client_for(store)
    for _ in range(2):
        vary = client.get(STYLESHEET_PATH).headers.get("vary", "")
        assert "accept" not in {f.strip().lower() for f in vary.split(",") if f.strip()} - {"accept-encoding"}

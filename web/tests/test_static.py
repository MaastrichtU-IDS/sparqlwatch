"""The stylesheet and the vocab-search script this site serves, and their routes."""

import pytest
from fastapi.testclient import TestClient

import app as app_module
from app import app


@pytest.fixture
def client():
    """No store override: this route reads no measurement.

    The stylesheet must serve before the first sweep -- it is what every other
    page depends on to be legible, so it cannot depend on there being data.
    """
    with TestClient(app) as built:
        yield built


def test_the_stylesheet_is_served_immutable(client):
    r = client.get(app_module.STYLESHEET_PATH)
    assert r.status_code == 200, "the hashed stylesheet must resolve"
    assert r.headers["content-type"].startswith("text/css")
    assert r.headers["Cache-Control"] == "public, max-age=31536000, immutable"


def test_a_wrong_hash_is_not_served(client):
    r = client.get("/static/site.000000000000.css")
    assert r.status_code == 404, (
        "an unversioned or stale URL must 404, not serve current bytes: "
        "the immutable header promises the bytes never change under a URL"
    )


def test_the_hash_covers_the_generated_verdict_rules(client):
    """The verdict CSS is generated, and it is part of what is cached.

    If the hash were taken over the static file alone, changing a verdict's
    border width would ship stale CSS to every browser holding a year-long
    immutable copy.
    """
    body = client.get(app_module.STYLESHEET_PATH).text
    assert ".enc-verified" in body, "generated verdict rules must be in the file"
    assert ".enc-text-verified" in body


def test_the_script_is_served_immutable(client):
    r = client.get(app_module.SCRIPT_PATH)
    assert r.status_code == 200, "the hashed vocab-search script must resolve"
    assert r.headers["content-type"].startswith("application/javascript")
    assert r.headers["Cache-Control"] == "public, max-age=31536000, immutable"


def test_a_wrong_script_hash_is_not_served(client):
    r = client.get("/static/vocab-search.000000000000.js")
    assert r.status_code == 404, (
        "an unversioned or stale URL must 404, not serve current bytes: "
        "the immutable header promises the bytes never change under a URL"
    )


def test_the_script_hash_covers_the_vocab_match_transliteration(client):
    """The served script is the JS half of the vocab_match.py contract.

    Hashing the bytes read from disk at import (rather than trusting a
    filename) is what makes the cache-busting URL honest; this pins that the
    body served under that hash is actually the ranked-matching
    transliteration, not some other script that happened to occupy the path.
    """
    body = client.get(app_module.SCRIPT_PATH).text
    assert "export function tokenize" in body
    assert "vocab_match.py" in body


def test_the_narrow_page_body_track_can_shrink_below_its_content(client):
    """A grid track that cannot shrink hands its overflow to the page.

    `.page-body` is a column and a 258px rail, and the wide rule has always used
    `minmax(0, 1fr)` for the column. The narrow rule below 900px reset it to a
    bare `1fr`, which means `minmax(auto, 1fr)`, and that `auto` floors the
    track at its content's MIN-CONTENT width. A child too wide to fit then
    stretches the grid instead of scrolling inside itself.

    Latent until the metrics table arrived on 2026-09-17 and made it visible:
    measured at 390px, the column went to 507px and took 131px of body overflow
    with it, and the table's own `overflow-x: auto` never engaged because it had
    been handed 507px to fill. The stylesheet's own note two rules further down
    -- "a table added later should scroll rather than push the page" -- is the
    intent this defeated.

    Pinned as the declaration because the suite has no browser. `1fr` on its own
    is the regression, whatever else moves around it.
    """
    css = client.get(app_module.STYLESHEET_PATH).text
    narrow = css[css.index("@media (max-width: 900px)"):]
    narrow = narrow[: narrow.index("\n}")]
    assert "grid-template-columns: minmax(0, 1fr)" in narrow, narrow
    assert "grid-template-columns: 1fr;" not in narrow, (
        "a bare 1fr floors the track at min-content and the page overflows"
    )


def test_every_token_is_declared_for_both_surfaces(client):
    body = client.get(app_module.STYLESHEET_PATH).text
    light = body.split("prefers-color-scheme: dark")[0]
    dark = body.split("prefers-color-scheme: dark")[1]
    for token in ("--bg", "--text", "--accent", "--good", "--warn", "--crit", "--fill"):
        assert f"{token}:" in light, f"{token} must have a light value"
        assert f"{token}:" in dark, f"{token} must have a dark value"


def _srgb_to_linear(c: float) -> float:
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _relative_luminance(rgb: tuple[float, float, float]) -> float:
    r, g, b = (_srgb_to_linear(v / 255) for v in rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def _contrast(a: tuple[float, float, float], b: tuple[float, float, float]) -> float:
    hi, lo = sorted((_relative_luminance(a), _relative_luminance(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def _hex_to_rgb(value: str) -> tuple[float, float, float]:
    value = value.strip().lstrip("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4))


def _composite(fill_rgb, alpha, surface_rgb):
    return tuple(alpha * f + (1 - alpha) * s for f, s in zip(fill_rgb, surface_rgb))


def test_the_verdict_fill_is_visible_on_both_surfaces(client):
    """The fill channel must survive the surface it is drawn on.

    A fill that composites too close to its background turns every filled chip
    into an empty one, and two of the seven states become one. This has already
    happened once: verdict_encoding.py records a 5%-white fill that vanished at
    chip size. That fill measures 1.139:1; the one that replaced it measures
    1.623:1. The floor below sits between them.
    """
    body = client.get(app_module.STYLESHEET_PATH).text
    light, dark = body.split("prefers-color-scheme: dark")

    for name, block, fill_rgb in (
        ("light", light, (0, 0, 0)),
        ("dark", dark, (255, 255, 255)),
    ):
        surface = _hex_to_rgb(block.split("--bg:")[1].split(";")[0])
        alpha = float(block.split("--fill:")[1].split(")")[0].split(",")[-1])
        filled = _composite(fill_rgb, alpha, surface)
        ratio = _contrast(filled, surface)
        assert ratio >= 1.4, (
            f"the {name} fill composites to {ratio:.3f}:1 against its surface; "
            f"below 1.4:1 a filled chip is indistinguishable from an empty one"
        )


def test_the_generated_rules_read_the_fill_token(client):
    """The fill must come from the token, not from a literal in Python.

    A literal cannot follow the surface, which is the whole defect this task
    exists to fix.
    """
    body = client.get(app_module.STYLESHEET_PATH).text
    assert "var(--fill)" in body
    assert "background: rgba(" not in body, (
        "a hard-coded fill in the generated rules cannot follow the surface; "
        "the fill must come from var(--fill). Note this asserts on the "
        "generated declarations, not on the --fill token itself, which is "
        "legitimately an rgba() literal in each of the two blocks."
    )


def test_no_route_serves_a_private_copy_of_the_verdict_encoding(store):
    """Every HTML route this site serves, not just the two that already had a
    page-specific version of this check (test_page.py's endpoint page,
    test_index.py's index).

    A private `.enc-` rule anywhere in a page's own markup loads after the
    hashed stylesheet (`head_extra` renders after the `<link>` in base.html)
    and wins on cascade order, so a fix to the generated table -- such as the
    fill token's rgba() value -- would silently fail to reach that one page.
    explore.html carried exactly this: nine rules copied from
    verdict_encoding.css_rules(), with the pre-fix literal hard-coded, which
    is what this test is written to catch.
    """
    import urllib.parse

    import verdict_encoding
    from app import (
        ABOUT_PATH,
        DOCS_METRICS_PATH,
        DOCS_PATH,
        DOCS_STATES_PATH,
        DOCS_VOID_PATH,
        ENDPOINT_PATH,
        EXPLORE_PATH,
        INDEX_PATH,
        get_store,
    )

    # The store fixture's one endpoint (run-with-samples.nq), which is what
    # every other suite calls KADASTER.
    kadaster = "https://data.kkg.kadaster.nl/query"

    app_module.app.dependency_overrides[get_store] = lambda: store
    try:
        client = TestClient(app_module.app)
        routes = [
            INDEX_PATH,
            ENDPOINT_PATH + "?url=" + urllib.parse.quote(kadaster, safe=""),
            DOCS_PATH,
            DOCS_METRICS_PATH,
            DOCS_STATES_PATH,
            DOCS_VOID_PATH,
            EXPLORE_PATH,
            ABOUT_PATH,
        ]
        assert len(routes) == 8, "this is meant to cover all eight HTML routes"
        for route in routes:
            response = client.get(route, headers={"accept": "text/html"})
            assert response.status_code == 200, f"{route} did not render"
            body = response.text
            for state in (*verdict_encoding.STATES, verdict_encoding.UNRECOGNISED):
                selector = "." + verdict_encoding.css_class(state.slug)
                assert selector + " " not in body, (
                    f"{route} serves a private copy of {selector}; the "
                    f"encoding must come from the hashed stylesheet or "
                    f"nowhere"
                )
    finally:
        app_module.app.dependency_overrides.clear()

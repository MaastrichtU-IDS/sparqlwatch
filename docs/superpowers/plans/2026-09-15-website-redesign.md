# Website Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the eight server-rendered pages one template layer, one cached stylesheet, and a light-default visual system, without changing what any page claims.

**Architecture:** A `base.html` supplies the chrome and three blocks; a single `static/site.css` — hashed over its own bytes *plus* `verdict_encoding.css_rules()` — supplies every token and rule and is served immutable from a route. The nineteen tokens the templates already declare keep their names and are re-valued for a light surface, with a `prefers-color-scheme: dark` block; the verdict fill becomes the twentieth token so the encoding follows the surface. The registry gains a server-side `?q=`, applied to the SELECT rows for HTML and to the constructed graph in Python for RDF, so both representations filter through one predicate.

**Tech Stack:** FastAPI, Jinja2, pyoxigraph, pytest, Playwright (mobile probe). No new runtime dependency and no JavaScript toolchain.

**Spec:** `docs/superpowers/specs/2026-09-15-website-redesign-design.md` (revision 2) — read it first; this plan argues from it.

## Global Constraints

- **Presentation only.** No query in `web/queries/` is edited. No endpoint is contacted differently. No page's claims change.
- **`queries/__init__.py:5-9` is binding:** a `.rq` file must stay runnable as pasted. Nothing templates or rewrites query text. Parameters reach a query through pyoxigraph variable substitution or not at all.
- **Token names are the existing nineteen** — `--bg --bg-secondary --bg-hover --bg-deep --border --text --text-bright --text-muted --text-dim --accent --on-accent --accent-blue --good --warn --crit --overlay --radius --radius-sm --mono` — plus one new `--fill`. Do not rename them; `verdict_encoding.py` emits `var(--text-muted)` and `var(--text-dim)` by those names.
- **Light values:** `--bg: #fcfcfb`, `--bg-secondary: #ffffff`, `--border: #e3e2de`, `--text: #1b1b1a`, `--text-muted: #5d5d59`, `--text-dim: #8a8a85`, `--accent: #0b5cad`, `--good: #0ca30c`, `--warn: #fab219`, `--crit: #d03b3b`, `--fill: rgba(0,0,0,0.20)`.
- **Dark values:** `--bg: #17171a`, `--bg-secondary: #1f1f23`, `--border: #33333a`, `--text: #e8e8e4`, `--text-muted: #a8a8a2`, `--text-dim: #7a7a75`, `--accent: #4fc3f7`, `--good: #22a532`, `--warn: #ffc94d`, `--crit: #ef5350`, `--fill: rgba(255,255,255,0.16)`.
- **The fill is load-bearing and was measured.** 20% black on `#fcfcfb` is 1.598:1; 16% white on `#17171a` is 1.627:1. A lighter fill was measured at 1.173:1 and is indistinguishable from empty. Never adjust either value by eye.
- **The amber's sub-3:1 contrast is discharged by text labels.** Every verdict mark ships a text label beside it. No view may show a mark without one.
- **`color-scheme` is `light dark`**, never `dark`.
- Run `pytest` from `web/`. Commit after every task.

---

## File Structure

| File | Responsibility |
|---|---|
| `web/static/site.css` | **Create.** Every token (light + dark) and every shared rule. The only stylesheet. |
| `web/static/vocab-search.js` | **Create.** Ranked vocabulary matching in the browser; a transliteration of `vocab_match.py`. |
| `web/vocab_match.py` | **Create.** The scoring rules, in Python, so they are testable under pytest. |
| `web/templates/base.html` | **Create.** `<head>`, header, footer, and the blocks `title`, `content`, `head_extra`. |
| `web/app.py` | Stylesheet route + `stylesheet_path` global; `?q=` on the index; nav list; drop `encoding_css` from two contexts. |
| `web/verdict_encoding.py` | `_declarations` emits `var(--fill)` instead of a white literal. |
| `web/explore_payload.py` | `endpoint_vocabulary` gains a `tokens` field. |
| `web/templates/*.html` (8) | Become `{% extends %}` + `{% block content %}`; six also use `head_extra`. |
| `web/tests/test_static.py` | **Create.** The stylesheet route and the fill's measured visibility. |
| `web/tests/test_vocab_match.py` | **Create.** The scoring table, the bands, the ordering, and JS/Python agreement. |
| `web/tests/fixtures/vocab_match_cases.json` | **Create.** The shared case table both implementations are checked against. |

---

### Task 1: The stylesheet route

**Files:**
- Create: `web/static/site.css`
- Create: `web/tests/test_static.py`
- Modify: `web/app.py` (beside the icon routes, ~`app.py:3311-3337`)

**Interfaces:**
- Produces: `STYLESHEET_PATH: str` (module constant, e.g. `/static/site.a1b2c3d4e5f6.css`), and the Jinja global `stylesheet_path` carrying the same string. Later tasks reference it in `base.html` as `{{ stylesheet_path }}`.

No template is touched in this task. The route exists and is tested before anything depends on it.

- [ ] **Step 1: Write the failing test**

Create `web/tests/test_static.py`:

```python
"""The one stylesheet this site serves, and the route that serves it."""

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


def test_every_token_is_declared_for_both_surfaces(client):
    body = client.get(app_module.STYLESHEET_PATH).text
    light = body.split("prefers-color-scheme: dark")[0]
    dark = body.split("prefers-color-scheme: dark")[1]
    for token in ("--bg", "--text", "--accent", "--good", "--warn", "--crit", "--fill"):
        assert f"{token}:" in light, f"{token} must have a light value"
        assert f"{token}:" in dark, f"{token} must have a dark value"
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd web && pytest tests/test_static.py -v`
Expected: FAIL — `AttributeError: module 'app' has no attribute 'STYLESHEET_PATH'`.

- [ ] **Step 3: Create the stylesheet with the token blocks**

Create `web/static/site.css`. Start with tokens only; later tasks append the shared rules as they delete them from templates.

```css
/* Every token this site uses, once. Until 2026-09-15 these nineteen were
   copy-pasted into all eight templates, which is why the palette could not be
   retuned in one edit. See docs/superpowers/specs/2026-09-15-website-redesign-design.md.

   Light is the default. Dark is selected -- its own steps, validated against
   its own surface, not an automatic inversion. */
:root {
  color-scheme: light dark;
  --bg: #fcfcfb; --bg-secondary: #ffffff; --bg-hover: #f4f3f0; --bg-deep: #f0efec;
  --border: #e3e2de; --text: #1b1b1a; --text-bright: #000000; --text-muted: #5d5d59;
  --text-dim: #8a8a85; --accent: #0b5cad; --on-accent: #ffffff; --accent-blue: #1a6fc4;
  --good: #0ca30c; --warn: #fab219; --crit: #d03b3b; --overlay: rgba(0,0,0,0.04);
  /* The verdict fill. 20% black measures 1.598:1 against --bg, matching what
     16% white achieves on the dark surface (1.627:1). A lighter value was
     measured at 1.173:1, against 1.139:1 for a fill already proven invisible
     at chip size. Do not adjust this by eye. */
  --fill: rgba(0, 0, 0, 0.20);
  --radius: 6px; --radius-sm: 4px;
  --mono: ui-monospace, 'Cascadia Code', 'Source Code Pro', monospace;
}

@media (prefers-color-scheme: dark) {
  :root {
    --bg: #17171a; --bg-secondary: #1f1f23; --bg-hover: #26262b; --bg-deep: #101013;
    --border: #33333a; --text: #e8e8e4; --text-bright: #ffffff; --text-muted: #a8a8a2;
    --text-dim: #7a7a75; --accent: #4fc3f7; --on-accent: #0a1929; --accent-blue: #81d4fa;
    --good: #22a532; --warn: #ffc94d; --crit: #ef5350; --overlay: rgba(255,255,255,0.05);
    --fill: rgba(255, 255, 255, 0.16);
  }
}
```

- [ ] **Step 4: Add the route**

In `web/app.py`, directly after the icon routes (`app.py:3337`, following `icon_mask`), add:

```python
# The one stylesheet. Assembled at import from the static file plus the verdict
# rules verdict_encoding generates, so a change to either moves the URL.
#
# Served from a route rather than a StaticFiles mount for the reason the icons
# are (app.py:3311): a mount would publish the directory they sit in, and the
# directory they sit in is the source tree.
_STYLESHEET = (
    (Path(__file__).resolve().parent / "static" / "site.css").read_text()
    + "\n\n/* Generated from web/verdict_encoding.py. */\n"
    + verdict_encoding.css_rules()
    + "\n"
).encode()
# Content-addressed, because the header below promises a year. The bytes never
# change under this URL; a new build gets a new URL and browsers refetch nothing.
_STYLESHEET_HASH = hashlib.sha256(_STYLESHEET).hexdigest()[:12]
STYLESHEET_PATH = f"/static/site.{_STYLESHEET_HASH}.css"
_STYLESHEET_CACHE = "public, max-age=31536000, immutable"


@app.get("/static/site.{digest}.css")
def stylesheet(digest: str) -> Response:
    """The site's stylesheet, at a URL that changes when its bytes do.

    Any other digest 404s rather than redirecting to the current one. A stale
    URL that answered with current bytes would be a lie about immutability, and
    the one thing a year-long cache header may not do is lie.
    """
    if digest != _STYLESHEET_HASH:
        return Response(status_code=404)
    return Response(
        content=_STYLESHEET,
        media_type="text/css",
        headers={"Cache-Control": _STYLESHEET_CACHE},
    )


_TEMPLATES.globals["stylesheet_path"] = STYLESHEET_PATH
```

Add `import hashlib` to the import block at the top of `app.py` (alphabetically, after `import html`).

- [ ] **Step 5: Run the tests**

Run: `cd web && pytest tests/test_static.py -v`
Expected: 4 passed.

- [ ] **Step 6: Run the whole suite**

Run: `cd web && pytest -q`
Expected: no new failures. Nothing consumes the route yet, so nothing else can break.

- [ ] **Step 7: Commit**

```bash
git add web/static/site.css web/app.py web/tests/test_static.py
git commit -m "Serve one stylesheet, at a URL that changes when its bytes do"
```

---

### Task 2: The verdict fill follows the surface

**Files:**
- Modify: `web/verdict_encoding.py:48` (`CHIP_FILL`) and `:207-220` (`_declarations`)
- Modify: `web/tests/test_static.py` (add the measurement test)

**Interfaces:**
- Consumes: `--fill` declared in both blocks of `static/site.css` (Task 1).
- Produces: `.enc-<slug>` rules whose `background` is `var(--fill)` or `transparent`. No signature changes.

**Why this task exists:** `verdict_encoding.py:48` hard-codes `rgba(255, 255, 255, 0.16)`. On a light surface that is invisible, so every filled chip would read as an empty one and the fill channel of the encoding — one of its three colour-independent channels — would silently carry nothing. Its own comment records this happening once before.

- [ ] **Step 1: Write the failing test**

Append to `web/tests/test_static.py`:

```python
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
    assert "rgba(255, 255, 255, 0.16)" not in body, (
        "a hard-coded white fill in the generated rules would be invisible "
        "on the light surface"
    )
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd web && pytest tests/test_static.py -k fill -v`
Expected: FAIL on `test_the_generated_rules_read_the_fill_token` — the generated CSS still contains the white literal.

- [ ] **Step 3: Make the fill a token**

In `web/verdict_encoding.py`, replace the `CHIP_FILL` constant (line 48) and its comment:

```python
# The fill channel, as a token rather than a literal.
#
# It was "rgba(255, 255, 255, 0.16)" until 2026-09-15 -- correct for the dark
# surface it was measured on, and invisible on any light one. The first attempt
# before that reused --overlay, 5% white, which is tuned for panels and vanishes
# at chip size: desaturating the page showed filled and empty chips reading
# identically, so the one channel separating "works" from "we never found out"
# carried nothing at all. Nothing about that was visible from the code.
#
# The values now live in web/static/site.css, one per surface, and are pinned by
# test_the_verdict_fill_is_visible_on_both_surfaces.
CHIP_FILL = "var(--fill)"
```

`_declarations` (line 207) needs no edit — it already interpolates `CHIP_FILL`. Confirm by reading it.

- [ ] **Step 4: Run the tests**

Run: `cd web && pytest tests/test_static.py -v`
Expected: 6 passed.

- [ ] **Step 5: Run the encoding's own tests**

Run: `cd web && pytest tests/test_page.py -q`
Expected: pass. `tests/test_page.py:392` and `:520` assert the seven triples are distinct; the triple is `(border, fill, weight)` where `fill` is the boolean, not the colour, so this change cannot affect them. If either fails, stop — it means the triples were reading the colour, which would be a defect worth reporting before proceeding.

- [ ] **Step 6: Commit**

```bash
git add web/verdict_encoding.py web/tests/test_static.py
git commit -m "Let the verdict fill follow the surface it is drawn on"
```

---

### Task 3: `base.html`, proved on the four docs pages

**Files:**
- Create: `web/templates/base.html`
- Modify: `web/templates/docs.html`, `docs-metrics.html`, `docs-states.html`, `docs-void.html`
- Modify: `web/app.py` (`_docs_context`, ~`app.py:2687`) — add the header nav list
- Modify: `web/static/site.css` — receives the shared rules these four pages give up
- Modify: `web/tests/test_docs.py`, `web/tests/test_explore.py:88`, `web/tests/test_index.py:1266`

**Interfaces:**
- Consumes: `{{ stylesheet_path }}` (Task 1).
- Produces: `base.html` with blocks `title`, `content`, `head_extra`; and `_nav_context() -> dict` returning `{"nav": [{"path": str, "label": str, "slug": str}, ...]}`, merged into every page context. Later tasks extend `base.html` and set those blocks.

The docs pages go first because they are the smallest (8,469–9,444 bytes) and the most purely duplicated, so a mistake in the shell shows up cheaply.

- [ ] **Step 1: Write the failing test**

Append to `web/tests/test_docs.py`:

```python
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
        assert nav == ["/", "/explore", "/docs", "/about"], f"{path} nav is {nav}"
```

Add `import app as app_module` to that file's imports.

- [ ] **Step 2: Run it to verify it fails**

Run: `cd web && pytest tests/test_docs.py -k "stylesheet_of_their_own or header_nav" -v`
Expected: FAIL — the pages still declare `--accent:` inline and their nav has two items.

- [ ] **Step 3: Write `base.html`**

Create `web/templates/base.html`:

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" href="/icon.svg" type="image/svg+xml">
<link rel="mask-icon" href="/icon-mono.svg" color="#0b5cad">
<title>{% block title %}sparqlwatch{% endblock %}</title>
<link rel="stylesheet" href="{{ stylesheet_path }}">
{% block head_extra %}{% endblock %}
</head>
<body>
<header>
  <a class="logo" href="{{ index_path }}">
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="var(--accent)"
         stroke-width="2" stroke-linecap="round" aria-hidden="true">
      <circle cx="11" cy="11" r="6"/><path d="M15.5 15.5 21 21"/><path d="M8 11h6"/></svg>
    sparqlwatch
  </a>
  <nav>
    {% for item in nav %}
    <a href="{{ item.path }}" data-nav="{{ item.path }}"
       {% if item.slug == here %}aria-current="page"{% endif %}>{{ item.label }}</a>
    {% endfor %}
  </nav>
</header>
<main>
{% block content %}{% endblock %}
</main>
<footer>
  <a href="{{ docs_path }}">how we measure</a>
  <a href="{{ void_path }}">VoID</a>
  <span class="version">{{ version }}</span>
</footer>
{% block body_end %}{% endblock %}
</body>
</html>
```

Note the `mask-icon` colour moves from `#81d4fa` (a dark-surface cyan, hard-coded) to the light accent.

- [ ] **Step 4: Add the nav table to `app.py`**

In `web/app.py`, immediately before `_docs_context` (~line 2687):

```python
def _nav_context() -> dict:
    """The header's links, from one list.

    The header carried two links and a conditional until 2026-09-15. It is a
    list here for the reason _docs_context is a table: a page added to the site
    should not mean editing the chrome of every page that already exists.
    """
    return {
        "nav": [
            {"path": INDEX_PATH, "label": "Registry", "slug": "index"},
            {"path": EXPLORE_PATH, "label": "Explore", "slug": "explore"},
            {"path": DOCS_PATH, "label": "Docs", "slug": "docs"},
            {"path": ABOUT_PATH, "label": "About", "slug": "about"},
        ]
    }
```

Merge it into `_docs_context`'s return by adding `**_nav_context(),` as its first entry, and into `_page_context` and `_index_context` the same way. Every page renders `base.html`, so every page context needs `nav`.

- [ ] **Step 5: Convert the four docs pages**

For each of `docs.html`, `docs-metrics.html`, `docs-states.html`, `docs-void.html`:

1. Delete everything from `<!doctype html>` through `</head>`, and the `<header>` and `<footer>` elements and `</body></html>`.
2. Replace with:

```html
{% extends "base.html" %}
{% block title %}Documentation{% endblock %}
{% block head_extra %}
<style>
  /* The docs section's own rules. Everything shared lives in site.css. */
  .doclist { list-style: none; margin: 0; padding: 0; display: grid; gap: 14px; }
  .doclist a { font-size: 15px; font-weight: 600; }
  .doclist p { margin: 4px 0 0; color: var(--text-muted); font-size: 13.5px; }
  .entry { border-top: 1px solid var(--border); padding: 16px 0 0; margin: 18px 0 0; }
  .entry:first-of-type { border-top: none; padding-top: 0; margin-top: 0; }
  .entry h3 { margin: 0 0 2px; font-size: 14.5px; font-weight: 600;
              color: var(--text-bright); font-family: var(--mono); }
  .entry .said { margin: 0 0 8px; font-size: 13px; color: var(--text-muted); }
  .entry p { margin: 0 0 8px; }
  .facts { display: flex; flex-wrap: wrap; gap: 6px 16px; margin: 0 0 8px;
           font-size: 12px; color: var(--text-dim); font-family: var(--mono); }
  .swatch-lg { display: inline-block; width: 26px; height: 18px;
               border-radius: 3px; vertical-align: -4px; margin-right: 8px; }
</style>
{% endblock %}
{% block content %}
<!-- the existing <main> contents, unchanged -->
{% endblock %}
```

The `{% block title %}` text differs per page — use each page's existing `<title>`. The `head_extra` block above is `docs.html`'s second style tag; each of the other three has its own, which moves the same way. **The page body inside `{% block content %}` is copied verbatim.** No copy changes.

3. Move the **first** `<style>` block's contents — the shared rules, not the tokens — into `web/static/site.css`, appending under a comment naming where they came from. Deduplicate as you go: the four docs pages share nearly all of it. Carry across, without fail: `.visually-hidden`, every `@media (max-width: 640px)` rule, and every `overflow-x: auto` container. Dropping one of those is how this change regresses silently.

- [ ] **Step 6: Fix the two nav tests that pin the old header**

`tests/test_explore.py:88` asserts `nav == [EXPLORE_PATH, "/docs"]` and `tests/test_index.py:1266` pins one `/docs` and one `/explore` href. Both are correct today and wrong after this task. Update them to the four-item list. **Do not loosen them to a substring check** — their intent is that the index carries one nav link and never one per row, and that intent is worth keeping exactly as strict as it is.

```python
    nav = [a["href"] for a in with_attribute(body, "data-nav")]
    assert nav == ["/", "/explore", "/docs", "/about"], f"nav is {nav}"
```

- [ ] **Step 7: Run the docs tests**

Run: `cd web && pytest tests/test_docs.py -v`
Expected: all pass.

- [ ] **Step 8: Run the whole suite**

Run: `cd web && pytest -q`
Expected: pass, including the two updated nav tests.

- [ ] **Step 9: Look at the pages**

Run: `cd web && uvicorn app:app --port 8099` and open `http://localhost:8099/docs`, `/docs/metrics`, `/docs/states`, `/docs/void`.
Check: light surface, readable text, the swatches on `/docs/states` visibly filled where they should be, no unstyled content. The tests cannot see any of that.

- [ ] **Step 10: Commit**

```bash
git add web/templates/base.html web/templates/docs*.html web/app.py \
        web/static/site.css web/tests/test_docs.py web/tests/test_explore.py \
        web/tests/test_index.py
git commit -m "Give the docs pages a shared shell and one stylesheet"
```

---

### Task 4: `about.html` and `explore.html`

**Files:**
- Modify: `web/templates/about.html` (30,355 B, 96 style lines), `web/templates/explore.html` (30,991 B, 194 style lines)
- Modify: `web/static/site.css`
- Modify: `web/tests/test_about.py`, `web/tests/test_explore.py`

**Interfaces:**
- Consumes: `base.html` and its three blocks (Task 3).
- Produces: nothing new. Two more pages on the shell.

`explore.html` keeps its vocabulary grid in `head_extra`; `about.html` should need none — check, and if it needs none, give it none.

- [ ] **Step 1: Write the failing test**

Add to both `tests/test_about.py` and `tests/test_explore.py`:

```python
def test_the_page_carries_no_stylesheet_of_its_own(client):
    body = client.get(ABOUT_PATH, headers={"accept": "text/html"}).text
    assert "--accent:" not in body, (
        "design tokens belong in web/static/site.css, not in this template"
    )
    assert app_module.STYLESHEET_PATH in body
```

Use `EXPLORE_PATH` in the explore copy. Add `import app as app_module` to both files.

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && pytest tests/test_about.py tests/test_explore.py -k stylesheet_of_its_own -v`
Expected: 2 FAIL.

- [ ] **Step 3: Convert both pages**

Same shape as Task 3 Step 5: `{% extends "base.html" %}`, `{% block title %}`, page-specific rules into `{% block head_extra %}`, body verbatim into `{% block content %}`, shared rules merged into `site.css`.

`explore.html:72` sets `outline: none` on its search input. Do not carry that across. In `site.css`, write instead:

```css
input[type="search"]:focus-visible,
a:focus-visible,
button:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}
```

- [ ] **Step 4: Run the tests**

Run: `cd web && pytest tests/test_about.py tests/test_explore.py -v`
Expected: pass.

- [ ] **Step 5: Look at both pages**

`/about` and `/explore` at `http://localhost:8099`. The explore page's vocabulary grid is the thing most likely to have lost a rule in the move — check its columns align and it scrolls rather than overflowing.

- [ ] **Step 6: Commit**

```bash
git add web/templates/about.html web/templates/explore.html web/static/site.css \
        web/tests/test_about.py web/tests/test_explore.py
git commit -m "Put about and explore on the shared shell"
```

---

### Task 5: `index.html` and the summary strip

**Files:**
- Modify: `web/templates/index.html` (56,314 B, 382 style lines — the largest)
- Modify: `web/app.py` (`_index_context`) — drop `encoding_css`, add the strip's figures
- Modify: `web/static/site.css`
- Modify: `web/tests/test_index.py`

**Interfaces:**
- Consumes: `base.html` (Task 3), `fleet_stats` (already imported at `app.py:74`).
- Produces: `summary` in the index context — `{"total": int, "answering": int, "not_answering": int, "last_sweep": str | None}`. Task 6 adds a `matching` key to it.

The strip is the one genuinely new element in this redesign. Everything else on this page is a restyle.

- [ ] **Step 1: Write the failing test**

Append to `web/tests/test_index.py`:

```python
def test_the_index_leads_with_the_fleet_in_four_figures(client_for, store):
    body = client_for(store).get("/", headers={"accept": "text/html"}).text
    figures = texts_with(body, "data-figure")
    assert len(figures) == 4, (
        f"the strip states endpoints, answering, not answering and freshness; "
        f"got {figures}"
    )


def test_the_index_declares_no_tokens_and_no_inline_verdict_css(client_for, store):
    body = client_for(store).get("/", headers={"accept": "text/html"}).text
    assert "--accent:" not in body
    assert ".enc-verified" not in body, (
        "the verdict rules are generated into the cached stylesheet now; "
        "inline they would be re-sent on every page view"
    )
    assert app_module.STYLESHEET_PATH in body
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && pytest tests/test_index.py -k "four_figures or declares_no_tokens" -v`
Expected: 2 FAIL.

- [ ] **Step 3: Drop `encoding_css` from the contexts**

In `web/app.py`, delete the `"encoding_css": verdict_encoding.css_rules(),` lines at `app.py:1278` and `app.py:2357`, and the `{{ encoding_css | safe }}` at `templates/endpoint.html:193` and `templates/index.html:309`. Those rules are in the hashed stylesheet as of Task 1.

- [ ] **Step 4: Add the summary to the index context**

In `_index_context`, beside the existing entries:

```python
        # The fleet in four figures, above the search. New in the 2026-09-15
        # redesign: the page led with rows, which answers "what is here" only
        # after the reader has counted. Every figure is derived from `entries`,
        # so a filtered page reports the filtered set (see ?q= below).
        "summary": {
            "total": len(entries),
            "answering": sum(
                1 for e in entries
                if not e.newest_sweep_declined_to_ask_this_endpoint
            ),
            "not_answering": sum(
                1 for e in entries
                if e.newest_sweep_declined_to_ask_this_endpoint
            ),
            "last_sweep": stats.last_sweep,
        },
```

**Types, so you do not have to go looking.** `_index_context(entries, store)`
is declared at `app.py:2214` and `entries` is a `list[EndpointMeasurements]` —
dataclass **objects, not dicts**. Its fields are `endpoint: str`,
`assessed: bool`, `run`, `generated_at`, `verdicts`, plus the dormancy
properties `_index_rows` already reads at `app.py:2052`
(`newest_sweep_declined_to_ask_this_endpoint`, `newest_dormancy_reason`).

`stats` is already in this context (`app.py:2241`) and is a `FleetStats`
(`fleet.py:74`) with fields `endpoints`, `sweeps`, `first_sweep`, `last_sweep`,
`changed`, `triples`, `triples_from`. Use `stats.last_sweep` rather than
recomputing freshness. Note `FleetStats` carries **no** answering count — that
is why the two above are derived from `entries`.

- [ ] **Step 5: Convert the template and add the strip**

`{% extends "base.html" %}` as in Task 3. In `{% block content %}`, above the existing rows:

```html
<div class="strip">
  <div class="stat"><span class="n" data-figure="total">{{ summary.total }}</span>
    <span class="k">endpoints</span></div>
  <div class="stat"><span class="n enc-text-verified" data-figure="answering">{{ summary.answering }}</span>
    <span class="k">answering</span></div>
  <div class="stat"><span class="n enc-text-declared-but-wrong" data-figure="not-answering">{{ summary.not_answering }}</span>
    <span class="k">not answering</span></div>
  <div class="stat"><span class="n" data-figure="freshness">{{ summary.last_sweep }}</span>
    <span class="k">last sweep</span></div>
</div>
```

The two coloured figures use the generated `enc-text-*` classes rather than a raw `var(--good)`, so they follow the encoding rather than duplicating it.

- [ ] **Step 6: Run the tests**

Run: `cd web && pytest tests/test_index.py -v`
Expected: pass.

- [ ] **Step 7: Look at the index**

`http://localhost:8099/`. This page carries the most rules of any; check the row grid, the chips, and the history matrix all survived the move.

- [ ] **Step 8: Commit**

```bash
git add web/templates/index.html web/app.py web/static/site.css web/tests/test_index.py
git commit -m "Lead the registry with the fleet in four figures"
```

---

### Task 6: `?q=` on the HTML representation

**Files:**
- Modify: `web/app.py` — `index_resource` (`app.py:2379-2383`), `_index_context`
- Modify: `web/templates/index.html` — the search form
- Modify: `web/tests/test_index.py`

**Interfaces:**
- Consumes: `summary` from Task 5.
- Produces: `_matches_query(endpoint: str, needle: str | None) -> bool` — the single predicate. Task 7 applies the same function to RDF subjects. Also `summary["matching"]: int | None` — `None` when unfiltered.

`queries/index.rq` returns `?endpoint` and per-metric verdicts and carries no name or vocabulary column, so the endpoint URL is the only identifying text there is to match. That is enough: it contains the host and, in practice, the name.

- [ ] **Step 1: Write the failing test**

Append to `web/tests/test_index.py`:

```python
def test_a_query_narrows_the_rows_without_javascript(client_for, store_registry_sample):
    client = client_for(store_registry_sample)
    everything = rows_of(client.get("/", headers={"accept": "text/html"}).text)
    narrowed = rows_of(client.get("/?q=uniprot", headers={"accept": "text/html"}).text)
    assert 0 < len(narrowed) < len(everything), (
        "?q= must be a server-side filter; a fixture yielding all or none "
        "proves nothing"
    )
    assert all("uniprot" in r.lower() for r in narrowed)


def test_a_filtered_page_states_its_denominator(client_for, store_registry_sample):
    """"18 endpoints" on a filtered page would misreport the fleet.

    The reader is looking at a subset. Every figure describes the subset, and
    every figure says what it is a subset of.
    """
    body = client_for(store_registry_sample).get(
        "/?q=uniprot", headers={"accept": "text/html"}
    ).text
    total = texts_with(body, "data-figure")[0]
    assert " of " in total, f"the count reads {total!r}, with no denominator"


def test_the_query_survives_in_the_input(client_for, store_registry_sample):
    """Otherwise the client-side enhancement re-filters with an empty needle
    and instantly widens the list the server just narrowed."""
    body = client_for(store_registry_sample).get(
        "/?q=uniprot", headers={"accept": "text/html"}
    ).text
    assert 'value="uniprot"' in body
```

Add a `rows_of(body)` helper to the file if one does not exist, returning each row's endpoint text via the existing `texts_with` / `with_attribute` helpers from `test_page.py`.

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && pytest tests/test_index.py -k "narrows_the_rows or denominator or survives_in_the_input" -v`
Expected: 3 FAIL — `index_resource` takes no `q`.

- [ ] **Step 3: Add the predicate**

In `web/app.py`, above `index_resource`:

```python
def _matches_query(endpoint: str, needle: str | None) -> bool:
    """Whether one endpoint answers to a search.

    Case-insensitive substring over the endpoint URL, which is the only
    identifying text queries/index.rq returns -- it carries ?endpoint and
    per-metric verdicts, and no name or vocabulary column.

    This is ONE function on purpose. Both representations of the index filter
    through it, so the page and the data agree by construction rather than by a
    test noticing later that they drifted.
    """
    if not needle:
        return True
    return needle.strip().lower() in endpoint.lower()
```

- [ ] **Step 4: Take the parameter and apply it**

Change the signature at `app.py:2380`:

```python
@app.get(INDEX_PATH)
def index_resource(
    request: Request,
    q: str | None = Query(
        None,
        description="Narrow the index to endpoints whose URL contains this text.",
    ),
    store: Store = Depends(get_store),
) -> Response:
```

In `_index_context`, filter `entries` through `_matches_query` before anything derives from it, and record both counts:

```python
    unfiltered_total = len(entries)
    entries = [e for e in entries if _matches_query(e.endpoint, q)]
```

and in the `summary` dict from Task 5, add:

```python
            # None when unfiltered, so the template can tell "18 of 212" from
            # "212" without comparing two numbers and guessing.
            "matching": None if not q else len(entries),
            "total": unfiltered_total,
```

with `"answering"` and `"not_answering"` still derived from the filtered `entries`. Pass `q` through to the template as `query`.

- [ ] **Step 5: Render the denominator and the input**

In `templates/index.html`:

```html
<form class="searchrow" method="get" action="{{ index_path }}">
  <label class="visually-hidden" for="q">Search the registry</label>
  <input id="q" name="q" type="search" autocomplete="off" value="{{ query or '' }}"
         placeholder="Search {{ summary.total }} endpoints — name or host…">
  <noscript><button type="submit">Search</button></noscript>
</form>
```

and the first figure:

```html
<span class="n" data-figure="total">
  {%- if summary.matching is not none -%}
    {{ summary.matching }} of {{ summary.total }}
  {%- else -%}
    {{ summary.total }}
  {%- endif -%}
</span>
```

- [ ] **Step 6: Run the tests**

Run: `cd web && pytest tests/test_index.py -v`
Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add web/app.py web/templates/index.html web/tests/test_index.py
git commit -m "Let the registry be narrowed by a query that lives in the URL"
```

---

### Task 7: `?q=` on the RDF representation

**Files:**
- Modify: `web/app.py` — the CONSTRUCT path at `app.py:2375-2376`
- Modify: `web/tests/test_negotiation.py`

**Interfaces:**
- Consumes: `_matches_query` (Task 6).
- Produces: nothing new.

**Read this before starting.** `queries/index_description.rq` is an unparameterised CONSTRUCT, and `queries/__init__.py:5-9` forbids templating query text — a `.rq` file must stay runnable as pasted, and parameters reach a query only through pyoxigraph variable substitution, which cannot express a substring test. **Do not edit the query.** The filter runs after it, on the triples it returns.

- [ ] **Step 1: Write the failing test**

Append to `web/tests/test_negotiation.py`:

```python
def test_a_filtered_index_describes_exactly_the_endpoints_it_lists(
    client_for, store_registry_sample
):
    """The two representations of a filtered index must name the same set.

    This is the same guarantee as
    test_neither_description_query_describes_an_endpoint_the_html_omits, now
    under a query parameter. A filter applied to one representation and not the
    other is the most ordinary way for them to drift.
    """
    client = client_for(store_registry_sample)
    listed = set(rows_of(client.get("/?q=uniprot", headers={"accept": "text/html"}).text))
    graph = parse_graph(
        client.get("/?q=uniprot", headers={"accept": "text/turtle"}).text
    )
    described = {
        str(s.value) for s in {t.subject for t in graph} if str(s.value).startswith("http")
    }
    assert listed, "the fixture must yield a non-empty subset"
    assert described & listed == listed, (
        "the RDF omits an endpoint the page lists"
    )
    assert not (described - listed - _service_level_subjects(graph)), (
        "the RDF describes an endpoint the page filtered out"
    )
```

Use the file's existing graph-parsing helper rather than a new one — read the top of `test_negotiation.py` and match it. `_service_level_subjects` returns the subjects that are not per-endpoint (the service itself, and the dormancy arm); write it in the test file.

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && pytest tests/test_negotiation.py -k filtered_index -v`
Expected: FAIL — the RDF ignores `q` and describes everything.

- [ ] **Step 3: Filter the constructed graph**

At `app.py:2375`, replace:

```python
    triples = store.query(_INDEX_DESCRIPTION_QUERY)
    return serialize(triples, format=RdfFormat.from_media_type(media_type))
```

with:

```python
    # The query runs unchanged and the filter is applied to what it returns.
    #
    # queries/__init__.py:5-9 is binding: a .rq file stays runnable as pasted,
    # and parameters reach a query through pyoxigraph variable substitution or
    # not at all. A substring test is not expressible that way, so filtering
    # here is the only route that does not bend that rule -- and it means the
    # page and the data share _matches_query rather than two spellings of it.
    triples = store.query(_INDEX_DESCRIPTION_QUERY)
    if q:
        # Statements whose subject is not an endpoint -- the service itself and
        # the dormancy arm, which is not per-endpoint -- are service-level and
        # are retained. The filter removes per-endpoint descriptions only.
        triples = [
            t
            for t in triples
            if not _describes_an_endpoint(t.subject)
            or _matches_query(str(t.subject.value), q)
        ]
    return serialize(triples, format=RdfFormat.from_media_type(media_type))
```

and add beside `_matches_query`:

```python
def _describes_an_endpoint(subject) -> bool:
    """Whether a constructed triple's subject is one of the endpoints listed.

    An endpoint is a NamedNode whose IRI is the endpoint URL itself. Blank
    nodes and the service's own IRI are not, which is what keeps the filter
    from stripping statements that describe the service rather than a member
    of the fleet.
    """
    return isinstance(subject, NamedNode) and subject != _SERVICE
```

Both names already exist in this module: `NamedNode` is imported at
`app.py:60`, and `_SERVICE = NamedNode("urn:sparqlwatch:service")` is declared
at `app.py:3027`. Nothing new to import.

- [ ] **Step 4: Run the tests**

Run: `cd web && pytest tests/test_negotiation.py -v`
Expected: pass, including the pre-existing lockstep test at `:1146`.

- [ ] **Step 5: Run the whole suite**

Run: `cd web && pytest -q`

- [ ] **Step 6: Commit**

```bash
git add web/app.py web/tests/test_negotiation.py
git commit -m "Filter both representations of the index through one predicate"
```

---

### Task 8: The endpoint page — column and rail

**Files:**
- Modify: `web/templates/endpoint.html` (37,170 B, 297 style lines)
- Modify: `web/static/site.css`
- Modify: `web/tests/test_page.py`

**Interfaces:**
- Consumes: `base.html` (Task 3). The context is unchanged — this is a layout change, not a data change.
- Produces: nothing new.

**Read this before starting.** The six sections stay six sections. In particular **"Classes sampled" keeps its own heading and is not folded into Vocabulary.** The two report different metrics (`sw:metric:classes` against `sw:metric:class-profiles`), and the Vocabulary section is wrapped in `{% if vocabulary %}`, which is false for every endpoint without a content profile — folding would delete the sample, its truncation sentence and its provenance line for most of the fleet. That is a change in what the page claims, which this plan's Global Constraints forbid, and roughly ten assertions in `tests/test_page.py` pin it.

What moves is the legend (into the rail, beside the marks it explains) and the identity facts (out of prose, into the rail).

- [ ] **Step 1: Write the failing test**

Append to `web/tests/test_page.py`:

```python
def test_the_page_keeps_all_six_sections(client_for, store):
    body = client_for(store).get(ENDPOINT_URL, headers={"accept": "text/html"}).text
    sections = [s["data-section"] for s in with_attribute(body, "data-section")]
    assert "vocabulary" in sections
    assert "sample" in sections, (
        "the classes sample reports a different metric from the vocabulary and "
        "renders for endpoints that have no vocabulary at all; it keeps its own "
        "section"
    )


def test_the_sample_survives_an_endpoint_with_no_vocabulary(client_for, store_sampled_profile):
    """The regression this task exists to avoid.

    Folding the sample into a section wrapped in {% if vocabulary %} would make
    it vanish for every endpoint without a content profile.
    """
    body = client_for(store_sampled_profile).get(
        ENDPOINT_URL, headers={"accept": "text/html"}
    ).text
    assert 'data-sample="present"' in body or 'data-sample="absent"' in body


def test_the_legend_sits_beside_the_marks(client_for, store):
    body = client_for(store).get(ENDPOINT_URL, headers={"accept": "text/html"}).text
    assert 'data-rail="legend"' in body, (
        "the legend moved into the rail so it is adjacent to what it explains"
    )
    assert len(with_attribute(body, "data-rail-fact")) >= 3, (
        "the rail states what the endpoint is, from facts the context already "
        "holds -- classes described and reported, whether the description is "
        "provably complete, when it was last checked"
    )
```

`store_sampled_profile` is a real fixture (`tests/conftest.py:218`), as are
`store` (`:130`) and `store_content_profiles` (`:206`). `ENDPOINT_URL` and the
`with_attribute` helper are already in `test_page.py`.

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && pytest tests/test_page.py -k "six_sections or legend_sits_beside or sample_survives" -v`
Expected: FAIL — no `data-rail` attributes exist.

- [ ] **Step 3: Convert the page and build the rail**

`{% extends "base.html" %}` as in Task 3, with the history matrix and vocabulary grid rules in `{% block head_extra %}`.

Inside `{% block content %}`, wrap the existing sections:

```html
<div class="page-body">
  <div class="column">
    <!-- conformance, history, vocabulary, classes sampled: existing markup,
         unchanged except for the legend section, which is removed from here -->
  </div>
  <aside class="rail">
    <h3>This endpoint</h3>
    <div class="fact" data-rail-fact="triples"><span class="k">triples</span><span class="v">{{ ... }}</span></div>
    <!-- classes, properties, named graphs, last checked, last profiled -->
    <a class="railbtn" href="{{ void_path }}?url={{ endpoint | urlencode }}">sparqlwatch's VoID</a>
    <div class="legend" data-rail="legend">
      <h3>What the marks mean</h3>
      {% for state in legend %}
      <div class="lg"><span class="swatch {{ state.css_class }}" aria-hidden="true"></span>
        <span class="l-label">{{ state.label }}</span></div>
      {% endfor %}
    </div>
  </aside>
</div>
```

**The rail states only facts already in this page's context.** This task adds
no context key, because a fact the context does not hold would mean a new query,
and the Global Constraints forbid one. Concretely, what is available:

| rail fact | source |
|---|---|
| classes described | `void.described` (`void_document.py:250`) |
| classes reported | `void.reported` |
| provably complete / sampled | `void.complete` |
| sampling ladder used | `void.samplings` |
| last checked | `generated_at`, already rendered in the page header |
| classes sampled | `sample.size`, already rendered in the sample section |

`void_summary` returns `None` when there is no derived description, so every
rail fact drawn from it sits inside `{% if void %}`. If a fact you wanted is not
in that table, it is not on this page today and it does not go in the rail.

In `site.css`:

```css
.page-body { display: grid; grid-template-columns: minmax(0, 1fr) 258px; gap: 26px;
             align-items: start; }
.rail { border: 1px solid var(--border); border-radius: var(--radius);
        background: var(--bg-secondary); padding: 14px 15px; position: sticky; top: 12px; }
@media (max-width: 900px) { .page-body { grid-template-columns: 1fr; }
                            .rail { position: static; } }
```

The rail drops below the column at 900px, legend last — which is the order it is already written in.

- [ ] **Step 4: Run the tests**

Run: `cd web && pytest tests/test_page.py -v`
Expected: pass. `test_page.py` is the largest test file here and pins a great deal of this page's content; if something fails, the content moved when it should not have.

- [ ] **Step 5: Look at the page**

`http://localhost:8099/` then into an endpoint. Check the rail sticks, the matrix scrolls rather than overflowing, and every chip is visibly filled or visibly empty — that last one is what Task 2 was for.

- [ ] **Step 6: Commit**

```bash
git add web/templates/endpoint.html web/static/site.css web/tests/test_page.py
git commit -m "Give the endpoint page a column for measurements and a rail for facts"
```

---

### Task 9: The matcher, in Python

**Files:**
- Create: `web/vocab_match.py`
- Create: `web/tests/test_vocab_match.py`
- Create: `web/tests/fixtures/vocab_match_cases.json`

**Interfaces:**
- Produces:
  - `tokenize(name: str) -> list[str]` — split on camelCase and on `_ - . : /`, lowercased.
  - `score_word(word: str, tokens: list[str], haystack: str) -> int` — 0–4.
  - `rank(terms: list[dict], query: str) -> list[dict]` — each input dict has at least `local`, `prefix`, `iri`, `tokens`; each output dict gains `band` (`"match"`, `"close"`) and `score`, ordered, with non-matching terms omitted.
  - Task 10 transliterates all three into `static/vocab-search.js` and checks them against the same JSON fixture.

The repo has no JavaScript test harness — CI is `cargo` plus `pytest` and there is no `package.json`. Rather than add a toolchain for one file, the rules live here and the browser gets a transliteration checked against a shared table.

- [ ] **Step 1: Write the failing test**

Create `web/tests/test_vocab_match.py`:

```python
"""The scoring rules behind the vocabulary search.

They live in Python so they can be tested here; web/static/vocab-search.js is a
transliteration, checked against the same table by
test_the_javascript_agrees_with_python.
"""

import json
from pathlib import Path

import pytest

from vocab_match import rank, score_word, tokenize

CASES = json.loads(
    (Path(__file__).parent / "fixtures" / "vocab_match_cases.json").read_text()
)


@pytest.mark.parametrize(
    "name,expected",
    [
        ("hasDrugTarget", ["has", "drug", "target"]),
        ("nuclear_receptor-family", ["nuclear", "receptor", "family"]),
        ("Drug", ["drug"]),
        ("rdfs:label", ["rdfs", "label"]),
        ("ID", ["id"]),
    ],
)
def test_tokenize_splits_the_way_names_are_written(name, expected):
    assert tokenize(name) == expected


@pytest.mark.parametrize(
    "word,tokens,haystack,expected",
    [
        ("drug", ["drug", "target"], "drugtarget drugbank", 4),
        ("dru", ["drug", "target"], "drugtarget drugbank", 3),
        ("rugt", ["drug", "target"], "drugtarget drugbank", 2),
        ("recpetor", ["receptor"], "receptor drugbank", 1),
        ("zzz", ["receptor"], "receptor drugbank", 0),
    ],
)
def test_the_five_scores(word, tokens, haystack, expected):
    assert score_word(word, tokens, haystack) == expected


def test_a_three_letter_typo_does_not_fuzzy_match():
    """At three characters, edit distance 1 matches almost everything."""
    assert score_word("dgu", ["drug"], "drug") == 0


def test_a_typo_finds_the_term_substring_matching_misses():
    terms = [{"local": "Receptor", "prefix": "drugbank", "iri": "", "tokens": "receptor"}]
    got = rank(terms, "recpetor")
    assert [t["local"] for t in got] == ["Receptor"]
    assert got[0]["band"] == "close"


def test_two_words_in_the_wrong_order_still_match():
    terms = [
        {"local": "targetOfDrug", "prefix": "drugbank", "iri": "", "tokens": "target of drug"},
        {"local": "Unrelated", "prefix": "drugbank", "iri": "", "tokens": "unrelated"},
    ]
    got = rank(terms, "drug target")
    assert [t["local"] for t in got] == ["targetOfDrug"]
    assert got[0]["band"] == "match"


def test_exact_outranks_prefix_outranks_substring():
    terms = [
        {"local": "DrugInteraction", "prefix": "db", "iri": "", "tokens": "drug interaction"},
        {"local": "Drug", "prefix": "db", "iri": "", "tokens": "drug"},
        {"local": "antidrugAgent", "prefix": "db", "iri": "", "tokens": "antidrug agent"},
    ]
    assert [t["local"] for t in rank(terms, "drug")][0] == "Drug"


def test_an_empty_query_returns_everything_in_order():
    terms = [
        {"local": "B", "prefix": "db", "iri": "", "tokens": "b"},
        {"local": "A", "prefix": "db", "iri": "", "tokens": "a"},
    ]
    assert [t["local"] for t in rank(terms, "")] == ["B", "A"]
    assert all("band" not in t for t in rank(terms, ""))


def test_the_shared_case_table_holds():
    """The same table web/static/vocab-search.js is checked against."""
    for case in CASES:
        got = [t["local"] for t in rank(case["terms"], case["query"])]
        assert got == case["expected"], f"{case['query']!r}: {got} != {case['expected']}"
```

- [ ] **Step 2: Write the shared case table**

Create `web/tests/fixtures/vocab_match_cases.json`:

```json
[
  {
    "query": "recpetor",
    "terms": [
      {"local": "Receptor", "prefix": "drugbank", "iri": "http://bio2rdf.org/drugbank_vocabulary:Receptor", "tokens": "receptor"},
      {"local": "Drug", "prefix": "drugbank", "iri": "http://bio2rdf.org/drugbank_vocabulary:Drug", "tokens": "drug"}
    ],
    "expected": ["Receptor"]
  },
  {
    "query": "drug target",
    "terms": [
      {"local": "DrugTarget", "prefix": "drugbank", "iri": "x:DrugTarget", "tokens": "drug target"},
      {"local": "targetOfDrug", "prefix": "drugbank", "iri": "x:targetOfDrug", "tokens": "target of drug"},
      {"local": "Target", "prefix": "drugbank", "iri": "x:Target", "tokens": "target"},
      {"local": "Pathway", "prefix": "drugbank", "iri": "x:Pathway", "tokens": "pathway"}
    ],
    "expected": ["DrugTarget", "targetOfDrug", "Target"]
  },
  {
    "query": "",
    "terms": [
      {"local": "B", "prefix": "db", "iri": "x:B", "tokens": "b"},
      {"local": "A", "prefix": "db", "iri": "x:A", "tokens": "a"}
    ],
    "expected": ["B", "A"]
  },
  {
    "query": "zzzz",
    "terms": [
      {"local": "Drug", "prefix": "db", "iri": "x:Drug", "tokens": "drug"}
    ],
    "expected": []
  }
]
```

- [ ] **Step 3: Run to verify it fails**

Run: `cd web && pytest tests/test_vocab_match.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'vocab_match'`.

- [ ] **Step 4: Write the matcher**

Create `web/vocab_match.py`:

```python
"""Ranked matching over one endpoint's vocabulary.

Keyword search over this list already worked -- a substring test against the
local name, prefix and IRI. What it could not do was find `Receptor` from
`recpetor`, or `hasDrugTarget` from `drug target`, and people type both.

The rules live in Python rather than only in the browser so that they are
testable under pytest: this repo's CI is cargo plus pytest and carries no
JavaScript harness. web/static/vocab-search.js is a transliteration of this
module, and tests/fixtures/vocab_match_cases.json is the table both are
checked against.
"""

from __future__ import annotations

import re

# camelCase boundaries, and the punctuation that separates words in an IRI.
_BOUNDARY = re.compile(r"(?<=[a-z0-9])(?=[A-Z])|[_\-./:]+")

# Below four characters, edit distance 1 matches almost everything, so the
# fuzzy tier would return the whole vocabulary for a three-letter typo.
MIN_FUZZY_LENGTH = 4


def tokenize(name: str) -> list[str]:
    """One term's name, as the words it is written from."""
    return [part.lower() for part in _BOUNDARY.split(name) if part]


def _within_one_edit(a: str, b: str) -> bool:
    """Levenshtein distance <= 1, decided without building a matrix."""
    if a == b:
        return True
    la, lb = len(a), len(b)
    if abs(la - lb) > 1:
        return False
    if la > lb:
        a, b, la, lb = b, a, lb, la
    i = j = 0
    edited = False
    while i < la and j < lb:
        if a[i] != b[j]:
            if edited:
                return False
            edited = True
            if la == lb:
                i += 1
            j += 1
            continue
        i += 1
        j += 1
    return True


def score_word(word: str, tokens: list[str], haystack: str) -> int:
    """How well one typed word answers to one term. 0 (not at all) to 4 (exact)."""
    if any(t == word for t in tokens):
        return 4
    if any(t.startswith(word) for t in tokens):
        return 3
    if word in haystack:
        return 2
    if len(word) >= MIN_FUZZY_LENGTH and any(_within_one_edit(word, t) for t in tokens):
        return 1
    return 0


def rank(terms: list[dict], query: str) -> list[dict]:
    """The terms that answer to a query, best first, each in its band.

    A term is a `match` when every typed word scores 2 or better -- it is
    present, even if not adjacently or in that order. It is a `close` match
    when at least half the words score at all, which is what a typo produces.
    Anything else is omitted rather than shown greyed: a list that never
    shortens does not answer the question "is this here".
    """
    words = query.strip().lower().split()
    if not words:
        return list(terms)

    ranked = []
    for term in terms:
        tokens = (term.get("tokens") or "").split()
        haystack = " ".join(
            (term.get("local", ""), term.get("prefix", ""), term.get("iri", ""))
        ).lower()
        scores = [score_word(w, tokens, haystack) for w in words]
        if all(s >= 2 for s in scores):
            band = "match"
        elif sum(1 for s in scores if s >= 1) * 2 >= len(words):
            band = "close"
        else:
            continue
        ranked.append({**term, "band": band, "score": sum(scores)})

    # Band first, then total score, then the shorter name, then alphabetically.
    # The last two exist so the order cannot shuffle between keystrokes.
    ranked.sort(
        key=lambda t: (
            0 if t["band"] == "match" else 1,
            -t["score"],
            len(t.get("local", "")),
            t.get("local", ""),
        )
    )
    return ranked
```

- [ ] **Step 5: Run the tests**

Run: `cd web && pytest tests/test_vocab_match.py -v`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add web/vocab_match.py web/tests/test_vocab_match.py \
        web/tests/fixtures/vocab_match_cases.json
git commit -m "Score a vocabulary search the way people type"
```

---

### Task 10: The matcher in the browser

**Files:**
- Create: `web/static/vocab-search.js`
- Modify: `web/explore_payload.py:247-261` (`endpoint_vocabulary`)
- Modify: `web/templates/endpoint.html` — the vocabulary list and the script it replaces
- Modify: `web/app.py` — publish the script beside the stylesheet
- Modify: `web/tests/test_vocab_match.py`, `web/tests/test_explore.py`

**Interfaces:**
- Consumes: `tokenize`, `score_word`, `rank` (Task 9); `STYLESHEET_PATH`'s route pattern (Task 1).
- Produces: `SCRIPT_PATH` and the Jinja global `script_path`; a `tokens` key on every dict `endpoint_vocabulary` returns.

The 30-line filter at `endpoint.html:680-713` is replaced. The `data-hay` attribute stays — it is the score-2 tier.

- [ ] **Step 1: Write the failing tests**

Append to `web/tests/test_vocab_match.py`:

```python
import shutil
import subprocess


@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
def test_the_javascript_agrees_with_python():
    """The browser and the server must score identically.

    There is no JavaScript harness in this repo's CI -- it is cargo plus pytest
    -- so this test runs only where node happens to exist, and the Python rules
    above are the ones that gate. That is the trade this arrangement makes: the
    rules are always tested, and the transliteration is checked wherever it can
    be. If the two ever drift, it will be here that it shows.
    """
    script = Path(__file__).resolve().parents[1] / "static" / "vocab-search.js"
    cases = Path(__file__).parent / "fixtures" / "vocab_match_cases.json"
    harness = f"""
      import {{ rank }} from {str(script)!r};
      import {{ readFileSync }} from 'node:fs';
      const cases = JSON.parse(readFileSync({str(cases)!r}, 'utf8'));
      const out = cases.map(c => rank(c.terms, c.query).map(t => t.local));
      console.log(JSON.stringify(out));
    """
    result = subprocess.run(
        ["node", "--input-type=module", "-e", harness],
        capture_output=True, text=True, check=True,
    )
    got = json.loads(result.stdout)
    assert got == [c["expected"] for c in CASES], (
        "vocab-search.js and vocab_match.py disagree; they are the same rules "
        "written twice and must stay that way"
    )


def test_every_term_carries_its_tokens(store_content_profiles):
    """The browser must not re-tokenise every row on every keystroke."""
    from explore_payload import endpoint_vocabulary

    terms = endpoint_vocabulary(store_content_profiles, ENDPOINT_WITH_VOCABULARY)
    assert terms, "the fixture must have vocabulary"
    for term in terms:
        assert term["tokens"] == " ".join(tokenize(term["local"]) + tokenize(term["prefix"]))
```

`store_content_profiles` is at `tests/conftest.py:206`. Take the endpoint
constant from `tests/test_explore.py`, which already names the endpoint that
fixture carries vocabulary for.

- [ ] **Step 2: Run to verify they fail**

Run: `cd web && pytest tests/test_vocab_match.py -k "javascript_agrees or carries_its_tokens" -v`
Expected: FAIL — no `static/vocab-search.js`, no `tokens` key.

- [ ] **Step 3: Add the tokens field**

In `web/explore_payload.py`, import `tokenize` and add one key to the dict at line 248:

```python
from vocab_match import tokenize
```

```python
            "state": t["at"][endpoint],
            # The term's name as the words it is written from, so the page's
            # search can match "drug target" against hasDrugTarget without
            # splitting every name in the browser on every keystroke. The
            # prefix joins it because people search by namespace too.
            "tokens": " ".join(tokenize(t["l"]) + tokenize(t["p"])),
```

- [ ] **Step 4: Write the script**

Create `web/static/vocab-search.js` as a direct transliteration of `web/vocab_match.py`. Same function names, same tiers, same tie-breaks. Export `tokenize`, `scoreWord` and `rank` so the agreement harness can import them, and attach the DOM behaviour behind an `if (typeof document !== 'undefined')` guard so importing the module under node does not need a DOM.

The DOM half replaces `endpoint.html:680-713`: read each `<li>`'s `data-hay` and `data-tok` once into an array, call `rank` on input, reorder by appending the ranked nodes to a single `DocumentFragment` and appending that to the list, hide the rest, and insert the two band headings. Update `#vocab-count` to `shown + " of " + total + " terms"` as it does today, and give it `aria-live="polite"`.

Keep the existing comment's point in the new file: nothing is fetched; this narrows what is already on the page.

- [ ] **Step 5: Serve the script**

In `web/app.py`, beside the stylesheet route from Task 1, add the same treatment — read at import, hash the bytes, serve at `/static/vocab-search.{digest}.js` with `media_type="application/javascript"` and the same immutable header, and expose `script_path`. Reference it from `endpoint.html`'s `{% block body_end %}`:

```html
{% block body_end %}<script type="module" src="{{ script_path }}"></script>{% endblock %}
```

- [ ] **Step 6: Render the tokens and the band headings**

In `templates/endpoint.html`, add `data-tok="{{ t.tokens }}"` beside the existing `data-hay`, and add the two empty band headings the script fills:

```html
<h3 class="band" data-band="match" hidden>matches</h3>
<h3 class="band" data-band="close" hidden>close matches</h3>
```

- [ ] **Step 7: Run the tests**

Run: `cd web && pytest tests/test_vocab_match.py tests/test_explore.py tests/test_page.py -v`
Expected: pass. On a machine without node, the agreement test reports as skipped — that is expected, not a failure.

- [ ] **Step 8: Type in the box**

Open an endpoint page with vocabulary. Type `recpetor`, then `drug target`. Both must return results under a `close matches` or `matches` heading. Then clear the box and confirm the list returns to its server order with no headings.

- [ ] **Step 9: Commit**

```bash
git add web/static/vocab-search.js web/explore_payload.py web/app.py \
        web/templates/endpoint.html web/tests/test_vocab_match.py
git commit -m "Find a term from a typo or two words in the wrong order"
```

---

### Task 11: Close the loop — mobile, the canonical document, and the sweep

**Files:**
- Modify: `web/tests/mobile/probe_mobile.py`
- Modify: `docs/design/verdict-encoding.md`
- Modify: `web/static/site.css` (whatever the probe finds)

**Interfaces:**
- Consumes: every preceding task.
- Produces: nothing. This is the task that proves the other ten did not regress anything.

- [ ] **Step 1: Add the missing route to the probe**

`web/tests/mobile/probe_mobile.py` covers seven of the eight pages — `/docs/void` is absent. Add it to the route list. The probe runs at 375, 412 and 430 (not 400); leave those viewports as they are.

- [ ] **Step 2: Run the probe**

Run: `cd web && python tests/mobile/probe_mobile.py`
Expected: no horizontal overflow on any of the eight pages at any of the three widths.

If a page overflows, the rule that prevented it was dropped in consolidation. Find it in `git show HEAD~N:web/templates/<page>.html`, and restore it to `site.css` — do not paper over it with `overflow-x: hidden`, which hides the symptom and clips the content.

- [ ] **Step 3: Update the canonical encoding document**

`docs/design/verdict-encoding.md` is canonical for the seven states. It carries no hex values today, so nothing there is stale — but it now needs to say that the fill is a token with two values, and why:

```markdown
## The fill, and the surface

The fill channel is `--fill`, declared once per surface in `web/static/site.css`:
`rgba(0,0,0,0.20)` on the light surface and `rgba(255,255,255,0.16)` on the dark
one. Both were measured against their own background — 1.598:1 and 1.627:1.

This matters more than it looks. A fill tuned for one surface is invisible on
the other, and an invisible fill collapses `verified` into `declared only` and
`confirmed, not declared` into `indeterminate`: four states become two, with no
error anywhere. It has happened once already, with a 5% fill measuring 1.139:1.

`web/tests/test_static.py::test_the_verdict_fill_is_visible_on_both_surfaces`
is what keeps it from happening again.
```

- [ ] **Step 4: Verify no template declares a token**

Run: `cd web && grep -rn -- '--accent:' templates/ && echo "FOUND — fix before continuing" || echo "clean"`
Expected: `clean`.

Two copies of the old tokens survive at `tools/render-run.mjs:232-241` and `tools/explorer/template.html`. Both are **out of scope** — neither is served by this site — and neither is touched.

- [ ] **Step 5: Run everything**

```bash
cd web && pytest -q
cd ../prober && cargo test --quiet && cargo clippy --all-targets -- -D warnings
```

Expected: green. The prober is untouched by this plan; run it anyway, because "untouched" is a claim worth checking once.

- [ ] **Step 6: Measure what this was for**

```bash
cd web && wc -c templates/*.html | tail -1
```

The eight templates were 190,395 bytes carrying 1,427 style lines. Record the new figure in the commit message. Then load the index and an endpoint page in a browser with the network panel open, and confirm the stylesheet is fetched once and served from cache on the second page.

- [ ] **Step 7: Commit**

```bash
git add web/tests/mobile/probe_mobile.py docs/design/verdict-encoding.md web/static/site.css
git commit -m "Prove the redesign kept every page legible at 375px"
```

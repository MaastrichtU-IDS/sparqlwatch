# Website redesign: a shared layer, a light visual system, and search that finds things

**Status:** draft for review
**Date:** 2026-09-15
**Decision:** approach B, taken by the owner on 2026-09-15 after two alternatives
were shown — a restyle in place, and a shared layer without a new visual
identity. B is the shared layer *and* the new identity.

**Goal:** Give the eight server-rendered pages one template layer, one cached
stylesheet, and a visual system that works on a light surface, without changing
what any page claims.

**Scope:** presentation. Templates, one new stylesheet, one new script, one new
query parameter on the index, one added field in `explore_payload.py` (§5), and
the ink values in `docs/design/verdict-encoding.md`.

**Non-goals:** the prober, the store, the queries, `void_document.py`, the
verdict vocabulary, the encoding's structure, and the REST/content-negotiation
behaviour of any resource. If a change here would alter what a machine reads
from this service, it is out of scope.

## Why

Three problems, all of them measurable today.

**The CSS is copied eight times.** `web/templates/` is 190,395 bytes across
eight files, of which 1,427 lines are `<style>` blocks:

| template | bytes | style lines |
|---|---|---|
| `index.html` | 56,314 | 382 |
| `endpoint.html` | 37,170 | 297 |
| `explore.html` | 30,991 | 194 |
| `about.html` | 30,355 | 96 |
| `docs-metrics.html` | 9,444 | 119 |
| `docs-states.html` | 9,017 | 113 |
| `docs-void.html` | 8,635 | 113 |
| `docs.html` | 8,469 | 113 |

There is no `{% extends %}`, `{% include %}`, `{% import %}` or `{% block %}`
anywhere in the directory. The design tokens are declared eight times. That is
why re-tinting the palette is currently eight edits that can disagree, and it is
the direct cause of the third problem below.

**The dark inks fail on any surface but their own.** Measured with the dataviz
validator, today's `#66bb6a / #ffb74d / #ef5350` separate by ΔE 6.0 for protan
vision — inside the 6–8 floor band, legal only because secondary encoding
carries the meaning. On a light surface they are worse.

**Nothing is cached.** Every page carries its whole stylesheet inline, so a
reader moving from the registry to an endpoint re-downloads the design system.

## 1. The visual system

### Surfaces and ink

Light is the default. Dark is *selected* — its own steps, validated against its
own surface, not an automatic inversion.

```css
:root {
  --surface:#fcfcfb;  --raised:#ffffff;  --rule:#e3e2de;
  --ink:#1b1b1a;      --ink-2:#5d5d59;   --ink-3:#8a8a85;
  --accent:#0b5cad;
  --good:#0ca30c;     --warn:#fab219;    --crit:#d03b3b;
  --fill:rgba(0,0,0,.07);
}
@media (prefers-color-scheme: dark) {
  :root {
    --surface:#17171a;  --raised:#1f1f23;  --rule:#33333a;
    --ink:#e8e8e4;      --ink-2:#a8a8a2;   --ink-3:#7a7a75;
    --accent:#4fc3f7;
    --good:#22a532;     --warn:#ffc94d;    --crit:#ef5350;
    --fill:rgba(255,255,255,.14);
  }
}
```

`color-scheme: dark` is removed and replaced with `color-scheme: light dark`.

### What was measured

`scripts/validate_palette.js` from the dataviz skill, status triple only:

| set | surface | CVD (protan) | normal | contrast |
|---|---|---|---|---|
| today's inks | `#1a1a19` | **6.0** — floor band | 20.0 | pass |
| proposed light | `#fcfcfb` | **11.3** | 27.6 | amber 1.79 (WARN) |
| proposed dark | `#17171a` | **16.6** | 27.2 | pass |

Two notes on those results, because both look like failures and neither is one.

The **amber contrast WARN on light is by design and is discharged
structurally.** The validator's obligation for a sub-3:1 status colour is
"visible labels or a table view". Every verdict on this site already ships a
text label beside its mark, on every page. The warning may not be dismissed
silently; it is dismissed by that label, and the label is therefore not
optional in any view added later.

The **lightness-band FAIL** in each run is the categorical-palette check
flagging the amber. A status palette is not categorical; the reference
documents the warning step as sitting outside the band deliberately. Attempting
to snap it inside was measured earlier this session and puts it ΔE 8–14 from
the red, against a floor of 15 — strictly worse.

### The accent

`#4fc3f7` is a dark-surface cyan and does not survive the move. On light it
becomes `#0b5cad` (6.50:1 on `#fcfcfb`, measured), chosen over `#1a4f8a` (more inert) and
`#0f7a8f` (crowds `#0ca30c` peripherally). **The dark theme keeps `#4fc3f7`** —
it measures 8.93:1 on `#17171a`, so the identity change is scoped to light mode.

### The encoding does not change

`docs/design/verdict-encoding.md` stays canonical. The seven states keep their
`(border-style, fill, weight)` triples, the triples stay injective, and colour
remains a redundant channel. Only the hex values in that document change, and
it gains a second table for the dark steps.

**Acceptance:** rendering the seven marks with all hue removed
(`filter: grayscale(1)`) must still yield seven visually distinct marks. This is
a test, not an assertion — see §7.

## 2. The shared layer

### `templates/base.html`

Owns `<!doctype>`, `<head>`, the header, the footer and nothing else. Three
blocks, deliberately:

- `{% block title %}` — the document title.
- `{% block content %}` — the page.
- `{% block head_extra %}` — page-specific CSS, used by exactly two pages:
  `endpoint.html` (history matrix) and `explore.html` (vocabulary grid).

A block per region was considered and rejected: it converts the template layer
into its own puzzle for no gain across eight pages.

### `static/site.css`

One file, served from a route rather than a `StaticFiles` mount, following the
precedent already set for `icon.svg` at `web/app.py:3311` — a mount would
publish the source directory. Read once at import, served from memory, with the
same `public, max-age=31536000, immutable` header the icons use.

Immutability requires a content-addressed URL. The route is
`/static/site.{hash}.css`, where `{hash}` is the first 12 hex characters of the
SHA-256 of the file's bytes, computed at import and exposed to templates as
`_TEMPLATES.globals["stylesheet_path"]` beside the existing `version` global. A
request for any other hash 404s; there is no unversioned alias.

### Navigation is data

The header's four items come from one list in `app.py`, and the docs sub-nav
from the existing `_docs_context()` pages table. Adding a page never edits
`base.html`.

### What is deleted

`color-scheme: dark`, the eight token blocks, and the eight inline `<style>`
bodies minus the two that move into `head_extra`.

## 3. The registry

### The summary strip

New: four figures above the search — endpoints, answering, not answering, time
since last sweep. This is the one genuinely new element in the redesign rather
than a restyle. It is derived from `fleet_stats`, which the page already loads.

### `?q=` becomes real

`index_resource` (`web/app.py:2380`) gains a `q: str | None = Query(None)`
parameter. Matching is a case-insensitive substring test against the endpoint
URL, which is the only identifying text `queries/index.rq` returns — it carries
`?endpoint` and per-metric verdicts, and no name or vocabulary column.

This changes three things for the better: a filtered view becomes a shareable
URL, filtering works without JavaScript, and the page stops shipping all rows
when the reader wants some. Today's filtering is client-side only and has none
of those properties.

**Content negotiation is unchanged in shape.** `?q=` applies to both
representations — a filtered index must serve the same filtered set as data as
it does as a page, or the two representations disagree, which
`tests/test_negotiation.py` exists to prevent.

The client-side filter stays as a progressive enhancement over the rendered
rows, so typing still narrows without a round trip; the round trip is what the
URL and the no-JS path use.

**Matching endpoint vocabulary from the index is deferred** — it needs a join
the index query does not do, and paying for it belongs in its own decision.

## 4. The endpoint page

Same six sections, reorganised into a main column and a 258px sticky rail. The
column holds what was measured: conformance, history, vocabulary. The rail holds
what the endpoint *is*: the answering badge, triples, classes, properties, named
graphs, last checked, last profiled, the link to sparqlwatch's VoID, and the
legend.

Three changes of substance; everything else is restyle:

- **The legend stops being a full section** and becomes a rail block, adjacent
  to the marks it explains rather than below them.
- **The identity facts leave prose** for the rail, where they can be read at a
  glance.
- **"Classes sampled" folds into Vocabulary** rather than repeating the same
  terms under a second heading. The sampling disclosure moves with it: the
  counts stay labelled as sample observations, never as population claims.

At 900px the rail drops below the column, legend last.

## 5. Vocabulary search

### What exists

`endpoint.html:507-539` renders the whole vocabulary server-side, each `<li>`
carrying `data-hay` (`local + prefix + iri`, lowercased) built in Jinja.
`endpoint.html:680-713` hides non-matching rows on `indexOf`. Keyword search
works. The gap is everything else: `recpetor` finds nothing, and `drug target`
finds nothing, because both need more than an adjacent substring.

### What replaces it

A ranked matcher, in `static/vocab-search.js`, written as one exported function
so the explore page can adopt it later without a second implementation. **Only
the endpoint page uses it in this spec.**

**Tokens.** Each vocabulary term gains a `tokens` field, added in
`explore_payload.endpoint_vocabulary` alongside `local`, `prefix`, `kind`,
`iri` and `state`: the local name and prefix split on camelCase boundaries and
on `_ - . : /`, lowercased, space-joined. The template renders it as
`data-tok`, beside today's `data-hay`.

It is computed in `explore_payload.py` rather than in the Jinja expression that
builds `data-hay`, because camelCase splitting is not something a template
expression should carry; and it is computed server-side at all for the reason
`data-hay` is — typing must not re-tokenise every row on every keystroke.

This is the one change in this spec that touches a module outside the
presentation layer. It adds a field and reads nothing new: no query changes, and
no endpoint is contacted.

**Query.** Trimmed, lowercased, split on whitespace into words.

**Per word, against one term, the best of:**

| score | condition |
|---|---|
| 4 | a token equals the word |
| 3 | a token starts with the word |
| 2 | the word occurs anywhere in `data-hay` |
| 1 | a token is within Levenshtein distance 1 of the word, and the word is ≥ 4 characters |
| 0 | none of the above |

The distance-1 test is a bounded early-exit check, not a full matrix. The ≥ 4
floor exists because at three characters edit-distance-1 matches almost
everything.

**Bands.** A term is a **match** if every word scores ≥ 2. Otherwise it is a
**close match** if at least half its words score ≥ 1. Otherwise it is hidden.
The two bands render under headings — `matches` and `close matches` — so a
fuzzy result is never presented as an exact one.

**Order.** Within a band: total score descending, then shorter local name, then
alphabetical. Ties resolve deterministically so the list does not shuffle
between keystrokes. An empty query restores the server's order and removes both
band headings.

**Highlighting.** Matched spans are wrapped in `<mark>` for scores 4, 3 and 2
only. A score-1 match has no exact span to highlight; the band heading is what
explains it. Inventing a highlight there would misreport what matched.

**Budget.** Reordering happens as one `DocumentFragment` append per keystroke,
and must stay under 16ms on the largest vocabulary in the store. §7 names the
test that finds that endpoint and measures it.

**Without JavaScript** the full list still renders and the browser's own find
works. The input does nothing, which is what it does today.

**Accessibility.** The result count is `aria-live="polite"`. Band headings are
real headings, not styled text.

### What was rejected

**Lexical over `rdfs:label` and `rdfs:comment`** is the only option that gets
`medication` → `drugbank:Drug`, and it is genuinely wanted. It needs a label
query on every profile pass, against the minimum-necessary-calls rule that
governs the prober. It deserves its own spec with its own cost argument, not a
rider on a redesign.

**Embedding similarity** is disproportionate to ranking a few hundred strings
already in the browser, and adds a model dependency, a build step and a store.

## 6. The other pages

`explore`, `about` and the four docs pages keep their content and inherit the
shell. The docs pages gain the sub-nav from `_docs_context()`. `explore` keeps
its vocabulary grid via `head_extra`. No copy changes.

## 7. Testing

The existing suite (`web/tests/`) already covers negotiation, page structure and
the encoding. What this adds:

1. **Token blocks are declared once.** A test asserting that no file under
   `templates/` contains `--accent:` — the tokens live in `static/site.css` and
   nowhere else. This is the regression that caused the problem in the first
   place.
2. **Grayscale distinctness.** Render the seven marks, strip hue, assert seven
   distinct `(border-style, border-width, background)` triples. Extends the
   existing `test_page.py` verdict coverage, which already caught one bad
   `absent` fix this session.
3. **`?q=` agrees across representations.** For a query returning a proper
   subset, the HTML row set and the data representation name the same
   endpoints. Belongs in `test_negotiation.py`.
4. **`?q=` without JavaScript.** The rendered page for `?q=uniprot` contains
   only matching rows.
5. **The matcher's table.** Unit tests for the five scores, the two bands and
   the ordering, including `recpetor` → `Receptor` and `drug target` →
   `DrugTarget`, and the ≥ 4 floor rejecting a 3-letter fuzzy match.
6. **Tokenisation is faithful.** `hasDrugTarget` → `has drug target`;
   `nuclear_receptor-family` → `nuclear receptor family`.
7. **Search budget.** Find the endpoint with the largest vocabulary in the
   fixture store, and assert a keystroke stays under 16ms.
8. **Mobile.** `web/tests/mobile/probe_mobile.py` runs against every page at
   400px and asserts no horizontal overflow — it exists and must stay green
   through the change.
9. **Stylesheet caching.** The hashed URL 200s with the immutable header; a
   wrong hash 404s.

## 8. What does not change

`web/app.py`'s routes and negotiation (beyond the added `?q=`), every file in
`web/queries/`, `void_document.py`, `endpoint_index.py`, the prober, the
registry, the deployment manifests, and the verdict vocabulary itself.
`explore_payload.py` gains one derived field and nothing else (§5). No page's claims change. No endpoint is contacted differently.

## 9. Deferred, and deliberately

- Vocabulary matching on the registry's `?q=` (needs a join).
- Label- and comment-aware similarity (needs a query per profile pass).
- The explore page adopting the matcher (the function is written to allow it).
- A user-facing theme toggle. `prefers-color-scheme` only, for now.

# Website redesign: a shared layer, a light visual system, and search that finds things

**Status:** draft for review
**Date:** 2026-09-15
**Decision:** approach B, taken by the owner on 2026-09-15 after two alternatives
were shown — a restyle in place, and a shared layer without a new visual
identity. B is the shared layer *and* the new identity.

**Goal:** Give the eight server-rendered pages one template layer, one cached
stylesheet, and a visual system that works on a light surface, without changing
what any page claims.

**Revision 2** rewrites revision 1 against an adversarial review that checked
every claim against the source. Revision 1's measurements and citations were
exact and its stylesheet route was implementable, but it would have shipped
three bugs: the verdict fill is a hard-coded literal in a module revision 1 did
not touch, `?q=` had no implementation on the RDF representation and no legal
way to get one, and folding "classes sampled" into Vocabulary would have deleted
the sample for most endpoints. Corrections are marked **[R2]**. The review is at
`scratchpad/fable-website-redesign-review.md`.

**Scope:** presentation. Templates, one new stylesheet, one new script, one new
query parameter on the index, the fill constant in `web/verdict_encoding.py`
**[R2]**, one added field in `explore_payload.py` (§5), and
`docs/design/verdict-encoding.md`.

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
  --fill:rgba(0,0,0,.20);   /* [R2] measured — see below */
}
@media (prefers-color-scheme: dark) {
  :root {
    --surface:#17171a;  --raised:#1f1f23;  --rule:#33333a;
    --ink:#e8e8e4;      --ink-2:#a8a8a2;   --ink-3:#7a7a75;
    --accent:#4fc3f7;
    --good:#22a532;     --warn:#ffc94d;    --crit:#ef5350;
    --fill:rgba(255,255,255,.16);  /* [R2] unchanged from today */
  }
}
```

`color-scheme: dark` is removed and replaced with `color-scheme: light dark`.

### What was measured

`scripts/validate_palette.js` from the dataviz skill, status triple only:

| set | surface | CVD (protan) | normal | contrast |
|---|---|---|---|---|
| today's inks | `#0a1929` | **6.0** — floor band | 20.0 | pass |
| proposed light | `#fcfcfb` | **11.3** | 27.6 | amber 1.79 (WARN) |
| proposed dark | `#17171a` | **16.6** | 27.2 | pass |

**[R2]** The baseline row was first measured against the validator's default
dark surface rather than the site's actual `--bg: #0a1929`. Re-measured against
`#0a1929` it is unchanged — 6.0 and 20.0 to the decimal — because the CVD and
normal-vision checks compare the inks to **each other**, not to the surface.
Only the contrast check reads the surface, and it passes on both. The review
was right that the wrong surface was used and wrong that it changed the number.

Three notes on those results, because two look like failures and neither is
one.

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

### The fill is not a token today **[R2]**

`web/verdict_encoding.py:48` hard-codes `CHIP_FILL = "rgba(255, 255, 255,
0.16)"`, and its comment records why: an earlier attempt reused a 5%-white
token, and desaturating the page showed filled and empty chips reading
identically — the channel separating "works" from "we never found out" carried
nothing. Revision 1 declared a `--fill` token that nothing reads, and picked 7%
black for it by eye.

Measured against the surface each sits on:

| fill | composite | contrast vs surface |
|---|---|---|
| 5% white on `#0a1929` — the value that **vanished** | `#1b2734` | 1.139:1 |
| 16% white on `#0a1929` — the value that **works** | `#313e4b` | 1.623:1 |
| 7% black on `#fcfcfb` — revision 1's guess | `#eaeae9` | **1.173:1** |
| **20% black on `#fcfcfb`** — **[R2]** | `#cacac9` | **1.598:1** |

Revision 1's 7% was nearer the known-invisible value than the known-working one.
It looked correct in a mockup, which is precisely the failure the code comment
describes.

**Therefore:** `CHIP_FILL` becomes surface-dependent — `rgba(0,0,0,0.20)` under
light, `rgba(255,255,255,0.16)` under dark — emitted by
`verdict_encoding.css_rules()` into both the base rule and the
`prefers-color-scheme: dark` block. `web/verdict_encoding.py` is in scope, and
the `--fill` token exists only so `css_rules()` has one place to read.

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

**Acceptance:** the seven `(border-style, fill, weight)` triples stay distinct
without reference to colour — already asserted at `tests/test_page.py:392` and
`:520` — **and** the fill clears the visibility threshold §1 measures on both
surfaces, which is the new test (§7.2). The first property was never at risk;
the second is what revision 1 broke.

## 2. The shared layer

### `templates/base.html`

Owns `<!doctype>`, `<head>`, the header, the footer and nothing else. Three
blocks, deliberately:

- `{% block title %}` — the document title.
- `{% block content %}` — the page.
- `{% block head_extra %}` — page-specific CSS, used by **six** pages **[R2]**:
  `endpoint.html` (history matrix), `explore.html` (vocabulary grid), and each
  of the four docs pages, which carry two `<style>` tags apiece today. Revision
  1 said two, and was wrong by four.

A block per region was considered and rejected: it converts the template layer
into its own puzzle for no gain across eight pages.

### `static/site.css`

One file, served from a route rather than a `StaticFiles` mount, following the
precedent already set for `icon.svg` at `web/app.py:3311` — a mount would
publish the source directory. Read once at import, served from memory, with the
same `public, max-age=31536000, immutable` header the icons use.

**The ninth style block. [R2]** `verdict_encoding.css_rules()` generates the
verdict CSS and `app.py:1278` and `:2357` inject it as `encoding_css` into the
`<style>` of `endpoint.html:193` and `index.html:309`. Revision 1's "one static
stylesheet" did not account for it. Because `css_rules()` is deterministic and
callable at import, `site.css` is assembled at import as the static file's bytes
**plus** `css_rules()` output, and the hash is taken over the concatenation.
`encoding_css` is then removed from both template contexts. This keeps the
verdict CSS generated from `verdict_encoding.py` — which is what makes the
encoding single-sourced — while still yielding one cacheable file.

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
`tests/test_negotiation.py:1146` exists to prevent.

**How, given that the query may not be rewritten. [R2]** The RDF representation
comes from `queries/index_description.rq`, an unparameterised CONSTRUCT.
`queries/__init__.py:5-9` forbids templating query text — a `.rq` file must stay
runnable as pasted, and parameters reach a query only through pyoxigraph's
variable substitution, which cannot express a substring test. Revision 1
asserted the behaviour and supplied no mechanism.

The filter is therefore applied **after** the CONSTRUCT, in Python: run the
query unchanged, then drop every triple whose subject is an endpoint the same
predicate rejected for the HTML path. One matching function serves both
representations, which is what makes them agree by construction rather than by
a test noticing later. The query file is not edited, and the rule in
`queries/__init__.py` is not bent.

This also settles the dormancy arm the review flagged: that UNION is not
per-endpoint, so triples it contributes are not attributable to a single
subject. They are retained unfiltered — the filter removes per-endpoint
descriptions, never service-level statements.

The client-side filter stays as a progressive enhancement over the rendered
rows, so typing still narrows without a round trip; the round trip is what the
URL and the no-JS path use.

**What the numbers mean under a filter. [R2]** `_index_context` derives the
fleet stats, the history matrix, the legend counts and the row count from the
same `entries` list. Revision 1 did not say which of those describe the subset.
They all do, and each is labelled with its denominator: the strip reads
`18 of 212 endpoints`, and the summary figures describe the matching set. An
unfiltered page is unchanged. The `?q=` input renders pre-filled with the
submitted value, so the client-side enhancement filters against the same needle
the server used rather than re-filtering with an empty one.

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
- **"Classes sampled" stays its own section. [R2]** Revision 1 folded it into
  Vocabulary. That was wrong twice over. The two sections report different
  metrics — `sw:metric:classes` against `sw:metric:class-profiles` — so merging
  them merges facts that are not the same fact; and the Vocabulary section is
  wrapped in `{% if vocabulary %}`, which is false for every endpoint without a
  content profile, so the sample, its truncation sentence and its provenance
  line would vanish for most of them. That is a change in what the page claims,
  which this spec's own Non-goals forbid, and roughly ten assertions in
  `tests/test_page.py` pin it. It keeps its heading and its disclosures; only
  its styling changes.

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

1. **Token blocks are declared once.** No file under `templates/` contains
   `--accent:` — the tokens live in `static/site.css` and nowhere else. This is
   the regression that caused the problem in the first place. **[R2]** Two more
   copies live outside that directory — `tools/render-run.mjs:232-241`, which
   `verdict-encoding.md` names as a working implementation, and
   `tools/explorer/template.html`. They are **out of scope** (neither is served
   by this site), and the test's docstring says so, so a later reader does not
   mistake its silence for their absence.
2. **The fill survives desaturation. [R2]** Revision 1 proposed a grayscale
   distinctness test; `tests/test_page.py:392` and `:520` already assert the
   seven triples are distinct, so that test exists. The genuinely new property
   is the one §1 measures: assert `CHIP_FILL`'s composite against its surface
   clears 1.4:1 in both modes. That is computable in-process from the constant
   and the surface token, unlike a rendered-pixel check, and it is what would
   have caught revision 1's 7%.
3. **`?q=` agrees across representations.** For a query returning a proper
   subset, the HTML row set and the RDF representation name the same endpoints.
   `run-registry-sample.nq` provides a proper-subset fixture for `?q=uniprot`.
   Belongs beside `tests/test_negotiation.py:1146`.
4. **`?q=` without JavaScript.** The rendered page for `?q=uniprot` contains
   only matching rows, and the input is pre-filled.
5. **Filtered counts carry their denominator. [R2]** The strip on a filtered
   page reads `N of M`, and `M` equals the unfiltered total.
6. **The matcher's table.** The five scores, the two bands and the ordering,
   including `recpetor` → `Receptor`, `drug target` → `DrugTarget`, and the ≥ 4
   floor rejecting a three-letter fuzzy match. **[R2]** The repo has no
   JavaScript test harness — CI is `cargo` plus `pytest`, and there is no
   `package.json`. Rather than add a node toolchain for one file, the scoring
   function is written in Python in `web/vocab_match.py` and tested with pytest;
   `static/vocab-search.js` is a direct transliteration of it, and a golden test
   asserts the two agree on a shared table of cases read from one JSON fixture.
   The alternative — a node runner in CI — is a real option and is noted in §9.
7. **Tokenisation is faithful.** `hasDrugTarget` → `has drug target`;
   `nuclear_receptor-family` → `nuclear receptor family`. Same harness as 6.
8. **Nav links. [R2]** `tests/test_explore.py:88` pins `nav == ["/explore",
   "/docs"]` exactly, and `tests/test_index.py:1266` pins one `/docs` and one
   `/explore` href. A four-item header and a docs sub-nav break both. Their
   intent — never one nav link per row — is worth keeping; the assertions are
   updated to the new set deliberately, in the task that changes the header, and
   not loosened to a substring check.
9. **Mobile. [R2]** `web/tests/mobile/probe_mobile.py` runs at 375, 412 and 430
   — not 400 as revision 1 said — and covers seven of eight pages, omitting
   `/docs/void`. It is also in no CI gate. This change adds `/docs/void` to its
   route list and runs it as part of the work; wiring it into CI is out of
   scope and named in §9. Before the eight style blocks are deleted, their
   `@media (max-width: 640px)` rules, every `overflow-x: auto` container, and
   `.visually-hidden` are inventoried into `static/site.css` — consolidation
   that drops one of them is how this change would regress silently.
10. **Stylesheet caching.** The hashed URL 200s with the immutable header; a
    wrong hash 404s. **[R2]** And the hash covers `css_rules()` output, so
    changing a verdict's presentation changes the URL.
11. **Focus is visible. [R2]** `endpoint.html:128` and `explore.html:72` both
    set `outline: none` on their search input. The consolidated stylesheet
    replaces that with a visible `:focus-visible` ring; a test asserts no
    `outline: none` survives without one.

## 8. What does not change

`web/app.py`'s routes and negotiation (beyond the added `?q=`), every file in
`web/queries/`, `void_document.py`, `endpoint_index.py`, the prober, the
registry, the deployment manifests, and the verdict vocabulary itself.
`explore_payload.py` gains one derived field and nothing else (§5), and
`verdict_encoding.py` gains a surface-dependent fill and nothing else (§1)
**[R2]**. No page's claims change; the "classes sampled" reversal in §4 is what
keeps that true. No page's claims change. No endpoint is contacted differently.

## 9. Deferred, and deliberately

- Vocabulary matching on the registry's `?q=` (needs a join).
- Label- and comment-aware similarity (needs a query per profile pass).
- The explore page adopting the matcher (the function is written to allow it).
- A user-facing theme toggle. `prefers-color-scheme` only, for now.
- **[R2]** A JavaScript test harness in CI. §7.6 avoids needing one by keeping
  the scoring rules in Python; if a node runner is wanted for its own sake, it
  replaces that arrangement rather than adding to it.
- **[R2]** Wiring `probe_mobile.py` into a CI gate. It runs, and it is not gated.
- **[R2]** The token copies in `tools/render-run.mjs` and
  `tools/explorer/template.html`, which this site does not serve.

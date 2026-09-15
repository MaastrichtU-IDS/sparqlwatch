# Review: 2026-09-15 website redesign spec

Reviewed against the source at 7594c15. Every line reference, byte count and
style-line count in the spec was re-derived; every oracle §7 leans on was
looked for; every function the design touches was read.

## Verdict

The spec's measurements and citations are exact, its stylesheet route is
implementable as written, and the `?q=` HTML path and the `tokens` field are
sound. It would nonetheless ship three bugs. First, the verdict fill is not a
token: `verdict_encoding.py:48` hard-codes `rgba(255,255,255,0.16)`, 16% white,
so on the spec's `#fcfcfb` surface every filled chip becomes an empty one and
the encoding's fill channel is gone — the exact defect `verdict-encoding.md`
records, and the spec's `--fill` token is read by nothing. Second, `?q=` on the
RDF representation has no implementation: `index_description.rq` is an
unparameterised CONSTRUCT, §8 forbids editing it, `queries/__init__.py` forbids
templating it, and its dormancy arm is not per-endpoint, so the only test that
guards HTML/RDF agreement gains a new way to fail. Third, "Classes sampled folds
into Vocabulary" merges two facts from two different metrics into a section that
does not render for any endpoint without a content profile, which is every
fixture but two, so the sample, its truncation sentence and its provenance
vanish — a change in what the page claims, which the spec's own Non-goals
forbid. Below those: the encoding CSS is a ninth inline style block the spec
never mentions, which makes "one static stylesheet hashed by file bytes" either
false or unimplementable as written; a filtered `?q=` changes every count on
the index page and the spec has not chosen which numbers describe the subset;
and of the nine tests, test 2 already exists in the form described while the
property that is actually new is untestable in-process, tests 5 and 7 have no
JavaScript harness anywhere in the repo, test 7's "largest fixture vocabulary"
is 15 terms, and test 8 describes a script that runs at three other widths,
covers seven of the eight pages, and is in no gate.

## Findings that would ship a bug

### 1. The fill channel disappears on the light surface

`web/verdict_encoding.py:43-48`:

```python
# 16% white over either background. The first attempt at this reused the theme's
# --overlay token, 5% white, which is tuned for panels and vanishes at chip
# size: desaturating the page showed filled and empty chips reading identically
CHIP_FILL = "rgba(255, 255, 255, 0.16)"
```

and `:216` `fill = CHIP_FILL if state.fill else "transparent"`. The fill is a
Python literal, not a CSS token. The spec's `--fill:rgba(0,0,0,.07)` (spec:66)
and `--fill:rgba(255,255,255,.14)` (spec:74) are declared and read by nothing
in the encoding. Consequences, in order:

- If `verdict_encoding.py` is left alone (§8 names `explore_payload.py` as the
  one module outside presentation that changes; `verdict_encoding.py` is not on
  the list), light mode renders 16% white over `#fcfcfb`: filled and empty chips
  are identical. `verified` collides with `declared-only`,
  `undeclared-but-verified` with `indeterminate`, `declared-but-wrong` with
  nothing but only because of its 2px weight. That is the two-collision
  encoding `docs/design/verdict-encoding.md` §"Why three channels" was written
  to retire.
- The existing guards do not fire. `test_page.py:392`
  `test_no_two_states_share_a_border_fill_weight_triple` compares the
  `(border, fill, weight)` booleans in the table, and `test_page.py:520`
  `test_no_two_generated_rules_differ_only_by_colour` compares generated CSS
  text; both pass while the rendered chips are indistinguishable.
- Even if `CHIP_FILL` is repointed at the spec's `--fill`, the value is wrong.
  The design doc and the code comment record that 5% was invisible on a 26x22
  chip and 16% survives; the spec proposes 7% for light. The spec's own §1
  acceptance criterion ("seven visually distinct marks with all hue removed") is
  the check that would catch this, and §7 test 2 cannot perform it (finding 6).

The fix is one line plus a `--fill` token, but the spec has to name
`verdict_encoding.py` as a touched module and set the light value from a
desaturated render, not from the panel-overlay analogy.

### 2. `?q=` on the RDF representation is unimplemented and the spec forbids the ways to implement it

§3: "`?q=` applies to both representations". §8: "every file in `web/queries/`"
does not change. `web/app.py:2373-2376`:

```python
def _index_rdf(store: Store, media_type: str) -> bytes:
    triples = store.query(_INDEX_DESCRIPTION_QUERY)
```

`_INDEX_DESCRIPTION_QUERY` is `queries/index_description.rq`, a CONSTRUCT with
three UNION arms and no substitution point. `web/queries/__init__.py:5-9`
states the house rule: "nothing here templates or rewrites the text: parameters
reach a query through pyoxigraph's variable substitution, never through string
formatting." So:

- A substring filter needs `FILTER(CONTAINS(LCASE(STR(?endpoint)), ?q))` in
  the query text. pyoxigraph 0.5.9's `substitutions` does accept a `Literal`
  (checked: `dict[Variable, NamedNode or BlankNode or Literal or Triple]`), so
  the *value* can be passed without templating — but the FILTER itself is an
  edit to the `.rq`, contradicting §8. And with no `q` given, `?q` is unbound,
  `CONTAINS` errors, the FILTER is false, and the unfiltered index returns
  zero triples; it needs a `!BOUND(?q) ||` guard or a second query.
- The third UNION arm (`index_description.rq`, the `?newestActivity` block) is
  not per-endpoint. It emits `sw:dormantEndpoint ?newestDormant` and
  `?newestDormant sw:dormancyReason ...` for every endpoint `sw:current` lists.
  A filtered index that keeps those emits dormancy facts about endpoints the
  HTML omits, which is exactly the shape
  `test_negotiation.py:1146 test_neither_description_query_describes_an_endpoint_the_html_omits`
  exists to refuse. The filter has to reach that arm too.
- The alternative — post-filter the CONSTRUCT's triples in Python — is not
  forbidden, but activity triples (`?activity a prov:Activity ...`) are shared
  across endpoints and the spec would have to say which survive a filter that
  drops every measurement pointing at that activity.

None of this is hard. All of it is undecided, and §3's "Content negotiation is
unchanged in shape" reads as though it were already settled.

### 3. "Classes sampled folds into Vocabulary" deletes a claim for most endpoints

The two sections are built from two metrics by two readers:

- **Classes sampled** (`endpoint.html:564-611`) renders `sample`, built by
  `_sample(measurements, content)` (`app.py:997`) from `endpoint_content`,
  the `sw:metric:classes` sample (`sw:sampledValue`, `sw:sampleTruncated`).
- **Vocabulary** (`endpoint.html:507-540`) renders `endpoint_vocabulary`
  (`explore_payload.py:235`), built from `explore_vocabulary.rq`, which is
  pinned to `sw:sampleRunMetric <urn:sparqlwatch:metric:class-profiles>` in
  every arm, and wrapped in `{% if vocabulary %}` (`endpoint.html:507`).

Measured over the fixtures with `build_payload`: `run-content-profiles.nq` and
`run-sampled-profile.nq` give one endpoint 15 terms each; `run-with-samples.nq`,
`run-registry-sample.nq` and every other fixture give **0**. The `store`
fixture (run-with-samples) has a classes sample and no vocabulary. Fold the
sample into a section that does not render for it and the sample, its
`truncation_text`, its `provenance_text` and its `this_run_text` disappear from
the page. That is a change in what the page claims, against the spec's own
Non-goals (spec:9-11, 17-20), and it takes out
`test_page.py:718 test_a_truncated_sample_says_so_in_text`, `:732`, `:741`,
`:753`, `:768 test_the_reason_for_a_missing_sample_is_read_from_the_graph`,
`:789 test_a_sample_of_zero_reports_its_size_rather_than_a_list`, `:997`,
`:1010` and the `data-sample`/`data-sample-run`/`data-sample-generated-at`
contract they read. §4 says "the counts stay labelled as sample observations"
as if the two were one dataset with two labels; they are two datasets from two
sweeps that are "routinely different runs" (`app.py:1227-1232`).

Either the sample keeps its own block (in the rail, if the point is layout) or
the spec has to say what a Vocabulary section shows for an endpoint with a
sample and no profile. Today the answer is "nothing", and that is a regression.

## Findings that would cost rework

### 4. `encoding_css` is a ninth inline style block the spec does not know about

`app.py:1279` `"encoding_css": verdict_encoding.css_rules()`, rendered inside
the `<style>` element at `endpoint.html:193` and `index.html:309`. §2 says
`static/site.css` is "read once at import, served from memory", hashed on "the
file's bytes", and that "the eight inline `<style>` bodies" are deleted. One of
these has to give:

- Keep the encoding rules inline: then every page still ships un-cached CSS,
  and "Nothing is cached" (spec:50) is only mostly fixed. Small, but §2 should
  say so.
- Move them into the stylesheet: then `site.css` is not a file, it is a file
  plus `css_rules()` assembled at import, and the hash must be over the served
  bytes, not "the file's bytes" (spec:148). Test 1 is unaffected either way
  because `css_rules()` declares no `--accent:`.

Related: the four docs pages carry a **second** `<style>` block
(`docs.html:104-121`, `.doclist`, `.entry`, `.swatch-lg`; same in
`docs-metrics.html`, `docs-states.html`, `docs-void.html` at :104). §2's
"`head_extra` used by exactly two pages" is wrong by four unless those rules
move to `site.css`. The spec should say which; "exactly two" reads as a
decision and is currently an oversight.

### 5. A filtered `?q=` changes every count on the page and the spec has not chosen which

`app.py:2410` `_index_html(endpoint_index(store), store)` →
`_index_context(entries, store)` (`app.py:2214-2260`). Everything is derived
from `entries`: `fleet_stats(store, history, entries)` (`fleet.py:124-138`,
`endpoints=len(entries)`, `triples=sum(...)` over entries),
`_index_metrics(entries)`, `_index_rows`, the legend counts (`drawn`),
`endpoint_count`, `_state_facets`, `_metric_state_matrix`,
`_newest_sweep_note(entries)`.

- Filter `entries` before `_index_context` and the new summary strip (§3) says
  "1 endpoint" for `?q=uniprot` while presenting itself as the fleet.
- Filter after, and the matrix counts disagree with the rows, which
  `test_index.py:1886 test_every_chip_count_is_the_rows_the_page_renders`
  pins, and the inline script (`index.html:942-955`) recounts chips from
  rendered rows on load, so the page visibly changes numbers on first paint —
  the flicker `index.html:942-945` warns about.
- Either way the `#q` input (`index.html:450`) must be pre-filled with `q`, or
  the script's `survives()` (`index.html:900`) and `apply()` (`:927`) filter the
  already-filtered subset with an empty needle while the URL says otherwise.

§3 says a filtered view "stops shipping all rows when the reader wants some";
it does not say what the strip, the grid, the legend and the "N endpoints"
lines describe. That is the decision, and it is not made.

### 6. Test 2 already exists in the form described, and the new property is untestable in-process

`test_page.py:392 test_no_two_states_share_a_border_fill_weight_triple` and
`:520 test_no_two_generated_rules_differ_only_by_colour` already assert seven
distinct `(border-style, border-width, background)` triples. Spec §7 test 2
describes them again as new. The property that *is* new — that the triples
survive desaturation on the light surface — cannot be tested in this suite:
`ci.yml:71-73` runs `pytest` in-process, `tests/mobile/README.md:3-5` records
that anything needing a browser is deliberately outside it, and
`verdict-encoding.md` §"How to check a change" says to render with
`tools/render-run.mjs`, screenshot with headless Chrome, and `sips` to grey.
§1 says "This is a test, not an assertion"; the source has it the other way
round. Given finding 1, the manual desaturation is the check that matters and
the spec should schedule it, not a duplicate of the structural test.

### 7. Tests 5 and 7 have no harness, and test 7's oracle is 15 terms

No `package.json`, no node test runner, no JS test file anywhere in the repo
(`find` over the tree: only `tools/render-run.mjs` and
`design/render-static.mjs`). CI runs `cargo` and `pytest` (`ci.yml`). Test 5
(unit tests of a JavaScript matcher) and test 7 (a keystroke under 16 ms) need
a JS runtime; test 7 needs a browser or a DOM. The spec names neither.

"The endpoint with the largest vocabulary in the fixture store": measured,
`run-content-profiles.nq` and `run-sampled-profile.nq` → 15 terms; every other
fixture → 0. §5's budget is "the largest vocabulary in the store" (production;
§5 rejected-options says "a few hundred strings"). Fifteen terms under 16 ms
measures nothing. Either a fixture of realistic size is added and a harness
named (Playwright, which `tests/mobile/` already uses, or node's built-in
runner against a jsdom), or test 7 is demoted to a manual measurement and said
to be one.

### 8. Test 8 misdescribes the mobile probe

`tests/mobile/probe_mobile.py:15`: viewports 375, 412, 430 — not 400.
`:17-22` `routes()`: seven routes, no `/docs/void`. `README.md:3-5`: "neither
is in the default pytest run"; needs a running site and Chromium; `ci.yml`
does not run it. So "must stay green through the change" is a manual step no
gate enforces, and it never looks at one of the eight pages.

What keeps it green today is a specific set of rules the consolidation must
carry: `table { display:block; overflow-x:auto; max-width:100% }` at
`about.html:95`, `docs*.html:81`, `endpoint.html:99` and `:203`,
`index.html:256` and `:323`; `.t-iri { min-width:0 }` at `explore.html:164`;
and the eight `@media (max-width: 640px)` blocks, the largest of which
(`index.html:325-354`) carries page-specific rules (`.tools input`,
`.f-head-label`). The probe (`probe_mobile.py:36-42`) exempts descendants of
`overflow-x: auto|scroll` containers, so dropping any of those `table` rules
turns a passing page into a failing one.

### 9. Two tests pin the header shape the spec redesigns

`test_explore.py:87-88`:

```python
nav = [a["href"] for a in with_attribute(body, "data-nav")]
assert nav == [EXPLORE_PATH, "/docs"], f"nav is {nav}"
```

An exact list. §2's "four items" header with `data-nav` on each breaks it.
`test_index.py:1266-1287` asserts exactly one `href="/docs"` and one
`href="/explore"` on the index; a docs sub-nav, a footer link, or a summary-strip
link to docs on the index breaks that. Both tests should move with the header;
the spec should list the four items (today the header holds the logo,
`vocabulary` and `docs` — three anchors on the main pages, two on docs pages —
so "four" is a design choice, not a description).

### 10. The baseline was measured against a surface the site does not use

Spec §1 table: "today's inks | surface `#1a1a19`". Every template declares
`--bg: #0a1929` (`index.html:15`, `endpoint.html:16`, the other six alike).
The ΔE 6.0 "floor band" figure that opens the "Why" was measured against a
neutral near-black rather than the blue-dark the pages render. The result may
well be similar; the number in the spec is not the site's.

### 11. Two more token copies outside `templates/`

`tools/render-run.mjs:232-241` carries the full token block
(`--accent:#4fc3f7; --good:#66bb6a ... --chip-fill:rgba(255,255,255,.16);
color-scheme: dark`), and `verdict-encoding.md:5` calls it "the working
implementation". `tools/explorer/template.html` also declares `--accent`.
"Declared eight times" undercounts; test 1 is scoped to `templates/` so both
drift silently. And `index.html:10-13` names `design/Main.dc.html` as "the
authority for ... these tokens"; the spec re-tokens without saying whether
that authority claim survives.

## Cosmetic

### 12. `verdict-encoding.md` holds no hex values

Scope (spec:15) "the ink values in `docs/design/verdict-encoding.md`" and §1
"Only the hex values in that document change" — the document has a
border/fill/weight table and no colour table. Nothing to change; two tables to
add.

### 13. `vocabulary_json` is dead

`app.py:1218` serialises the vocabulary into the context; no template
references `vocabulary_json` (grep over `templates/`: nothing). Adding
`tokens` costs nothing there, but the key is a full JSON encode per request
for nothing. Not the spec's fault; the spec should not "account for" it, it
should delete it. The reader golden (`test_reader_golden.py`) covers the three
readers, not `endpoint_vocabulary`, so `tokens` breaks no golden.

### 14. `outline: none` on both search inputs

`endpoint.html:128` and `explore.html:72` remove the focus outline and rely on
`border-color: var(--accent)`. §5's accessibility paragraph covers `aria-live`
and headings only. The consolidation is the moment to keep a visible focus
indicator by design rather than by accident.

### 15. Shared rules with no inventory

`.visually-hidden` is defined in two templates (`index.html:234`,
`endpoint.html:93`) and used in both (`index.html:514`, `endpoint.html:474`,
`:514`). `mask-icon color="#81d4fa"` is hard-coded in all eight heads and is a
dark-surface cyan. The spec deletes eight style bodies and gives no list of
what must be recreated; findings 8 and 14 are two entries on that list.

## What checked out

- `web/app.py:2380` is `def index_resource` (decorator at `:2379`).
  `web/app.py:3311` is the "Served from routes rather than a StaticFiles
  mount" comment; the icon route itself is `:3323`, `_ICON_CACHE` at `:3320`.
  `endpoint.html:507-540` is the vocabulary section (`{% if vocabulary %}`
  through `{% endif %}`), `:680-713` the search IIFE. All four exact.
- Byte counts: all eight match `wc -c` exactly; total 190,395. Style lines:
  all eight match a count inclusive of the `<style>` and `</style>` lines
  (index 382, endpoint 297, explore 194, about 96, docs-metrics 119, the other
  three 113); total 1,427. No `extends`, `include`, `import`, `block`, `from`
  or `macro` tag anywhere in `templates/`.
- `queries/index.rq` projects `?endpoint`, `?run`, `?generatedAt`, `?metric`,
  `?verdict`, `?level`, `?elapsedMs`, `?reason`, the two counts and the
  newest-run columns — no name, no vocabulary. The spec's matching-scope claim
  is right.
- No caching on the index path. The only `lru_cache` is `_opened_store`
  (`app.py:348`), keyed on the store path; the only `Cache-Control` is the
  icons'. No ETag, no `Vary`. `Query` is already imported (`app.py:56`) and
  used for `url` on the endpoint route (`:1299`); `q: str | None = Query(None)`
  is the house shape.
- The hashed stylesheet route is implementable as written: the hash is known
  at import, and `@app.get(f"/static/site.{digest}.css")` is a fixed path at
  registration time, so any other hash 404s for free. `Dockerfile` copies
  `web/` wholesale, so `static/` ships. `_TEMPLATES.globals["version"]`
  (`app.py:2870`) is the precedent the spec cites and it exists.
- `endpoint_vocabulary` returns `iri, kind, local, prefix, namespace, engine,
  state` (`explore_payload.py:246-256`); the spec lists five of seven, but a
  `tokens` field fits, and `data-hay` really is `local ~ prefix ~ iri`
  lowercased in Jinja (`endpoint.html:528`).
- `_docs_context()` exists (`app.py:2687-2740`) with a four-entry `pages`
  table, so the docs sub-nav has its data source.
- `?q=uniprot`: `run-registry-sample.nq` holds nine endpoints, one of them
  uniprot, so test 4's proper subset exists. `test_negotiation.py:1109`,
  `:1146` and `test_index.py:778`, `:815` are the index-negotiation oracles
  test 3 would extend.
- Today's inks `#66bb6a / #ffb74d / #ef5350`, accent `#4fc3f7`, and
  `color-scheme: dark` are as the spec says, in all eight templates.
- `endpoint.html` has exactly six `<section>` elements (`:401`, `:430`, `:462`,
  `:508`, `:542`, `:564`).
- Test 9 is straightforward with `TestClient`.

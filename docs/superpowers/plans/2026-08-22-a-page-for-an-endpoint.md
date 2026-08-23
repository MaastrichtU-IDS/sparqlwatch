# Stage 3-1: A Page For An Endpoint

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve one real endpoint page over HTTP, backed by the store, content-negotiated so a person gets HTML and a machine gets RDF.

**Architecture:** FastAPI over the existing Oxigraph store. Two queries as files, one already written. Server-rendered HTML, no build step and no JavaScript framework yet. The prober is untouched.

**Tech Stack:** Python 3.12 in the existing `web/.venv`, FastAPI, Jinja2, pyoxigraph. `httpx2` for the test client.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`, whose web tier lists a per-endpoint page among its read paths and requires **content negotiation on every resource**: "HTML for people, RDF for machines. A quality-measurement service that is not itself machine-readable would be self-defeating."

## Why server-rendered HTML, and why not React yet

The eventual front end reuses ontoexplorer's React and Vite setup, and the design
canvas was drawn for it. This slice does not start there, for one reason that is
about the spec rather than about effort: **content negotiation makes an HTML path
mandatory anyway.** A resource that serves RDF to machines and HTML to people needs
the HTML to come from the server, and once it does, the first version of every
screen is server-rendered whether or not a framework arrives later.

React earns its place when a screen needs interactivity that a page reload cannot
give: faceted search across 548 endpoints, and the embedded query editor. Neither
is in this slice. Starting with a build step, a bundler and a component library to
render one page of static facts would be scaffolding standing in for progress.

So: HTML now, React when a screen needs it, and no apology for either.

## What already exists, verified

- `web/load_run.py` loads a run into an on-disk store, replacing the run's graph.
- `web/queries/endpoint_content.rq` answers what classes the most recent run
  sampled for an endpoint, with the size and the truncation flag. Real values:
  kadaster 59, ontop 50, qlever none.
- 28 Python tests, 271 Rust tests, both green.
- `docs/design/verdict-encoding.md` is the **canonical** verdict encoding. The
  design artboards predate it and disagree; the doc wins.

Dependencies, all installed and verified into `web/.venv` before this plan was
written: fastapi 0.141.1, starlette 1.6.0, uvicorn 0.52.4, jinja2 3.1.6,
httpx2 2.12.0. **Use `httpx2`, not `httpx`**: starlette 1.6.0's test client emits a
deprecation warning with `httpx` and is silent with `httpx2`. Pinning a deprecated
path into a new subsystem, in a project that rejected its predecessor for having an
EOL stack, would be that same mistake in miniature.

## Design decisions

**D1. Content negotiation on one resource, properly.** The same URL serves HTML to
a browser and RDF to a machine, chosen by `Accept`. This is spec-mandated rather
than a flourish, and it is cheap to get right now and expensive to retrofit once
routes multiply.

**D2. The RDF representation matches what the HTML shows.** Both describe the
**most recent** run's facts about that endpoint. Serving all history under the same
URL would make the two representations disagree about what the resource is, and
history deserves its own resource when a query for it exists.

**D3. The verdict encoding comes from the canonical doc, and a test enforces that
it stays injective.** Seven states must map to seven distinguishable presentations.
This is where the viewer already went wrong twice: `chipStyle` and the legend
drifted apart, and an earlier encoding had two colour-only collisions. A rendering
test cannot see colour, but it **can** assert that no two states share the same
(border style, fill, weight) triple. That is exactly the property the canonical doc
exists to guarantee, so assert it directly rather than trusting a template.

**D4. The store is injected, not opened globally.** Tests need their own store, and
a module-level open would make every test share one. FastAPI's dependency
mechanism is the seam.

## Global Constraints

- **The prober is not touched.** No file under `prober/`. `cargo test --manifest-path prober/Cargo.toml`
  must still report 271 passed, 0 failed, 2 ignored, and that is a check rather
  than a formality: a Python slice that quietly edited the prober would be caught
  nowhere else.
- Dependencies pinned to exact versions in `web/requirements.txt`.
- **A page must not assert more than the data supports.** A truncated sample must
  never render as a complete class list, and "no sample" must never render as "no
  classes". Both facts are in the graph; the page has no excuse.
- No network in any test. The store is built from a committed fixture.
- No em-dashes anywhere.
- Prove every test by mutation, and verify each mutation applied **semantically**
  rather than textually.
- `git status --porcelain` clean at the end. **Do not use `git add -A`**: stage
  files by name.

---

## Task 1: The measurements query

The page needs the endpoint's verdicts, and no query returns them yet.

**Files:**
- Create: `web/queries/endpoint_measurements.rq`
- Modify: whatever module holds the query wrappers, beside `endpoint_content`
- Test: `web/tests/test_endpoint_measurements.py`

**Interfaces:**
- Given an endpoint, return one row per metric measured in the **most recent** run
  that measured it: the metric id, the verdict, the level where present, and the
  elapsed time where present.
- Also return, distinguishably, the metrics recorded as **not measured** in that
  run. A declined metric is not a missing one, and the graph says which.

- [ ] **Step 1: Write the failing tests**

Use the committed fixtures, whose real values are known. Against
`run-with-samples.nq`, kadaster has 8 measurements and every verdict in it is
known; check the fixture and assert exact values rather than counts alone.

Tests that must exist, because each catches a wrong implementation that would pass
the others:

- kadaster's verdicts are returned, by metric id, with the exact verdict strings.
- **ontop's verdicts are not returned when asking for kadaster.** A query ignoring
  its parameter returns three endpoints' rows and passes any test that only checks
  a plausible count.
- **the most recent run wins.** Use `run-two-sweeps.nq`, where the same endpoint is
  measured in two runs. Assert on a value that differs between them, not on a
  count: a union of two runs can have a plausible size.
- **a level is returned where the graph has one** (`service-description` carries
  one) and absent where it does not.
- **an endpoint with no measurements at all** returns nothing, and the caller can
  tell that from an endpoint whose metrics were all declined.

Reuse the `prov:generatedAtTime` ordering and the `FILTER NOT EXISTS` shape from
`endpoint_content.rq`, and read that file's header comment first: it records why an
`ORDER BY DESC(...) LIMIT 1` subquery is wrong here (a substituted endpoint does not
reach inside a subquery's projection, so it picks the newest run in the whole store
and returns nothing for an endpoint whose newest measurement is in an older run).
That trap applies identically to this query, and at 548 endpoints it is routine
rather than exotic.

- [ ] **Step 2: Run them and watch them fail**

- [ ] **Step 3: Implement**

Keep the SPARQL in the `.rq` file. Python must not filter what the query could
filter itself: if the query returns another endpoint's rows and Python removes
them, the parameter test passes and the query is still wrong.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations, each verified applied: drop the endpoint filter; drop the
`FILTER NOT EXISTS` so every run is unioned; return not-measured rows as ordinary
measurements; drop the level. Each must fail its own test.

- [ ] **Step 5: Commit**

---

## Task 2: The resource, negotiated

**Files:**
- Create: `web/app.py`
- Modify: `web/requirements.txt` (the new pins)
- Test: `web/tests/test_negotiation.py`

**Interfaces:**
- One route for an endpoint resource. Choose a URL shape and say why in a comment;
  an endpoint is identified by a URL, so it has to be encoded into the path or
  carried as a query parameter, and both have consequences worth one sentence.
- `Accept: text/html` serves HTML. An RDF type serves RDF. No `Accept` at all
  serves HTML, because a bare `curl` is a person more often than a machine.
- An unknown endpoint is a 404, not an empty page.
- The store arrives by dependency injection (D4).

- [ ] **Step 1: Write the failing tests**

- `Accept: text/html` gives 200 and `text/html`.
- `Accept: text/turtle` gives 200 and a turtle content type, and the body parses
  as RDF. **Parse it, do not merely check it is non-empty**: a body that is not
  valid RDF is worse than a 406, because a machine will act on it.
- The RDF body contains the endpoint's measurements. Assert a specific quad, not a
  size.
- `Accept: application/json` with no RDF alternative gives 406, not HTML. Guessing
  wrong is worse than refusing.
- An unknown endpoint gives 404 in both representations.
- **The two representations agree.** Assert that the verdict shown in the HTML for
  one metric is the verdict present in the RDF for that same metric. Two
  representations of one resource that disagree is the defect this test exists to
  prevent, and it is not hypothetical: they are built from two different code paths.

- [ ] **Step 2: Run them and watch them fail**

- [ ] **Step 3: Implement**

For the RDF representation, serialise the quads the two queries returned rather
than re-deriving facts in Python. The store already holds RDF; turning it into
objects and back into RDF is where the two representations would drift apart.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations: serve HTML regardless of `Accept`; serve an empty body for RDF; return
200 for an unknown endpoint; make the HTML read from a different run than the RDF
(the agreement test must fail).

- [ ] **Step 5: Commit**

---

## Task 3: The page itself

**Files:**
- Create: `web/templates/endpoint.html`, and a stylesheet or an inline style block
- Modify: `web/app.py`
- Test: `web/tests/test_page.py`

- [ ] **Step 1: The encoding, from the canonical document**

Read `docs/design/verdict-encoding.md` first. It defines seven states across three
channels: border style, fill, and border weight, so that colour never carries
meaning alone. Implement that table **once**, in one place, and derive both the
chips and any legend from it. The existing JavaScript viewer had two copies that
drifted, and the legend ended up explaining `absent` with a swatch that did not
match the chip.

Write the test the doc asks for: **no two of the seven states share the same
(border style, fill, weight) triple.** That is the injectivity property the whole
encoding rests on, it is testable without rendering, and it is what actually broke
before.

- [ ] **Step 2: Write the failing page tests**

- The page names the endpoint and shows one chip per measured metric.
- **A truncated sample says so, in text.** The existing viewer renders "truncated:
  more may exist beyond the limit" rather than relying on a border style, which is
  legible in greyscale and to a screen reader. Match that. Use
  `run-truncated.nq`.
- **An endpoint with no sample does not render an empty class list.** qlever in the
  fixture has no sample because its enumeration exceeded the request budget; the
  page must say we did not get one, not imply the endpoint holds no classes.
- A declined metric renders as declined, distinguishably from a verdict.
- The class list for kadaster contains 59 entries and one of them is
  `http://www.w3.org/2002/07/owl#Class`.

- [ ] **Step 3: Implement, then look at it**

Render it and **look at the page**, in a browser or a screenshot. Then desaturate
the screenshot and look again: that is how the JavaScript viewer's fill token was
caught being invisible at chip size, which no code review would have found. Say in
your report what you looked at and what the greyscale render showed.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations: give two states the same triple (the injectivity test must fail); drop
the truncation text; render an empty list for an unsampled endpoint.

- [ ] **Step 5: Commit**

---

## Task 4: Run it, and document it

**Files:**
- Modify: `web/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: Serve it for real**

Load a real run into a store, start `uvicorn`, and fetch the page for each of the
three endpoints. Report what each showed, including qlever, whose page is the one
most likely to overstate. Then fetch the same URLs with `Accept: text/turtle` and
confirm the RDF parses.

This is the first time this project serves anything over HTTP, so the report
should say what actually happened rather than that the tests pass.

- [ ] **Step 2: Document**

In `web/README.md`: how to run the server, the URL shape and why, what content
negotiation does, and which read paths exist. Be explicit that a leaderboard, a
metric page, history and evidence do **not** exist yet, since the spec lists them
as read paths and a reader will expect them.

- [ ] **Step 3: Spec**

Mark stage 3 partly delivered, following the table's existing convention. Name what
exists (one endpoint resource, content-negotiated) and what does not (leaderboard,
metric pages, history, evidence, the embedded editor, faceted search). Do not
claim stage 3.

- [ ] **Step 4: Commit**

---

## Done criteria

- `cargo test --manifest-path prober/Cargo.toml` still reports 271 passed, 0 failed, 2 ignored.
- The Python suite passes, and the endpoint page renders from a store loaded with a
  real run.
- The same URL serves HTML and parseable RDF, and the two agree about the verdicts.
- No two verdict states share a presentation triple, enforced by a test.
- A truncated sample says so in text; an unsampled endpoint does not render as
  having no classes.
- The README says which read paths do not exist yet.

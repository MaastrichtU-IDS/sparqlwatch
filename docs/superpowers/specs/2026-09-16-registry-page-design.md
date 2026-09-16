# The registry page: names, facets, and what a visitor meets first

**Status:** draft for review
**Date:** 2026-09-16
**Decision:** taken by the owner on 2026-09-16, across four questions in a
brainstorm: use the catalogue's titles but never unattributed; demote the metrics
matrix rather than hide it; show the host for endpoints that serve many datasets.

**Goal:** Make the registry scannable — the page leads with a search and named
endpoints instead of a grid of jargon and a wall of URLs.

## Why this exists, and why it is a second spec

`docs/superpowers/specs/2026-09-15-website-redesign-design.md` gave the site a
shared layer, a light visual system, a summary strip and a server-side `?q=`. It
did not change the registry's information design, and it should have: the
brainstorm that produced it chose "shared layer + **search-first index** + new
visual identity", and the screens the owner approved showed named rows and facet
pills. Between those screens and that spec, the index's layout was dropped. §3 of
that document has exactly two subsections — the strip and `?q=` — and the plan
implemented it faithfully, so every review downstream validated against a spec
that had already lost the design.

The result shipped and reads, correctly, as a recolour. This spec is the missing
half.

## What a visitor meets today

A summary strip, a filter box, then a 10x7 grid of metric names and state
names — 39 clickable facet buttons — and only below that, the endpoints, each
rendered as a bare URL. The first thing on the page is `cors-preflight` against
`declared-only`. The endpoints the visitor came for are beneath it.

## 1. Where a name comes from

**Measured, not assumed.** `lod-data.json`, the dump `seed-registry` already
downloads and parses, carries a `title` on every dataset. Checked against the
current registry:

| | |
|---|---|
| registry endpoints with a title in the dump | **543 of 543** |
| with exactly one title | 503 |
| with more than one | **40** (one has 42) |
| with a `domain` | 596 of 719 dataset entries; **118 endpoints have none** |

`seed.rs:134` walks `dataset["sparql"][]["access_url"]` and steps straight past
`title` and `domain` in the same object. Both are therefore available **at seed
time, at zero cost to anybody's server** — no new probe, no extra request, and
nothing that touches the minimum-necessary-calls rule the prober is built around.

**The title is a claim, and it is published as one.** The front page says this is
a registry "measured rather than asserted", and a catalogue's title is an
assertion — sometimes a stale one; the kg-catalog work in this project found 5 of
9 URLs simply wrong. So the name is never presented as something sparqlwatch
found. It sits above the URL, which is the thing we did verify, and the page says
once where the names come from.

## 2. The forty that serve many datasets

`http://linked.opendata.cz/sparql` carries 42 titles. "Results of R&D" is not
that server's name; it is one of forty-two things it hosts.

**Endpoints with more than one title show the host, not a title** —
`linked.opendata.cz`, with `42 datasets` where a single-dataset row shows its
name. Choosing one of 42 to lead with would assert something untrue about the
server, which is the opposite of why the catalogue's titles were acceptable.

This also removes a stability problem. "First title" means first in the dump's
iteration order, which is not a rank and can reshuffle between dumps, so a row's
name could change for no reason at all.

## 3. The nine that are not from the dump

`prober/registry/kg-catalog.toml` is hand-maintained and carries no titles; its
names exist only as prose in `docs/kg-catalog-endpoints.md`. That file is
explicitly hand-edited ("Editing this by hand is therefore correct, and nothing
overwrites it"), so the nine titles are added to it by hand, in the same shape the
generated registry uses. An endpoint with no title in either registry renders as
its host, exactly like the forty.

## 4. The registry file grows two fields

`prober/registry/*.toml` becomes an array of tables rather than an array of
strings:

```toml
[[endpoint]]
url = "http://taxonomy.bio2rdf.org/sparql"
title = "Bio2RDF::Taxonomy"
domain = "life_sciences"

[[endpoint]]
url = "http://linked.opendata.cz/sparql"
datasets = 42          # no title: this host serves many
domain = "government"
```

`seed-registry` writes it; its fixed-point test keeps hand edits out of the
generated files.

**Both forms must parse, and that is not free.** `registry.rs:40-43` deserialises
`endpoint: Vec<String>`. All four registry files are bare strings today —
`endpoints.toml` and `endpoints.container.toml` (12 each, hand-written) as well as
the two generated ones. Only the generated files gain fields, so the reader must
accept a list that is *either* strings or tables. In serde that is an untagged
enum over the two shapes, not an added optional field; a struct with optional
fields would reject every bare string and break the dev registries on the first
run. `metrics.toml` already uses `[[metric]]` array-of-tables, so the file shape
is not novel here — only the mixed reader is.

Beyond parsing, **the prober's behaviour does not change**: it probes the same
endpoints in the same order and sends nothing different to anybody.

## 5. The title never enters the run graph

A run graph records what a sweep observed. A catalogue's title is an input, not an
observation, and `queries/index_description.rq` rests on the guarantee that every
triple it constructs "appears, verbatim, in some run graph in this store". Putting
a title there would break that for a fact nobody measured.

So the web process reads the registry files directly, at import, the way it
already reads `icon.svg` — `COPY prober/registry/ /app/prober/registry/` is
already in the Dockerfile, so the files are present in the image.

**The consequence that matters, and the reason this is stated here.** If the HTML
matched titles and the RDF could not, `?q=` would filter the two representations
differently — precisely the defect the last spec's §3 was corrected for. So
`_matches_query` consults the same title map for **both** representations: it
matches URL, title and host, and both branches call it. The RDF still publishes no
title (it invents nothing); it simply filters on the same predicate. The invariant
that survives is the one that matters: *the two representations name the same
endpoints.*

## 6. What the page becomes

Top to bottom:

1. **The summary strip** — unchanged.
2. **Search** — unchanged mechanically, now matching name and host as well as URL.
3. **Facet pills** — a short row: `answering`, `declares VoID`, `federates`, and
   the three largest domains (`government` 134, `life sciences` 124,
   `publications` 105), each carrying its count. Pills are links carrying a query
   parameter, so a faceted view is shareable and works without JavaScript, like
   `?q=`.
4. **The endpoints** — name, URL beneath, size, and the existing verdict cells.
5. **Metrics and states** — the 10x7 matrix, unchanged and still clickable, moved
   below the rows.

**The matrix is demoted, not hidden.** It is the only way to ask a precise
question ("which endpoints do geo-data properly"), and hiding a feature behind a
disclosure makes it undiscoverable. What is wrong today is its position, not its
existence.

**118 endpoints have no domain**, so a topical pill can never be the only way to
narrow the list. Search and the measured pills cover everything; domain pills are
an extra.

## 7. Testing

1. **Every registry entry parses, with and without the new fields.** The prober's
   reader must accept the old bare-string form and the new table form, because a
   registry file and a binary are deployed together but written apart.
2. **The forty show a host, not a title.** Assert against a fixture carrying a
   multi-dataset endpoint that no title appears in its row and the count does.
3. **`?q=` still names the same endpoints in both representations** when the query
   matches a *title* rather than a URL — the case that did not exist before and is
   the one that could break the invariant. The existing negotiation tests cover
   URL matches only.
4. **A title is never published as RDF.** Assert the constructed index contains no
   `dcterms:title` for an endpoint.
5. **An endpoint absent from every registry renders.** The store can hold runs for
   an endpoint since dropped from the registry; its row shows the URL and does not
   raise.
6. **The matrix still filters** from its new position — the existing facet tests
   move with it and must not be weakened.

## 8. What does not change

The prober, every file in `web/queries/`, the verdict encoding, the endpoint page,
the vocabulary search, `void_document.py`, and what any page claims about any
endpoint. No endpoint is contacted differently, and no new request is made to
anyone's server. `seed-registry` downloads the same dump it already downloads.

## 9. Deferred

- Publishing the title as attributed RDF (`dcterms:title` with
  `prov:wasDerivedFrom` the dump). Defensible, but it is a vocabulary addition and
  belongs in its own decision.
- Keywords and descriptions from the dump, which are also present and unused.
- Re-seeding to pick up a newer dump. This spec changes the format; when the
  registry is next re-seeded is a separate call.

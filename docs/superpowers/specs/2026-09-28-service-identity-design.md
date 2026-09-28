# Service identity: preferred, alternative, invalid

**Status:** design, 2026-09-28.

## The problem, from three failures in one week

**One.** `registry/yummydata.toml` lists AgroLD's endpoint as `http://sparql.southgreen.fr/`.
That URL answers `406` to every Accept a SPARQL client sends and `200 text/html`
to `*/*`, so the sweep read it as a server that does not speak the protocol and
reported it unresponsive for weeks. The service is at
`https://sparql.southgreen.fr/sparql` and answers in 0.20s.

**Two.** Correcting that URL did not fix the row. It created a SECOND endpoint.
`rebuild_current` drops an endpoint only once no retained run graph mentions it,
and retention is not built, so the old URL sat on the page indefinitely — same
name, same service, reported unresponsive beside the working one. The patch was
to mark it `inactive`, which is the wrong word: the service was never retired.

**Three.** The reconnaissance pass over the LOD Cloud found 51 live endpoints,
six of which are the same host under two schemes (`dbpedia.org`,
`sparql.uniprot.org`, `opendata.aragon.es`, `ldf.fi`, `id.ndl.go.jp`,
`vocabulary.semantic-web.at`). Admitting them as listed would put six services
on the page twice.

All three are one missing distinction: **the registry records URLs, and the
thing it means to record is services.**

## What Bioregistry does

One record per resource. `uri_format` is the single canonical form;
`providers[]` are alternates, each with a stable `code`, a `name` and its own
`uri_format`; `synonyms[]` are other accepted spellings of the prefix;
`deprecated` is a property of the resource. Any observed form resolves *to* the
record. The record is never one-per-URL, and alternates are curated by hand
rather than inferred.

That is the shape this adopts.

## The hard constraint

`emit::subject_iri` embeds the endpoint URL reversibly in every subject, and
`dqv:computedOn` names it as a resource — in run graphs this project never
rewrites. **History cannot be re-identified.** A design that requires rewriting
what was published is not available, and one that pretends the old URL was
never swept would falsify the record.

So the resolution happens at the READ side. The store keeps saying what it
always said; the site maps what it finds to the service it belongs to.

## Schema

```toml
[[service]]
endpoint = "https://sparql.southgreen.fr/sparql"   # preferred; the only one swept
title    = "Agronomic Linked Data (AgroLD)"
domain   = "Life sciences"
datasets = 3
# inactive = "why"   # optional; the SERVICE is gone

  [[service.alternative]]
  url  = "http://sparql.southgreen.fr/sparql"
  note = "http; redirects to the https form"

  [[service.invalid]]
  url    = "http://sparql.southgreen.fr/"
  source = "yummydata"
  found  = "2026-09-27"
  reason = "406 to every SPARQL Accept; the console, not the endpoint"
```

Three categories, and they say different things:

| | what it asserts | swept |
|---|---|---|
| `endpoint` | this is the service's SPARQL endpoint | yes |
| `alternative` | another spelling of the SAME service | never |
| `invalid` | a URL somebody published that is NOT this endpoint | never |

`inactive` returns to meaning exactly one thing — **the service is gone** — and
stops doubling as "this URL was superseded", which is what it was stretched to
cover for southgreen. A superseded URL is an `alternative` or an `invalid`
depending on whether it ever worked.

The existing `[[endpoint]]` form — a bare string or a table — keeps parsing. The
`Entry` enum is already untagged for exactly this reason, and 543 seeded entries
must not need rewriting on day one.

## Decisions

**D1. Resolution is display-only.** The sweep probes `endpoint` and publishes
under it, as now. The site resolves any endpoint IRI in the store — including
ones swept under a spelling since demoted — to its service. Run graphs stay
immutable and nothing published is falsified. The alternative, re-emitting
history under the preferred URL, would rewrite the record to make a page tidier.

**D2. Alternates are curated, never inferred.** `http://` and `https://` on one
host LOOK like one service, and five such pairs sit in `lod-cloud.toml` — but
southgreen is the case where two near-identical URLs behaved completely
differently, and "same service" is not provable by probing. `seed-registry` is
unchanged by this design and continues to write one entry per URL. `recon` may
REPORT candidate pairs; a person makes the claim.

**D3. An alias is not a row.** A URL listed as `alternative` or `invalid` is not
an endpoint in its own right, so the index does not list it and `/endpoint` for
it redirects to the preferred URL's page. Its measurement history is not merged
into the service's: deciding which of two runs' verdicts is "the service's"
would be a claim this project has no basis for. The history stays reachable
under the URL that was actually swept.

**D4. A URL may appear once.** A URL that is a preferred `endpoint` on one
service and an `alternative` or `invalid` on another is a curation error, and
loading fails rather than picking one. The exclusions rule sets the precedent:
a registry that cannot be read unambiguously stops the run.

## What this does not do

It does not merge measurement history (D3), it does not rewrite published graphs
(the constraint), and it does not add a service IRI to the published vocabulary
— `dqv:computedOn` keeps naming the endpoint URL that was actually asked. A
service-level subject is a larger change to what this project publishes and is
not needed to fix any of the three failures above.

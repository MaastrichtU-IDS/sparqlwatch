# Vocabulary explorer

A page for exploring the classes and properties observed in monitored endpoints,
with autocomplete and faceted filtering. Prototype: it holds one probe of two
endpoints, and it is not wired into the site.

```bash
web/.venv/bin/python tools/explorer/build.py      # writes tools/explorer/explore.html
```

## Recovered 2026-09-01, and why that matters

This was built on 2026-08-28 with a generator written in `/tmp`, and served from
a directory outside git. `/tmp` was cleaned. What was left was one 219 KB
generated file that nothing could rebuild.

`build.py` is that generator rewritten from the artifact and verified against it:
its output is byte-identical to the page that was still running. The template is
the artifact with its payload swapped for a placeholder, so every design comment
in the CSS and the inline script is the original.

The lesson is cheap to state: the design reasoning survived because it was
written into the artifact rather than only into the tool that produced it.

## What the page is, and the three findings built into it

**A term is the row, its endpoints are its cells.** The transpose of the index,
where the endpoint is the row.

**The evidence states are the site's existing verdict vocabulary, not a new one.**
Used and declared IS `verified`; used but not declared IS
`undeclared-but-verified`; declared but not used IS `declared-only`. Same slugs,
same `enc-` classes copied from `verdict_encoding.css_rules()`. A reader who
learned `/docs/states` already knows this page.

**Endpoint and state filter jointly, not independently.** A row here spans
endpoints, so testing them separately let `dbpedia` plus `declared, not
confirmed` return a term whose declared-only evidence came from the other
endpoint. A term qualifies only when one of its own (endpoint, state)
observations satisfies both filters at once. Same lesson that made the index's
metric and state into a matrix, and a matrix is unavailable here because the
endpoint axis is 543 long.

**Autocomplete ranks by how many endpoints carry the term.** Alphabetical order
put a five-term vocabulary's `Person` above foaf's. Accepting a suggestion pins
that exact term, because `<.../foaf/0.1/Person>` is a substring of
`<.../foaf/0.1/PersonalProfileDocument>`.

## The two honesty rules

Both are in `build.py`'s docstring and both were forced by what the probe
returned. A capped sample cannot carry a negative claim, and an empty sample from
a REFUSED query is not evidence of absence either. Both collapse to
`indeterminate`.

The result is the page's whole point: of dbpedia's 572 terms, 13 are claimable
and 559 read "not determined", with both negative-evidence chips at zero.

## It IS a route, as of 2026-09-01

`GET /explore`, on the same port as everything else. Port 8731 is retired and the
standalone copies are deleted: `template.html` plus `build.py` are the source now,
and `web/explore_payload.json` is what the route serves.

Run `build.py` and copy its payload into `web/explore_payload.json` to refresh it:

```bash
web/.venv/bin/python tools/explorer/build.py --payload-only > web/explore_payload.json
```

## What still has to change before it is more than a prototype

The payload is embedded and holds two endpoints. That is fine at 214 KB and does
not scale: 196 bytes per term, and these two endpoints share only 25 of 990 terms
because each publisher brings its own ontology, so 543 endpoints projects to 20 to
37 MB against a 750 KB page budget.

So the route serves a committed file today and must compute its facets and
autocomplete server-side later. That needs content data in the store, which needs
the content-profile work. The page says "prototype: 2 endpoints" in its header for
as long as that is true, and `test_the_page_says_it_is_a_prototype` keeps it
saying so.

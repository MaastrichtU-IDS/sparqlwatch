# The DBpedia KG Catalog's SPARQL endpoints, validated

**What this is.** The nine SPARQL endpoints the [DBpedia Knowledge Graph
Catalog](https://kg-catalog.dbpedia.org/) declares, each one probed on
2026-09-14, with the url that actually answers beside the url the catalog
states. Six of the nine disagree.

**Why it is written down.** The catalog's page is rendered by JavaScript and
carries no endpoint in its HTML, so the list is not readable from the site: it
comes from `void:sparqlEndpoint` in the MOSS metadata store behind it. Anyone
wanting this list otherwise has to rediscover that, and then rediscover that
most of the urls do not work.

## How the list was obtained

The site's `assets/js/index.js` names two SPARQL endpoints of its own, and the
metadata lives in the second:

```
https://databus.dbpedia.org/sparql        the Databus: titles, sizes, licences
https://moss.dev.dbpedia.link/sparql      MOSS: the VoID metadata, incl. endpoints
```

```sparql
PREFIX void: <http://rdfs.org/ns/void#>
SELECT DISTINCT ?s ?e WHERE { ?s void:sparqlEndpoint ?e } ORDER BY ?e
```

Ten rows, of which `http://example.org/sparql` belongs to a `hello-world`
entry and is a placeholder. `void:sparqlEndpoint` is the ONLY property in that
store carrying an endpoint: the single `sd:endpoint` value is Virtuoso's own
internal `local:/sparql`, and no `dcat:accessURL` contains a SPARQL url. Ten
endpoints against roughly 673 catalogued knowledge graphs, so ~1.5% of the
catalog says where it can be queried at all.

## The endpoints

Every url below was probed with `sparqlwatch-prober --max-cost expensive`.
"Counted" is what the endpoint answered, not what it claims.

| Knowledge graph | Working endpoint | Triples | Classes |
|---|---|---|---|
| DBLP | `https://sparql.dblp.org/sparql` | 1,592,528,741 | 44 |
| DBnary | `https://kaiko.getalp.org/sparql` | 1,255,715,624 | 132 |
| YAGO | `https://yago-knowledge.org/sparql/qlever` | 972,511,260 | 199,498 |
| SemRepo | `https://semrepo.org/sparql` | 70,540,788 | 23 |
| ORKG | `https://orkg.org/triplestore` | 12,961,244 | — |
| SIDEKICK | `https://sidekick.bio2vec.net/fuseki/sidekick/query` | 3,155,104 | 16 |
| DSKG | `http://dskg.org/sparql` | 1,711,894 | 25 |
| Wikidata | `https://query.wikidata.org/sparql` | declared, not countable | |
| SemOpenAlex | `https://semopenalex.org/sparql` | not countable | |

**≈3.91 billion triples across the seven that can be counted**, and not one of
those seven declares its own size. Every count reads
`undeclared-but-verified`: there was a number to measure and nothing to check
it against.

## Where the catalog is wrong

Three of nine urls are correct. The rest:

| Knowledge graph | Catalog states | Actually answers at |
|---|---|---|
| DBLP | `https://sparql.dblp.org` | `…/sparql` |
| Wikidata | `https://query.wikidata.org/` | `…/sparql` |
| SIDEKICK | `https://sidekick.bio2vec.net/sparql` | `…/fuseki/sidekick/query` |
| ORKG | `https://www.orkg.org/orkg/sparql/` | `https://orkg.org/triplestore` |
| YAGO | `https://yago-knowledge.org/sparql` | `…/sparql/qlever` |
| MAKG | `https://makg.org/sparql` | superseded by `https://semopenalex.org/sparql` |

MAKG is the only entry that is not a typo. Its host refuses the TLS handshake
outright -- three client stacks (LibreSSL, OpenSSL, rustls) all get
`tlsv1 alert internal error`, while a bare `openssl s_client` handshake to the
same host succeeds -- and the project it belongs to was succeeded by
SemOpenAlex. The catalog points at a retired service.

Five of the six wrong urls ANSWER, with an HTML console rather than the SPARQL
protocol, which is why they cannot be dismissed by checking whether the host is
up.

## Two endpoints that are interesting rather than broken

**Wikidata declares its size and will not let you check it.** `triple-count`
and `class-count` both read `declared-only`: the declaration is there, and the
query service refuses the `COUNT` in ~300 ms. It is the only one of the nine
that gets that verdict, and the exact inverse of the other eight.

**SemOpenAlex answers a `COUNT` with an empty result and a 200.** After ~10.8 s
it returns `{"results":{"bindings":[]}}` -- no error status, no rows. A `COUNT`
aggregate returns exactly one row by definition, so this is a store timing out
internally and reporting success. It is reported `indeterminate` and not `0`,
which is the whole reason the counting rule distinguishes "no row" from "zero":
the alternative reading would publish that SemOpenAlex holds no triples.

## Capabilities

- **GeoSPARQL functions**: SemRepo and SemOpenAlex answer `geof:sfWithin`
  correctly. Nobody declares it.
- **CORS**: DBLP, DBnary, Wikidata, ORKG, SIDEKICK and YAGO allow a browser to
  query them. DSKG, SemRepo and SemOpenAlex do not.
- **Service descriptions**: DBLP, SIDEKICK and YAGO serve nothing at all at
  their endpoint url; the rest serve something that is not a readable
  description. Of nine endpoints, none publishes one this prober can read.

## Reproducing this

```sh
cargo run --release --bin dormancy -- init --state /tmp/kg/dormancy.toml
cargo run --release --bin sparqlwatch-prober -- \
  --at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --endpoints registry/kg-catalog.toml \
  --metrics metrics.toml \
  --state /tmp/kg/dormancy.toml \
  --max-cost expensive \
  --out /tmp/kg/run.nq
```

`prober/registry/kg-catalog.toml` holds the working urls from the table above.

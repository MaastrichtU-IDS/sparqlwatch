# YummyData's endpoints, measured a second time

**What this is.** The 63 SPARQL endpoints
[YummyData](https://yummydata.org/endpoint) lists, swept by this project's
prober on 2026-09-18 at the cheap cost ceiling, with our per-attribute verdicts
set beside YummyData's single Umaka Score. Their data is from 2026-09-17, a day
earlier.

**Why it is written down.** YummyData is the closest thing to prior art this
project has -- Umakadata, from DBCLS -- and it takes the opposite position on
the one question this service is built around. It combines several attributes
into a score out of 100 and a letter rank A-E. This service refuses to, and
says so on /docs/metrics: "Each carries its own verdict and none is combined
with another into a score: there is no ranking on this site, and these are the
reasons why one would be misleading." Their list is the chance to check that
claim against something rather than assert it.

**A different population, which is the other reason to have it.** Only 6 of
these 63 appear anywhere in the 544 endpoints this repository already held.
DBCLS curates life sciences; the rest of this registry is a LOD Cloud dump and
a DBpedia catalogue.

## Where we agree

Strongly, on the things both services measure.

| | agreement |
|---|---|
| their `alive` = true | 56 of 57 read `verified` for us |
| their `service_description` = true | 40 of 42 read `verified` for us |

That second row is the one that corrected something here. This repository's
`kg-catalog-endpoints.md` said "Of ten endpoints, none publishes a service
description this prober can read", which read as a fact about SPARQL endpoints.
It is a fact about those ten. Forty of these publish one our own prober reads,
fetched exactly the way it fetches: endpoint url, no query, `Accept:
text/turtle`, answered with `sd:Service` and `sd:endpoint`. The catalogues
seeded here are the undescribed corner of the fleet, not the fleet.

## Where we disagree, and why

Three endpoints, each re-checked by hand on 2026-09-18 after the sweep.

| Endpoint | YummyData | This sweep | Checked |
|---|---|---|---|
| GlyGen | `alive: false`, score 5 | `availability: verified` | answers `ASK` in 382 ms |
| SwissLipids | `alive: false`, score 20 | `availability: verified` | see below |
| IMGT-KG | `alive: true`, score 86 | `availability: indeterminate` | no answer in 40 s |

**SwissLipids is a url, not an outage.** YummyData lists
`https://beta.sparql.swisslipids.org/`, which answers 301 and serves no
protocol. `https://beta.sparql.swisslipids.org/sparql` answers. It is the same
class of error as five of the six in the DBpedia catalog: a path missing from
the listing, where the host is up and a liveness check on it passes.

**IMGT-KG is the honest disagreement.** It scores 86 there and did not answer
us inside 40 seconds. Both readings can be true of different moments, which is
what `indeterminate` is for: it says we did not establish an answer, not that
the endpoint is down.

## What a single number hides

This is the part that is an argument rather than a table. Forty-two of these
63 endpoints share a score with at least one other. Twenty-two of those -- in
seven groups -- differ from a score-mate on at least one of availability,
service description and CORS: the same number, a different fault.

The clearest case is the bottom of the table, score 5, where four endpoints sit
on three different shapes:

| Endpoint | availability | service description | CORS |
|---|---|---|---|
| Bio2RDF | indeterminate | *nothing recorded* | *nothing recorded* |
| neXtProt | indeterminate | indeterminate | indeterminate |
| Agronomic Linked Data | indeterminate | indeterminate | indeterminate |
| GlyGen | **verified** | indeterminate | **absent** |

GlyGen answers queries in under half a second. Bio2RDF's hostname does not
resolve at all -- SERVFAIL from its own authoritative nameservers, recorded in
`prober/endpoints.toml` since 2026-09-05. One of those is a working endpoint
that does not send CORS headers; the other is not an endpoint this week. They
score the same, and the score is the only thing either page shows you first.

The same shape repeats up the table. At score 63, PDBj-BMRB publishes a service
description and blocks browsers; FORUM does both. At 65, RDF Portal Bioportal
publishes a description and allows CORS, GlyConnect does neither. A reader
choosing an endpoint to build a browser application on needs the CORS column,
and no single number has one.

None of this says the Umaka Score is wrong. It says it is an answer to a
different question -- "how good is this endpoint overall" -- and that the
question this service asks, "which attribute is missing", cannot be recovered
from it.

## Reproducing this

```sh
cargo run --release --bin dormancy -- init --state /tmp/yd/dormancy.toml
cargo run --release --bin sparqlwatch-prober -- \
  --at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --endpoints registry/yummydata.toml \
  --metrics metrics.toml \
  --state /tmp/yd/dormancy.toml \
  --max-cost cheap \
  --out /tmp/yd/run.nq
```

CHEAP, not expensive, and that is a decision rather than a default. These are
sixty-three strangers' servers on first contact, and the expensive metrics run
`COUNT` aggregates -- one of those took 17.5 seconds on a single endpoint
earlier the same day. The attributes compared above are all cheap ones.
YummyData's own list and flags come from `https://yummydata.org/endpoint.json`.

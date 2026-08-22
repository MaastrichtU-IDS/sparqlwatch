# The task list: candidates derived from the survey

**Status: proposal awaiting the owner's cut.** Nothing here is decided.

The design makes a task-first surface the front door: "what do you need this
endpoint to do", with the verdict matrix behind it. This document proposes the
tasks, and for each one says **which shipped metrics answer it** and **what is
missing**, so the list is grounded in what we can measure rather than what sounds
useful.

Every number below comes from `~/code/umaka-test` (`SURVEY.md`, `GEO_DATA.md`,
`REPORT.md`): a probe of all 548 distinct SPARQL endpoint URLs in the LOD Cloud
dump of 2026-06-15.

## The rule applied

A task earns a place on the front door only if answering it changes which
endpoint somebody picks. That means it needs at least one metric whose verdicts
actually **differ** across endpoints. A task every endpoint passes, or every
endpoint fails, is a decoration.

## Answerable today, with shipped metrics

### 1. Run geospatial queries

| | |
|---|---|
| Metrics | `geo-functions` (function binding), `geo-data` (geometry present) |
| Discriminates | Yes, strongly, and in three directions |

The flagship, because the survey's headline finding is exactly this: **0 of 548
endpoints advertise GeoSPARQL, while 18 demonstrably evaluate `geof:sfWithin`**.
Better still, the verdicts are not binary. Of the 65 live endpoints:

- 18 evaluate the point-in-polygon filter correctly
- **9 answer it wrongly**, having the function bound but getting the geometry or
  CRS axis order wrong, which is `declared-but-wrong` and is a state no other
  monitor publishes
- 8 explicitly reject it as an unknown function
- 7 hold `geo:asWKT` data, and **only 5 have both** the functions and the data

Two independent probes are what make that visible, and the survey says so
directly: function support and data presence are independent. `vocabs.ardc.edu.au`
holds geometry and rejects the function; `publications.europa.eu` answers the
function and its 352 `asWKT` values are **all `rdf:nil`**, which is why the
literal guard exists.

### 2. Query it from a browser application

| | |
|---|---|
| Metrics | `cors` (header on a simple GET), `cors-preflight` (a real preflight) |
| Discriminates | Expected to, and this is the task that gates our own editor |

Stage 3b embeds a query editor per endpoint, and it cannot work against an
endpoint that refuses a preflight. Two metrics rather than one because an endpoint
can genuinely have the simple-GET header and refuse `OPTIONS`, which passes a
naive check and still fails in a browser.

### 3. Understand what is in it before writing a query

| | |
|---|---|
| Metrics | `has-classes`, `service-description` (graded 0 to 4), `classes` (expensive, opt-in) |
| Discriminates | Yes, and the grading is what makes it discriminate |

A binary "publishes a service description" would be useless here: **21 of the 28
descriptions are byte-identical 14-triple Virtuoso stubs**, so the binary rewards
the engine rather than the publisher. Only four descriptions are substantial, and
they are substantial because of VoID rather than `sd:`: `sparql.uniprot.org`
carries 1517 `void:propertyPartition` and 226 `void:classPartition`.

## Not answerable today, one cheap metric each

### 4. Get results in the format I need

| | |
|---|---|
| Needs | A content-negotiation probe: one request per format, or one with a multi-format `Accept` |
| Effort | Small. A new probe kind, or `AskFilter` with an `Accept` parameter |

`sd:resultFormat` lists nine formats across the 28 descriptions, but it is
Virtuoso's default list, and **RDFa appearing in it is the tell**. TSV is declared
by only 3 of 28. A consumer who needs CSV cannot trust the declaration, which is
the survey's lesson about declarations generally.

### 5. Use SPARQL 1.1 features

| | |
|---|---|
| Needs | One active probe, for example a `VALUES` clause or a subquery |
| Effort | Small. Fits the existing `AskFilter` kind with no new code |

**23 of 28 endpoints claim `sd:SPARQL10Query` only, while many of the same
endpoints demonstrably answer SPARQL 1.1 and GeoSPARQL.** The survey's explicit
advice is not to gate 1.1 tests on the declaration. This task turns that finding
into something a user can filter on.

## Proposed, but needs design before it can be built

### 6. Federate to it from my own store

Only 2 of 28 declare `sd:BasicFederatedQuery`, and the survey's first lesson is
that declarations are worthless for capability detection. So this needs an active
probe, and an active federation probe is awkward in a way the others are not: the
obvious shape asks **their** server to make an outbound request, which is a
different and larger imposition than answering a query, and could make us look
like we are using them as a relay.

A safer shape may be to test whether a `SERVICE` clause is *parsed* at all, using
a deliberately unreachable remote, and distinguish "SERVICE unsupported" from
"remote unreachable" by the error. That distinction may not be reliable across
engines. **Recommend deferring until the shape is settled**, rather than shipping
a probe whose politeness we cannot defend.

## Not a task, but the UI must show it

### Liveness

65 of 548 URLs answered a trivial query: **11.9%**. And the LOD Cloud's own
`status` field disagrees with reality in both directions, with 10 endpoints it
marked FAIL serving us a description and 54 it marked OK serving nothing.

This is the precondition for every task rather than a task itself, so the
recommendation is a **default filter** ("only show endpoints that answered")
rather than a front-door tile.

### A front-end that is not an endpoint

**114 of 548 URLs returned HTTP 200 with HTML**, a query console rather than a
protocol endpoint. All three endpoints in `REPORT.md` were of this kind, and the
real endpoint was at a different path in every case.

This is registry work (stage 1d resolves front-ends) surfacing in the UI as a
distinct state. It must not read as "broken": the service is often fine and the
URL is merely wrong.

## What this implies for the UI

Three tasks are buildable now against real measurements, and each maps to metrics
whose verdicts genuinely differ across endpoints. Two more become available for
one small metric each. One needs design.

The recommendation is to ship the front door with tasks 1 to 3, add 4 and 5 when
their metrics land, and leave a visible "browse everything" path for the user who
does not think in tasks. That path is also the honest default while the registry
holds three endpoints rather than 548.

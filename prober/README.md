# sparqlwatch prober

Probes public SPARQL endpoints and writes what it observed as RDF. One sweep
produces one immutable named graph of DQV quality measurements with PROV
provenance. That graph is the whole output: there is no database, no state
carried between runs, and no score.

## The rule the whole thing serves

**It must never report a confident wrong answer.** Each (endpoint, metric) pair
resolves to exactly one of six verdicts:

| Verdict | Meaning |
| --- | --- |
| `verified` | A probe confirms it works, and where a declaration is possible, the endpoint declares it |
| `undeclared-but-verified` | Works, and the endpoint could have declared it but did not (see the endpoint's `declarationsRead` fact for whether we could read its description) |
| `declared-but-wrong` | Answered, and answered incorrectly |
| `declared-only` | Claimed, not confirmable by probe |
| `absent` | Neither claimed nor observed |
| `indeterminate` | We never got to find out |

The declared/observed axis applies only where a declaration is possible, so
only a metric carrying a `declared_by` in `metrics.toml` can ever produce
`undeclared-but-verified`. `geo-functions` is the only one today. For every
other metric there is no term in the service-description vocabulary that could
advertise the capability (liveness, CORS headers, class counts), so "the
endpoint declares nothing" would say nothing about the endpoint, and a
confirmed probe reads simply `verified`.

`absent` may only be claimed when the evidence actually establishes absence:
the endpoint itself has to have answered the question we asked. For most
probes that means it answered with a 2xx status, in a form we could read. Two
probes have a further exception, in both cases because the status code *is* the
answer to their question rather than a fact about our request:

- the description fetch, where a `404` or `410` counts as absence, because
  those two statuses speak to what is published at the URL itself;
- the CORS preflight, where a `405` or a `501` counts as absence, because a
  browser's `fetch` requires the preflight to answer with an ok status, so
  those statuses are the endpoint saying it will not serve a cross-origin
  query. A redirect is not one of them: the probe resolves the chain and
  judges the response at the end of it, and a chain it could not resolve is
  `indeterminate`.

A timeout, an unreachable host, a 429, a gateway error, an HTML query console,
an unparseable body (anything else we never got to interpret) is
`indeterminate`. There is deliberately no composite score and no ranking, here
or downstream.

All judgement lives in one pure function, `resolve()` in `src/resolve.rs`. The
HTTP client returns evidence and no opinion; the emitter is a pure function of
its inputs, with no clock read and no randomness.

A service description is fetched once per endpoint with a queryless GET request
that asks for RDF (`text/turtle, application/rdf+xml;q=0.9, application/ld+json;q=0.8`),
and its declarations are compared against what the probes observe. Where a
declaration is possible, a confirmed capability usually reports as
`undeclared-but-verified`, because almost no endpoint declares its
capabilities: in the survey behind this project, 18 endpoints evaluate
`geof:sfWithin` and not one declares it. That is the finding this verdict
exists to publish, which is why metrics nothing could declare are kept out of
it.
The `service-description` metric now carries a graded level (0 to 4) rather than
always being indeterminate, grading by informativeness rather than presence.

## Running it

```sh
cargo run -- --at 2026-08-20T12:00:00Z --out run.nq
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--at` | *required* | The run instant, ISO-8601 with an explicit timezone |
| `--endpoints` | `endpoints.toml` | Endpoint list to sweep |
| `--metrics` | `metrics.toml` | Metric definitions to apply |
| `--out` | `run.nq` | Where to write the N-Quads |
| `--max-cost` | `cheap` | Only run metrics with cost at or below this value (`cheap` or `expensive`) |
| `--min-gap-ms` | `2000` | Minimum pause between two consecutive requests to one host |
| `--retry-after-cap-s` | `20` | Longest `Retry-After` waited out before one retry of a throttled request |
| `--concurrency` | `4` | How many HOSTS to probe at once; one host is never asked two things at once |

`--at` is required and is **not** read from the clock, deliberately. It names
the run graph, it is published as the activity's `prov:generatedAtTime`, and a
scheduled `CronJob` passes the scheduled instant, so a retry of a failed sweep
lands in the same graph rather than inventing a second one. That also makes a
run reproducible: same `--at`, same output identifiers. It is validated before
any probing starts, because it is interpolated into IRIs and published as an
`xsd:dateTime`.

Every outbound request passes a per-host gate that gives three guarantees: never
two requests in flight to one host, at least `--min-gap-ms` between one request
finishing and the next one to that host starting, and nothing at all to a host
before the instant that host asked us to come back at. The gate is taken
once per outbound **request**, not once per probe, and in one place only. No
probe follows a redirect implicitly: a chain is walked a hop at a time, and each
hop takes the gate for the host that hop actually touches, which is not
necessarily the host the probe was pointed at. A three-hop chain therefore costs
two gaps, and that is the honest price of a promise with no exceptions in it. A
chain longer than five hops, a cycle, or a `Location` we cannot resolve is
`indeterminate`: we never reached an answer.

`--concurrency` does not weaken any of the three guarantees, and it generalises
the first one only as far as the host an endpoint NAMES. Endpoints are grouped
by that host and each group is probed by one task, so two endpoints of one host
are never in flight together and never contend for its gate: raising the flag
adds hosts in flight, never requests to a host it was pointed at. The hop case
just described is the exception, because a redirect is gated on the host the hop
actually touches: an endpoint redirecting into another host of the same sweep
queues at that host's gate, which a sequential sweep never did because nothing
else was running. The gate itself still holds, one request in flight and one
gap; what it costs is that the wait is charged to the redirecting endpoint's
metric budget. See Known limitations.

The default of 4 is politeness rather than throughput. Four hosts in flight,
each of them spacing its own requests by 2 seconds, is roughly two requests per
second in aggregate, which is a defensible load for a service that probes
strangers uninvited. Zero is refused by the parser rather than repaired
downstream, because `Semaphore::new(0)` does not fail, it hangs, and a sweep
that probes nothing and reports nothing is the worst failure this crate has.

One endpoint per host in flight is not only politeness: it is what keeps
`--min-gap-ms`'s startup check the only budget relation a sweep needs. The
per-host guard is held until the request returns, so a second endpoint of the
same host would wait for the guard rather than for the gap alone. That wait is
gap plus the whole first request, and against a throttled host gap plus request
plus the honoured `Retry-After` plus the retry, the same 2 + 30 + 20 + 30 = 82
seconds the retry arithmetic below arrives at, inside a 60 second metric budget.
Grouping on the same `host_key` the gate acquires removes that hold term for the
host an endpoint names, which is why the check compares the gap against the
metric budget and nothing else. Relaxing per-host serialisation would mean
rewriting that check around the 82 second hold instead of around the gap, and
nobody has taken that decision. The one place the hold term survives is the
cross-host redirect priced under Known limitations.

The unit of all three guarantees is `host_key`'s answer, not the server itself.
Two spellings it keys apart (`a.example.` and `a.example`, or `http://x:443/`
and `https://x/`) are two groups, so with concurrency they can carry two request
streams to one server, and a `Retry-After` stand-down recorded under one key
does not defer the other. There is no budget consequence, because the grouping
key and the gate key are the same function and so nothing waits on anything; the
cost is politeness, and the fix is a registry that spells one server one way.

`--min-gap-ms` is validated at startup, before any probing: the gap plus the
30s request budget has to stay under the 60s metric budget, or the pause alone
consumes the budget the measurement needs and every metric reports
`indeterminate` against an endpoint that answered perfectly.

A `429` or `503` carrying a `Retry-After` in delta-seconds defers the **host**
until that instant, whether or not the delay is one we are willing to wait out:
a server that tells us to come back later is not sent a different question in
the meantime. Every later request to that host, from any metric, waits at the
gate until the instant passes. Within `--retry-after-cap-s` the request is also
retried **once** after the wait; a longer delay, an HTTP-date value or junk is
not retried at all, and the throttle is reported as observed rather than turned
into a guess. The retried walk takes the gate again, hop by hop, like a first
one, and it is that acquisition which waits the delay out, so there is exactly
one place in the crate that decides how long we wait.

The deferral is deliberately not bounded. A host that asks for an hour gets an
hour, the metric and endpoint budgets cancel the requests that queue behind it,
and those metrics report `indeterminate`, which is exactly what happened: we
never got to ask. A second bound here would be a second place deciding how long
we wait.

The cap's ceiling is arithmetic, not taste, and the arithmetic is about the
ordinary throttle rather than the worst case. Four things come out of one 60s
metric budget, in this order: the gap, the first request, the honoured wait, and
the retried request. A throttle usually comes back quickly, since refusing a
request is cheap for the server refusing it, so what has to fit is
`gap + cap + request budget < metric budget` (2 + 20 + 30 = 52 < 60). A larger
cap eats that margin, so raising it makes a cancelled retry **more** likely, not
less.

No cap value makes the worst case fit. A first request that runs its full 30s
timeout before the throttle arrives costs 2 + 30 + 30 = 62 > 60 even with a cap
of zero: a gap plus two full-timeout requests is already over the metric budget
on its own. A retry is therefore best-effort inside that budget. When it does
not fit, `tokio` cancels it and the metric reports `indeterminate`, which is
correct, because we never got an answer. A retry guaranteed to fit in every case
would need a metric budget above 82 seconds, and nobody has taken that
decision.

Three nested budgets bound the work, per request (30s), per metric (60s), and per
endpoint (600s), and every one of them cancels the future rather than
reporting afterwards that it took too long. A metric the budget never reached
is `indeterminate` and carries no `elapsedMs`, because nothing was measured.

## Sweep cost

At the default `--min-gap-ms` of 2000 and `--max-cost cheap`, a sweep's
wall-clock time is arithmetic. This is a rough estimate, not a measurement,
because no full registry sweep has been run yet.

Seven metrics are `cheap` at the default cost ceiling: availability, cors,
cors-preflight, geo-functions, geo-data, service-description, has-classes.

Per endpoint, the prober makes 7 requests. Between consecutive requests to one
host, there is a gap. Endpoints are grouped by host and `--concurrency` bounds
how many of those groups run at once, so concurrency buys parallelism **across
different hosts** and never within one: per-host concurrency stays at one
request, whatever the flag says. So, per host group:

- Per endpoint: 7 requests with some latency (call it L per request) plus 6 gaps.
  An endpoint that redirects costs one more gated request and one more gap per
  hop, since every hop is a request in its own right.
- Gap time per endpoint: 6 gaps × 2 seconds = 12 seconds.
- Request time per endpoint: 7 requests × L.
- With an average request latency of 300 ms (a rough middle ground for
  network round-trip), the per-endpoint floor is roughly (7 × 0.3) + 12 = 14.1
  seconds.
- For 548 endpoints at 14.1 seconds each: 548 × 14.1 = 7,726.8 seconds
  sequentially, or about 2 hours 9 minutes; at the default `--concurrency 4`,
  roughly a quarter of that, about 32 minutes.

Every number above is a **floor**, and the failure path is what actually prices
a sweep. A black-holed endpoint costs its whole 600-second endpoint budget
rather than 14.1 seconds, so the real bound is `sum(per-endpoint cost) /
concurrency`: 548 dead endpoints would be `548 × 600 / 4` = 22.8 hours, and at
a plausible 10% dead it is `(493 × 14.1 + 55 × 600) / 4` = 9,988 seconds, or
about 2 hours 46 minutes. That is the number to plan a scheduled job around,
not the healthy-endpoint floor.

It also assumes all endpoints are distinct hosts. Registry URLs that share a
host are one group and are probed one after another, so a host carrying several
endpoints costs the sum of them however high `--concurrency` is set. Request
latencies vary widely too; the 300 ms above is a middle estimate.

Measured, not estimated, on this repository's three-endpoint `endpoints.toml`
on 2026-08-24: 14.4 seconds at the default `--concurrency 4` and 38.7 seconds
at `--concurrency 1`. Three endpoints on three hosts is not a registry sweep,
so it says nothing about the 548-endpoint figures above; it is here because it
is the only concurrency measurement this project has actually taken.

## Configuration files

**`endpoints.toml`** is the list to sweep, one array of URLs. A URL that is not
a valid IRI is warned about and skipped at emission time, not fatal: the
registry is seeded from a real-world dump known to contain junk, and one bad
string must not discard a whole sweep's work.

The list is deduplicated at load, first-seen order preserved, with a warning
naming each entry dropped. One row per (endpoint, metric) and one
`declarationsRead` fact per endpoint held per list ENTRY, not per endpoint, so a
URL listed twice published two facts about one endpoint IRI in one run graph,
and two differing fetches made them contradict each other with nothing in the
graph to resolve it. Deduplicating at load also stops the sweep sending one
stranger's server two identical sets of requests. The comparison is on the
exact string: `http://x/sparql` and `http://x/sparql/` stay two entries, even
though the declaration scoper treats them as one service, because two spellings
in a registry are a registry problem to see rather than one to collapse
silently.

A URL whose authority carries a **non-empty userinfo** component is then
dropped, with a warning naming the host it was pointed at and never the URL,
because repeating the URL would copy the credential into the log the drop
exists to keep it out of. This happens after the deduplication and before
anything is swept. `http://alice:s3cret@a.example/sparql` is refused and so is
`ftp://alice:s3cret@a.example/sparql`: the authority is delimited on `//`, so
the check does not care about the scheme. An empty userinfo
(`http://@a.example/sparql`) carries no credential and is kept. A bare `@`
anywhere else in the URL is not touched at all, because
`http://a.example/sparql?contact=x@y.example` is a legitimate endpoint and a
`contains('@')` check would silently remove it from every sweep. This lives in
the registry rather than in the emitter because it changes what gets swept: the
endpoint string ends up inside the subject of every fact about it, in a per-run
graph that is never rewritten, so a credential admitted here would be published
permanently. What it does not cover is a credential in a query string; see
Known limitations.

**`metrics.toml`** is the metric definitions, as *data*. Each names a probe
kind from a closed set (`Liveness`, `Cors`, `CorsPreflight`, `AskFilter`,
`AskData`, `SelectIris`, `FetchWellKnown`) plus its parameters, so adding a metric that
fits an existing kind needs no Rust change. An unknown kind, an unrecognised
`cost` value, a bindings-reading kind with no `var`, a key the loader does not
recognise, and an `id` defined twice are each a loud load error rather than a
silent default. A metric that says nothing about `cost` is `cheap`, which is the
one silent default that remains, so state the cost explicitly.

An unrecognised key matters more than it sounds: before the loader refused them,
`cost_class = "expensive"` loaded as `cheap` and ran a planet-scale scan against
every endpoint in the registry. A duplicate `id` matters because the id is the
metric's published identity: two definitions under one id can land on opposite
sides of the cost ceiling and give the same (endpoint, metric) pair both a
verdict and a not-measured fact. Note the contrast with the endpoint registry
above, which drops a duplicate with a warning: that list is seeded from
real-world dumps whose repeats say the same thing, while `metrics.toml` is
written by hand and its repeats say different things, so there is nothing
honest to guess.

Every metric declares a cost of `cheap` or `expensive`, stating whether
probing it is inexpensive enough to run everywhere or should be opt-in (via `--max-cost expensive`).
The definitions are hashed into a `metricDefinitionRevision` recorded
on every run, a pure function of the definitions themselves, so a measurement
can be read against the definition that produced it.

`FetchWellKnown` probes by fetching the queryless GET described above. It returns
`Verified`, plus a level reflecting the description's informativeness, only when
a **2xx** response arrives under an RDF-specific media type (`text/turtle`,
`application/rdf+xml`, `application/ld+json`, `application/n-triples`,
`application/trig`, `application/n-quads`) and parses to **at least one triple**.
All three conditions are load-bearing. `RdfFormat::from_media_type` also accepts
the generic `text/plain`, `application/json` and `application/xml`, under which
an empty throttle body, a `{"error":"boom"}` page and a SPARQL-results document
all parse cleanly, so a generic media type is not a positive identification of
RDF and neither is a zero-triple parse. A genuine RDF/XML document served as bare
`application/xml` therefore reports `indeterminate`, which is honest, rather than
a confident verdict. The media type is read through one shared function
(`src/media.rs`), which strips the header's parameters (`; charset=utf-8`)
before matching, because that decision has to be identical here and in the
declaration parser: when there were two copies they drifted, and an RDF/XML
description served with a charset parameter classified as RDF, then got
reparsed as Turtle, publishing `verified` with level 0 (which means "none
served"), `declarationsRead false` for a document we had read, and losing every
declaration in it. A `404` or `410` response returns `Absent` with level 0. Any
other status is `Indeterminate`, recording that the request failed rather than
that the description is absent.

The level ladder follows the design doc: 0 none served, 1 a stub, 2 names a
default dataset or graphs, 3 carries VoID class or property partitions, 4
declares an entailment regime, example resources, or extension functions. It is
monotonic: a description that declares more never grades lower.

## Cost ceilings and not measured

When a metric is `expensive`, a sweep with `--max-cost cheap` (the default) skips
it. That skip is published as a fact, not as an omission. Per declined (endpoint, metric)
the run emits a resource of type `urn:sparqlwatch:NotMeasured`, with:

- `urn:sparqlwatch:notMeasuredOn` pointing to the endpoint
- `urn:sparqlwatch:notMeasuredMetric` pointing to the metric definition
- `urn:sparqlwatch:notMeasuredReason` set to `"cost-ceiling"`
- `prov:wasGeneratedBy` pointing to the run's activity, the same link every
  measurement carries, so the fact reaches the `urn:sparqlwatch:maxCost` that
  explains it without anyone having to assume the `activity:{at}` naming
  convention
- **no** `dqv:value`
- **no** level

Those first two are sparqlwatch's own predicates on purpose, and no DQV or Data
Cube predicate appears on the fact at all. `dqv:computedOn` is declared with
domain `dqv:QualityMeasurement` and `dqv:isMeasurementOf` with domain
`qb:Observation`, so either one here would entail, under plain RDFS, that a
quality measurement exists for a pair we deliberately did not measure: a
consumer materialising domains would read a value-less measurement rather than a
declined one. Reusing a predicate whose declared domain is a class you are not is
an assertion, not a convenience. Sparqlwatch declares no domain and no range for
its own two, because an undeclared predicate entails nothing, and endpoint and
metric stay just as joinable.

This is not a seventh verdict. The verdict vocabulary stays at six: this fact says a
measurement did not happen, while a verdict says what was learned about a capability.
Someone querying for verdicts will never encounter a value that is not one of the six.
Someone asking why a verdict is missing gets an answer: either it was declined, or the
budget was exhausted. A second reason, `prober-failed`, says the prober itself
never got to ask: the task probing that endpoint's host did not return, so
there was no observation at all rather than an inconclusive one, and every
metric on that endpoint carries the fact rather than a verdict.

The run's PROV activity records three things about the run itself:
`urn:sparqlwatch:maxCost` naming the ceiling used,
`urn:sparqlwatch:concurrency` naming how many hosts were probed at once, and
`urn:sparqlwatch:failedEndpoints` counting the endpoints the prober failed on.
The last two are `xsd:integer`.

What a consumer may conclude from those two is narrow, and worth stating,
because both invite more. `concurrency` does **not** change what `elapsedMs`
measures: each hop's timer starts after the per-host gate has been acquired and
stops when the response returns, and a redirect chain publishes the sum of its
hops' own durations rather than the wall clock of the walk, precisely so our own
politeness is never published as somebody's response time. What it does explain
is the run's wall-clock duration, and the one case where another host's gate can
sit inside a metric budget: a run at `--concurrency 1` cannot have lost a metric
to the cross-host redirect described under Known limitations, because no other
group was running, and a run at 4 can. `failedEndpoints` counts the endpoints
this run observed nothing at all about. It is published on every run, including
the ordinary `0`, so that an absent quad means "emitted before this fact
existed" rather than "nothing failed". A non-zero value means that many
endpoints carry `prober-failed` facts in place of verdicts and carry no
`declarationsRead` fact either, so a consumer expecting one row per (endpoint,
metric) has to read this rather than assume it. Such a run also exits non-zero,
after writing its output, so the run is preserved and the scheduler still learns
it was incomplete.

### How a run is written, and what a truncated one says

A run is emitted as three kinds of section: a header of run-level facts, one
self-contained chunk per endpoint, and a footer. Each section ends with its own
terminator, `urn:sparqlwatch:emission "incremental"` for the header,
`urn:sparqlwatch:completedEndpoint <endpoint>` for a chunk and
`urn:sparqlwatch:finalised "true"^^xsd:boolean` for the footer, all three on the
run's activity. N-Quads has no prologue and every line ends in a newline, so any
prefix of the file parses, which means a crash leaves a readable file whose only
risk is that its lines contradict each other. The terminators are what remove
that risk: a reader that holds a section's terminator holds the whole section,
and a reader that does not may drop the fragment.

A consumer reads three cases off facts that were each true when they were
written. `emission` with `finalised` is a complete run. `emission` without
`finalised` is a run that did not finish, and its `failedEndpoints` count says
nothing, because that count summarises chunks that were never written. Neither
one is a run emitted before this scheme existed, which promised nothing either
way. `finalised` is a boolean rather than `prov:endedAtTime` because nothing in
the prober can produce that instant soundly: `emit` reads no clock by design,
`std` cannot format a `SystemTime` as `xsd:dateTime`, no date library is in the
lock file, and a flag supplied at launch would publish a predicted future into a
graph that is never rewritten.

`completedEndpoint` is per endpoint and not per run, so "did this run reach this
endpoint" is a fact rather than an inference from absence. It is also the reason
two ordering rules hold inside the file: every chunk types its own endpoint
with `dcat:DataService` rather than relying on an earlier chunk having done it,
and no fact family publishes its own summary before the things it summarises.
The second is why `sampleSize` and `sampleTruncated` come after the last
`sampledValue`, and why `failedEndpoints` sits in the footer. A cut inside a
sample's values then loses the sample, which every consumer already handles,
instead of leaving `sampleSize 200, sampleTruncated false` standing beside three
values, which a page would render as two hundred classes sampled, complete.

The labels in that file state only what was actually measured. `geo-data` and
`has-classes` and `classes` query the default graph AND every named graph, via a `UNION` with a
`GRAPH ?anyg { ... }` branch, so an endpoint holding everything in named
graphs is not reported as holding nothing. The graph variable is never the
metric's own result variable: `GRAPH ?g { ?s geo:asWKT ?g }` would join the
graph name against the geometry literal, match nothing, and publish a silent
false `absent`, which is exactly the failure this widening exists to remove.
What the suite checks about those queries is structural only; see the
named-graph entry under Known limitations for what that does and does not
establish.

The two CORS metrics are deliberately separate facts, and neither subsumes the
other. `cors` observes an `access-control-allow-origin` header on a **simple
GET**: what a `curl` user sees. `cors-preflight` sends the `OPTIONS` preflight a
browser sends before a cross-origin query (`Origin`,
`Access-Control-Request-Method: GET`, `Access-Control-Request-Headers:
content-type`) and is what decides whether an embedded query editor can talk to
the endpoint at all. An endpoint that sets the header on GET and refuses
`OPTIONS` is common, and it reports `verified` on the first and `absent` on the
second, which is the honest pair of answers, and the shape
`the_two_cors_metrics_are_not_the_same_probe` pins end to end. The preflight
probe never follows a redirect implicitly: a `303` would rewrite the `OPTIONS`
into a `GET` and hand back exactly the simple-GET header we already have,
publishing a grant for that endpoint. It resolves the chain deliberately
instead, re-issuing the same `OPTIONS` at each `Location` for up to 5 hops, and
draws the verdict from the response at the end. A chain with no usable
`Location`, a cycle, or more hops than that is `indeterminate`: we never
reached a preflight answer. Minting a redirect itself as `absent` published
"does not answer a browser preflight" for services that answer one one hop
away, contradicting the `cors` row in the same run, which had followed the very
same redirect. `verified` on `cors-preflight` requires a 2xx
whose `access-control-allow-origin` is `*` or our own origin
(`https://sparqlwatch.dev.k8s.semanticscience.org`, the same host the
`User-Agent` names) and whose `access-control-allow-methods`, if it sends one,
lists GET. A header naming somebody else's origin is a grant to somebody else.

The two class metrics (`has-classes` and `classes`) are also deliberately separate,
for a reason worth recording: they answer different questions at vastly different costs.
On qlever.dev/api/osm-planet (planet-scale OpenStreetMap), `SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 200`
timed out past 45 seconds, while `SELECT ?c WHERE { ?s a ?c } LIMIT 1` answered in 0.166 seconds.
The cost is `DISTINCT` scanning every class name in the endpoint, not the named-graph
`UNION`. So `has-classes` (cheap, "this endpoint holds typed resources") runs everywhere,
while `classes` (expensive, "here are up to 200 distinct resource types") is opt-in.
`has-classes` uses probe kind `SelectIris`, never `AskData`, and this choice matters:
`AskData` extracts bindings with a literal guard, and since `?c` in `?s a ?c` binds an IRI,
the guard would find no literal and the metric would publish `absent` for an endpoint full of
typed resources. `SelectIris` has no such guard and returns `verified` when any type is found.

## Content samples

`classes` already ran `SELECT DISTINCT ?c ... LIMIT 200` to answer "does this
endpoint publish a bounded list of types"; it now publishes the bindings it
reads instead of discarding them once the verdict is drawn. A metric that
uses probe kind `SelectIris` may declare `sample_limit` in `metrics.toml`,
checked at load against the `LIMIT` its own query actually carries (SPARQL
comments stripped first, so a comment mentioning a different number cannot
stand in for the real one), so the two numbers cannot drift apart. `classes` is
the only metric that declares one, at 200, matching its query's `LIMIT 200`.
`has-classes` runs `SELECT ?c WHERE { ?s a ?c } LIMIT 1` and deliberately
declares none: its single binding is whichever type the endpoint happened to
return first, and publishing that as a "sample" would suggest it says something
about the endpoint's vocabulary, when it says only that at least one typed
resource exists.

`AskData` may not declare a `sample_limit` either, even though it does read
bindings: it reads the lexical forms of literals, and every sampled value is
published as an IRI, so an `AskData` sample would drop most values and
republish any whose lexical form happens to parse as an IRI as a resource the
endpoint never mentioned, losing the datatype either way. Lifting that
restriction needs a sample that carries, per value, whether it is an IRI or a
literal with its datatype, and an emitter that emits accordingly. Until both
exist the path stays closed, because a half-supported path publishes a wrong
fact about somebody's data.

Per sample, the run graph carries only sparqlwatch's own predicates plus
`rdf:type` and `prov:wasGeneratedBy`:

- `rdf:type urn:sparqlwatch:ContentSample`
- `urn:sparqlwatch:sampledFrom` the endpoint
- `urn:sparqlwatch:sampledBy` the metric definition
- `urn:sparqlwatch:sampledValue`, one per IRI published, repeated
- `urn:sparqlwatch:sampleTruncated` an `xsd:boolean`
- `urn:sparqlwatch:sampleSize` the count of values published, an `xsd:integer`
- `prov:wasGeneratedBy` the run's activity, the same link every other fact in
  this graph carries

Written in that order, with the two summarising quads after the values they
describe, so that a file cut inside the value list loses the sample rather than
misstating its size. See How a run is written above.

Values are published in the order the endpoint returned them: not sorted, not
deduplicated beyond what `SELECT DISTINCT` already did, because reordering
would discard evidence about the endpoint for a tidiness nobody asked for.

`sampleSize` counts the values actually published, so it is always verifiable
against the `sampledValue` quads beside it. A value that cannot be written as
an IRI is dropped and not counted, and the drop is logged with the bound and
published counts, because the graph has no way to say "there was one more and
we could not name it". A sample whose values are all unwritable publishes no
node at all, since `sampleSize 0` would read as "this endpoint has no classes".
Against the three endpoints in this project's own registry, nothing was dropped
(kadaster 59 of 59, ontop 50 of 50, qlever no sample).

A sample is published only where the measurement for that same metric came out
positive, `verified` or `undeclared-but-verified`. This is gated on the verdict
rather than on the response status deliberately: `resolve()` already encodes
every status rule this project has, so the sample and the measurement cannot
disagree, and a change to those rules carries the sample with it. Without that
gate, a `429` or a `503` carrying a parseable SPARQL-results body published the
endpoint's full class list, marked complete, in the same graph whose
measurement for that metric read `indeterminate`.

`sampleTruncated` is `true` when the number of values reached the metric's
declared `sample_limit`, using `>=` rather than `==`: an endpoint that ignores
its own `LIMIT` and returns more than asked is still not called complete,
because the query still bounded what we could see, not because the count came
out exactly right. A list a reader believes is complete when it is not is the
content equivalent of a confident wrong answer, so this is published rather
than left for a consumer to infer from the count alone.

Measured with `--max-cost expensive` against this project's own registry:
`data.kkg.kadaster.nl` returned 59 distinct classes, not truncated, mostly the
schema vocabulary its data is built from (`owl:Class`, `owl:Restriction`,
`rdfs:Class`, `rdf:Property`). `ontop.certain.ai.ustp.at` returned 50, not
truncated, real domain vocabulary from its own namespace
(`https://w3id.org/aidoc-ap#AISystemCapability`,
`https://w3id.org/aidoc-ap#ComputationalResource`, and others). `qlever.dev/api/osm-planet`
produced no sample at all: its class enumeration exceeds the 30s request
budget, so `classes` there is `indeterminate`, there are no bindings to
publish, and correctly nothing is published as an empty sample either. The
expensive sweep of these three endpoints took 1m18s, against roughly 41s at
the default cost ceiling.

A content sample is deliberately **not** a dataset description, and this
project deliberately does **not** publish VoID from it. What a sample holds is
an observation from one bounded query, not a description of a dataset: the
thing behind a SPARQL endpoint may be several datasets, or a virtual graph over
a relational store rather than a dataset at all (`ontop`, in this project's own
registry, is exactly that). `void:classPartition` and `void:class` both carry
`rdfs:domain void:Dataset`, so reusing either predicate here would entail,
under plain RDFS, that a content sample IS a dataset, which is not something
this project can honestly assert about an arbitrary endpoint. This project has
already shipped that class of defect once: the `NotMeasured` fact originally
reused `dqv:computedOn`, entailing that 548 deliberately declined pairs were
quality measurements that never happened, and nothing was visibly wrong with
it until a consumer ran inference. `ContentSample` uses sparqlwatch's own
predicates for exactly that reason.

Samples appear only under `--max-cost expensive`, because `classes` is the
only metric that produces one and it is `expensive`. A default sweep therefore
publishes no samples at all. That absence is not silence: the `NotMeasured`
fact already published for `classes` under the default ceiling (see [Cost
ceilings and not measured](#cost-ceilings-and-not-measured) above) is what
tells a reader "we did not look" rather than "there is nothing there".

## Fact identity, and why order is not part of it

Two properties are easy to confuse here, and they do not have the same
standing. Identity is a property of the graph. Order is a property of one
emitted file.

**Identity.** Every run-scoped fact's subject is a pure function of (run,
endpoint, metric):

```
urn:sparqlwatch:<kind>:<run>:<percent-encoded endpoint>:<metric id>
```

`<kind>` is `measurement`, `not-measured` or `content-sample`, the three fact
families scoped to a run, and it is in the IRI so that a measurement and a
not-measured fact about one pair can never land on one node that both has and
has not a verdict. `<run>` is the `--at` instant verbatim. The endpoint is
percent-encoded keeping RFC 3986's unreserved set (`ALPHA / DIGIT / "-" / "." /
"_" / "~"`) over UTF-8 bytes with uppercase hex, so it carries no `:` of its
own, and the metric id is checked against `[a-z0-9][a-z0-9-]*` both by the
metrics loader and again by the subject builder, so it carries none either.
Those two facts together are what make the mapping injective. One real subject,
from this repository's own fixtures:

```
urn:sparqlwatch:measurement:2026-08-22T20:00:00Z:https%3A%2F%2Fqlever.dev%2Fapi%2Fosm-planet:availability
```

Nothing positional is left in it. Reordering `endpoints.toml` renames no
subject, two runs at the same `--at` name the same nodes, and one endpoint's
facts can be written on their own, which is what stage 1c-b4 needs. No
normalisation happens on the way in: `registry::dedupe` treats two spellings of
one endpoint as two entries, so they are two subjects here too, and the endpoint
a subject encodes is the string the registry actually contains. The fourth
published fact, `declarationsRead`, is deliberately outside this scheme: its
subject is the endpoint IRI itself, because it says something about the endpoint
rather than about an (endpoint, metric) pair.

**A subject must never be parsed.** Two reasons, and the first alone is
sufficient.

- Every fact already carries what it is about, as triples. A measurement has
  `dqv:computedOn` and `dqv:isMeasurementOf`; a content sample has
  `sw:sampledFrom` and `sw:sampledBy`; a not-measured fact has
  `sw:notMeasuredOn` and `sw:notMeasuredMetric`. Joining on those is always
  available, and it is the only access path this project supports.
- A store holds every run ever loaded, and this scheme has already changed once.
  Before this branch the tail of a subject was the row's index in the emitted
  file (`urn:sparqlwatch:measurement:<run>:0`). Those runs are still valid
  history, a run graph is never rewritten, and history is kept, so a store holds
  **two subject shapes** for as long as it holds history. A consumer that splits
  on `:` reads one of them wrong. It would also read the new one wrong, since the
  run segment is an unencoded `xsd:dateTime` and carries colons of its own.

**Order.** A file is the header, then one chunk per endpoint, then the footer.
`emit_nquads` derives the endpoint sequence from the union of all four fact
lists in first-appearance order, because an endpoint can appear in one list
only: a prober-failed endpoint is in the not-measured list alone, and an
endpoint whose description was read but whose metrics produced no row is in the
`declarationsRead` list alone. For a run assembled by `assemble`, whose lists
are already grouped by endpoint, that reproduces input order, never completion
order: each endpoint keeps its input index as a slot and `assemble` walks the
slots afterwards. Within one chunk the order is measurements, then not-measured
facts, then content samples, then the `declarationsRead` fact, then the chunk's
terminator. Within one family it is the order of the definition list the facts
came from, which is not `metrics.toml` order in general: `within_cost`
partitions that file into the metrics that run and the metrics the ceiling
declines, and `assemble` writes an endpoint's not-measured facts as the metrics
that would have run and then the metrics the ceiling declined, so a cheap metric
listed after an expensive one comes out first. That is worth having for diffing
two files by eye, and it is all it is worth. Beyond the section terminators and
the two rules above, it is **not** a property of the data:

- N-Quads serialises a set. No consumer may read meaning from the order of lines
  in one.
- `web/load_run.py` parses the file and inserts the quads into Oxigraph, which is
  order-blind, so the order is gone before any query sees it.
- Writing each endpoint's chunk as it completes will make the chunk sequence
  completion order rather than input order, which is the whole point of writing
  incrementally, and nothing downstream loses anything when it happens. What a
  consumer may read from the order is only what the terminators say, and those
  say it as facts rather than as position.

**Output is not byte-identical between two runs of one `--at`.** `emit_nquads`
reads no clock, no environment and no global, so it is a pure function of its
inputs. Its inputs are not: `elapsedMs` comes from an `Instant::now()` taken
around each request, so two runs of one endpoint differ in it, and a verdict can
differ too because the endpoint can. Do not write a test that diffs two runs'
bytes. Compare subjects, verdicts and levels.

## Proxy environment

The deployment target has no direct egress, so a proxy is mandatory there:

```sh
export HTTP_PROXY=http://egress-proxy.platform.svc.cluster.local:3128
export HTTPS_PROXY=$HTTP_PROXY
export http_proxy=$HTTP_PROXY
export https_proxy=$HTTP_PROXY
export NO_PROXY=localhost,127.0.0.1,.svc,.cluster.local
```

`reqwest`'s `system-proxy` feature is load-bearing rather than a convenience:
without a proxy every endpoint would fail identically, which looks exactly like
a dead registry.

**Correction to the spec's stage-0 findings.** The finding that "uppercase
`HTTP_PROXY` is ignored for `http://` URLs" is **curl-specific**: curl ignores
the uppercase form there because of a CGI variable collision. This client uses
reqwest, and hyper-util resolves the proxy with
`get_first_env(&["HTTP_PROXY", "http_proxy"])` (`matcher.rs:232`, hyper-util
0.1.20), reading both cases with uppercase first, so that constraint does not
apply to this code. Setting both cases, as above, is harmless
belt-and-braces and worth keeping for any sidecar or shell tooling that does
follow curl's rule, but the spec's note should not be read as binding here.

Two caveats on that correction, neither of which weakens it:

- `get_first_env` decides presence with `std::env::var(name).is_ok()`, so a
  correctly-set lowercase `http_proxy` is silently shadowed by an uppercase
  `HTTP_PROXY` that is merely set to an empty string. That is exactly the
  registry-wide silent-failure shape the spec's own stage-0 finding warns
  about, and the four-line export block above is precisely what a templated
  Helm values file could leave empty for one case while filling in the
  other.
- hyper-util disables environment-variable proxying entirely, uppercase and
  lowercase both, when `REQUEST_METHOD` is set (`matcher.rs:230`, with the
  early return at `matcher.rs:305`). The CGI collision curl guards against
  therefore exists here too, in a stronger form: it drops the proxy outright
  rather than merely picking the wrong case. "That constraint does not apply
  to this code" above is true only for the uppercase-versus-lowercase
  question, not for the CGI collision itself.

## Known limitations

The following are deferred deliberately, not oversights:

- The fetch is **unconditional**: an endpoint pays one queryless GET even if no
  configured metric actually needs the result, because the probe kind doesn't know
  which metrics use it. This is a small cost traded for simpler logic.

- A description larger than the 256 KiB body cap is never graded: it reports
  `indeterminate`, because we did not read it. Classification and the
  declaration join now read the same truncated bytes, which is what makes that
  answer coherent. They used not to: classification read the whole body while
  the join read the truncated one, so a description whose only triples sat past
  the cut classified as `Rdf` (licensing `verified`) and then graded `Level(0)`,
  which means "none served". That row asserted both that a description is
  published and that it says nothing, and its level was indistinguishable from
  an `absent` row's.

- One cost of that cap remains, in the conservative direction. Declarations are
  read from the same truncated body, so a declaration sitting past the cut is
  lost, and a metric that should read `verified` reports
  `undeclared-but-verified` instead. Measured with a 300 KiB Turtle description
  whose `sd:extensionFunction geof:sfWithin` sits past 256 KiB, `geo-functions`
  reported `undeclared-but-verified`. That understates a real endpoint rather
  than asserting something false about it, which is the direction this project
  errs in on purpose. Whether to raise the cap, stream the parse, or leave it is
  stage 1c's call; real descriptions are typically hundreds of bytes, not
  hundreds of kilobytes.

- The **named-graph half of the `geo-data` and `classes` queries is unverified
  by execution.** The suite contains no SPARQL engine, so what it can check
  about those queries is structural: `the_content_metrics_reach_named_graphs_without_colliding_variables`
  asserts that each query has exactly one `GRAPH ?g { ... }` branch, that the
  graph variable is not the metric's result variable, and that the block binds
  that result variable, so a branch incapable of contributing a row fails the
  test. That is correctness by reading, not by execution. `endpoints.toml` holds
  no endpoint known to keep its data in named graphs (the live test's
  `data.kkg.kadaster.nl` answers both branches from its default graph, so it
  would pass with the `GRAPH` branch deleted), so nothing here demonstrates that
  a partitioned endpoint is actually reached. Closing that gap needs an endpoint
  that holds its data that way. It becomes testable at stage 1d, where the
  registry is seeded from 548 real endpoints, some of which are certainly
  partitioned.

- **An API key in an endpoint's query string is published, permanently.** Every
  run-scoped fact's subject is `urn:sparqlwatch:<kind>:<run>:<percent-encoded
  endpoint>:<metric>`, so the endpoint URL is a reversible part of the identifier
  of everything we say about it, and the `declarationsRead` fact names the
  endpoint IRI itself. All of it sits in a per-run named graph that is immutable
  and append-only: a published identifier can never be corrected. `registry.rs`
  refuses a URL whose authority carries a non-empty userinfo component, whatever
  the scheme, so both `http://user:secret@host/sparql` and
  `ftp://user:secret@host/sparql` are dropped at load. It does nothing about
  `https://host/sparql?apikey=...`, because a query parameter's meaning is the
  operator's, not ours, and a name-based blocklist (`key`, `token`, `apikey`, ...)
  would be a confident wrong answer in both directions: it would drop legitimate
  endpoints whose query carries a dataset selector, and admit a credential under
  a name nobody guessed. Closing it properly needs the registry to distinguish a
  public URL from a credentialed one, which is a stage 1d question about how the
  list is seeded. Until then: do not put a secret in `endpoints.toml`.

- **A cross-host redirect can wait at another endpoint's gate, and that wait is
  charged to the metric budget.** `--concurrency` groups endpoints by the host
  they name, but a redirect is gated on the host each hop actually touches (see
  `gated_hop`, and the test `a_probe_redirected_to_another_host_gates_the_new_host`),
  so an endpoint that redirects into another host being swept at the same moment
  queues at that host's gate. The review of stage 1c-b3 verified it with mocks:
  two endpoints at `--concurrency 2`, the first redirecting to the second's
  host, produced arrivals `a, b, b, a, b, b` with consecutive arrivals at the
  shared host 456 ms apart against a `delay + gap` of 450 ms, so one guard was
  serving two groups. At production values that wait is up to
  `gap + request` = 32 s, or `2 + 30 + 20 + 30` = 82 s if the shared host is
  throttled, inside the 60 s metric budget; a cancelled metric budget is silent,
  so the endpoint would read `indeterminate` with nothing saying why. Grouping
  cannot close it, because the redirect target is only knowable by following the
  redirect, and following one without the gate is exactly what the per-hop gate
  exists to prevent. The common case is unaffected: `host_key` ignores the
  scheme, so an `http` to `https` redirect on one host stays inside one group and
  costs another gap and nothing else. How often the cross-host case arises in a
  real registry is **unquantified**. Of the three endpoints in the shipped
  `endpoints.toml`, measured on 2026-08-24, none redirects at all: a queryless
  GET returns 404, 200 and 500 respectively, with no `Location`. So the shipped
  list does not exercise this, and 1d's 548-endpoint registry is where it would
  first be measurable.

- **The run's output is still written once, at the end.** The design's
  per-endpoint isolation rule has two halves and this branch delivers the first
  only. No endpoint's slowness delays another endpoint's PROBING any more, since
  hosts are grouped and probed in separate tasks. "Results are written per
  endpoint as they complete" is **not** met: `run_sweep` joins every task before
  it returns, `assemble` builds the four fact lists from all the slots at once,
  and `main` calls `emit_nquads` and `std::fs::write` once, afterwards. So one
  endpoint burning its whole 600-second budget still delays the run's output by
  up to 600 seconds, and a crash at endpoint 500 of 548 leaves no file at all.
  Concurrency divided the constant; it did not change the shape. Crash-safe
  incremental writing is stage 1c-b4's, and the derived subjects above are what
  it rests on: an endpoint's facts can be written alone only because no
  identifier in them depends on how many endpoints came before.

## Tests

```sh
cargo test                                  # unit + integration, all offline
cargo test --test live_smoke -- --ignored   # hits real third-party endpoints
cargo clippy --all-targets -- -D warnings
```

Everything but `live_smoke` runs against a local `wiremock` server, so the
suite is deterministic and CI never touches a stranger's endpoint. Rust 1.96,
edition 2021, no nightly features.

## Looking at a run

The web tier is stage 3 and does not exist yet. Until it does, render a run as a
standalone local page:

```
cargo run -q -- --at 2026-08-20T12:00:00Z --out run.nq
node ../tools/render-run.mjs run.nq run.html
```

It is a read-only viewer over the emitted N-Quads, using the same verdict encoding
as the design: dashed borders mark "works but not declared" and "indeterminate",
and `absent` has no border at all, because it is the only verdict that claims a
negative. The real web tier will query Oxigraph rather than parse a file.

A run containing content samples (that is, one from an `--max-cost expensive`
sweep) also gets a "Content samples" panel, one detail block per (endpoint,
metric) sample: the endpoint, the metric, the value count, and whether the
list is truncated, with the truncation state carried by its own badge rather
than left to be inferred from the count. The value list itself sits behind a
disclosure toggle rather than always on screen, since 59 or more IRIs is too
much to put in a table row.

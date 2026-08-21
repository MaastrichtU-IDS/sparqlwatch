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

`--at` is required and is **not** read from the clock, deliberately. It names
the run graph, it is published as the activity's `prov:generatedAtTime`, and a
scheduled `CronJob` passes the scheduled instant, so a retry of a failed sweep
lands in the same graph rather than inventing a second one. That also makes a
run reproducible: same `--at`, same output identifiers. It is validated before
any probing starts, because it is interpolated into IRIs and published as an
`xsd:dateTime`.

Three nested budgets bound the work, per request (30s), per metric (60s), and per
endpoint (600s), and every one of them cancels the future rather than
reporting afterwards that it took too long. A metric the budget never reached
is `indeterminate` and carries no `elapsedMs`, because nothing was measured.

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

**`metrics.toml`** is the metric definitions, as *data*. Each names a probe
kind from a closed set (`Liveness`, `Cors`, `CorsPreflight`, `AskFilter`,
`AskData`, `SelectIris`, `FetchWellKnown`) plus its parameters, so adding a metric that
fits an existing kind needs no Rust change. An unknown kind, a
bindings-reading kind with no `var`, or a metric with no `cost` field is a loud load error rather than a silent
default. Every metric declares a cost of `cheap` or `expensive`, stating whether
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

- `dqv:computedOn` pointing to the endpoint
- `dqv:isMeasurementOf` pointing to the metric definition
- `urn:sparqlwatch:notMeasuredReason` set to `"cost-ceiling"`
- **no** `dqv:value`
- **no** level

This is not a seventh verdict. The verdict vocabulary stays at six: this fact says a
measurement did not happen, while a verdict says what was learned about a capability.
Someone querying for verdicts will never encounter a value that is not one of the six.
Someone asking why a verdict is missing gets an answer: either it was declined, or the
budget was exhausted. The run's PROV activity also records `urn:sparqlwatch:maxCost`
naming the ceiling used.

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

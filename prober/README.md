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
| `verified` | A probe confirms it works, and the endpoint declares it |
| `undeclared-but-verified` | Works, but the endpoint advertises nothing |
| `declared-but-wrong` | Answered, and answered incorrectly |
| `declared-only` | Claimed, not confirmable by probe |
| `absent` | Neither claimed nor observed |
| `indeterminate` | We never got to find out |

`absent` may only be claimed when the evidence actually establishes absence.
For every probe except the description fetch, that means the endpoint itself
answered, with a 2xx status, in a form we could read. The description fetch
has one further exception: a `404` or `410` also counts as absence, because
those two statuses speak to what is published at the URL itself, not to our
request or the server's general health. A timeout, an unreachable host, a
429, a gateway error, an HTML query console, an unparseable body (anything
else we never got to interpret) is `indeterminate`. There is deliberately no
composite score and no ranking, here or downstream.

All judgement lives in one pure function, `resolve()` in `src/resolve.rs`. The
HTTP client returns evidence and no opinion; the emitter is a pure function of
its inputs, with no clock read and no randomness.

A service description is fetched once per endpoint with a queryless GET request
that asks for RDF (`text/turtle, application/rdf+xml;q=0.9, application/ld+json;q=0.8`),
and its declarations are compared against what the probes observe. Most confirmed
capabilities report as `undeclared-but-verified`, because almost no endpoint declares
its capabilities. `Verified` is reachable but will stay rare by design: in the survey
behind this project, 18 endpoints evaluate `geof:sfWithin` and not one declares it.
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

**`metrics.toml`** is the metric definitions, as *data*. Each names a probe
kind from a closed set (`Liveness`, `Cors`, `AskFilter`, `AskData`,
`SelectIris`, `FetchWellKnown`) plus its parameters, so adding a metric that
fits an existing kind needs no Rust change. An unknown kind, or a
bindings-reading kind with no `var`, is a loud load error rather than a silent
default. The definitions are hashed into a `metricDefinitionRevision` recorded
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
a confident verdict. A `404` or `410` response returns `Absent` with level 0. Any
other status is `Indeterminate`, recording that the request failed rather than
that the description is absent.

The level ladder follows the design doc: 0 none served, 1 a stub, 2 names a
default dataset or graphs, 3 carries VoID class or property partitions, 4
declares an entailment regime, example resources, or extension functions. It is
monotonic: a description that declares more never grades lower.

The labels in that file state only what was actually measured. Two of them are
narrower than they look: `geo-data` and `classes` query only the **default
graph**, so an endpoint holding everything in named graphs answers empty; and
`cors` observes an `access-control-allow-origin` header on a **simple GET**,
which is weaker than the preflighted request a browser editor makes. Probing
`GRAPH ?g` and an `OPTIONS` preflight are the real checks and are deferred.

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

- A **failed** description fetch and a description that **genuinely declares
  nothing** currently produce the same result, because the "declared" flag is a
  simple boolean with no way to express "unknown". An endpoint whose description
  times out is therefore credited as undeclared rather than unknown. This requires
  a three-state value in the resolver, deferred to stage 1c.

- The fetch is **unconditional**: an endpoint pays one queryless GET even if no
  configured metric actually needs the result, because the probe kind doesn't know
  which metrics use it. This is a small cost traded for simpler logic.

- Declarations are collected **graph-wide**, with no scoping to the service
  actually being probed. A document describing two co-hosted services can
  therefore credit endpoint A with endpoint B's `sd:extensionFunction`, turning
  an `undeclared-but-verified` into a `verified` that endpoint never earned.
  Fixing it needs graph traversal (match the service node by `sd:endpoint`, then
  follow `sd:defaultDataset` for the VoID partitions), deferred to stage 1c and
  **before any real registry sweep**.

- The declaration join reads a body truncated at 256 KiB while classification
  (`body_kind`, deciding `Rdf` vs. `Other`) reads the whole body. Those two
  reads can disagree once a description crosses the cut, and the cost is
  worse than a grading error alone: measured with a 300 KiB Turtle
  description whose `sd:extensionFunction geof:sfWithin` and
  `sd:defaultEntailmentRegime` both sit past 256 KiB, the `geo-functions`
  metric silently lost its declaration and reported
  `undeclared-but-verified` instead of `verified`, and `service-description`
  graded `verified level=1` rather than level 4, a three-level miss, not a
  one-level one. This is acceptable in practice today: real descriptions are
  typically hundreds of bytes, not hundreds of kilobytes. Unpicking the
  asymmetry (reading the same, untruncated bytes for both the join and the
  classification) is stage 1c's job, not this branch's.

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

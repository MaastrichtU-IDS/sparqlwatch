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

`absent` may only be claimed when the evidence actually establishes absence:
the endpoint itself answered, with a 2xx status, in a form we could read. A
timeout, an unreachable host, a 429, a gateway error, an HTML query console, an
unparseable body — anything we never got to interpret — is `indeterminate`.
There is deliberately no composite score and no ranking, here or downstream.

All judgement lives in one pure function, `resolve()` in `src/resolve.rs`. The
HTTP client returns evidence and no opinion; the emitter is a pure function of
its inputs, with no clock read and no randomness.

Declaration parsing is not built yet, so nothing is `declared` at this stage:
every confirmed capability currently reports as `undeclared-but-verified`.

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

Three nested budgets bound the work — per request (30s), per metric (60s), per
endpoint (600s) — and every one of them cancels the future rather than
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

`FetchWellKnown` has no probe implemented yet. Its metric stays in the file and
still gets a row — `indeterminate` — so the gap is visible in the output, but
no request is issued for it.

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

`reqwest`'s `system-proxy` feature is load-bearing rather than a convenience —
without a proxy every endpoint would fail identically, which looks exactly like
a dead registry.

**Correction to the spec's stage-0 findings.** The finding that "uppercase
`HTTP_PROXY` is ignored for `http://` URLs" is **curl-specific**: curl ignores
the uppercase form there because of a CGI variable collision. This client uses
reqwest, whose documented proxy resolution reads `HTTP_PROXY` *or*
`http_proxy` (and likewise for HTTPS and `ALL_PROXY`), so that constraint does
not apply to this code. Setting both cases, as above, is harmless
belt-and-braces and worth keeping for any sidecar or shell tooling that does
follow curl's rule — but the spec's note should not be read as binding here.

## Tests

```sh
cargo test                                  # unit + integration, all offline
cargo test --test live_smoke -- --ignored   # hits real third-party endpoints
cargo clippy --all-targets -- -D warnings
```

Everything but `live_smoke` runs against a local `wiremock` server, so the
suite is deterministic and CI never touches a stranger's endpoint. Rust 1.96,
edition 2021, no nightly features.

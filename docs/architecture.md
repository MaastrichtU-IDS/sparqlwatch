# Architecture

What sparqlwatch is made of, how the pieces are separated, and why they are
separated that way. Written 2026-08-30 against the tree at that date; the line
counts and the claims about who touches the network were measured, not
remembered.

Read this before changing anything that crosses a component boundary. For what
the project is FOR, read
`docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`. For how a
component works inside, read its own README: `prober/README.md`,
`web/README.md`.

## The shape: two tiers, one file between them

```
  registry/*.toml ────┐
  metrics.toml ───────┼──▶  prober (Rust)  ──▶  run-<instant>.nq
  state/dormancy.toml ┘          │                (immutable N-Quads,
                                 │                 one named graph per sweep)
                    probes the public internet          │
                                                        ▼
                                     web/load_run.py ──▶ store.db (Oxigraph)
                                                          │  + urn:sparqlwatch:current
                                                          ▼      (derived index)
                                                    web/app.py (FastAPI)
                                                      ├── HTML  (SELECT  -> Jinja)
                                                      └── RDF   (CONSTRUCT -> serialise)
```

The two tiers **share no code and no database**. The interface between them is a
file of N-Quads on disk.

That is the most consequential decision in the project. It means the prober can
be rewritten, run on a different machine, run on a schedule nobody here
controls, or run by a third party, and the web tier neither knows nor cares. It
also means a bug in one tier cannot corrupt the other's state, because neither
holds the other's state.

The cost is real and worth naming: nothing tests the seam end to end. See
"Known weaknesses" below.

## Tier 1: the prober

Rust, about 17,000 lines across `prober/src/`. Three binaries:

| binary | what it does |
|---|---|
| `sparqlwatch-prober` | runs one sweep and writes one run file |
| `seed-registry` | builds the endpoint registry from a LOD Cloud dump |
| `dormancy` | inspects and edits the admission state (`init`, `list`, `wake`, `sleep`, `prune`) |

### Layered by what each module may touch

This is the prober's organising principle, and it is enforced by convention
rather than by the compiler, so it is worth stating plainly. Measured on
2026-08-30:

| layer | modules | network | filesystem | clock |
|---|---|---|---|---|
| judgment | `resolve`, `verdict`, `metrics` | no | no | no |
| policy | `dormancy` | no | no | no |
| shaping | `emit` | no | no | no |
| effects | `client` | YES | no | yes |
| | `write`, `state_file` | no | YES | no |
| | `politeness`, `budget` | yes | no | yes |
| | `registry` | yes | yes | no |

Two consequences of that table are load-bearing:

**All judgment lives in one pure function.** `resolve(def, declared, obs) ->
Verdict`, at `prober/src/resolve.rs:129`. It takes a metric definition, what the
endpoint declared, and either an observation or `Expired`. It performs no I/O and
reads no clock. Every verdict the project publishes comes out of it, so the rule
"never report a confident wrong answer" has exactly one place to be enforced and
exactly one place to be reviewed.

**The admission policy has no clock.** `dormancy.rs` is 2,215 lines and contains
zero calls to `Instant::now`, `SystemTime` or `Utc::now`. It takes `now` as a
string parameter. That is what makes a policy about seven-day cadences and
two-strike relegation testable without waiting a week, and it is why the
dormancy tests run in milliseconds.

### The largest modules, and why

- `emit.rs` (3,912 lines) owns the published RDF vocabulary. It is large because
  it is the boundary where an in-memory observation becomes a permanent public
  claim. It derives every subject IRI reversibly from (run, endpoint, metric) so
  two runs can be diffed, and it follows a documented section protocol governing
  the ORDER quads are written in, so a truncated write loses a fact rather than
  misstating one.
- `dormancy.rs` (2,215 lines) is the cost-weighted admission policy: which
  endpoints this sweep will probe, which are relegated, which an operator has
  pinned awake or asleep.
- `metrics.rs` owns what a metric IS, as data, and the two independent axes
  that decide whether a given sweep asks it. See "Two axes" below.
- `registry.rs` (1,385 lines) loads and validates the endpoint list, applies the
  exclusion list, and drops URLs carrying credentials.

## Two axes: what a question costs, and how often it is asked

`Cost` says what a metric costs the endpoint it points at (`cheap`,
`expensive`, `exhaustive`) and `--max-cost` is the ceiling one sweep will pay.
`Cadence` says how often that cost is worth paying (`hourly`, `daily`) and
`--cadence` is the rhythm one sweep is. They are independent, and they have to
be: the four hourly metrics are all `cheap`, and so are two of the daily ones,
so no ceiling can separate them. That is the whole reason the second axis
exists.

As deployed: `0 * * * *` runs `--max-cost expensive --cadence hourly`, which is
availability, service-description, cors and cors-preflight. `30 3 * * *` runs
`--max-cost exhaustive --cadence daily`, which is everything. `Daily` is a
SUPERSET rather than the other half of a partition, and `Cadence::default()` is
`Daily` — a metric added to the file without the field is asked once a day
rather than sixteen times more often, so forgetting it makes a sweep quieter.

**A metric a sweep does not ask is DECLINED, never omitted.** This is the part
worth reading twice, because the obvious implementation is wrong and was
measured to be wrong on 2026-09-18. Giving the hourly job a smaller metrics
file left SEVEN OF TEN COLUMNS AS GAPS on the endpoint page: a metric absent
from the definitions produces no fact at all, `endpoint_measurements` reports
one sweep's facts, and the hourly run is the newest run — so the daily
measurements went invisible for 23 hours of every 24. "Not mentioned" is not
"not measured this time". So an out-of-cadence metric gets a `NotMeasured` fact
with reason `cadence`, exactly as the ceiling has always given one with reason
`cost-ceiling`, and the column survives saying why it is empty.

**Cost is decided first, and the two reasons stay apart.** `triple-count` is
both expensive and daily; on a cheap-ceiling sweep it reads `cost-ceiling`,
because that decision would still apply on a daily run. `geo-data` is cheap and
daily, so only cadence can decline it. `emit`'s duplicate-subject guard refuses
two `NotMeasured` facts for one (endpoint, metric) pair, so exactly one reason
exists to give and it must be the one that was decided first. Collapsing them
would tell an operator we judged their endpoint too expensive to query when the
truth is that tonight's sweep will ask.

Both axes are in `metricDefinitionRevision`, the FNV hash of the definition
list published on every run header. Two definition sets differing only in a
metric's cadence measure different things on the same sweep, and run graphs are
immutable, so sharing one revision would leave history that cannot be
reinterpreted. The destructuring pattern in `definitions_revision` has no `..`,
which is what makes a new field fail to compile until somebody decides whether
it belongs there.

## The interface: the run file

One sweep produces one immutable named graph, written as N-Quads.

Three properties of that file do most of the work:

**It is immutable.** A run graph describes one sweep at one instant. Re-running
the same `--at` is a deliberate replay, and the dormancy policy recognises a
replay by comparing instant STRINGS, which is why the instant form is validated
narrowly before any request is sent.

**It is the source of truth.** `web/load_run.py` states this outright: the
`.nq` files are authoritative and the Oxigraph store is derived. A sweep
observes a changing world, so re-running one does not reproduce it. A lost run
is not reproducible, it is gone. Hence `~/code/sparqlwatch-runs/`, outside git,
with `SHA256SUMS.txt` and a README describing each run.

**It is self-describing.** Every fact carries what it is about: a measurement
carries `dqv:computedOn` and `dqv:isMeasurementOf`, a sample carries
`sw:sampledFrom` and `sw:sampledBy`, a decline carries `sw:notMeasuredOn` and
`sw:notMeasuredMetric`. Nothing depends on row position, so a consumer can read
any subset of the file.

### The vocabulary

Standard where a standard term means the right thing: W3C DQV for measurements,
PROV for provenance, DCAT for the service, XSD for literals.

Sparqlwatch-owned (`urn:sparqlwatch:`) where no standard term is honest.
Currently about 30 predicates, in three families: the run header
(`emission`, `finalised`, `proberVersion`, `maxCost`, `concurrency`,
`completedEndpoint`, `failedEndpoints`, `metricDefinitionRevision`), the
declines (`NotMeasured`, `notMeasuredOn`, `notMeasuredMetric`,
`notMeasuredReason`), the content samples (`ContentSample`, `sampledFrom`,
`sampledBy`, `sampledValue`, `sampleSize`, `sampleTruncated`), and dormancy
(`dormantEndpoint`, `dormantSince`, `dormancyReason`, `dormantCount`).

**Why own them rather than borrow.** `void:class` and `void:classPartition`
both carry `rdfs:domain void:Dataset`. Reusing either for a content sample would
entail, under plain RDFS, that the sample IS a dataset, which cannot be honestly
asserted about an arbitrary endpoint: the thing behind a SPARQL endpoint may be
several datasets, or a virtual graph over a relational store.

This project shipped that class of defect once. `NotMeasured` reused
`dqv:computedOn`, and thereby entailed that 548 deliberately declined pairs were
quality measurements that never happened. Nothing looked wrong until a consumer
ran inference.

## Tier 2: the web tier

Python 3.12 in `web/`, about 5,200 lines, on FastAPI and pyoxigraph.

### `load_run.py` (1,670 lines)

One job: put a run graph into the store and maintain the derived graph.

Graphs are **replaced, never merged**. Two loads of the same run IRI must not
produce a graph where one measurement carries two values.

**`urn:sparqlwatch:current` is a materialised index.** It exists because
deciding "which run is newest for this endpoint" at query time does not scale:
measured at 1.2 ms over one run, 115.6 ms over seven, and 6,488.5 ms over
thirty. The pointer replaced that.

It holds recency pointers plus a verbatim copy of each endpoint's measurement
and decline quads, and deliberately holds:

- no sample values, because the index reads current for verdicts and never for
  sample values, so copying several hundred `sw:sampledValue` triples per
  endpoint would grow the scanned graph and buy nothing
- no run-level facts, because finishing is something a RUN does, and copying
  `sw:finalised` onto an endpoint would publish a wrong fact
- no `prov:Activity` and no `prov:generatedAtTime`, ever. Every reader treats a
  typed activity with a timestamp as a run, so one in the derived graph makes it
  the newest run by construction, and every page becomes a 500

It is **derived and reconstructible from the run graphs alone**. `rebuild_current`
must produce exactly what incremental loading produces, and a run file naming
`urn:sparqlwatch:current` as its graph is refused before any store is opened.

Drift is detected rather than assumed away: a run graph that shrinks, or is
dropped, leaves the derived graph attributing facts to a run that no longer
states them. That shows up as an endpoint whose pointer names a run whose graph
no longer mentions it, reported in `LoadResult.drifted`.

### `app.py` (2,692 lines)

Serves each resource in five representations: HTML, Turtle, N-Triples, RDF/XML
and JSON-LD.

**HTML and RDF are derived independently, on purpose.** HTML comes from SELECT
queries through view-model modules (`endpoint_measurements.py`,
`endpoint_content.py`, `endpoint_index.py`) into Jinja templates. RDF comes from
CONSTRUCT queries in `web/queries/*.rq`, serialised straight out of the store.
Nothing rebuilds a triple in Python.

Building the RDF from the SELECT rows would mean re-deriving the graph from a
flattened copy of itself, which is where two representations of one resource
start to disagree. Keeping the CONSTRUCT means the RDF cannot state anything the
store does not hold, and it means `web/tests/test_negotiation.py` compares two
genuinely independent derivations rather than two views of one list.

### The queries

`web/queries/` holds five files, each with a header comment carrying its own
measured cost figures. `index_description.rq` is the one to be careful with: it
has a measured 28x cost regression in its history and a mutation guard because
of it.

## The rules that explain the shape

Five, and most of the design follows from them.

**1. Never report a confident wrong answer.** The whole point. Every
(endpoint, metric) pair resolves to exactly one of six verdicts, and `absent`
may only be claimed when the endpoint itself answered the question. A timeout,
an unreachable host, a 429, an HTML query console, an unparseable body: all
`indeterminate`, forever. There is no composite score and no ranking.

| verdict | meaning |
|---|---|
| `verified` | confirmed and declared |
| `undeclared-but-verified` | confirmed, not declared |
| `declared-only` | declared, not confirmed |
| `declared-but-wrong` | declared, but incorrect |
| `absent` | neither declared nor confirmed |
| `indeterminate` | not determined |

Plus `not-measured`, which is not a verdict: it records that a question was
never asked, and why.

**2. The vocabulary is closed.** Six verdicts and two non-verdict fact kinds.
The web tier draws an unrecognised value distinctly rather than dropping it,
because a value we do not understand is a fact about the data and not a gap.

**3. An absent qualifier is a positive claim.** On the published pages, the
absence of a marker asserts something. That is why a capped sample cannot carry
a negative claim, and why a refused query is not evidence of absence.

**4. Politeness is structural, not a setting.** Three nested budgets (30 s per
request, 60 s per metric, 600 s per endpoint), a 2 s minimum gap between
consecutive requests to one host, never two requests to one host in flight, a
`Retry-After` cap, an exclusion list re-read every run so an entry takes effect
at the next sweep, and a dormancy state file that fails closed if it cannot be
read. The User-Agent names the site and points at `/about`.

**5. Measure, do not assert.** Nearly every design comment in this codebase
carries numbers, and several record having been wrong. That is deliberate: the
comments are the evidence, and a claim without a number is treated as a guess.

## Testing

- **Rust: 477 tests over 16 targets.** Unit tests inline; integration tests in
  `prober/tests/` (11 files), several driving a real HTTP server through
  wiremock. `live_smoke.rs` is the only one that touches a real endpoint.
- **Python: 342 tests over 10 files, with 19 committed run fixtures.** Each
  fixture is a store scenario, built through `load_run()` and never through
  `Store.load()`, so no test runs against a store shape the deployment cannot
  have.
- **A golden file** (`web/tools/capture_reader_golden.py`) pins what the readers
  produce, so a change in output is visible rather than inferred.

## Licensing, which is two licences and not one

The **software** is Apache-2.0 (`LICENSE`). The **measurements this service
publishes** are CC BY 4.0 (`LICENSE-DATA`). They differ on purpose.

Apache-2.0 is a software licence: its terms speak of source and object form, of
contributions and of patent grants, none of which map onto a set of
observations. A catalogue that lists datasets — the LOD Cloud, YummyData —
looks for a data licence, and would not find one in Apache-2.0. So the data
carries its own, and `/.well-known/void` states it with `dcterms:license`
alongside `dcterms:creator` and `dcterms:rightsHolder`: CC BY requires
attribution, and a consumer told they must attribute but not told to whom
cannot comply.

What the data licence covers is what this service observed — the run graphs,
the derived `current` graph, the VoID documents, and what the SPARQL endpoint
returns. It does not cover the endpoints measured. This dataset records what we
saw of them; it does not contain their data, and nothing here licenses anybody
else's.

`web/tests/test_well_known_void.py` pins each licence to its own file, because
the two now deliberately disagree and a test pinning one against the other
would enforce exactly the confusion this split removes.

## Known weaknesses

Recorded because they are real, not as a to-do list.

- **`app.py` at 2,692 lines is doing too much**: routes, view models, page copy,
  docs content, politeness constants and their justifications. It is the
  clearest candidate for a split.
- **The seam between the tiers is untested end to end.** 477 Rust tests and 342
  Python tests, and nothing runs a real prober output through a real load into a
  real rendered page in one assertion. The fixtures are captured prober output,
  which is close, but capture happens by hand.
- **Nothing tests that the two cronjobs pass the cadence they mean.** The
  hourly and daily sweeps are distinguished by two env vars in
  `MaastrichtU-IDS/services`, and `sweep.sh` requires both rather than
  defaulting either -- so a job with neither fails loudly. What is unguarded is
  a job with the WRONG one: an hourly schedule passing `--cadence daily` would
  ask every metric every hour, and nothing here or there would notice. The
  `registry_pair.rs` treatment, a test comparing the two manifests against what
  each is for, is what this wants and does not have.
- **CI gates only the Rust side.** `.github/workflows/ci.yml` runs `cargo
  build`, `cargo test` and `cargo clippy -D warnings`. The 342 web tests never
  run there. Raised repeatedly, never decided.
- **The web tier's read path is pinned to `sw:metric:classes`** in three places,
  so a sample from any other sampling metric is written correctly by the prober
  and invisible to the site. Plan
  `docs/superpowers/plans/2026-08-29-metric-agnostic-sample-pointers.md` exists
  to fix it.
- **No endpoint has a name.** The registry carries URLs. A first-party name is
  available from a service description for about 5% of endpoints, and a
  third-party name from the LOD Cloud dump for all of them, which are claims of
  different standing. Deferred, not resolved.

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

**An endpoint fact produced on a slow cadence needs its own pointer.** `current`
holds a few facts on the endpoint rather than on a measurement —
`sw:declarationsRead`, `sw:descriptionSource`, and the `sw:void*` family. The
load path rewrites those from the incoming run, and the rebuild reads them from
each endpoint's newest run. Both are correct only while the fact is republished
by *every* run.

`service-description` is hourly, so `declarationsRead` and `descriptionSource`
are. The well-known VoID is daily, so 23 runs in 24 carry none — and the
unguarded rewrite meant every hourly run deleted the daily reading and put
nothing back, while the rebuild read it from a run that never had it. Shipped
2026-09-28; on 2026-09-29, 27 endpoints published a VoID, the run graphs held
all 27, and `current` held zero.

So the delete is now guarded on the incoming run actually carrying one, and the
rebuild reads from `_VOID_ENDPOINTS` — the newest run that *published* one —
the way samples already use their own pointer. Anything else moved to a slower
cadence needs the same treatment.

**The fleet-wide caches are warmed at startup, in the site process.** An
endpoint page was 3–5 s on first view and 23 s after a restart, and the cause
was one call: `endpoint_vocabulary` reached `explore_payload.build_payload` —
the uncached function — rather than the `@lru_cache`d wrapper here, so every
page rebuilt the whole fleet's vocabulary (15,844 terms from ~19,000 rows) to
keep one endpoint's 156. Fixing that moved the cost onto whoever opened the
first page after a restart, 20.5 s of it, so `app.py` now pays it in a startup
handler: index, fleet history, vocabulary payload, about 26 s inside the
container's 40 s HEALTHCHECK start-period. Pages are 0.4–1.3 s.

It has to be THIS process. These are `@lru_cache`s keyed on the store handle,
and that handle belongs to whoever serves requests — an earlier attempt warmed
from the `build-store` init container, which then exits, and achieved nothing
measurable: it filled the node's page cache and left every Python-side result
to be computed again. `build_payload` is almost entirely that Python side.

Four theories were wrong before instrumentation settled it: cold Longhorn
reads, the unbounded history scan (bounding it is twenty times *worse*), CPU
starvation, and size-proportional rendering (`render` is 0.00–0.10 s on a
754 KB page). Every query timed against the `sparql` container came back in
hundredths of a second — a different process with its own warm handle. The
per-request timing line in the endpoint route is what ended the guessing.

**A run that lands out of order triggers a rebuild.** A run file is stamped with
the instant its sweep STARTED and written when it FINISHES, so the daily pass —
which starts at 03:30 and, at 126 endpoints, finishes near 06:30 — arrives after
three hourly runs have already been loaded. `current` then points at something
newer and the daily run's facts are skipped, which `LoadResult.kept_newer`
reports. That was silent data loss: on 2026-09-29 the daily pass measured
`void-well-known` for 118 endpoints and the page showed `cadence` for all 124,
while the readings sat unreachable in the run graph. `geo-coordinates` had been
losing the same way since it was added.

The repair is `rebuild_current`, not a merge rule in `_REPLACE_MEASURED`: that
rebuild already derives current from every run graph in instant order and is
already the path a deploy takes, so it is known correct for exactly this input.
A second implementation of "which run wins for this pair" could disagree with
the first. It fires only when a file lands behind current — in steady state once
a day, after the daily pass, and never on the hourly restart.

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

**The endpoint page is also a third representation, in microdata.** `<main>`
carries `itemscope itemtype="dcat:DataService"` with the endpoint URL as its
`itemid`, and each measured row carries a `dqv:QualityMeasurement` item stating
`dqv:computedOn`, `dqv:isMeasurementOf` and `dqv:value`. Same vocabulary as the
RDF, same direction, so a crawler that parses only HTML gets the same facts.

It is held to a one-way rule: **the microdata may say less than the RDF, never
anything else.** Measurement items carry no `itemid`, so they extract as blank
nodes — an existence claim rather than the named measurement the run graph
holds — and a declined row is marked up as nothing at all, because a decline is
not a measurement. `test_every_microdata_verdict_is_one_the_rdf_representation_states`
checks the HTML's verdicts against the Turtle directly.

Each measurement is a *top-level* item rather than nested under the service.
Nesting would need an inverse of `dqv:computedOn`, and the obvious candidate
declares a domain a `dcat:DataService` does not satisfy; an element with
`itemscope` and no `itemprop` is a separate item, which lets the page state the
relation in the graph's own direction instead.

**The vendor's own description is linked where there is one.** The prober
publishes `sw:descriptionSource` — the URL a service description actually
parsed from, which is the endpoint URL itself per the SPARQL 1.1 Service
Description spec, or wherever a redirect landed. Everything else on an endpoint
page is what *this* service observed; that link is the one thing that lets a
reader check the two accounts against each other. Absent when nothing parsed,
which is not the same as an empty description.

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

## What it is called in the cluster

Deploying is a second repo's job (`MaastrichtU-IDS/services`,
`ids3/projects/sparqlwatch/dev/`), and its README holds the full table. The one
fact worth repeating here, because it is the one that gets guessed wrong: the
web Deployment is **`site`**, not `sparqlwatch`. The project name belongs to the
namespace (`sparqlwatch-dev`) and to the things that are shared — the ingress
and the three PVCs — while the components are named for what they are: `site`,
`synthetic`, `prober`, `prober-profile`.

    kubectl -n sparqlwatch-dev logs deploy/site -c site --tail=50

The `site` pod runs two containers, `site` and `sparql`, after an init container
`build-store`; `-c` is therefore not optional.

## Sweeping one endpoint, on demand

Adding a url to `prober/endpoints.toml` does not measure it. Four metrics
arrive at the top of the next hour; the other six wait for the nightly profile
pass at 03:30 UTC. So a freshly added endpoint has an empty page for up to a
day, which is exactly when somebody wants to look at it.

`--only <url>` narrows a sweep to named urls, and `ops/catch-up-sweep.yaml` is
a Job that runs one at the exhaustive cadence:

    sed 's|ENDPOINT_URL|https://example.org/sparql|' ops/catch-up-sweep.yaml \
      | kubectl -n sparqlwatch-dev create -f -

The alternative is `kubectl create job --from=cronjob/prober-profile`, which
works and sweeps **every** endpoint at exhaustive cost — about 61 third-party
servers, a second full pass on top of the night's. Cadence is a property of
each metric definition, not of when an endpoint was last probed, so nothing in
that job would skip the servers it already visited. The registry files call
probing somebody else's server an explicit act; re-probing sixty of them to
fill in a page for one of ours is not one worth making.

Three properties of `--only` are load-bearing:

- **It is applied after the exclusion list, never before.** The hosts somebody
  asked to be left alone are gone before `--only` is read, so naming one is an
  error rather than a probe. A convenience must not be able to reach past a
  promise.
- **An unknown url stops the sweep.** A mistyped one that matched nothing would
  exit 0 and write a run naming no endpoint — indistinguishable from a
  successful catch-up pass, so the operator would believe the endpoint had been
  profiled while its page stayed empty.
- **It is safe for the shared dormancy state.** `dormancy::update` clones the
  state on disk and touches only the endpoints the sweep listed, so a
  one-endpoint run carries every other entry forward untouched.

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

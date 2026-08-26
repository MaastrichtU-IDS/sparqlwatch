# SPARQL endpoint monitoring service: design

**Status:** draft for review
**Date:** 2026-08-19
**Name:** `sparqlwatch`

## Purpose

A public service that continuously monitors SPARQL endpoints for a general
(non-biomedical) community, publishes quality measurements about them, and lets
data consumers raise issues with providers.

Its thesis is narrower and more actionable than "quality": **measure tool-readiness,
then remediate what is missing.**

CORS headers, a VoID description with class-property partitions, a service
description, and published example queries are precisely what determine whether an
endpoint can be used by tooling. They are also what an embedded query editor needs in
order to autocomplete. So for every accessible endpoint we do two things:

1. Measure whether it publishes what tools need, and report each gap as a specific,
   actionable finding rather than as a number.
2. **Host the metadata it fails to publish.** No VoID, so we derive and serve one. No
   examples, so we host curated ones. The editor is then pointed at our metadata, and a
   poorly described endpoint becomes queryable through our service even though it is not
   queryable on its own.

That makes the score a to-do list instead of a judgement, and makes the remediation a
public good rather than a leaderboard.

It is deliberately not a fork of YummyData/umakadata. That evaluation is recorded
in [Why not umakadata](#appendix-a-why-not-umakadata).

## Requirements

Decided with the project owner:

| Decision | Choice |
|---|---|
| Audience | A community, publicly hosted |
| Metrics | Our own definitions, not YummyData's six-metric model |
| Prober | Rust |
| Web tier | Python / FastAPI |
| Endpoint registry | Curated, plus public submissions with moderation |
| Placement | Standalone service, own repo, own ids3 project-env |
| Deployment target | MaastrichtU-IDS ids3 k3s cluster (GitOps / ArgoCD) |
| UI | Dynamic faceted search, browse, charts and figures |
| Query editor | Embedded, with autocomplete, per accessible endpoint |
| Content metadata | Extracted for accessible endpoints, tiered by cost |
| Example queries | Ingested from `sib-swiss/sparql-examples`, discovered from endpoints, contributable |
| Conformance | Per-attribute verdicts and graded levels. **No composite score, no ranking.** |
| Primary surface | Task-first ("what do you need it to do"), verdict matrix as the detail view |
| Registry seed | LOD Cloud dump + YummyData's list, probed once, only responders admitted |
| Themes | Four, from ontoexplorer: green dark/light and blue dark/light. Blue is the default. |

Non-negotiable constraints from the environment:

- **No JVM anywhere.** This rules out reusing TripleDataProfiler and SPARQLES.
- **ids3 pods have no direct internet.** All external egress must traverse
  `http://egress-proxy.platform.svc.cluster.local:3128` (Squid). A monitor whose
  entire job is reaching the public web makes this the single highest project risk;
  see [Risks](#risks).

## Core architectural idea

**Separate measurement from judgement.**

Store immutable, append-only *measurements*. Derive every *verdict* as a query over them.

umakadata conflates the two: its crawler writes one column per criterion into a
26-column `evaluations` table and computes the score in a `before_save` hook. The
consequence is that changing a metric definition or a score weighting requires a
schema migration and invalidates history, because past scores cannot be recomputed
from what was stored.

Separating them buys three things that matter directly given the owner wants to
define and evolve their own metrics:

1. A metric definition can change and every historical verdict can be recomputed
   without re-probing a single endpoint.
2. Adding a metric is publishing a definition plus a probe, not migrating a table.
3. Consumers can disagree with our judgement and derive their own from the same
   published measurements.

## Data model

RDF, using established vocabularies rather than a bespoke schema. Stored in
Oxigraph.

**Endpoints** are `dcat:DataService` (SPARQL endpoints are DCAT's canonical example
of one), with `dcat:endpointURL`, `dcat:endpointDescription`, `dcat:servesDataset`,
plus `dcterms:title` and submission provenance.

**Metric definitions** use W3C DQV:

- `dqv:Category` groups dimensions (e.g. availability, conformance, richness)
- `dqv:Dimension` groups metrics
- `dqv:Metric` is one measurable thing, with `dqv:expectedDataType`, `dqv:inDimension`,
  a human definition, and a link to the probe that computes it

These definitions are **dereferenceable resources on the service**, not private
config. Publishing them is the differentiator over both YummyData and SPARQLES,
and it is squarely in the owner's research area.

**Measurements** are `dqv:QualityMeasurement` with `dqv:computedOn` (the service),
`dqv:isMeasurementOf` (the metric), `dqv:value`, and PROV terms:
`prov:wasGeneratedBy` a probe run `prov:Activity`, `prov:generatedAtTime`.

**Runs** are `prov:Activity`, one per scheduled sweep, recording the prober version
and the metric-definition revision used. Each run writes into **its own named graph**,
which makes history immutable and lets a bad run be dropped wholesale.

A derived `urn:sparqlwatch:current` graph holds each endpoint's newest facts so the
UI's common queries stay cheap as history grows. It is derived from the run graphs
and never hand-edited.

**Corrected 2026-08-25 (stage 3-2), after the graph was built and measured.** This
paragraph used to say the graph holds "the latest measurement per (endpoint, metric)"
and is "rebuilt after each run". Both halves are wrong, and this is a correction of
the spec rather than a deviation from it.

- **Per endpoint, not per (endpoint, metric).** `web/queries/endpoint_measurements.rq`
  reasons that recency belongs to the endpoint: one sweep either measured an endpoint
  or it did not, so the newest run that recorded anything for an endpoint supplies all
  of that endpoint's facts. Taking each metric's own newest run instead builds a row
  whose parts came from different sweeps, which is precisely the defect stage 3-1 was
  fixed for: a 59-class sample attributed to a sweep that had declined `classes`. The
  spec predicted the graph, and the code's reasoning about its granularity is better
  than the spec's, so the spec is what changes.
- **Incrementally maintained, not rebuilt after each run.** `web/load_run.py` updates
  it as each run loads, in one transactional `store.update()` per endpoint, touching
  only the endpoints that run mentions. A full rebuild is kept as a repair and
  migration tool (`load_run.py --rebuild`) rather than the maintenance mechanism,
  because it re-derives all 543 endpoints every time and measures seconds per run
  (0.8 s to 3.2 s over a 30-run store) where the read it protects is milliseconds: at
  30 runs the endpoint page went from 5,801.8 ms to 0.79 ms and its RDF representation
  from 11,704.3 ms to 20.75 ms.

The graph carries **two** pointers per endpoint, `sw:currentRun` and
`sw:currentSampleRun`, because the newest run that measured an endpoint and the newest
that sampled its classes are routinely different runs: a sweep at the default cost
ceiling declines `sw:metric:classes`. It carries no `prov:Activity` and no run-level
fact, so it states nothing a run graph does not. `web/README.md` holds the shape, the
measurement tables, the load cost this buys the read with, and the one rebuild pass an
existing store needs.

**Raw evidence** (request, response headers, timing, truncated body) is retained per
measurement, because "why did this endpoint score badly" is the first question a
provider asks. umakadata gets this right with its `activities` table and it is worth
copying.

## Components

### 1. Prober (Rust)

One binary, invoked per run. Responsibilities: read the endpoint registry and metric
definitions, execute each metric's probe against each endpoint, emit N-Quads.

Design rules, each of which encodes a failure we observed in umakadata:

- **Cancellable timeouts at three levels**: per HTTP request, per metric, and a total
  per-endpoint budget. umakadata's per-endpoint timeout is only checked *between*
  measurements, so a single stalled query ran for 24 minutes against a 4-hour cap and
  nothing could interrupt it. Use `tokio::time::timeout` around every probe so the
  budget is enforced by cancellation, not by inspection.
- **Bounded retries with a total budget.** umakadata retries a 300-second read timeout
  three times, so one query costs 15 minutes. At most one retry, and it counts against
  the endpoint budget.
- **Per-endpoint isolation.** One endpoint's failure or slowness must never delay
  another's results or block the run from finalizing. Results are written per endpoint
  as they complete. *(Delivered. Stage 1c-b3 stopped one endpoint's slowness
  delaying another endpoint's probing; stage 1c-b4 delivered the per-endpoint write.
  Each endpoint's facts are emitted as one self-contained chunk and flushed the
  moment that endpoint finishes, so no endpoint's slowness delays another
  endpoint's OUTPUT either, and a sweep killed at endpoint 500 of 548 leaves 500
  endpoints on disk.)*
- **Per-host politeness**: concurrency cap of 1 per host, configurable delay, a real
  `User-Agent` naming the service with a URL, and honouring `Retry-After`.
- **Proxy support is mandatory**, configured by `HTTP_PROXY`/`HTTPS_PROXY`, because of
  the ids3 egress constraint.
- **A probe never writes a score.** It writes a measurement and its evidence.

Probe kinds are a closed set (an HTTP check, a SPARQL ASK, a SPARQL SELECT with a
result assertion, a URI dereference check, a content-negotiation check). A metric
definition names a probe kind and its parameters. This is what makes metrics
declarative: new metrics that fit an existing kind need no Rust changes.

### 1b. Content metadata extraction (tiered)

Autocomplete needs to know an endpoint's classes and the properties used with each.
Producing that for an arbitrary remote endpoint is the expensive part of this project,
so it is explicitly tiered. SIB's `void-generator` is the reference implementation of
the idea but is unusable here: it is Java 17, and its own documentation says to run it
locally on the endpoint without a proxy in between, which is impossible for third-party
endpoints.

Class discovery is split into two metrics: `has-classes` (cheap, answerable as a fast existence check) and `classes` (expensive, enumerating up to 200 distinct types). Both are measured at tiers 1 and 2 according to this plan.

| Tier | Method | Cost | Recorded as |
|---|---|---|---|
| 1 | Fetch what the endpoint publishes: `/.well-known/void`, a VoID graph, the SPARQL service description | one or two requests | authoritative, and its presence is itself a metric |
| 2 | Bounded sampling: distinct classes (`classes` metric), properties per class, counts, each under a hard cancellable budget. `has-classes` is a fast existence check that always completes at this tier. | tens of queries, capped | **explicitly marked sampled and incomplete** |
| 3 | Give up | none | `not measured`, never a zero |

Tier 3 is not a failure mode to be embarrassed about, it is a required outcome. During
the umakadata evaluation, a metadata query against QLever's osm-planet ran for 24
minutes and never returned within budget. Reporting `0 triples` there would be a lie;
reporting `not measured` is the truth and is more useful to a reader.

Derived VoID is published per endpoint in its own named graph, clearly attributed to us
rather than to the provider, and offered back to the provider as the remediation.

**Tier 2 status, updated 2026-08-22 (stage 2b-1): partly delivered.** The
`classes` metric samples distinct classes and publishes them as a
`ContentSample` fact, bounded by a declared `sample_limit` (200, matching the
metric's own `LIMIT`) and explicitly marked `sampleTruncated` when that limit
was reached. See `prober/README.md`'s "Content samples" section for the exact
predicates. **Not built**: properties per class, and counts. The tier is
partly delivered, not delivered.

That fact is deliberately **not** VoID, which supersedes the paragraph above
for what tier 2 actually emits today. A content sample is an observation from
one bounded query, not a description of a dataset: the thing behind a SPARQL
endpoint may be several datasets, or a virtual graph over a relational store
rather than a dataset at all (`ontop`, in this project's own registry, is
exactly that). `void:classPartition` and `void:class` both carry
`rdfs:domain void:Dataset`, so reusing either predicate here would entail,
under plain RDFS, that a content sample IS a dataset, which this project
cannot honestly assert about an arbitrary endpoint. This project has already
shipped that class of defect once, when the `NotMeasured` fact reused
`dqv:computedOn` and thereby entailed that 548 deliberately declined pairs
were quality measurements that never happened, with nothing visibly wrong
until a consumer ran inference. Whether derived VoID is still worth building
alongside the sample, rather than instead of it, is an open question for
whichever stage revisits tier 1's "derive and serve VoID" idea from the
Purpose section above.

### 1c. Query editor and autocomplete

Embed `@sib-swiss/sparql-editor`, a YASGUI-based web component that takes one script
tag and a custom element. It autocompletes from VoID class-property partitions and
surfaces example queries, which is exactly the data model we already hold.

**The editor's two request types are handled differently**, and this is the decision
that removes the need for a query proxy:

- **Autocomplete metadata** is served from *our* origin, with our own CORS headers. We
  already collect it in order to score endpoints, so autocomplete works on every
  accessible endpoint, including those that publish no VoID and send no CORS headers.
- **The user's query** goes browser-to-endpoint directly. Where the endpoint sends no
  CORS headers the query cannot run, and we report that as a specific finding naming the
  exact header the operator needs to add.

**We do not proxy queries.** An open SPARQL relay would let anyone route arbitrary,
arbitrarily expensive queries at third-party endpoints anonymously, with our address
taking the blame and eventually the blocks, which is a poor trade for a service whose
value depends on cooperative relationships with providers. On ids3 it is worse: pods
have no direct internet, so proxied traffic would traverse Squid and the university's
egress, putting the institution's identity on user-supplied queries to arbitrary hosts.

Measured evidence that this is an acceptable trade: of the four endpoints touched
during evaluation, **all four send CORS headers and answer preflight** (`qlever.dev`,
`data.kkg.kadaster.nl`, `ontop.certain.ai.ustp.at`, `sparql.uniprot.org`). Not a random
sample, but consistent with public endpoints expecting browser clients. Missing CORS
looks like a minority failure, not the common case.

### 1d. Example queries

Three sources, one model. Examples are stored as they already are in
`sib-swiss/sparql-examples`: Turtle using SHACL, with `sh:select`, `sh:prefixes`,
`rdfs:comment`, `schema:keywords`, `schema:target` and `spex:federatesWith`. Because
that is already RDF, it loads directly into Oxigraph beside the observations with no
translation layer.

1. **Ingest** the SIB corpus (UniProt, Rhea, SwissLipids, ChEMBL, MetaNetX and others),
   tracked upstream so updates flow in.
2. **Discover** examples an endpoint publishes itself, via the
   `/.well-known/sparql-examples` named-graph convention. Whether an endpoint does this
   is a tool-readiness metric.
3. **Accept contributions** from the community, through the same moderation path as
   endpoint submissions.

`spex:federatesWith` is worth honouring specifically: it records which endpoints a query
joins across, which is the raw material for a federation view.

### 2. Storage (Oxigraph)

Its own instance for this service, deployed in the project's namespace with a PVC.
Not shared with the BioPortal instance, to keep the public service's blast radius and
backup story separate. It exposes a **read-only public SPARQL endpoint** over the
observations, which is both good practice for this kind of service and free given the
data model.

### 3. Web tier (Python / FastAPI)

Four themes, surfaced from ontoexplorer: green dark (`:root`) and light, plus blue dark
(`blueprint`, `#0a1929`) and blue light (`arctic`, `#f8fafe`). **Blue is the default.**
The chart series palette was validated against all four panel surfaces and passes every
check on each, so the theme choice carries no accessibility cost.

Read paths, all SPARQL-backed:

- Leaderboard, filterable and sortable by dimension
- Per-endpoint page: current measurements, history, evidence for each measurement
- Per-metric page: the definition, how it is computed, which endpoints fail it
- **Content negotiation on every resource**: HTML for people, RDF for machines. A
  quality-measurement service that is not itself machine-readable would be
  self-defeating.

Write paths:

- Submission form: an endpoint URL plus contact, validated by actually probing it
  once before acceptance, then queued for moderation
- Moderation queue behind authentication
- Per-endpoint discussion threads, so consumers can raise issues with providers

Submissions are the only untrusted input, so they get rate limiting, a CAPTCHA or
equivalent, and validation that the URL is actually a SPARQL endpoint. That last check
matters more than it sounds: of three endpoint URLs supplied by hand during the
umakadata evaluation, **two were HTML UI pages rather than query endpoints**. A
submission flow that accepts a YASGUI page will silently fill the service with
endpoints scoring zero for the wrong reason. The validator must require a
`application/sparql-results+json` response to a trivial query.

### 4. Scheduling

A Kubernetes `CronJob` per run, not an in-process scheduler. umakadata runs
sidekiq-scheduler inside the app container, which couples the web tier's uptime to
the crawler's and makes a stuck job a restart of the whole app. On ids3 a CronJob
gives isolation, retries, and history for free.

Per-endpoint isolation argues for the prober being invoked with a shard of the
registry, so one hung endpoint cannot delay a sweep.

## Testing and CI

Non-negotiable from the first commit, because the absence of both is a large part of
why umakadata was rejected (13 examples, 1 failing, 11 generator stubs, no CI).

- Prober: unit tests per probe kind against a local mock SPARQL server; a fixture
  suite of recorded endpoint responses including malformed ones
- Scoring: pure functions over stored measurements, so property tests are cheap
- Web: request tests per route, including **the empty-database case**, which is exactly
  the bug that 500'd umakadata's dashboard
- CI on every push: build, test, lint, and a smoke run of the prober against a fixture
  endpoint

## Deployment (ids3)

Standalone project-env per the `converting-compose-to-ids3` conventions: a
`sparqlwatch-dev` namespace first, promoting the same pinned image to
`sparqlwatch-prod` once proven.

Components: prober `CronJob`, web `Deployment` plus `Service` plus `Ingress`, Oxigraph
`Deployment` plus PVC plus `Service`, the mandatory egress `NetworkPolicy`, and Vault
for the moderation credentials. Images pinned by tag or digest, never `latest`.

No relational database, so no migration hook is needed. The equivalent concern is the
metric-definition revision, which is versioned data loaded into a named graph.

## Risks

| Risk | Severity | Handling |
|---|---|---|
| ~~Squid egress blocks arbitrary-host `CONNECT`~~ | **RESOLVED 2026-08-20** | Spike passed. See [Stage 0 result](#stage-0-egress-spike-result). |
| Scope creep: monitoring dashboards are deceptively large | High | First release is the leaderboard, endpoint pages, metric pages and submissions. Threads can follow. |
| Probing looks like abuse to endpoint operators | Medium | **PARTLY DELIVERED 2026-08-21** (Stage 1c-b2): per-host concurrency of 1, configurable delays between requests (`--min-gap-ms`), honest User-Agent with a contact URL, and `Retry-After` handling (up to `--retry-after-cap-s`, delta-seconds form only) are done. Stage 1c-b3 added `--concurrency`, which parallelises across **hosts** only and leaves per-host concurrency at 1; its default of 4 is chosen for politeness, being roughly two requests per second in aggregate at the default 2-second gap. Published probe schedule is **not built**. |
| Very large datasets make some metrics intractable, as osm-planet did | Medium | **DELIVERED 2026-08-21** (Stage 1c-b1): metrics declare a cost class (`cheap` or `expensive`) and a declined metric records "not measured" rather than a misleading zero. The ceiling is **per run**, set by `--max-cost`, not per endpoint: an expensive metric is opt-in for the whole sweep. Per-endpoint opt-in is a reasonable later refinement and is **not built**, which matters because the measured case (`classes` costs 15.7s on kadaster and exceeds the 30s budget on qlever) is exactly the case a per-endpoint ceiling would serve. |
| Duplicating a live service (SPARQLES or a successor) | Medium | Survey prior art before building. Not yet done. |
| Endpoints without CORS cannot run queries in the embedded editor | Low | Autocomplete is served from our origin so it still works; the gap is reported as an actionable finding. Measured 4 of 4 evaluated endpoints already send CORS. |
| Depending on an external web component (`@sib-swiss/sparql-editor`) | Low | Pin the version, vendor the bundle rather than loading from a CDN, since the ids3 CSP and egress rules make CDN loading unreliable anyway |

### Stage 0 egress spike: result

Run on 2026-08-20 in a throwaway `sparqlwatch-spike` namespace carrying a verbatim copy
of `ids3/projects/_base/restrict-egress.yaml`, so the pod had exactly a real project-env's
restrictions. Namespace deleted afterwards.

**The answer is yes, the prober can run in-cluster.**

| Check | Result |
|---|---|
| Direct egress with no proxy | **Blocked**, curl exit 7 in 0.06s, as the policy intends |
| Via `egress-proxy.platform.svc.cluster.local:3128` | **200 in 0.14s** |
| Arbitrary-host `CONNECT` | **Not whitelisted.** qlever.dev, data.kkg.kadaster.nl, foodie-cloud.org, query.wikidata.org, sparql.rhea-db.org all returned `application/sparql-results+json` |
| A real GeoSPARQL probe | `geof:sfWithin` returned `{"boolean": true}` through the proxy |

Three findings that change the prober's configuration:

1. **Uppercase `HTTP_PROXY` is ignored for `http://` URLs (curl-specific).** curl honours only lowercase
   `http_proxy` there (uppercase is deliberately ignored because of CGI collision), while
   `HTTPS_PROXY` works uppercase. Measured in the spike with curl: with only the uppercase pair
   set, plain-http endpoints bypassed the proxy and failed *directly on port 80* in 3ms, which
   looks exactly like a dead endpoint. **472 of 548 LOD Cloud URLs are plain `http://`**, so a
   client with curl's rule would silently mis-verdict 86% of the registry. Setting lowercase
   `http_proxy` fixed all of them.
   **Correction:** the conclusion does not transfer to this prober. It uses reqwest, and
   hyper-util resolves the proxy with `get_first_env(&["HTTP_PROXY", "http_proxy"])`
   (`matcher.rs:232`, hyper-util 0.1.20), reading both cases with uppercase first, so the
   uppercase pair alone is sufficient here. The measurement stands as part of the spike record and the hazard
   is real for curl; it simply does not bind this code. Setting both cases of both variables is
   harmless belt-and-braces and worth keeping for sidecars and shell tooling that do follow
   curl's rule.

   Two caveats on that correction, neither of which weakens it. First,
   `get_first_env` decides presence with `std::env::var(name).is_ok()`, so a
   correctly-set lowercase `http_proxy` is silently shadowed by an uppercase
   `HTTP_PROXY` that is merely set to an empty string, which by this same
   stage-0 finding's own logic looks exactly like a dead registry. Second,
   hyper-util disables environment-variable proxying entirely, uppercase and
   lowercase both, when `REQUEST_METHOD` is set (`matcher.rs:230`, with the
   early return at `matcher.rs:305`): the CGI collision curl guards against
   therefore does exist for this client too, in a stronger form that drops
   the proxy outright rather than merely picking the wrong case. The
   correction above, that the constraint does not bind this code, is true
   only for the uppercase-versus-lowercase question, not for the CGI
   collision itself.
2. **Some hosts fail cluster DNS.** `ontop.certain.ai.ustp.at` gave `SERVFAIL` from the pod
   and `CONNECT tunnel failed, response 503` from Squid, though it resolves fine off-cluster.
   Name resolution is an independent failure mode from egress, and must resolve to
   `indeterminate`, not `absent`.
3. **A 301/303 through the proxy is a real response**, not a proxy error. `opendata.aragon.es`
   and `dbpedia.org` redirect; the prober must follow or record redirects rather than treat
   them as failures.

## Delivery sequence

This design is too large for a single implementation plan. It decomposes into stages
that each end in something demonstrable, and each gets its own plan.

| Stage | Delivers | Gate before starting |
|---|---|---|
| **0. Egress spike** | Proof that a pod in an egress-locked ids3 namespace can run a SPARQL query against an arbitrary public endpoint through Squid | none, do this first |
| **1. Data model + prober core** | Metric definitions for an initial set, probe kinds, N-Quads output, tests against a mock endpoint. Runs locally, writes to a local Oxigraph. | stage 0 passes, or the prober is relocated outside the cluster |
| **1b. Declarations and fetch** | Service description fetch with a queryless GET, declaration parsing, verdict resolution against declarations. A service-description metric with graded levels 0-4. | stage 1 |
| **1c-b1** | **DELIVERED 2026-08-21**: Cost class on metrics, split the class metric, and the `not measured` record for declined metrics. See the plan file `docs/superpowers/plans/2026-08-21-safe-at-scale.md` for the superseded original plan and why it was split. | stage 1b |
| **1c-b2** | **DELIVERED 2026-08-21**: Per-host politeness (concurrency cap of 1, configurable `--min-gap-ms` between requests), `Retry-After` handling (delta-seconds within `--retry-after-cap-s`), and honest User-Agent. | stage 1c-b1 |
| **1c-b3** | **DELIVERED 2026-08-24**: Stable measurement identifiers and bounded concurrency across hosts. Every run-scoped fact's subject is derived from (run, endpoint, metric) rather than from a row index, so reordering the registry renames nothing and one endpoint's facts can be written alone; a metric id that would make a subject ambiguous is refused both at load and in the subject builder, a registry URL carrying userinfo is dropped with a warning, and a repeated (endpoint, metric) with conflicting facts publishes **nothing** about that pair rather than a contradiction. `--concurrency` (default 4) bounds how many **hosts** are talked to at once: endpoints group by host, one sequential task per group, so per-host concurrency stays at 1. A panicked group publishes `NotMeasured` with reason `prober-failed` for every metric it held, the activity publishes `sw:concurrency` and `sw:failedEndpoints`, and the run exits non-zero after writing its output. Measured on the three committed endpoints: 14.4s at `--concurrency 4` against 38.7s at 1, both producing 156 quads. This delivered only the **first half** of the per-endpoint isolation rule above. "Results are written per endpoint as they complete" was **not built here**: `run_sweep` joined every group before returning and the whole sweep was buffered and written once, so one endpoint burning its 600s budget still delayed the run's output by that much, and a crash lost the file. Concurrency shrank the constant, not the shape. Stage 1c-b4 built the missing half. | stage 1c-b2 |
| **1c-b4** | **DELIVERED 2026-08-24**: Crash-safe incremental writing, so output survives a crash at endpoint 500 of 548 and the second half of the per-endpoint isolation rule is met. A run is emitted as a header, one self-contained chunk per endpoint written and flushed as that endpoint completes, and a footer, each section closed by its own terminator quad on the run's activity: `sw:emission "incremental"` for the header, `sw:completedEndpoint <endpoint>` for a chunk, and `sw:finalised true` for the footer, which is the last line a finished run ever writes. No fact family publishes its own summary before the things it summarises, so `sw:sampleSize` follows its values and `sw:failedEndpoints` moved into the footer. A run in progress is written to `<out>.<at>.partial` and renamed onto `--out` at the end, so a crash cannot touch the previous complete run, and a retry sharing an `--at` is **refused** rather than overwriting the earlier attempt's partial. `web/load_run.py` cuts an incomplete file back to its last terminator line, matched in predicate position rather than by substring, and reports the bytes it discarded; a file corrupted before that line is still refused whole. The read tier says both "this run did not finish" and "a later run did not finish and never reached this endpoint", in HTML and in RDF, carrying the inputs to that derivation rather than a derived flag. As predicted, the file's order is now **completion** order, deliberately breaking the old input-order property; the `Sweep` returned in memory is still input order, and those are two different properties. Measured: a `SIGKILL` mid-sweep left a 25,634-byte partial that loaded as 114 quads with 0 discarded, marked unfinished, one completed endpoint with its 8 verdicts intact, and the previous `--out` byte-identical; a retry at the same `--at` exited 1 and left that partial byte-identical. The guarantee is bounded at process death: there is no `fsync` per chunk, so a power loss can still lose a flushed chunk, and the zeroed tail such a loss can leave is refused whole rather than rescued. | stage 1c-b3 |
| **1d. Registry seeding** | Ingest LOD Cloud + YummyData candidates, resolve front-ends to real endpoints, probe with politeness, admit responders. **PARTLY DELIVERED 2026-08-24** (stage 1d-a): ingest from LOD Cloud alone, plus the first registry-scale sweep. `seed::candidates` turns the 2026-06-15 dump (1683 datasets, 725 `access_url` entries, 548 distinct URLs) into **543 seeded candidates**, refusing 5 with a count per reason, and `seed-registry` writes `prober/registry/lod-cloud.toml` beside a parseable provenance file naming the dump, its SHA-256 (which the tool computes itself, refusing to write if `--sha256` disagrees) and every count. Endpoint-list policy has one implementation, because the seeder calls `registry.rs`, and the two refusals added there are split by the question each answers: `without_unroutable_hosts` in the seeder alone, since an operator may legitimately probe their own machine and this suite does so through wiremock, while `without_reserved_names` and `without_unpublishable_iris` are wired into `load_endpoints` so they hold whoever supplied the list. The dump's own `status` field gates nothing and a test pins that. The sweep: 543 candidates, cheap ceiling, `--concurrency 4`, **1h26m21s**, 3801 measurements, 0 failed endpoints, 6.3 MB, `finalised=true`, so 1c-b4's incremental protocol held at registry scale; 57 of 543 answered a query where the survey found 65 of 548, and **26** published a parseable service description (`service-description` `verified`; 30 returned some parseable RDF to the queryless GET, which is the weaker `declarationsRead` claim). Among the living, 24 of the 57 carry a description, 42.1%, against the survey's 28 of 65, 43.1%, so this sweep is slightly lower on both. Of the 26, **23 are at level 1, 2 at level 2 and one at level 4**, which is the survey's Virtuoso-stub finding reproduced by the graded metric. **Not built: front-end resolution, YummyData's list, and the admission policy**, so nothing yet stops the dead being re-probed and the registry is **not operable on a daily cadence**. The three deferrals with their reasons, the two items the measurement retired, and an answer to each of the four questions this row used to ask are under [Endpoint registry](#endpoint-registry). | stage 1c-b4 |
| **2. Scoring as queries** | Score computation as pure SPARQL/functions over stored measurements, with recomputation over history proven. **PARTLY DELIVERED 2026-08-22** (stage 2-1): Storage in Oxigraph is in place, and one read query (`endpoint_content.rq`) is implemented and tested. Score computation is not built. | stage 1b |
| **2b. Content metadata + examples** | Tiered VoID extraction, SIB example ingestion, `/.well-known/sparql-examples` discovery. **PARTLY DELIVERED 2026-08-22** (stage 2b-1): distinct classes are sampled and published as a `ContentSample` fact, deliberately not as VoID; see the tier-2 status note under [1b](#1b-content-metadata-extraction-tiered). Properties per class, counts, SIB ingestion, and example discovery are not built. | stage 1d |
| **3. Web read tier** | Faceted search, browse, endpoint pages, metric pages, charts, content negotiation, read-only public SPARQL endpoint. **PARTLY DELIVERED 2026-08-23** (stage 3-1): One endpoint resource served at `GET /endpoint?url=...` with content negotiation returning HTML or any of four RDF serialisations (Turtle, N-Triples, RDF/XML, JSON-LD), the HTML and the RDF agreeing on every verdict the run recorded. **MORE DELIVERED 2026-08-25** (stage 3-2): an index at `GET /` listing all 543 endpoints in one page of 424.6 KiB, grouped by the availability verdict's own values with a denominator on every count; `GET /about`, the page the prober's `User-Agent` points at, saying who is querying, how often, how politely, why that endpoint, and how to ask to be left alone; and all three read paths moved onto the derived `urn:sparqlwatch:current` graph, which is what makes a whole-registry page a flat scan. All three resources negotiate. Leaderboard, per-metric pages, history, evidence per measurement, embedded query editor, and read-only public SPARQL endpoint are still not built. **Faceted search is blocked rather than unbuilt**: faceting by vocabulary or class needs content data, and stage 2b has produced no vocabulary or property data and one class sample per endpoint at best, since `sw:metric:classes` is declined at the default cost ceiling. | stage 2, 2b |
| **3b. Embedded editor** | `@sib-swiss/sparql-editor` per endpoint, fed autocomplete metadata from our origin | stage 2b, 3 |
| **4. ids3 deployment** | `sparqlwatch-dev` project-env: prober CronJob, web, Oxigraph, ingress, egress policy | stage 3 |
| **5. Submissions and moderation** | Public submission with endpoint validation, moderation queue, rate limiting; example contribution shares this path | stage 4 |
| **6. Discussion threads** | Per-endpoint threads for consumer-to-provider issues | stage 5 |

Stage 0 is a spike whose output is an answer, not code. Stages 1 through 3 are the
minimum for a service worth showing anyone. Stages 5 and 6 are what make it a community
service rather than a dashboard, but they add the only untrusted input paths, so they
come after the read side is solid.

**Stage 1b and beyond: dependency-driven splitting.** Stage 1b (declarations and fetch)
depended on nothing beyond stage 1, while stages 1c and 1d (politeness and seeding)
depend on infrastructure that 1b does not use. The split reflects the dependency
order rather than arbitrary chunking, so each stage builds directly on what came
before.

## Conformance model

**No composite score and no ranking.** Both were rejected deliberately: a number cannot
express "we could not determine this", and the evidence below shows that is the single
most common honest answer.

### Why, from measured evidence

A survey of the LOD Cloud dump (`~/code/umaka-test`, 2026-06-15 release, 548 distinct
endpoint URLs) found that **declaration and behaviour are almost entirely decoupled**:

- **0** endpoints advertise GeoSPARQL through `sd:feature` or `sd:extensionFunction`,
  while **18** demonstrably evaluate `geof:sfWithin` correctly.
- **9** endpoints return the *wrong* answer to a point-in-polygon filter that a
  conformant engine must answer `true`. That is a distinct failure from absence.
- **23 of 28** service descriptions claim SPARQL 1.0 only, while many of the same
  endpoints answer SPARQL 1.1.
- **21 of 28** service descriptions are byte-identical 14-triple Virtuoso stubs, so a
  binary "publishes a service description" credits the *engine*, not the publisher.
- Virtuoso rewrites `DATATYPE(?g)` to `virtrdf#Geometry` for 100% of literals, so
  `geo:wktLiteral` conformance **cannot be tested at all** through three of the four
  real geospatial endpoints. A naive checker marks them non-conformant when the
  underlying data may be fine.

### Verdicts, not values

Every attribute resolves to one verdict from a fixed vocabulary. The vocabulary exists
because each state was observed in the survey.

| Verdict | Meaning | Observed example |
|---|---|---|
| `verified` | A probe confirms it works, and where a declaration is possible, the endpoint declares it | 18 endpoints evaluating `geof:sfWithin` |
| `undeclared-but-verified` | Works, and the endpoint could have declared it but did not | the same 18, none of which mentions geo |
| `declared-but-wrong` | Claimed or bound, but behaves incorrectly | the 9 answering `false` |
| `declared-only` | Claimed, not confirmable by probe | e.g. entailment regimes |
| `absent` | Neither claimed nor observed | |
| `indeterminate` | The engine or the budget prevents an answer | Virtuoso datatype rewriting; 90s aggregate timeouts |

`declared-but-wrong` ranks as worse than `absent`, because a false claim misleads a
client that trusts it. `indeterminate` is a required outcome, never a silent zero.

The declared/observed axis applies only where a declaration is possible: only an
attribute for which some term in the service-description vocabulary could speak
(a metric carrying a `declared_by`) can resolve to `undeclared-but-verified`.
For liveness, CORS headers or a class count there is no such term, so
"undeclared" would say nothing about the endpoint, and a confirming probe
resolves to `verified` on its own. Keeping those attributes out of
`undeclared-but-verified` is what keeps that verdict readable as the finding it
records, that declaration and behaviour are decoupled.

### Graded levels where binary misleads

Some attributes are a degree, not a yes/no. Service-description informativeness is the
worked example, graded by what it actually tells a client:

| Level | Criterion |
|---|---|
| 0 | none served |
| 1 | stub: endpoint, result formats, supported language (the Virtuoso default) |
| 2 | names a default dataset or graphs |
| 3 | carries VoID class or property partitions |
| 4 | declares an entailment regime, example resources, or extension functions |

The same shape applies to VoID completeness. This is what "degree of conformance"
means where it applies.

### Task-first presentation

The primary surface asks what the user needs the endpoint to *do*, then answers with
endpoints whose verdicts satisfy it. Tasks are defined as verdict predicates over
attributes, so they are data and not code:

- "Run geospatial queries" = geo functions `verified` AND geometry data `verified`
- "Autocomplete in an editor" = CORS `verified` AND (VoID level >= 3 OR sparqlwatch-derived)
- "Federate with X" = `sd:BasicFederatedQuery` verified, or a `spex:federatesWith` example exists

The verdict matrix (endpoints x attributes) is the detail view behind that, not the
front door. Ranking is deliberately absent from both.

### Probe rules this forces

- **Two independent probes per capability**: a data-free filter tests function binding;
  an `ASK` over data tests presence. In the survey 18 endpoints have the functions, 4
  hold usable geometry, and only 3 do both.
- **Guard against false positives.** `publications.europa.eu` passes a naive
  `ASK { ?s geo:asWKT ?g }` while every object is `rdf:nil`, so literal probes need an
  `isLiteral` guard. One endpoint's entire geospatial content is a single test triple.
- **Never gate a test on a declaration**, since declarations under-report.
- **A timeout is `indeterminate`**, never `absent`.

## Endpoint registry

Seeded from two sources, then probed before admission:

1. The **LOD Cloud** dump: 1683 datasets, 713 declaring an endpoint, 548 distinct URLs.
2. **YummyData's** curated biomedical list.

Only URLs that answer a trivial query are admitted as monitored endpoints. The rest are
retained as an `unreachable-candidates` list, which is data worth publishing but is not
re-probed daily. This matters at the observed rates: **only 65 of 548 LOD Cloud URLs
(11.9%) answer at all**, so importing everything would leave a directory that is ~88%
tombstones and waste most of every sweep.

Two resolution steps are mandatory before admission, both from measured failure modes:

- **114 URLs return HTTP 200 with HTML** to a queryless GET. Treat an HTML response as a
  query front-end rather than a protocol endpoint, and resolve the real protocol URL
  first: **some** of those hosts do have a working endpoint at a different path
  (`SURVEY.md:215-218`, which is where this recommendation comes from), and it is not a
  corner case, since it happened with all three endpoints hand-supplied during evaluation.
  What was measured is the 114 and the "some". How many of the 114 have a resolvable
  endpoint is not measured, and stage 1d-a did not build the resolution step, so this
  remains a recommendation rather than a delivered capability.
- **472 of 548 URLs are still plain `http://`**, which is itself a decay signal and worth
  recording rather than silently upgrading.

The LOD Cloud's own `status` field is not a usable liveness oracle: it disagreed with
observation in both directions, marking 10 responders FAIL and 54 non-responders OK.

**Stage 1d-a status, 2026-08-24: 1d is partly delivered.** Ingest from LOD Cloud alone,
plus the first registry-scale sweep of what it produced. See `prober/README.md`'s "The
seeded registry" and "Sweep cost" sections for the detail, and
`docs/superpowers/plans/2026-08-24-seed-the-registry.md` for the plan. Built:
`seed::candidates` turns the 2026-06-15 dump into **543 seeded candidates**, refusing 5 of
its 548 distinct URLs with a count per reason; `seed-registry` writes
`prober/registry/lod-cloud.toml` beside a parseable provenance file naming the dump, its
SHA-256 and every count; endpoint-list policy has one implementation, since the seeder calls
`registry.rs`; the dump's `status` field gates nothing and a test pins that; and one sweep
of all 543 ran in **1h26m21s** at `--concurrency 4` under the cheap ceiling, producing 3801
measurements, 0 failed endpoints and `finalised=true`, so stage 1c-b4's incremental protocol
held at registry scale.

**Three deferrals, with their reasons.**

1. **YummyData's candidate list.** It lives in that application's database rather than in a
   checked-in file: `~/code/umakadata/db/migrate/20190904034259_create_endpoints.rb`
   creates the table and nothing in that repository carries the rows. Acquiring it needs a
   running instance or a dump, which is its own task, so this slice seeds from LOD Cloud
   alone and says so.
2. **Front-end resolution.** Turning the 114 HTML responders above into endpoints is a
   judgement capability with its own failure modes: guessing a path, following a form
   action, mistaking a console for an endpoint. None of it is built.
3. **The admission policy and the `unreachable-candidates` list.** What makes a daily sweep
   affordable is not re-probing the dead, and that policy should be written against the
   first sweep's real numbers rather than ahead of them. Those numbers now exist, which is
   what makes this the next slice: **486 of 543 answered nothing**, and the 43 candidates
   the dump had marked timed-out cost **two thirds of the sweep's serial probe time**,
   2.28 of its 3.46 serial hours. Until it is built the
   seeded registry is **not operable on a daily cadence**, because nothing stops the dead
   being re-probed on every sweep.

**Two things the measurement retired.**

1. The **scheme allowlist** deferred to this stage is unnecessary **for this dump**, which
   holds only `http` (472) and `https` (76) and nothing else. Not retired in general: a
   later source may differ.
2. The **credential refusal** from stage 1c-b3 fires on nothing here, since this dump
   carries no credentialed URL. It stays, because stage 5 accepts public submissions.

**The four questions the 1d row asked, each answered or explicitly not.**

1. **An endpoint that keeps its data in named graphs.** NOT ANSWERED, and the deferral
   stands. `classes` is `expensive`, so the cheap sweep never ran it, and
   `prober/tests/live_smoke.rs:52-62` records the shipped query passing with the `GRAPH`
   branch deleted, so a passing query would prove nothing about that branch anyway. What
   the sweep produced is a list for a dedicated verification: 18 candidates among the
   responders, six of them named in the stage ledger, `dbpedia.org`, `data.bnf.fr`,
   `data.cervantesvirtual.com`, `dati.camera.it`, `ldf.fi/warsa` and `ldf.fi/ww1lod`.
   That 18 is not re-derivable from the preserved run, since `classes` was declined 543
   times; it points at those six endpoints rather than measuring anything.
2. **How often a cross-host redirect really queues at another host's gate.** CANNOT BE
   ANSWERED from this run, and no rate may be inferred from it. The hop-level line logs at
   DEBUG (`prober/src/client.rs:353`) and the run was at `info`, so the run's count of
   cross-host hops is zero for the wrong reason. What the run does show is a different
   fact: 14 chains looped and were not resolved. Answering the question needs a run at
   `RUST_LOG=debug` or a counter on the client.
3. **Whether a seeded URL can be told from a credentialed one.** For this dump the question
   does not arise: it contains **no credentialed URL at all**, so `without_credentials`
   refused 0 of the 548. That is recorded rather than dropped precisely because it was
   asked, and it is a property of this dump and not of seeding in general. The refusal
   stays for stage 5, and the query-string case (`?apikey=...`) is untouched and still
   open.
4. **Whether a chunk per endpoint wants an `fsync`.** Now measured, and the measurement
   **reverses this project's own argument**. `prober/README.md` had said that the `fsync`
   calls "cost something on the deployment's volume that nobody here has measured" and used
   that as part of why there is none. Measured: 543 chunks over a 5181-second sweep, so
   even at a pessimistic 10 ms each the whole sweep pays 5.4 seconds, about 0.1% of its
   wall clock. **Cost is no longer the argument.** Whether to `fsync` is a
   durability-versus-simplicity call and has to be argued on that basis. Nothing in the
   code changed.

One finding qualifies the paragraph this note follows. The `status` field is a bad
**liveness** oracle, which is what that paragraph says and what the sweep confirms, and at
the same time a good **cost** predictor for one bucket, since its 43 timed-out candidates
accounted for 66% of the sweep's serial cost. Both are true, and this project gates on the field in
neither direction.

## Deliberately out of scope for v1

- Offline dataset download and analysis (umakadata's TripleDataProfiler equivalent).
  It needs a JVM today, it is the least reliable part of that system, and it is not
  needed for a credible first release.
- Federated query testing across endpoints.
- Authenticated or paywalled endpoints.
- **A query proxy.** See [Query editor and autocomplete](#1c-query-editor-and-autocomplete)
  for why, and for what replaces it.
- Comparability with YummyData's umaka score. Different metric model by choice.

## Open decisions

1. **The task list.** Three are sketched (geospatial querying, editor autocomplete,
   federation). The full set needs the owner's input, since tasks are the front door.
2. **Attribute set beyond the tool-readiness core.** Availability, response time, CORS,
   service-description level, VoID level, published examples, content negotiation,
   cool URIs, GeoSPARQL function support, GeoSPARQL data presence. umakadata's
   `criteria/` is a usable reference for several detection routines, and
   `~/code/umaka-test` already has working probes for the geo ones.
3. ~~**Public domain name**~~ **Decided:** `https://sparqlwatch.dev.k8s.semanticscience.org`.
   It sits under the institutional domain, and the `dev` label matches the
   `sparqlwatch-dev` project-env in stage 4. Two consequences: the prober's
   `User-Agent` points at `/about` on that host, so **stage 3 owed an `/about`
   page** explaining who is probing and how to ask us to stop, and stage 4's
   ingress host is fixed rather than open. The page was **delivered 2026-08-25**
   (stage 3-2), with every figure on it read out of the prober's own source by a
   test. Two things it cannot say yet, recorded under known gaps in
   `web/README.md`: no person or institution is named as the operator, and no
   repository URL exists anywhere in this project, so the page's invitation to
   check the public exclusion list names no place to check it.

## Appendix A: why not umakadata

Recorded because the decision should be reviewable, and because the evidence was
gathered by actually running the system rather than reading it.

**What was measured.** 4,975 lines of Rails app (2,447 Ruby, 1,589 Slim, 617 JS) plus a
3,039-line crawler gem. Test suite: 13 examples, 1 failure, 11 generator stubs. No CI
(`.github` does not exist). Dependency automation dead, using retired Dependabot v1
config. Runtime: Ruby 2.6.3, Rails 5.2, Node 14, Postgres 11, Redis 5, Debian buster,
**every one end-of-life**.

**Defects found in a few hours of ordinary use.** The project did not build at all
(EOL buster apt sources, then a hardcoded `linux-x64` Node URL, then nokogiri's
aarch64 gem needing a newer glibc than buster ships). The dashboard 500'd on an empty
database by comparing nil with `>`. Two scripts blocked rendering for 1.5 to 2.9
seconds. A nil-pattern bug in the gem's VoID publisher extraction logs an error on
every endpoint. Fixes for these are upstream as dbcls/umakadata#102.

**Why the crawler gem does not rescue it.** The gem is the valuable part, but its value
is `criteria/`, the metric implementations, which is exactly what gets replaced when
defining our own metrics. What remains is HTTP and SPARQL plumbing, better served by
`reqwest` and `pyoxigraph` in the chosen stack. The gem also pins
`activesupport >= 5.2, < 7.0`, so it blocks modernizing the very stack it sits in.

**The decisive structural argument.** Custom metrics fight umakadata's design. Metrics
are columns in a 26-column `evaluations` table and hardcoded names in
`Evaluation#scores`. Notably its `measurements` table *is* generic (`name`, `value`),
so the crawler already emits generic measurements and the app deliberately flattens
them. Building on it would mean fighting that decision indefinitely.

**What is kept.** The fork stays as a working reference implementation for the fiddly
detection logic (VoID, service descriptions, cool URIs, content negotiation), and as a
baseline to compare our own measurements against. PR #102 is worth landing regardless.

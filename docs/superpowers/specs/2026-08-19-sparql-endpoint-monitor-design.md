# SPARQL endpoint monitoring service: design

**Status:** draft for review
**Date:** 2026-08-19
**Working name:** `sparqlwatch` (placeholder, see Open decisions)

## Purpose

A public service that continuously monitors SPARQL endpoints for a general
(non-biomedical) community, publishes quality measurements about them, and lets
data consumers raise issues with providers.

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

Non-negotiable constraints from the environment:

- **No JVM anywhere.** This rules out reusing TripleDataProfiler and SPARQLES.
- **ids3 pods have no direct internet.** All external egress must traverse
  `http://egress-proxy.platform.svc.cluster.local:3128` (Squid). A monitor whose
  entire job is reaching the public web makes this the single highest project risk;
  see [Risks](#risks).

## Core architectural idea

**Separate measurement from scoring.**

Store immutable, append-only *measurements*. Compute *scores* as a query over them.

umakadata conflates the two: its crawler writes one column per criterion into a
26-column `evaluations` table and computes the score in a `before_save` hook. The
consequence is that changing a metric definition or a score weighting requires a
schema migration and invalidates history, because past scores cannot be recomputed
from what was stored.

Separating them buys three things that matter directly given the owner wants to
define and evolve their own metrics:

1. A metric definition can change and every historical score can be recomputed
   without re-probing a single endpoint.
2. Adding a metric is publishing a definition plus a probe, not migrating a table.
3. Consumers can disagree with our weighting and compute their own from the same
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

A derived `urn:sparqlwatch:current` graph holds the latest measurement per
(endpoint, metric) so the UI's common queries stay cheap as history grows. It is
rebuilt after each run, never hand-edited.

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
  as they complete.
- **Per-host politeness**: concurrency cap of 1 per host, configurable delay, a real
  `User-Agent` naming the service with a URL, and honouring `Retry-After`.
- **Proxy support is mandatory**, configured by `HTTP_PROXY`/`HTTPS_PROXY`, because of
  the ids3 egress constraint.
- **A probe never writes a score.** It writes a measurement and its evidence.

Probe kinds are a closed set (an HTTP check, a SPARQL ASK, a SPARQL SELECT with a
result assertion, a URI dereference check, a content-negotiation check). A metric
definition names a probe kind and its parameters. This is what makes metrics
declarative: new metrics that fit an existing kind need no Rust changes.

### 2. Storage (Oxigraph)

Its own instance for this service, deployed in the project's namespace with a PVC.
Not shared with the BioPortal instance, to keep the public service's blast radius and
backup story separate. It exposes a **read-only public SPARQL endpoint** over the
observations, which is both good practice for this kind of service and free given the
data model.

### 3. Web tier (Python / FastAPI)

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
| Squid egress blocks or throttles arbitrary-host `CONNECT`, so the prober cannot reach endpoints at all | **Project-ending** | Spike it before writing any code. See below. |
| Scope creep: monitoring dashboards are deceptively large | High | First release is the leaderboard, endpoint pages, metric pages and submissions. Threads can follow. |
| Probing looks like abuse to endpoint operators | Medium | Per-host concurrency of 1, delays, honest User-Agent with a contact URL, honour `Retry-After`, published probe schedule |
| Very large datasets make some metrics intractable, as osm-planet did | Medium | Metrics declare a cost class; expensive ones are opt-in per endpoint and record "not measured" rather than a misleading zero |
| Duplicating a live service (SPARQLES or a successor) | Medium | Survey prior art before building. Not yet done. |

The egress risk deserves emphasis. The prober's entire function is reaching arbitrary
public hosts from inside a namespace whose NetworkPolicy permits only DNS,
cluster-internal traffic, and the apiserver. If Squid will not pass `CONNECT` to
several hundred arbitrary HTTPS hosts, the options narrow to running the prober
outside the cluster and shipping observations in. **This must be settled first.**

## Delivery sequence

This design is too large for a single implementation plan. It decomposes into stages
that each end in something demonstrable, and each gets its own plan.

| Stage | Delivers | Gate before starting |
|---|---|---|
| **0. Egress spike** | Proof that a pod in an egress-locked ids3 namespace can run a SPARQL query against an arbitrary public endpoint through Squid | none, do this first |
| **1. Data model + prober core** | Metric definitions for an initial set, probe kinds, N-Quads output, tests against a mock endpoint. Runs locally, writes to a local Oxigraph. | stage 0 passes, or the prober is relocated outside the cluster |
| **2. Scoring as queries** | Score computation as pure SPARQL/functions over stored measurements, with recomputation over history proven | stage 1 |
| **3. Web read tier** | Leaderboard, endpoint pages, metric pages, content negotiation, read-only public SPARQL endpoint | stage 2 |
| **4. ids3 deployment** | `sparqlwatch-dev` project-env: prober CronJob, web, Oxigraph, ingress, egress policy | stage 3 |
| **5. Submissions and moderation** | Public submission with endpoint validation, moderation queue, rate limiting | stage 4 |
| **6. Discussion threads** | Per-endpoint threads for consumer-to-provider issues | stage 5 |

Stage 0 is a spike whose output is an answer, not code. Stages 1 through 3 are the
minimum for a service worth showing anyone. Stages 5 and 6 are what make it a community
service rather than a dashboard, but they add the only untrusted input paths, so they
come after the read side is solid.

## Deliberately out of scope for v1

- Offline dataset download and analysis (umakadata's TripleDataProfiler equivalent).
  It needs a JVM today, it is the least reliable part of that system, and it is not
  needed for a credible first release.
- Federated query testing across endpoints.
- Authenticated or paywalled endpoints.
- Comparability with YummyData's umaka score. Different metric model by choice.

## Open decisions

1. **Name.** `sparqlwatch` is a placeholder. A public service's name is the owner's
   call.
2. **The metric set itself.** The architecture makes metrics declarative precisely so
   this can be settled later, but v1 needs an initial set. Availability, response time,
   service-description presence, VoID presence, content-negotiation support and cool-URI
   conformance are the well-understood candidates, all of which umakadata's `criteria/`
   can be read as a reference implementation for.
3. **Which community, and hence which endpoint list**, seeds the registry.
4. **Public domain name** and whether it sits under an institutional domain.

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

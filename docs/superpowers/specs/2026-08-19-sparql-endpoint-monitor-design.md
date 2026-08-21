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
| Probing looks like abuse to endpoint operators | Medium | Per-host concurrency of 1, delays, honest User-Agent with a contact URL, honour `Retry-After`, published probe schedule |
| Very large datasets make some metrics intractable, as osm-planet did | Medium | **DELIVERED 2026-08-21** (Stage 1c-b1): Metrics declare a cost class; expensive ones are opt-in per endpoint and record "not measured" rather than a misleading zero. See stage 1c-b description. |
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
| **1c-b2** | Per-host politeness and `Retry-After` handling | stage 1c-b1 |
| **1c-b3** | Stable measurement identifiers and bounded concurrency with deterministic output | stage 1c-b2 |
| **1c-b4** | Crash-safe incremental writing | stage 1c-b3 |
| **1d. Registry seeding** | Ingest LOD Cloud + YummyData candidates, resolve front-ends to real endpoints, probe with politeness, admit responders | stage 1c-b4 |
| **2. Scoring as queries** | Score computation as pure SPARQL/functions over stored measurements, with recomputation over history proven | stage 1b |
| **2b. Content metadata + examples** | Tiered VoID extraction, SIB example ingestion, `/.well-known/sparql-examples` discovery | stage 1d |
| **3. Web read tier** | Faceted search, browse, endpoint pages, metric pages, charts, content negotiation, read-only public SPARQL endpoint | stage 2, 2b |
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

- **114 URLs return HTTP 200 with HTML**, i.e. a query front-end rather than a protocol
  endpoint. The real endpoint often lives at a different path. This is not a corner case:
  it happened with all three endpoints hand-supplied during evaluation.
- **472 of 548 URLs are still plain `http://`**, which is itself a decay signal and worth
  recording rather than silently upgrading.

The LOD Cloud's own `status` field is not a usable liveness oracle: it disagreed with
observation in both directions, marking 10 responders FAIL and 54 non-responders OK.

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
   `User-Agent` points at `/about` on that host, so **stage 3 owes an `/about`
   page** explaining who is probing and how to ask us to stop, and stage 4's
   ingress host is fixed rather than open.

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

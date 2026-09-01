# Content profiles: design

**Status:** draft for review
**Date:** 2026-08-29
**Supersedes:** nothing. Completes tier 2 of
[1b. Content metadata extraction (tiered)](2026-08-19-sparql-endpoint-monitor-design.md)
in the founding spec, whose tier-2 status note of 2026-08-22 lists "properties
per class, and counts" as not built. This is that work.
**Answers:** the open question the founding spec leaves at the end of 1b,
"whether derived VoID is still worth building alongside the sample, rather than
instead of it".

## Purpose

For each class an endpoint exposes, publish which properties its instances
actually carry and how often, so that a query editor can autocomplete against
an endpoint that describes itself badly or not at all.

The founding spec's second promise is "host the metadata it fails to publish.
No VoID, so we derive and serve one." That promise has never been kept, and
until 2026-08-29 nobody had measured whether it needed keeping.

## The evidence this design rests on

Three measurements, all reproducible from artefacts kept outside the repository
in `~/code/sparqlwatch-runs/`. None of them is an estimate.

### Almost nobody publishes content-level VoID

Sweep `run-2026-08-29T14-10-00Z-void-coverage-57.nq`, the 57 endpoints whose
newest sweep recorded availability `verified`, three bounded questions each,
171 measurements, 0 failed endpoints.

| tier | endpoints | distinct hosts |
|---|---|---|
| names a `void:Dataset` at all | 21 | 17 |
| names VoID **class** partitions | 4 | 3 |
| names VoID **property** partitions | 3 | **2** |

The two hosts are `linkeddata.uriburner.com` and `sparql.uniprot.org`. UniProt
is SIB Swiss, who wrote `void-generator`. So content-level VoID exists on two
distinct services out of 56, one of them the tool's own authors.

This settles tier 1 as a *source*: fetching what the endpoint publishes is
still worth doing, and its presence is still worth reporting as a metric, but
it will supply content metadata for approximately nobody. Deriving is not a
fallback. It is the primary path.

It also sizes the verification idea, for the record: UniProt's VoID is 55
classes and 32 properties, neither truncated. A complete first-party content
declaration is around 90 terms and entirely cheap to verify. The obstacle was
never that declarations are too big to check. It is that they do not exist.

### `shexer` has the right algorithm and an unusable transport

Benchmarked against a graph whose shape is authored, so precision and recall
have a ground truth rather than a judgement. Harness and query logs in
`~/code/sparqlwatch-runs/shexer-spike-2026-08-29/`; no third-party endpoint was
involved.

- **Uncapped, the algorithm is correct**: 100% precision and 100% recall at
  `acceptance_threshold=0.9`, keeping the 0.95 and 0.90 properties and dropping
  the 0.80, 0.70, 0.50, 0.30 and 0.05 ones.
- **Cost, fitted exactly across four scenarios**:
  `queries = SUM over classes of min(instances, cap) + 2 per class`. One HTTP
  round trip per instance, `SELECT ?p ?o WHERE { <instance> ?p ?o }`, no
  batching. The seed query is issued twice.
  Projected on class counts this project has actually sampled: dbpedia at 200
  classes and cap 500 is 100,400 queries, 16,733 times the per-endpoint sweep
  budget, 55.8 hours for one endpoint at the 2-second minimum gap the prober
  enforces. And 200 is a truncated class list.
- **Every datatype is wrong**: six declared datatypes, six re-inferred from the
  lexical form and six wrong (`gYear` to integer, `gMonthDay` to string,
  `token` to string, `anyURI` to IRI, `decimal` to float, `byte` to integer).
  The instrument was verified to serve the correct datatype in the response.
- **`instances_cap` fails silently**: the 500 instances sampled from 2,000 were
  index 1500..1999 exclusively, because `LIMIT` without `ORDER BY` returns a
  contiguous block in store iteration order. `foaf:mbox` measured 80.0% against
  a true 95.0%, fell below the threshold, and was omitted with no indication. A
  confident false negative, not an incomplete shape.

`shexer` is therefore adopted as an **offline reference implementation** to
validate our own profiles against, run over dumps, and rejected as a probe.

### One query per class gets the same answer

Same harness. A grouped count over a bounded instance subquery reproduces
`shexer`'s decisions exactly: five of five frequencies agree to three decimals
and `foaf:mbox` comes out at 0.800 as well, so it is dropped identically, bias
included. Six queries rather than 1,306. Uncapped it scores 11 of 11 correct
**including every datatype**, because `DATATYPE(?o)` is evaluated by the
endpoint instead of guessed from a string.

### How to sample, measured rather than assumed

On one class of 200,000 instances whose property frequencies vary with position
in the id space, so that a scan-order slice is unrepresentative in a way the
numbers reveal:

| strategy | cost | sample | max error |
|---|---|---|---|
| whole class, no sampling | 209 ms | 200,000 | **0.000** |
| hash prefix `0` (1/16), no LIMIT | **85 ms** | 12,554 | **0.002** |
| hash prefix `00` (1/256), no LIMIT | 67 ms | 777 | 0.031 |
| hash prefix `000` (1/4096), no LIMIT | 68 ms | 60 | 0.083 |
| `ORDER BY SHA256(STR(?s))` + `LIMIT 500` | 413 ms | 500 | 0.026 |
| `LIMIT 500` alone, which is `shexer`'s | 1 ms | 500 | **0.950** |

Two findings, both structural rather than particular to pyoxigraph:

1. **The scan is the floor and the sort is pure cost on top of it.** Cost is
   nearly flat across prefix lengths (85, 67, 68 ms) because the filter still
   evaluates the hash for every instance; only the result set shrinks. Sorting
   cost twice a full exact scan and returned a worse answer than the full exact
   scan. An earlier draft of this design proposed `ORDER BY SHA256`; the
   measurement killed it.
2. **`LIMIT` is what reintroduces the bias**, so the sample size must be chosen
   by the hash prefix and not cut afterwards. The hash-prefix strategy with a
   `LIMIT` still bolted on measured 0.076 error; without it, 0.002.

## Ruling 1: how a sample is drawn

**A profile query samples with a SHA256 prefix filter on the subject IRI and no
`LIMIT`, and never with `ORDER BY`.**

Rationale is the table above. Cost if wrong: on a store that cannot push the
hash evaluation down, the filter scan costs what a full scan costs and we gain
only a smaller response body. That is a small loss, not a failure, and it is
the same order as the full-scan path we would otherwise have taken.

## Ruling 2: a profile is not a measurement, and carries no threshold

**A frequency profile is published as a new fact kind, `ContentProfile`, with
no `dqv:value` and no verdict. We publish frequencies and denominators, and we
do not threshold at all.**

This is the load-bearing decision in the design and it has two halves.

The first half follows precedent. `ContentSample` already exists as a
non-verdict fact kind for exactly this reason, and the founding spec's 1b
explains at length why it is deliberately not VoID: `void:classPartition` and
`void:class` both carry `rdfs:domain void:Dataset`, so reusing either would
entail under plain RDFS that the observation IS a dataset, which cannot be
honestly asserted about an arbitrary endpoint. That reasoning applies verbatim
to a profile, and this project has already shipped that class of defect once,
when `NotMeasured` reused `dqv:computedOn`.

The second half is new and is the lesson of the `shexer` benchmark.
Thresholding is how a 95% property became a silent absence: `shexer` collapsed
a frequency, a sample size and a sampling method into a binary decision, and
published only the decision. The six-verdict vocabulary cannot absorb a
thresholded frequency without acquiring the same defect, because a verdict is a
claim about what was observed and a threshold is an inference from a sample.

So the profile publishes the inputs and leaves the inference to the consumer:
the property, the subject count, the denominator, the sampling method and its
prefix, and the datatypes seen. A reader can recompute any frequency and can
see the sample size it rests on. An autocomplete consumer that wants a 0.9 cut
applies it itself, with the denominator in hand.

This also disposes of the sample-size problem without a rule. The measured
error tracks the sample size as expected of sampling error (0.002 at n=12,554,
0.031 at n=777, 0.083 at n=60), so a small denominator is visible in the
published fact rather than hidden behind a threshold decision made on the
reader's behalf.

## The probe

One query per class. The `rdf:type` group row supplies the denominator, so no
separate count query is needed; this is the shape actually measured at 0.002
error, not a refinement of it.

```sparql
SELECT ?p (COUNT(DISTINCT ?s) AS ?subjects)
          (COUNT(DISTINCT ?dt) AS ?datatypes)
          (SAMPLE(?dt) AS ?anyDatatype)
WHERE {
  { SELECT ?s WHERE {
      ?s a <CLASS> .
      FILTER(STRSTARTS(SHA256(STR(?s)), "PREFIX"))
  } }
  ?s ?p ?o .
  BIND(IF(isIRI(?o), "IRI", DATATYPE(?o)) AS ?dt)
}
GROUP BY ?p
```

`COUNT(DISTINCT ?s)` and not `COUNT(*)`, so a multi-valued property cannot push
a frequency above 1.0. `COUNT(DISTINCT ?dt)` beside a `SAMPLE`, so a property
carrying mixed datatypes is visible as such rather than silently reduced to
whichever one the store returned first.

`PREFIX` empty means no sampling and an exact profile, which the fact records.

### The fallback ladder

Tier 3 of the founding spec applies unchanged: giving up is a required outcome,
not an embarrassment. The ladder is attempted in order and never retried harder
than it ends:

| step | prefix | what it means |
|---|---|---|
| 1 | none | exact profile over every instance of the class |
| 2 | `0` | approximately 1/16 of instances, marked sampled |
| 3 | `00` | approximately 1/256, marked sampled |
| 4 | give up | `NotMeasured`, naming the reason. Never a zero, never `absent` |

A refusal is not a timeout and must not be recorded as one. dbpedia refused a
plain property enumeration in 44 ms, which is a policy decision by the operator
rather than a budget exhaustion, and the distinction is already carried by the
existing verdict rules.

## Cost, and the budget this has to be charged against

An earlier draft of this section said the limit was 6 requests per endpoint per
sweep and concluded that covering one endpoint would take 21 weeks. That was
wrong, and the error is worth recording because it points at the real
architectural requirement.

`POLITENESS["requests-per-endpoint"] = 6` is **descriptive, not a cap**: its
own comment in `web/app.py` says it is "one per metric that is cheap at the
default cost ceiling, counted from `prober/metrics.toml`". It reports what the
current sweep happens to consume. Nothing enforces it.

The enforced limits are in `prober/src/budget.rs`, and they are wall clock:

| budget | value | enforced by |
|---|---|---|
| request | 30 s | `reqwest`'s own `.timeout(budget.request)`, `client.rs:153` |
| metric | 60 s | `Budget::with_metric_budget` |
| endpoint | 600 s | `Budget::with_endpoint_budget` |

Plus a 2-second minimum gap between consecutive requests to one host, with
requests to one host never in flight together.

So the ceiling depends entirely on **which budget the work is charged to**, and
that is a design decision rather than a constraint:

- **As a single metric**, profiling gets the 60-second metric budget. At the
  2-second gap that is about 30 requests, so about 30 classes. dati.camera.it
  has 105 sampled classes and dbpedia at least 200, so a single metric cannot
  cover either.
- **Charged to the 600-second endpoint budget**, the same arithmetic gives
  roughly 200 classes per endpoint per sweep at one-second queries, and about
  270 at fast ones. That covers dati.camera.it's 105 classes in a single sweep
  and reaches dbpedia's (truncated) 200 at the edge of one.

**This is the architectural requirement:** a profile pass is not shaped like the
existing metrics. Every current metric is one declared question with one
bounded answer, which is why a 60-second metric budget fits it. A profile pass
fans out over classes **discovered at runtime**, so its work cannot be declared
in `metrics.toml` and its cost cannot be known before the class list comes
back. It has to be budgeted at the endpoint level and be cancellable
mid-fan-out, surrendering whatever it has completed rather than losing the lot.

That also decides what a partial pass publishes. Profiles for the classes that
finished are published as profiles; the classes not reached are published as
`NotMeasured` naming budget exhaustion. A class not reached is never an absent
profile, for the same reason tier 3 reports `not measured` rather than a zero.

With the endpoint budget as the ceiling, round-robin class coverage across
sweeps stops being a necessity and becomes a fallback for the endpoints whose
class lists exceed one pass. Per-class freshness still has to be published,
because on those endpoints different classes will have been profiled in
different sweeps.

## Data model

A new fact kind beside `ContentSample`, sparqlwatch-owned predicates
throughout, for the reason given in Ruling 2.

```
<urn:sparqlwatch:profile:RUN:ENDPOINT:METRIC:CLASS>
    a                        sw:ContentProfile ;
    sw:profiledFrom          <endpoint> ;
    sw:profiledBy            sw:metric:<id> ;
    sw:profiledClass         <class> ;
    sw:profileDenominator    12554 ;          # subjects in the sample
    sw:profileSampling       "sha256-prefix" ; # or "exact"
    sw:profileSamplingPrefix "0" ;             # absent when exact
    sw:profileProperty       <urn:sparqlwatch:profileprop:RUN:ENDPOINT:METRIC:CLASS:PREDICATE> .

<urn:sparqlwatch:profileprop:RUN:ENDPOINT:METRIC:CLASS:PREDICATE>
    sw:property              <predicate> ;
    sw:subjectCount          11897 ;
    sw:datatypeCount         1 ;
    sw:anyDatatype           xsd:string .
```

Subject IRI derivation follows `subject_iri` in `prober/src/emit.rs`, extended
with the class, and inherits its constraint that every component be reversibly
encoded so two runs can be diffed.

The summary predicates are written **after** the per-property nodes, following
rule 2 of the section protocol at the top of `emit.rs`: a chunk cut inside the
property list must lose the profile rather than leave a denominator standing
beside three properties, which a reader would take for a complete profile.

## The recency pointer has to become per metric

The spec above lists "generalise the read path off `sw:metric:classes`" as a
precondition. Attempting it revealed that this is not a mechanical rename, and
the design question it raises has to be settled here rather than in the edit.

`sw:currentSampleRun` today means "the newest run that published a
`sw:metric:classes` sample for this endpoint". It exists as a **second** pointer
beside `sw:currentRun` because the newest run that MEASURED an endpoint and the
newest that SAMPLED it are different runs the moment a cheap sweep declines
`classes`, which `endpoint_content.rq` records as the steady state and not an
edge case: the 543-endpoint registry sweep declined `classes` for every one of
them. Deciding recency at query time was the original mistake, and the measured
cost of the `FILTER NOT EXISTS` shape it replaced was 6,488.5 ms at 30 runs
against 1.2 ms at one.

The same argument applies once more, one level down. With several sampling
metrics, a run may profile `classes` and decline `properties`, or reach 40 of an
endpoint's 105 classes before its budget expires. So:

**A single pointer per endpoint is wrong for the same reason a single notion of
recency was wrong.** If run B sampled classes and run A sampled properties,
one pointer naming B loses the properties sample outright. That is precisely the
defect stage 3-1 exists to have fixed, reintroduced one level down.

**Ruling 3: the pointer is keyed on (endpoint, metric), with a derived IRI.**

```
GRAPH sw:current {
  <urn:sparqlwatch:sampleptr:ENDPOINT:METRIC>
      sw:sampleRunFor    <endpoint> ;
      sw:sampleRunMetric sw:metric:classes ;
      sw:sampleRunIs     <run> .
}
```

A derived IRI rather than a blank node, for the reason open decision 3 raises
about `sw:profileProperty` and which is settled here: this project diffs runs,
and `load_run.py` maintains the derived graph with one `DELETE WHERE` plus
`INSERT DATA` per pointer. A blank node cannot be addressed by that `DELETE`
without a `WHERE` clause that matches on its properties, which is both slower
and fragile against a partial write. The derivation follows `emit::subject_iri`'s
existing constraint that every component be reversibly encoded.

This subsumes open decision 3: `sw:profileProperty` takes a derived IRI too, on
the same grounds.

**What this costs.** Three call sites change rather than one, and the migration
is not a pure addition: `sw:currentSampleRun` triples already in a store must be
rewritten, and the store is a derived artefact rebuilt from the `.nq` files,
which `load_run.py` states are the source of truth. So the migration is a
reload, not an in-place rewrite, and it is cheap for that reason. The pointer
predicates are new names rather than a reinterpretation of the old one, so a
store carrying both is unambiguous during the reload.

## Ruling 4: `classes` stops being a verdict, and stays a step

**Added 2026-09-01**, on the plan owner's observation that `classes` and
`has-classes` are both better served by this work. They are, but not in the same
way, and the difference decides what gets deleted.

### `has-classes` is already gone, and its argument is the precedent

Removed from `metrics.toml` on 2026-08-28. It survives on the page only because
the store holds the 2026-08-24 run that measured it, and the web tier keeps
describing it marked `retired`. The reason it went is the reason this ruling
exists, quoted from the metric file:

> As a CONTENT fact "something here is typed" is close to worthless: every RDF
> dataset worth monitoring has types, and 54 verified against 489 indeterminate
> on the 2026-08-24 sweep says the metric was mostly reporting whether a query
> came back at all. That is what `availability` asks, with a smaller query, so
> the pair measured responsiveness twice and content once badly.

### `classes` has never produced a verdict. Not once.

Counted across every preserved run in `~/code/sparqlwatch-runs`, all four,
checksummed:

| | `sw:metric:classes` mentions | of which declines | measurement rows |
|---|---|---|---|
| lod-cloud-543 | 543 | 543 | 0 |
| calibration-54 | 54 | 54 | 0 |
| content-probe-2 | 0 | 0 | 0 |
| void-coverage-57 | 0 | 0 | 0 |
| **total** | **597** | **597** | **0** |

It is `expensive`, the default ceiling is `cheap`, so every sweep this project
has ever run declined it. On the page it is a column of 543 `not-measured`,
which tells a reader nothing about their endpoint.

And if it did run, `verified` would mean "we enumerated some classes", which is
`has-classes`'s worthless fact one step along. The value of this metric was
always the `ContentSample` it attaches, never the verdict beside it.

### But the query is this design's own first step

`classes` is not replaced by the content pass. It is CONSUMED by it. The probe in
this spec is one query per class:

```sparql
{ SELECT ?s WHERE { ?s a <CLASS> . FILTER(STRSTARTS(SHA256(STR(?s)), "PREFIX")) } }
```

and the cost model is `SUM over classes of min(instances, cap)`. Something has to
produce that class list, and this is the thing that produces it. Deleting the
query would delete the input to the work this document specifies.

### The ruling

**Retire `classes` as a verdict-bearing metric. Keep the class enumeration as the
first step of the content pass.**

- The class list stops being a `[[metric]]` carrying a `dqv:value`. It becomes
  the profile pass's opening query, publishing a `ContentSample` and nothing
  else. `ContentSample` is already a non-verdict fact kind, so this is Ruling 2
  applied one level earlier rather than a new idea.
- The 543 `NotMeasured` facts the column currently contributes stop being
  written. A column that is nothing but declines is not coverage information, it
  is an artefact of pricing a metric above the ceiling it runs under.
- Measurements already published STAND, and the web tier keeps describing the
  metric marked `retired`, exactly as `has-classes` is handled. A run graph is a
  record of what one sweep observed and nothing rewrites one.

### What this costs, said plainly

The matrix drops to **five metrics, all cheap, and no content column at all**
until the profile pass ships. The site will say less about content than it does
today.

That is honest rather than good. What it removes is a column of 543 declines and
a verdict nobody has ever seen; what it does not do is add anything in their
place. Anyone reading this later should know the gap was chosen with open eyes,
and that it closes when the profile pass lands and not before.

### Sequencing, which matters more than the ruling

**Do not retire `classes` before the profile pass can produce its replacement.**
Retiring it first buys a tidier matrix and loses the only content sampling the
prober can do. The order is: build the profile pass, prove it produces class
lists and profiles, then retire the metric in the same change that ships the
replacement.

## What this requires of the existing code

- **A new metric kind** in `prober/src/metrics.rs`. Existing kinds resolve to a
  verdict; this one resolves to a profile and to no verdict, which is the same
  shape `ContentSample` needed.
- **`endpoint_content.rq` and `load_run.py` are pinned to
  `sw:metric:classes`** in three places (`endpoint_content.rq:84`, and
  `load_run.py:314` and `:360`, the two pointer-maintenance queries). A
  properties metric would be written correctly by the prober and be invisible
  to the web tier. A `run-properties-sample.nq` fixture using
  `sw:metric:properties` already exists, so the intent predates this design.
  Generalising these is governed by Ruling 3 above and is a precondition of
  this work, not a follow-up. It is also the largest single piece of it: it
  touches the derived-graph maintenance in `load_run.py`, whose commentary on
  transactionality and pointer semantics is the most carefully reasoned in the
  web tier, so it belongs in an implementation plan rather than in a
  free-handed edit.
- **`metrics.rs::without_sparql_comments` truncates each line at the first
  `#`**, which is also the IRI fragment separator, so a metric whose query
  carries a full IRI and a `LIMIT` on one line is refused with a message saying
  it has no `LIMIT`. Two exploratory metric sets have now had to put every
  `LIMIT` on its own line to work around it. Fix it, with a test, before adding
  a metric kind.
- **The founding spec's 1b is stale**: it describes `has-classes` as a shipped
  cheap existence check. That metric was removed on 2026-08-28 and the section
  needs updating when this lands.

## Deliberately out of scope

- **Shapes.** This publishes frequencies, not ShEx or SHACL. A shape asserts
  cardinality and closure; a frequency profile asserts neither. Deriving shapes
  from profiles is a later question and the `shexer` benchmark is the reference
  for judging any answer to it.
- **Running `void-generator` against third parties.** Its own documentation
  forbids it and the founding spec already recorded this. Advocating that
  publishers run it themselves is an ecosystem activity, not a feature, and the
  coverage measurement says it currently has two adopters.
- **Any composite score.** Unchanged from the founding spec.

## Open decisions

1. Whether a profile pass runs inside the ordinary sweep charged to the
   endpoint budget, or as a separate content sweep on its own cadence. The
   arithmetic above says one pass fits inside the existing 600-second endpoint
   budget for most endpoints, so a separate sweep is no longer required by
   cost. It may still be wanted so that a slow profile pass cannot delay
   availability reporting.
2. Whether an exact profile of a small class should be preferred over a sampled
   profile of a large one when the budget only allows a few, or whether the
   ordering should be by how much a consumer would use the class.
3. ~~Whether `sw:profileProperty` should be a blank node or a derived IRI.~~
   Settled by Ruling 3: a derived IRI, on the grounds that this project diffs
   runs and `load_run.py` addresses derived-graph rows by subject in a
   `DELETE WHERE`.

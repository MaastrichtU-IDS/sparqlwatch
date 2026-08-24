# Stage 1d-a: Seed The Registry

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the LOD Cloud dump into a candidate registry the prober can sweep, refusing what must not be probed, and measure one sweep in stages rather than pointing 543 strangers' servers at once and hoping.

**Architecture:** A second binary in the prober crate reads a LOD Cloud dump and writes a **new** registry file, applying `registry.rs`'s rules by calling them so endpoint-list policy has one implementation. `prober/endpoints.toml` is untouched: it is the three-endpoint development list every test and every hand-run uses. The seeded list is passed with `--endpoints`. Then a bounded, staged measurement.

**Tech Stack:** Rust 1.96, edition 2021. **No new dependencies**: `serde_json = "1"` is already a direct dependency at `prober/Cargo.toml:13`. Python 3.12 + pyoxigraph for loading the result.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## This plan was rewritten after review

The first draft was reviewed before execution and found to contain **5 Critical and 15 Important defects**. The review is at `.superpowers/reviews/2026-08-24-plan-1d-a.md`. Five findings changed the design rather than the wording:

1. **The seeded list must not replace `prober/endpoints.toml`.** `registry.rs:317-322` asserts that file loads exactly 3 entries with `qlever` first, so replacing it reds `cargo test`; and it is the default `--endpoints`, so every ordinary `cargo run` would become a multi-hour sweep of hundreds of strangers. **None of the three development endpoints is even in the dump** (I checked all three), so the seeded list is a different thing with a different purpose and belongs in a different file.
2. **Deferring admission entirely means probing things that must not be probed.** The dump contains `http://localhost:3030/Dataset/query`, which on a prober host means probing whatever happens to be running there, plus `http://example.org` and `http://www.example.org`, which RFC 2606 reserves precisely so nobody sends them traffic. Publishing any of them permanently as a `dcat:DataService` is worse than useless. Probing-based admission stays deferred; **refusing what cannot be a public endpoint does not, because it is a property of the string and not of a probe.**
3. **Two candidates cannot be published at all.** `https://query.wikidata.org/bigdata/namespace/wdq/sparql?query={SPARQL}` and a `trackloaded.com` URL with an example query inlined both carry `{`/`}`, which no IRI may contain, so `NamedNode::new` rejects them and `emit.rs` skips their rows with a warning after they have already been probed. Both are documentation artifacts in the metadata rather than endpoints. Refusing them at seed time is honest; probing them and silently dropping their facts is not.
4. **Task 3's named-graph question could not be answered by the sweep it specified.** `classes` is an `expensive` metric, so a `--max-cost cheap` sweep never runs it, and `live_smoke.rs:52-62` records that the query passes even with the `GRAPH` branch deleted. So a passing query proves nothing about that branch. The sweep can **identify candidates** for a dedicated verification; it cannot retire the deferral, and the first draft's Done-when claimed it did.
5. **The measurement was unbounded, unrepeatable and un-resumable.** Measured makespan at `--concurrency 4` is 1.2, 4.5 or 10.5 hours depending on what the 227 unknown-cause candidates cost, and `write.rs:127` uses `create_new`, so a killed run cannot be retried under the same `--at`. A first contact with hundreds of strangers' servers is not something to start blind.

## What this slice is, and the three deferrals with their reasons

**This slice is ingest plus one measured sweep.** The spec's 1d row asks for four things:
ingest LOD Cloud plus YummyData candidates, resolve front-ends to real endpoints, probe with
politeness, admit responders. Those are separable capabilities with different judgement
rules, and this project has twice been right to split a stage along that seam.

Deferred, and these are the reasons Task 4 writes into the spec. **Do not take them from the
spec's existing text**, which carries an overstatement this plan corrects (see below):

1. **YummyData's candidate list.** It lives in that application's database, not a checked-in
   file: `~/code/umakadata/db/migrate/20190904034259_create_endpoints.rb` creates the table
   and nothing in the repo carries the rows. Acquiring it needs a running instance or a dump,
   which is its own task. This slice seeds from LOD Cloud alone and says so.
2. **Front-end resolution.** The survey measured that **114 URLs returned HTTP 200 with
   HTML** to a queryless GET, and recommended treating that as a front-end and resolving the
   real protocol URL first. Turning those into endpoints is a judgement capability with its
   own failure modes: guessing a path, following a form action, mistaking a console for an
   endpoint.
3. **The admission policy and the `unreachable-candidates` list.** What makes a daily sweep
   affordable is not re-probing the dead, and that policy should be written against the first
   sweep's real numbers rather than ahead of them.

**One correction Task 4 must make rather than copy.** The spec at `:535-537` says those 114
are "a query front-end rather than a protocol endpoint" and that the real endpoint "often
lives at a different path". The survey it draws on says **some** of those hosts do
(`SURVEY.md:215-218`), not often. Narrow the spec's claim to what was measured while keeping
the recommendation, which is the survey's own.

## What is measured, and what it deletes from this plan

I measured the dump before the first draft and the review re-derived every number. The corrected table:

| Question | Measured on `~/code/umaka-test/lod-data.json` |
| --- | --- |
| Datasets | 1683, **all** carrying a `sparql` key, of which **970 are empty arrays** |
| Candidate entries | 725 `access_url` values, **548 distinct**, matching the survey |
| Schemes | **472 `http`, 76 `https`, nothing else** |
| Blank or whitespace `access_url` | **none** |
| URLs carrying credentials | **zero** |
| Loopback or RFC 2606 reserved | **3** (one `localhost:3030`, `example.org`, `www.example.org`) |
| Cannot be an IRI (`{`/`}`) | **2** |
| Single-label host, no public DNS possible (`https://test-svu/sparql`) | **1, deliberately seeded** |
| Distinct hostnames | **439**, of which **407 hold exactly one endpoint**. Note the sweep groups by `politeness::host_key`, which folds a scheme-default port and keeps a non-default one, so the group count it sees may differ slightly; Task 3 reports the number the run actually used. |
| Largest host group | `api.talis.com`, **27** |
| Near-duplicates kept apart on purpose | e.g. `http://sparql.odw.tw` and `http://sparql.odw.tw/`, which `registry::dedupe` deliberately keeps as two entries |

So the seed **seeds 543 candidates** and reports 5 refusals with their reasons.

**`https://test-svu/sparql` is seeded on purpose.** It is a single-label host, so no public
resolver can answer for it, but it is neither harmful to probe nor unpublishable: DNS refuses
it quickly and its facts are ordinary. The refusals in this slice are for what must not be
contacted or cannot be published, and a host that simply does not resolve is neither. It will
appear in the run as an honest failure, which is the correct outcome.

Two things this retires:

1. **The scheme allowlist deferred to this stage is unnecessary for this dump**, which contains only `http` and `https`. Record it as retired **for this dump**, not in general: a later source may differ, and the first draft overstated it.
2. **The credential refusal from stage 1c-b3 fires on nothing here.** It stays, because stage 5 accepts public submissions.

## The failure modes, corrected

The dump's own `status` field, bucketed **by distinct URL** (the first draft quoted 87 OK, which counts entries; by URL it is 72):

| Count | Bucket |
| --- | --- |
| 72 | OK |
| 187 | an HTTP status, so fast |
| 43 | timed out, so slow |
| 16 | refused or unreachable, so fast |
| 227 | names only a host, cause unrecorded, **cost unknown** |
| 3 | no status recorded at all |

That sums to 548. The 227 are what make the sweep's cost a range rather than a number, which is why Task 3 is staged.

## The risk this slice exists to measure

**A host group runs sequentially**, so a dead host with many endpoints cannot be helped by `--concurrency`. For `api.talis.com`'s 27 candidates:

| If one dead endpoint costs | that one group alone takes |
| --- | --- |
| 3s (DNS failure or connection refused) | about 1.4 minutes |
| 222s (seven requests each burning the 30s request budget, plus six 2s gaps) | about 100 minutes |
| 600s (the endpoint budget, which a redirect chain reaches, not seven probes) | 270 minutes |

But the review's second correction matters more: **407 of 439 groups hold exactly one endpoint**, so in most regimes the aggregate over 439 groups decides the sweep and `api.talis.com` is a tail, not the driver. Both facts belong in the plan because they bound the answer from different sides.

**Do not build a circuit breaker in this slice.** Measure first, then design against numbers, which is how the class-metric split was decided in 1c-b1.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.** `-D warnings` and clippy clean.
- **`prober/endpoints.toml` is not modified by this stage.** It is the development list, it is asserted in `registry.rs:317-322`, and it is the default `--endpoints`.
- **`cargo test` and `pytest` must be green at every commit**, and no test may reach the network.
- **Never report a confident wrong answer.** The six-verdict vocabulary is closed plus the non-verdict `NotMeasured` and `ContentSample` facts. A candidate that could not be probed is not a candidate that failed. All judgement stays in `resolve.rs`.
- **One implementation of endpoint-list policy.** The seeding path calls `registry.rs`; it never repeats it.
- **The endpoint string is used verbatim.** No normalisation, no lowercasing, no trailing-slash stripping, **and no trimming**: the first draft said "trim it", the dump contains no whitespace-padded value, and trimming is exactly the silent normalisation the constraint forbids. Reject a value with whitespace rather than repairing it.
- **No em-dashes** anywhere. Every comment and doc sentence defensible by pointing at a line of code.
- Commit in logical steps, staging by name. Never `git add -A`.

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `prober/src/seed.rs` | create | Extraction: dump bytes in, ordered candidates plus a refusal report out. Unit-testable without a binary. |
| `prober/src/bin/seed-registry.rs` | create | The thin CLI: read a dump, call `seed`, write the registry file and its provenance. |
| `prober/src/registry.rs` | modify | Gains the two refusals that are properties of the string: not routable or reserved, and not expressible as an IRI. Beside `without_credentials`, sharing its warn-and-drop shape. |
| `prober/Cargo.toml` | modify | A second `[[bin]]`. No dependency change, and no comment: this file has none and the first draft claimed a convention that does not exist. |
| `prober/tests/fixtures/lod-cloud-sample.json` | create | A small cut of the real dump shape, committed so tests are offline. |
| `prober/registry/lod-cloud.toml` | create | The 543 seeded candidates. **Not** `endpoints.toml`. |
| `prober/registry/lod-cloud.provenance.toml` | create | Source URL, dump date, dump SHA-256, extraction rule, counts, refusals by reason. A separate parseable file, not a comment: a comment cannot be tested and the first draft's test for it could not have worked. |
| `prober/README.md`, spec | modify | How to re-seed, what the measurement found, what 1d still owes. |

---

### Task 1: Extract candidates, and refuse what must not be probed

**Files:**
- Create: `prober/src/seed.rs`, `prober/tests/fixtures/lod-cloud-sample.json`
- Modify: `prober/src/registry.rs`

**Interfaces:**
- Produces: `pub fn candidates(dump: &[u8]) -> anyhow::Result<Seeded>`, where `Seeded` carries the ordered seeded list plus counts: datasets seen, datasets with a non-empty `sparql` array, entries found, distinct after dedupe, and one count per refusal reason.
- Produces in `registry.rs`: a refusal for a host that is loopback, private, link-local or RFC 2606 reserved, and a refusal for a candidate that cannot be a `NamedNode` (which subsumes the whitespace case: a value containing whitespace is not a valid IRI, so it is **one rule reported under one reason**, not two).
- **Both are wired into `load_endpoints`, not only into the seeder**, exactly as `without_credentials` already is. A registry file can be hand-edited, and stage 5 accepts public submissions, so the rule belongs on every path that turns text into an endpoint list. Say that in the doc comment, and let the existing `load_endpoints` tests cover it.
- Consumes: `registry::dedupe`, `registry::without_credentials`.

**The extraction rule, written before it is coded.** For each dataset in the top-level object, for each entry in its `sparql` array, take `access_url` and keep it if non-empty. Then apply, in order: dedupe, drop credentials, drop unroutable or reserved, drop what cannot be an IRI. Order matters only for the counts, so fix it and report it.

**Do not consult the dump's `status` field to admit or refuse.** It reports 72 URLs OK where the survey's own probing found 65 live, so it is a third party's stale judgement. Record it as provenance; never gate on it.

**Why the two new refusals are not the deferred admission policy.** Admission asks "did this endpoint answer", which needs a probe and is deferred. These two ask "can this string be a public endpoint we may publish", which is answerable without contacting anyone: a loopback address names the prober's own host, RFC 2606 reserves `example.org` so that nobody sends it traffic, and a URL containing `{` cannot be an IRI so **every fact about it would be unpublishable**. Refusing at seed is strictly better than probing and then dropping the facts at emission, which is what happens today.

- [ ] **Step 1: Write the failing tests**

Correct premises from the measurement, because the first draft's were impossible:

```rust
#[test]
fn the_sample_dump_yields_the_candidates_it_names() {
    // The exact list, not a count: a count passes on an extractor that reads
    // the wrong field from the right number of entries.
}

#[test]
fn a_dataset_whose_sparql_array_is_empty_contributes_nothing() {
    // 970 of 1683 datasets have an EMPTY sparql array. None lacks the key,
    // which is what the first draft's test wrongly assumed, so a fixture
    // built on a missing key would test a shape the dump never contains.
}

#[test]
fn two_datasets_naming_one_endpoint_yield_one_candidate() {
    // 725 entries collapse to 548, so this is the common case.
}

#[test]
fn the_dumps_own_status_field_does_not_gate_a_candidate() {
    // A FAIL entry is still a candidate. 72 OK against 65 live measured.
}

#[test]
fn a_loopback_or_reserved_host_is_refused_with_its_reason() {
    // localhost:3030, example.org, www.example.org. Assert the reason, not
    // just the absence, or the test passes on a refusal for the wrong cause.
}

#[test]
fn a_candidate_that_cannot_be_an_iri_is_refused_before_it_is_probed() {
    // The two `{`-bearing URLs. Assert that NamedNode::new agrees, so the
    // test cannot drift from the emitter's actual constraint.
}

#[test]
fn a_value_containing_whitespace_is_refused_not_trimmed() {
    // The dump has none, so this is a rule for a future source. Trimming
    // would be the silent normalisation the constraints forbid.
}

#[test]
fn a_credentialed_candidate_is_dropped_by_the_shared_rule() {
    // By registry::without_credentials, not a second implementation here.
}

#[test]
fn malformed_json_is_an_error_naming_the_problem() {}
```

Cut the fixture from the real dump so its shape is real: a dataset with an empty `sparql` array, one with two entries, two datasets sharing an endpoint, a `FAIL` status, one of the `{`-bearing URLs, `localhost:3030`, and one hand-added credentialed URL with a comment saying it is hand-added and why.

- [ ] **Step 2: Run them, watch them fail, record what each printed**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run against the real dump and check every number**

Expected, from the table above: 1683 datasets, 713 with a non-empty array, 725 entries, 548 distinct, 0 credentialed, 3 unroutable or reserved, 2 not IRIs, **543 seeded**. **If any number differs, stop and tell me** rather than adjusting the plan: these were measured twice, so a disagreement means one of us has the extraction rule wrong.

- [ ] **Step 5: Commit**

---

### Task 2: Write the registry and its provenance, without touching the development list

**Files:**
- Create: `prober/src/bin/seed-registry.rs`, `prober/registry/lod-cloud.toml`, `prober/registry/lod-cloud.provenance.toml`
- Modify: `prober/Cargo.toml`

**`prober/endpoints.toml` is not touched.** It is the three-endpoint development list, it is asserted in `registry.rs:317-322`, it is the default `--endpoints`, and none of its three endpoints appears in the dump. Conflating the two would red CI and turn every hand-run into a multi-hour sweep.

**Provenance is a parseable file, not a comment.** The first draft put it in a TOML comment and then specified a test that the comment survives, which no loader can observe. A separate file can be read, tested, and diffed. It carries: the dump's source URL, the date taken, its SHA-256, the extraction rule's version, and every count from Task 1 including the refusals by reason. The hash identifies **which dump produced this list**, so a re-seed that differs can be
attributed to a new dump rather than to a changed rule. It does not make the dump retrievable:
LOD Cloud publishes no archive of past versions, so if the source moves on, the hash records
what was used and nothing recovers it.

- [ ] **Step 1: Write the failing tests**

Two things can silently go wrong, so test exactly those: that what the binary writes is what `registry::load_endpoints` reads back, unchanged and in order, so a quoting or escaping mistake across 543 URLs cannot pass; and that the provenance file parses and its seeded count equals the registry file's length. **Do not** assert that the three development endpoints are present: none of them is in the dump, so the first draft's check would have failed by construction.

- [ ] **Step 2: Run them, watch them fail**
- [ ] **Step 3: Implement, then generate both files against the real dump**

Put the counts in the commit message.

- [ ] **Step 4: Read the generated file**

Enough of the 543 to be satisfied they are endpoint URLs and not a parsing accident, and confirm the 5 refusals are absent by name.

- [ ] **Step 5: Confirm the suites are still green and commit**

Including `registry.rs:317-322`, which must still pass untouched.

---

### Task 3: Measure one sweep, in stages

**Files:**
- Modify: `prober/README.md`
- Create: whatever makes the measurement reproducible

This task writes no product code. It is the measurement every later 1d decision depends on, and it is the first time this project contacts hundreds of hosts that have never heard from it.

**Two operational facts to respect.** A killed run cannot be retried under the same `--at`, because `write.rs:127` refuses to reopen an existing partial: use a fresh `--at` per attempt and keep the partials. And a bounded stage needs a bounded input, so cut a sample registry file rather than adding a `--limit` flag this stage has no other use for.

- [ ] **Step 1: A calibration sample, sized so its projection means something**

The point of this stage is the per-endpoint cost of the **227 unknown-cause** candidates,
because that one number turns the full sweep's duration from a range into arithmetic. So size
the sample for that, not for tidiness: **30 from the unknown bucket**, plus 8 `OK`, 8
`timed out` and 8 `an HTTP status` as controls. Twelve unknown-bucket draws gives roughly
plus or minus 28 percentage points, which would put the projection anywhere from 2.1 to 3.5
hours and leave the abort threshold inside its own error bar. Thirty draws is the difference
between a projection and a guess.

**Do not draw them all from distinct hosts.** Cost in the unknown bucket is a property of the
**host**, and the three largest groups (`api.talis.com` 27, `services.data.gov.uk` 8,
`sparql.linkedopendata.it` 7) are 42 of the 227 and the largest single lever on the makespan.
Include members of each, and report their per-endpoint cost separately from the singletons,
because a group of 27 at the slow rate is the one shape that dominates everything else.

Write the sample as its own registry file, sweep it at `--max-cost cheap --concurrency 4`, and
record wall clock, per-endpoint cost by bucket, per-endpoint cost inside the three big groups,
and how many answered.

- [ ] **Step 2: Report the projection before running the full sweep**

State the projected makespan from Step 1's numbers, **with the interval, not just the point
estimate**. **If the upper end of the interval exceeds 4 hours, stop and tell me.** A number
rather than "a few hours", because the previous draft's threshold sat inside its own error
bar, which is no threshold at all. At that point the honest next move is the circuit breaker
or the admission policy, not a ten-hour run, and that is my decision rather than this task's.

- [ ] **Step 3: The full sweep, if Step 2's projection is acceptable**

`--max-cost cheap --concurrency 4` over the 543. Record wall clock, how many answered, how many timed out, how many failed fast, the largest group's contribution, and whether the aggregate or the tail dominated.

- [ ] **Step 4: Load the result**

`web/load_run.py` on the output: quad count and discarded bytes. This is the first evidence the incremental writer and the loader hold two orders of magnitude beyond anything they have carried.

- [ ] **Step 5: Answer what can be answered, and say what cannot**

1. **Named graphs.** Identify responders that look like candidates for keeping data in named graphs. **This cannot retire the deferral**: `classes` is `expensive` so a cheap sweep never runs it, and `live_smoke.rs:52-62` records the query passing with the `GRAPH` branch deleted. Name the candidates for a dedicated verification and say plainly that the deferral stands.
2. **Cross-host redirects.** Say how you counted, or say it needs instrumentation the prober does not have and leave it. Do not infer a rate from a number the run does not record.
3. **Cost distribution**, against this plan's two tables.
4. **`fsync`.** Report the writer's share of the sweep. If the run does not separate it, say the question is settled by arithmetic instead and give the arithmetic.

- [ ] **Step 6: Write it down where it will be found**

`prober/README.md` currently says of its own cost estimate that it is "a rough estimate, not a measurement, because no full registry sweep has been run yet". Replace that with what was measured, keep whatever arithmetic is still useful, and be explicit about which numbers are measured and which remain estimates.

- [ ] **Step 7: Commit**

---

### Task 4: Say what is seeded and what 1d still owes

**Files:**
- Modify: `prober/README.md`, `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: The README**

How to re-seed; that the list is LOD Cloud only and why; that the dump's `status` field is recorded and never gates; the five refusals and their reasons; and that `endpoints.toml` remains the development list while `registry/lod-cloud.toml` is the seeded one.

**Say plainly that the registry is not yet operable on a daily cadence**, because nothing yet stops the dead being re-probed every sweep. That is the next slice, and a reader should not have to infer it.

- [ ] **Step 2: The spec**

Mark 1d **partly delivered** as 1d-a, following the neighbouring rows' convention. Name the three deferrals with their reasons, and the two items the measurement retired, noting the scheme allowlist is retired **for this dump** rather than in general. The spec's 1d row asks four questions (named graphs, cross-host redirect frequency, telling a
seeded URL from a credentialed one, and whether 548 chunks want an `fsync`). Answer each with
what Task 3 found or say why it still stands, and **do not drop the credential question**,
which the first draft silently lost: the answer is that this dump contains none, which is
worth recording precisely because it was asked.

- [ ] **Step 3: Commit**

---

## Done when

- The extractor turns the real dump into 543 seeded candidates with 5 reported refusals, every count matching this plan, or the disagreement is reported rather than absorbed.
- The dump's `status` field gates nothing, proven by a test.
- A loopback, reserved, credentialed, whitespace-bearing or non-IRI candidate is refused before anything is probed, each with its own reason asserted.
- Endpoint-list policy has one implementation: the seeding path calls `registry.rs`.
- `prober/endpoints.toml` is byte-identical to what it is today, and `registry.rs:317-322` passes untouched.
- `registry/lod-cloud.toml` round-trips through `registry::load_endpoints`, and the provenance file parses with a count that matches it.
- A 40-candidate calibration sample has been swept and the per-endpoint cost of the unknown bucket is known.
- Either the full sweep has run, loaded, and been written down, or the projection said not to and that is recorded as the finding.
- The named-graph deferral is either retired with evidence or explicitly still standing, and nothing claims otherwise.
- The spec records 1d-a, its three deferrals, its two retirements, an answer to each of the four questions, and that daily operation needs the next slice.

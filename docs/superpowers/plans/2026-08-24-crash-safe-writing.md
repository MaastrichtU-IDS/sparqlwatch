# Stage 1c-b4: Crash-Safe Incremental Writing

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A sweep that dies at endpoint 500 keeps the 499 endpoints it already measured, the file it leaves behind loads, and nothing in it says more than it observed.

**Architecture:** Emission splits into a header, one self-contained chunk per endpoint, and a footer. Each chunk ends with a marker naming the endpoint it just completed, so the unit of truncation is a chunk and not a line. `run_sweep` writes each endpoint's chunk as that endpoint finishes, over a bounded channel from the host-group tasks, which is completion order and deliberately abandons the input-order property stage 1c-b3 documented. The loader learns to drop a trailing incomplete chunk. Output goes to a sibling path and is renamed onto `--out` at the end, so a crash never destroys the previous run.

**Tech Stack:** Rust 1.96, edition 2021. tokio (`sync` for `mpsc`, already declared at `Cargo.toml:11`), oxrdf 0.3 + oxrdfio 0.2, wiremock 0.6, clap 4. Python 3.12 + pyoxigraph 0.5.9 for Tasks 2 and 4.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## This plan was rewritten after review

The first draft was reviewed before execution and found to contain **5 Critical and 16
Important defects**. The review is at `.superpowers/reviews/2026-08-24-plan-1c-b4.md` and
is worth reading alongside this plan. Five of its findings changed the design rather than
the wording:

1. **Truncation happens at chunk granularity, not line granularity, and the first draft's
   line-level tolerance is what would have turned a half-written chunk into a published
   wrong fact.** `emit.rs:809-818` writes a sample's `sw:sampleSize` and
   `sw:sampleTruncated` **before** its `sw:sampledValue` lines, so a chunk cut anywhere
   inside the value list yields a node asserting `sampleSize 200, sampleTruncated false`
   beside three values. `endpoint_content.rq` matches it, and
   `templates/endpoint.html:187` renders "200 classes sampled" above a list of three,
   labelled complete. That needs no partial write at all: a cut at a line boundary inside
   a chunk parses, ends in a newline, and the first draft's tolerance never even fires.
   So the chunk becomes the unit of tolerance, via a marker line, and the fact families
   stop putting their summaries before what they summarise.
2. **The duplicate-subject guard is a payload pre-scan that publishes nothing on
   conflict, not a `HashSet`.** Carrying a set across chunks could only mean
   skip-the-later, which is precisely the design stage 1c-b3's review rejected: it would
   publish `verified` about a pair also measured `absent`. A pre-scan cannot span chunks,
   because chunk 1 is on disk before chunk 2 exists. So the pre-scan is per chunk, which
   is complete because an endpoint's facts are all in one chunk, and that becomes a
   **checked** invariant rather than an inherited convention.
3. **The drain loop as first specified hangs.** `run_sweep` clones a `Sender` into each
   group and, unless it drops its own copy before the loop, `recv()` never returns `None`.
   The plan chose the shape and then declined to say who owns the senders.
4. **`assemble` does three things, not one.** It reassembles by slot, fills a failed slot
   with `ProberFailed` facts, **and** appends one `NotMeasured { CostCeiling }` per
   (endpoint, declined metric). Under the default `--max-cost cheap` that fact is the only
   thing a run says about `classes`, and the first draft's "send each `EndpointSweep` and
   write it" dropped the whole family.
5. **`RunWriter::new` truncating `--out` at t=0 means a crashed sweep destroys the
   previous run**, and `web/load_run.py` says in as many words that the .nq files are the
   source of truth. A sweep is an observation of a changing world; nothing re-creates last
   night's file.

Two premises the review verified, which the design rests on and which belong in writing:

- **The last newline really is a statement boundary.** oxrdfio escapes a newline inside a
  literal, so no emitted line contains a raw newline, and no proper prefix of an N-Quads
  statement is itself a valid statement, because the terminating `.` is always in the
  truncated part. That is what makes a byte-level rule sound at all.
- **The activity quads are in the header and every read query needs them.**
  `endpoint_measurements.rq`, `endpoint_content.rq` and `endpoint_description.rq` all
  require `?activity a prov:Activity ; prov:generatedAtTime ?generatedAt` in the same
  graph. Header-first is load-bearing, not tidy.

## The measurement that shapes this plan

Truncating `web/tests/fixtures/run-with-samples.nq` to 3000 bytes, which is what a crashed
prober leaves:

```
$ python web/load_run.py store.db partial.nq
ValueError: not valid N-Quads: Parser error at line 18 between columns 89 and 98:
Unexpected end of file (line 18)
```

**The whole file is refused, not the last line.** Writing incrementally without teaching
the loader to survive a truncated tail would deliver a guarantee that is false in exactly
the case it exists for, which is why Task 2 is in this stage.

## What this stage finishes, and the promise it breaks

The spec's per-endpoint isolation bullet (`:139-143`) requires that "results are written
per endpoint as they complete". Stage 1c-b3 delivered the first half of that bullet for
probing and explicitly did not deliver this half. This stage does, and the bullet then
holds in full.

**It abandons the order property on purpose.** Stage 1c-b3 documented output order as
input order, scoped as a property of one emitted file, and said in `prober/README.md` and
in its own plan that this stage was expected to break it. Writing each chunk as it
completes is completion order. That is safe only because of 1c-b3's other half: every
subject is a pure function of (run, endpoint, metric), so a chunk can name its nodes
knowing nothing about the rest of the run. **The identity property is load-bearing. Do not
let it regress.**

## What "crash-safe" means here, exactly

It protects against **the prober process dying**: a panic, a kill, an OOM, a cancelled CI
job. Each chunk is written and flushed as a unit, ending with its marker line, so between
chunks the file on disk is a complete prefix of the run.

It does **not** promise durability against power loss or a kernel crash, because there is
no `fsync` per chunk. The honest reasons: a lost run is a **gap in history rather than a
false fact**, the next scheduled sweep restores coverage, and 548 `fsync` calls have a
cost this project has not measured. Do not write that the file is reproducible: it is not.
`web/load_run.py` says the .nq files are the source of truth and the store is the derived
artefact, and a sweep observes a changing world, so a lost run is gone.

State the boundary too: after a power loss a delayed-allocation filesystem can leave a
**zeroed tail**, which neither the line rule nor the chunk rule can rescue, and the file is
then refused whole. That is the edge of the guarantee and it belongs in the README.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.** `-D warnings` and
  `cargo clippy --all-targets -- -D warnings` clean.
- **Never report a confident wrong answer.** The six-verdict vocabulary is closed plus the
  non-verdict `NotMeasured` and `ContentSample` facts. `verified` and `absent` are
  assertive and require evidence. All judgement stays in `resolve.rs`.
- Run graphs are **immutable and append-only**. Nothing rewrites a chunk already on disk,
  and **no fact may be published that a later chunk would need to correct**. In
  particular: publish nothing at t=0 that asserts something about the future.
- **No quad about an endpoint, a metric, a measurement or a sample changes for a run that
  completes.** The activity gains **two run-level facts** (`sw:emission`, `sw:finalised`)
  plus **one `sw:completedEndpoint` per endpoint**, so the synthesized run in Task 1 moves
  its baseline from 43 to 47 quads rather than to 46: get the arithmetic right in the test
  and in the paragraph. The frozen baseline in `only_the_subjects_changed` moves with a
  paragraph explaining why, as that test's own comment requires. Nothing else moves.
- `emit` reads **no clock**: `emit.rs:271-274` states that the same inputs always produce
  the same document, so the finalisation instant is a parameter supplied by `main.rs`.
- **No em-dashes** anywhere. Every comment and doc sentence defensible by pointing at a
  line of code. Read the line before writing the sentence.
- Commit in logical steps, staging by name. Never `git add -A`.

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `prober/src/emit.rs` | modify | Splits into `emit_header`, `emit_endpoint`, `emit_footer`. `emit_nquads` becomes the composition. The sample's summary quads move after its values. The duplicate pre-scan becomes per chunk. |
| `prober/src/write.rs` | create | `RunWriter`, generic over `io::Write` so a failing writer can be injected. Owns the sibling file, writes header once, a chunk per endpoint, footer once, renames onto `--out` at the end, and refuses a second chunk for an endpoint it has already written. |
| `prober/src/lib.rs` | modify | `run_sweep` takes the writer and a bounded `mpsc`, writes each endpoint as it arrives, and still returns a whole `Sweep`. `assemble` becomes the per-endpoint builder. |
| `prober/src/main.rs` | modify | Constructs the writer, supplies the finalisation instant, no longer calls `std::fs::write`. |
| `prober/tests/end_to_end.rs` | modify | 38 call sites, and two tests that are about this stage: `output_order_is_input_order_not_completion_order` (`:2216`) must be inverted, and `a_panicked_group_publishes_prober_failed_for_every_endpoint_it_held` (`:2476`) must keep its input-order assertions meaningful. |
| `prober/tests/live_smoke.rs` | modify | The 39th call site. `#[ignore]`d but compiled under `-D warnings`. |
| `web/load_run.py` | modify | Drops a trailing incomplete chunk in `_parsed_graphs`, reports the discarded bytes on `LoadResult`. |
| `web/queries/endpoint_measurements.rq`, `web/endpoint_measurements.py`, `web/queries/endpoint_description.rq`, `web/app.py`, `web/templates/endpoint.html` | modify | The read tier says when a run did not finish, in both representations. |
| `prober/README.md`, spec | modify | The guarantee, its limits, the new order, and four places that currently disagree. |

---

### Task 1: Split emission, and stop summarising before the thing summarised

**Files:**
- Modify: `prober/src/emit.rs`
- Test: `prober/src/emit.rs`'s own tests

**Interfaces:**
- Produces: `pub fn emit_header(RunHeader) -> anyhow::Result<String>`, `pub fn emit_endpoint(&mut EmitState, EndpointFacts) -> anyhow::Result<String>`, `pub fn emit_footer(RunFooter) -> anyhow::Result<String>`.
- `EndpointFacts` is one endpoint's slice of **all four** fact families: its `MeasurementRow`s, its `DeclarationsRead`, its `NotMeasured` facts (**both** the `CostCeiling` and the `ProberFailed` families), and its `ContentSample`s.
- `EmitState` carries **exactly one thing across chunks**: the set of endpoints already written. Nothing else, because anything else makes a later chunk depend on an earlier one and a chunk must stand alone.
- `RunFooter` carries `failed_endpoints` only. It carried a finalisation instant in an
  earlier draft; the boolean replaced it, and this bullet was the last place still saying
  otherwise.
- `emit_nquads` becomes the composition and **keeps its signature**. Dropping the
  finalisation instant is what buys this: the footer is a constant, so `RunEmission` gains
  no field and the 41 struct literals across the crate and its tests are untouched. That is
  a second reason the boolean was the right call.

**The new facts: two run-level, one per endpoint, and why each is true when written**

- Header: `<activity> sw:emission "incremental"`. Says **how** this run is being written.
  True at t=0. It is not a promise about the future, which an append-only graph could
  never retract.
- Chunk marker, last line of every chunk: `<activity> sw:completedEndpoint <endpoint>`.
  Says this activity finished that endpoint. True when written, and it is what makes the
  chunk the unit of truncation. **Per endpoint, not run-level.**
- Footer, the last line the writer ever writes: `<activity> sw:finalised "true"^^xsd:boolean`.
  Says **that** the run finished.

A consumer then reads three cases off facts that were each true when written: emission
plus `finalised` is a complete run; emission without `finalised` is a run that did not
finish; neither is a run from before this stage, which promised nothing.
`failedEndpoints` is only trustworthy in the presence of `finalised`, and one nominated
quad, `finalised`, is the marker, so that "footer present" is not a predicate that depends
on which of two quads a consumer looked for.

**Why a boolean and not `prov:endedAtTime`.** The instant would be the better fact and PROV
already has the predicate, but nothing in this crate can produce it soundly. `emit` may not
read a clock (`emit.rs:271-274`: the same inputs always produce the same document),
`main.rs` reads none by design, `std` cannot format a `SystemTime` as `xsd:dateTime`, and
there is no `chrono`, `time`, `humantime`, `jiff` or `iso8601` anywhere in
`prober/Cargo.lock` under this plan's no-new-dependencies rule. The two alternatives are
both worse. Hand-rolling civil-time arithmetic from Unix seconds would publish a typed
`xsd:dateTime` literal into a graph that is never rewritten, and `nn()` validates IRIs, not
literal lexical spaces, so a month-length mistake would be permanent. And an `--ended-at`
flag, though it is exactly what `--at` does one field over, does not work for this fact: the
process cannot know when it will finish, so a flag supplied at launch publishes a **predicted
future**, which is the thing this section exists to avoid. A run's duration stays derivable
from the measurements if anyone needs it, and a time dependency can be argued on its own
merits when something needs more than a boolean. Do **not** write that a run's duration
stays derivable from the measurements: `elapsedMs` starts after the per-host gate is
acquired and excludes politeness by design (`emit.rs:277-291`, `README.md:366-375`), and at
concurrency 4 the sum of them is not even a bound on the run. A monotonic `Instant` delta
published as integer seconds would need no formatting and assert nothing about the future,
which is the shape to reach for if a duration is ever wanted.

The marker also gives the read tier what Task 4 needs: whether a given run reached a given
endpoint is now a fact, not an inference from absence.

**Chunk derivation, which is the only non-trivial part of the split**

`emit_nquads` receives four flat lists and no endpoint order. It derives the endpoint
sequence from the **union of all four lists in first-appearance order**, because an
endpoint can appear only in `declarations_read` (see
`declarations_read_emits_a_boolean_quad_shaped_for_the_run` at `emit.rs:1513`, which passes
`rows: &[]`), and slices each list per endpoint. First-appearance order over the union is
what keeps today's whole-run output stable for a run whose lists are already grouped by
endpoint, which is what keeps the order-sensitive existing tests green (for example
`an_unmeasured_elapsed_time_emits_no_quad_rather_than_zero` indexes `elapsed[0]` at
`emit.rs:1413-1418`).

**Two ordering changes inside a chunk**

1. `sw:sampleSize` and `sw:sampleTruncated` move to **after** the last `sw:sampledValue`.
   The marker already makes a partial chunk droppable, but a partial chunk read by a
   loader that predates Task 2 would otherwise publish a false size, and defence in depth
   costs one line here. With the summary last, a truncated sample loses the sample rather
   than misstating it, which is the right failure direction. The general rule, worth
   writing into the module: **no fact family may publish its own summary before the things
   it summarises.**
2. `typed_endpoints` (`emit.rs:385`, inserts at `:497`, `:603`, `:768`, `:857`) becomes
   **per chunk**, so every chunk types its own endpoint. A chunk that carries facts about
   an endpoint it never typed is not self-contained.

**The duplicate-subject guard**

Describe it correctly and keep it: `emit.rs:418-446` is a `BTreeMap<subject, BTreeSet<payload>>`
pre-scan plus `conflicted()` at `:233`, and a subject whose payloads disagree publishes
**nothing** (`:481-489`, `:753-761`); the `emitted: BTreeSet<String>` at `:446` separately
skips a byte-identical repeat. The pre-scan becomes per chunk, which is complete **because
an endpoint's facts are all in one chunk**, and Task 3 makes that a checked invariant
rather than a convention inherited from `registry::dedupe` (which is not on this path:
`run_sweep` is `pub`, takes `&[String]` unchecked, and 39 test call sites feed it
hand-built lists).

- [ ] **Step 1: Freeze the baseline, before touching anything**

The proof that the split changed nothing has to compare against a frozen artefact, not
against `emit_nquads`: once `emit_nquads` **is** the composition, comparing the two is a
tautology that can never fail again. So, exactly as `only_the_subjects_changed` does:

```rust
#[test]
fn the_split_emission_publishes_what_the_whole_one_did() {
    // Baseline frozen from the pre-1c-b4 emitter on 2026-08-24 over the
    // synthesized run below: the sorted (predicate, object) multiset and the
    // quad count. Subjects are not part of it (1c-b3 froze those separately);
    // the two new run-level facts and the per-endpoint marker are listed in
    // test, which is what a deliberate addition costs.
    const BASELINE_PAIRS: &[(&str, &str)] = &[ /* filled in from the current code */ ];
    const BASELINE_QUADS: usize = /* filled in */;
}
```

Build the synthesized run to cover every family: several endpoints, measurements with and
without a level and an elapsed time, a declined metric, a sample with the truncation flag
both ways, a `declarationsRead` both true and false, and one endpoint appearing **only** in
`declarations_read`.

- [ ] **Step 2: Run it, confirm it passes against today's code, and commit it alone**

A baseline committed green against the old emitter is the only kind worth having.

- [ ] **Step 3: Implement the split, the three ordering rules and the new facts**

- [ ] **Step 4: Run the suite**

The baseline test passes with 43 becoming 47, two run-level facts plus one marker per
endpoint of the synthesized run, and
`only_the_subjects_changed`'s constants move by exactly those three with the paragraph its
comment demands. Report every other test that needed editing and why: this task is
supposed to change nothing else a run publishes.

- [ ] **Step 5: Commit**

---

### Task 2: Make the loader drop an incomplete trailing chunk

**Files:**
- Modify: `web/load_run.py`
- Test: the existing `load_run` test file in `web/tests/`

Without this the prober's guarantee is false in the only case it exists for.

**Where it lives.** In `_parsed_graphs`. `main()` validates every input with
`_parsed_graphs(data)` at `load_run.py:200` **before** `Store()` is constructed, and
`load_run()` calls it again at `:147`, so a tolerance implemented in `load_run()` is never
reached by the CLI and the plan's own reproduction command would still fail. Both callers
get it from `_parsed_graphs`, and the discarded-byte count must not be reported twice for
one file, since `main` parses each file twice by design.

**The terminator set has three members, and getting this wrong corrupts every complete
run.** A complete file's last line is **not** a chunk marker: it is the footer. A rule that
truncates back to the last marker would therefore discard both footer quads from every
complete file, and Task 4's condition (a) would then fire on every page and every RDF
representation, telling every visitor that every finished sweep did not finish. That is a
confident wrong answer about every run this project publishes, produced by the tolerance
built to prevent one. So a file is loadable as far as it goes if its last statement is any
of:

1. the footer's `sw:finalised` quad (a complete run),
2. a chunk marker (an unfinished run, whole chunks only),
3. the header's `sw:emission` quad (a run that died before its first endpoint), which
   `emit_header` must therefore write **last**. That is the third ordering rule this stage
   introduces, alongside the sample summary moving after its values and the footer's
   `sw:finalised` being the last line the writer ever writes.

Truncate to whichever occurs **latest**. All three are reachable, which is why all three
are listed, and each is a nominated predicate rather than "the last line of a section".

**The rule, tightly**

- If the bytes parse **and** their last statement is one of the three terminators, nothing
  changes: same behaviour, same counts, same messages.
- Otherwise, retry from the end of the latest terminator line. If that parses, load it and
  report the discarded byte count.
- **If there is no terminator at all, refuse with the original parse error.** Never a
  zero-quad success path: the first draft's rule turned a file that is entirely one
  truncated line into "discarded 21 bytes, loaded 0 quads" followed by
  `input names no graphs; nothing to load`, a true sentence that misdiagnoses the file and
  buries the real error.
- A file that ends in a newline and parses but ends mid-chunk is a run whose last chunk is
  incomplete: drop that chunk. This is the case a line-level rule cannot even see.
- Anything else refuses the whole file exactly as today. A syntax error in the middle is a
  corrupt file, not a crashed writer, and the two must not be conflated.

**Recognising a terminator cannot be a substring search.** Sampled class IRIs come from
strangers' endpoints, so a sample may legitimately contain a value IRI spelled
`urn:sparqlwatch:completedEndpoint`, and a naive byte scan would treat that
`sw:sampledValue` line as a chunk boundary and truncate a run in the middle of a chunk.
Match on the **predicate position** of a full line: the line must begin with the activity
IRI and carry the marker predicate as its second term. A class IRI can only ever appear in
object position, so anchoring on the predicate is sufficient. Test it with a fixture whose
sample contains exactly that spoofing value.

`load_run.py` has no logging and reports through `print` in `main`, so "log a warning" is
unimplementable: return the discarded byte count on `LoadResult`, print it in `main`
beside the existing "loaded N quads" line, and have tests assert the returned value rather
than captured output.

**Own this out loud in the docstring:** a hand-edited file whose last chunk is corrupt is
indistinguishable from a truncated one and will be tolerated. That is inherent in the rule
and it is the right trade.

- [ ] **Step 1: Write the failing tests**

The test file is `web/tests/test_load_run.py`. Cover, each asserting a value:

- **a complete file loads with its footer and the same quad count.** First on the list
  because getting the terminator set wrong breaks every complete run, and no other case
  catches it.
- a chunk truncated mid-line;
- a chunk truncated **at a line boundary**, which parses and must still be dropped: the case
  a line-level rule cannot see;
- a run that died before its first endpoint, so the header's last line is the terminator and
  the file loads to just the activity;
- a sample containing a value IRI spelled `urn:sparqlwatch:completedEndpoint`, which must
  **not** be mistaken for a chunk boundary;
- a syntax error in the middle, still refusing the whole file;
- a file with no terminator at all, refused with the parse error rather than succeeding with
  zero quads;
- the discarded byte count on `LoadResult`;
- an empty file and a comments-only file, both still refused as "names no graphs".

- [ ] **Step 2: Run them, watch them fail, record what each printed**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run both suites and commit**

---

### Task 3: Write each endpoint as it finishes

**Files:**
- Create: `prober/src/write.rs`
- Modify: `prober/src/lib.rs`, `prober/src/main.rs`, `prober/tests/end_to_end.rs`, `prober/tests/live_smoke.rs`
- Test: `prober/tests/`, plus unit tests in `write.rs`

**Interfaces:**
- `RunWriter<W: io::Write>`, with a path-based constructor for `main.rs` and a
  `with_writer` constructor so a test can inject a writer that fails on the third chunk.
  `write_endpoint(&mut self, EndpointFacts)`, `finish(self, RunFooter)`.
- `run_sweep` keeps returning a whole `Sweep` and gains the writer. **Do not shrink
  `Sweep`.** 39 call sites destructure it, with 110 references to `rows` alone and 114 to
  the other three lists, and `main.rs:205-209` logs their lengths.

  **Slots must survive.** Arrival order is completion order, so a drain loop that appends
  the four lists as it writes cannot also return a `Sweep` in input order, and 39 call
  sites plus roughly 220 assertions read that order. So: write on arrival, accumulate into
  slots indexed by input position, and build the returned `Sweep` from the slots after the
  loop. Both properties then hold at once, and they are different properties: **the file is
  in completion order and the returned `Sweep` is in input order**. Say that explicitly,
  because it is the honest split and it is what the inverted order test asserts.

**Output goes to a sibling and is renamed at the end**

`RunWriter` writes to `<out>.<at>.partial`, where `<at>` is the run's own required,
validated `--at` label, and `finish` renames it onto `--out`. The label rather than a fixed
suffix, because a fixed `<out>.partial` means the next scheduled sweep truncates the
previous crash's partial run on its first write, which is the very loss this section exists
to prevent, displaced by one run. Two invocations sharing an `--at` are the same run and may
overwrite. `rename` within
a directory is atomic on macOS and Linux, so `--out` is always either the previous complete
run or this one; a crash leaves the partial file under its own name where `load_run.py` can
take it. Document the partial file's name in the README beside the guarantee, and test that
a crashed sweep leaves the previous `--out` byte-identical.

**Buffering, stated so both failure modes have the same worst case**

Flush at the end of every chunk, and give the buffer capacity for at least one chunk. State
the largest chunk the shipped `metrics.toml` can produce: the committed
`run-with-samples.nq` spends 19,692 bytes on 109 `sampledValue` lines, so one endpoint's
`classes` sample at `sample_limit = 200` is roughly 40 KB, well over `BufWriter`'s 8 KB
default. **`Drop` on the writer must not be relied on for correctness**, because a
`SIGKILL` runs no destructors.

**The channel, specified rather than left to the implementer**

- **Bounded**, with a named capacity. Bounded is safe because the drain loop is not doing
  the probing: a blocked `send` holds its group's semaphore permit, which delays that
  host's next endpoint but cannot deadlock while the receiver is live. Bounded also bounds
  memory at 548 endpoints.
- **`run_sweep` drops its own `Sender` before entering the loop.** Otherwise `recv()`
  never returns `None` and the sweep hangs after the last group finishes. This is the most
  common way to write this shape wrong.
- **A send error means the sweep is over, so the task returns quietly.** Do not reach for
  `.expect()` the way `lib.rs:232` does for the semaphore: converting a write failure into
  a panic per group re-enters the panic path and writes `prober-failed` facts about
  endpoints that were measured.
- **Order at the end:** all real chunks, then the `prober-failed` chunks for endpoints a
  panicked group never sent, then the footer. A footer written before the panic sweep
  certifies a run whose failed endpoints are not yet in the file. Working out which
  endpoints never arrived needs no new machinery: stage 1c-b3 already keeps `covering`
  (`lib.rs:182`) mapping a task id to the endpoints its group holds, reachable from
  `JoinError::id()`, and the writer already knows which endpoints it has written.

**`EndpointFacts` and the cost-ceiling family**

`assemble` (`lib.rs:266-321`) does three things: reassembles by slot, fills a failed slot
with `ProberFailed` facts, and appends one `NotMeasured { CostCeiling }` per (endpoint,
declined metric) **for every endpoint, measured or failed** (`:311-319`). `declined` is a
`run_sweep` parameter and lives nowhere else. Under the default `--max-cost cheap`,
`classes` is declined, so that fact is the **only** thing a run says about class content,
and both `README.md:530-534` and `app.py:558-562` hang their honesty on it.

So: keep `assemble` as the **per-endpoint builder**, renamed to say so, so its unit test
`a_failed_group_still_carries_facts_for_every_endpoint_in_it` survives as the unit-level
pin on the `ProberFailed` contract. The drain loop calls it on receipt to build
`EndpointFacts` including both `NotMeasured` families. `declined` is still passed to
`run_sweep` and still never interpreted there.

**Write once per endpoint, checked**

`write_endpoint` **refuses a second chunk for an endpoint it has already written**, with an
error naming the endpoint, because once the earlier chunk is on disk refusing is the only
thing a writer can do. That is what turns "an endpoint's facts are all in one chunk" from
an inherited convention into a checked invariant.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_cancelled_sweep_leaves_a_loadable_file_of_what_it_finished() {
    // Covers CANCELLATION, not a crash: dropping a future runs destructors and
    // SIGKILL does not. Step 4's manual kill is the only check of the crash
    // claim, which is why the buffering rule above exists.
    // Parse the file with oxrdfio rather than eyeballing it.
}

#[tokio::test]
async fn an_endpoint_is_on_disk_before_the_sweep_returns() {
    // Mechanism, because reading the file after awaiting run_sweep passes on a
    // buffer-and-write-once implementation: spawn the sweep as a task, poll the
    // file until the fast endpoint's marker appears, THEN release the slow
    // mock, all inside without_deadlocking. The two endpoints must be on
    // DIFFERENT hosts or host grouping stops the fast one finishing at all.
}

#[test]
fn a_chunk_plus_the_header_answers_the_read_queries() {
    // Not "a chunk parses on its own", which any subset of N-Quads lines does.
    // Load header plus one chunk and assert the joins all three .rq files
    // require are satisfied: ?activity a prov:Activity ; prov:generatedAtTime.
}

#[tokio::test]
async fn a_second_chunk_for_one_endpoint_is_refused() {
    // The checked invariant the per-chunk duplicate pre-scan now relies on.
}

#[tokio::test]
async fn a_panicked_group_still_gets_its_prober_failed_chunks_written() {
    // The 1c-b3 contract on the incremental path, footer last.
}

#[test]
fn a_chunk_write_failure_stops_the_sweep_with_what_was_written_intact() {
    // Needs `with_writer`: a writer that fails on the third chunk. An
    // unwritable path fails in the constructor, before anything is written, so
    // the first draft's version of this test was vacuous.
}

#[tokio::test]
async fn a_crashed_sweep_leaves_the_previous_out_file_untouched() {
    // The C5 guarantee: byte-identical, because that file is the source of truth.
}
```

- [ ] **Step 2: Run them, watch them fail, record what each printed**
- [ ] **Step 3: Implement, including the two named test edits**

`output_order_is_input_order_not_completion_order` (`end_to_end.rs:2216`) is now false by
design: invert it into a test that the file is in completion order and that the returned
`Sweep` is still in input order, which is the honest split. Keep
`a_panicked_group_publishes_prober_failed_for_every_endpoint_it_held` (`:2476`) working; its
assertions are `Vec` equality in input order and `Sweep` staying whole is what preserves
them.

- [ ] **Step 4: Run it for real, including a kill**

A live sweep of `endpoints.toml`, loaded with `web/load_run.py`, quad count reported. Then
`SIGKILL` a sweep partway (not Ctrl-C, so no destructor runs) and load what is on disk.
Report both, and report that the previous `--out` survived.

- [ ] **Step 5: Commit**

---

### Task 4: Say when a run did not finish

**Files:**
- Modify: `web/queries/endpoint_measurements.rq`, `web/endpoint_measurements.py`, `web/queries/endpoint_description.rq`, `web/app.py`, `web/templates/endpoint.html`
- Test: `web/tests/`

The page names the run its facts came from, so if that run did not finish the page must say
so. The previous stage shipped exactly this shape of defect across a layer boundary: the
prober gained a vocabulary term and the read tier kept rendering the old meaning.

**The derivation is two conditions, not one.** `endpoint_measurements.rq` picks the most
recent run that recorded anything **for this endpoint**, so an endpoint a crashed run never
reached has no facts in it, the page shows the previous finished run, and a one-condition
derivation never fires. That is a regression in exactly the population this stage serves.
So:

- (a) **the run whose facts are shown did not finish** (it has `sw:emission` and no
  `sw:finalised`), or
- (b) **a newer run exists, did not finish, and has no `sw:completedEndpoint` for this
  endpoint.** This is what a crash produces at scale, and Task 1's marker is what makes it
  a fact rather than an inference from absence.

  Note what (b) costs: it needs **the newest activity in the store**, which is the first
  selection in either query that is not scoped to one endpoint.
  `endpoint_measurements.rq`'s contract has been per-endpoint since stage 2-1, so this is a
  deliberate widening: put it in its own named branch of the query, say in the query header
  why the branch is not per-endpoint, and say that the per-endpoint selection for the
  endpoint's own facts is unchanged.

Name the sentence each produces, and keep them distinct: they say different things.

**Both representations, and the RDF must not carry a derived flag.** The RDF comes from
`endpoint_description.rq`, whose own header states that the two representations must agree,
and stage 3-1's spec row claims they do. But condition (b) is an **absence** (no
`sw:completedEndpoint` for this endpoint, no `sw:finalised` for that run) and a CONSTRUCT
emits only presence, so a derived "unfinished" flag could be built for the HTML and not for
the RDF, and the two would diverge on exactly the case this task exists for.

So the CONSTRUCT emits the **inputs to the derivation**, not its result: the run's
`sw:emission`, its `sw:finalised` when present, and its `sw:completedEndpoint` for this
endpoint when present, for the newest run as well as the one being shown. An RDF consumer
then draws the same two conclusions from the same facts that the HTML draws, which is a
stronger form of agreement than copying a flag across. The negotiation test asserts the RDF
carries every input the HTML's sentence was derived from, **and the converse**: a finished
run's RDF carries `sw:finalised`. Without the positive case an absent quad is unreadable,
because a consumer cannot tell "this run did not finish" from "this representation does not
carry that fact".

- [ ] **Step 1: Write the failing tests** (a finished run, an unfinished run showing its own
  facts, an unfinished newer run that never reached this endpoint, and a pre-1c-b4 run that
  promised nothing and must read exactly as it does today)
- [ ] **Step 2: Run them, watch them fail, record what each printed**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Serve all four pages and look at them**
- [ ] **Step 5: Commit**

---

### Task 5: Document the guarantee and its limits

**Files:**
- Modify: `prober/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: The README**

The guarantee, its limits (no `fsync`, the zeroed-tail boundary, and the honest reasons a
lost run is acceptable), the three header/footer cases, the `<out>.partial` name, and the
new order.

**Four places currently disagree and all four are named:**

1. `README.md:590-596`, the order paragraph, which describes slots, `assemble` walking
   them, and `emit_nquads` writing family-by-family. The new order is: header, one chunk per
   endpoint in completion order, footer; within a chunk, that endpoint's measurements,
   not-measured facts, samples and `declarationsRead`.
2. `README.md:376-378`, the `failedEndpoints` sentence about an absent quad, invalidated by
   the move to the footer.
3. `emit.rs:293-297`, that field's own doc comment, which says it lets a reader tell a
   complete run from an incomplete one. `sw:finalised` now does that; `failedEndpoints`
   is only trustworthy beside it.
4. `README.md:747-759`, the known limitation "The run's output is still written once, at
   the end", which this stage removes.

Add the load-order sentence the review verified, because it is the non-obvious half of the
design working in our favour: `load_run.py` replaces a graph wholesale, so loading a
truncated run and later the same run's complete file leaves the complete one, and loading a
run twice is idempotent.

- [ ] **Step 2: The spec**

The 1c-b4 row at `:406` **already exists** and predicts this stage: update it to DELIVERED
rather than adding a row. Then the per-endpoint isolation bullet's "(Half delivered)" note
at `:141-143` becomes claimable in full, and the 1c-b3 row at `:405` carries its own
paragraph saying this half is not built, which becomes false. Fix all three, and say what
1d needs next.

- [ ] **Step 3: Commit**

---

## Done when

- The split publishes what the whole emission did, proven against a baseline frozen and
  committed green **before** the split, plus exactly two named run-level facts and one
  marker per endpoint.
- No fact family publishes its own summary before the things it summarises.
- A chunk truncated at a line boundary is **dropped**, not loaded; a syntax error in the
  middle still refuses the whole file; a file with no marker refuses with the parse error
  rather than succeeding with zero quads.
- An endpoint's facts are on disk before the sweep returns, proven by a test that would
  fail on a buffer-and-write-once implementation.
- A chunk plus the header answers the joins every read query needs.
- A second chunk for one endpoint is refused, which is what makes the per-chunk duplicate
  pre-scan complete.
- A chunk write failure stops the sweep and leaves what was written intact, proven with an
  injected failing writer rather than an unwritable path.
- A crashed sweep leaves the previous `--out` byte-identical, and its own partial file
  loadable.
- A panicked group still gets `prober-failed` chunks, written before the footer.
- The read tier says both things: this run did not finish, and a newer unfinished run never
  reached this endpoint. In both representations.
- All four disagreeing documentation sites are corrected, the spec's 1c-b4 row says
  DELIVERED, and the isolation bullet is claimed in full with no row left disagreeing.

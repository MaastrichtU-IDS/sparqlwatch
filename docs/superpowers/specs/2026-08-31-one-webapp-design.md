# One webapp: the prober in Python

**Status:** draft for review, revision 2
**Date:** 2026-08-31
**Decision:** taken by the owner on 2026-08-31, after being shown the cost and an
alternative that delivered the deployment win without the rewrite. Recorded here
because a spec that hides the tradeoff it was warned about is not reviewable.

**Revision 2** rewrites revision 1 against an adversarial review that checked
every claim against the source. Two of revision 1's four mitigations had holes
that would have shipped bugs, its module inventory was missing four modules
carrying real guarantees, its migration order was impossible, and the oracle its
whole verification bar rested on did not exist. Those corrections are marked
**[R2]** throughout so a reader can see what moved. The review is at
`scratchpad/fable-spec-review.md`.

**Goal:** One Python web application that probes, judges, stores and serves, with
no separate Rust binary and no `.nq` file handed between processes.

**Supersedes:** the two-tier split in `docs/architecture.md`, which must be
rewritten when this lands.

## What this buys, stated once and honestly

One language, and one operational fix.

`web/README.md:94` records that only one process may hold the Oxigraph store
open, so loading a run today means stop the server, load, start the server, and
it calls the deployment version of that an unanswered question. A prober running
inside the web process writes straight to the store, and the question stops
existing.

Everything else on the ledger is a cost.

## What this costs

- **477 Rust tests over 16 targets**, 7 of them driving a real HTTP server
  through wiremock. They do not port; they are rewritten.
- **`cargo clippy -D warnings`**, which gates this repo and does fire. **[R2]**
  Revision 1 called what it caught on 2026-08-30 a "real defect"; it was a dead
  `mut` failing a lint. The gate is real, that example was overstated, and this
  project's own culture says to keep such claims honest.
- **Three guarantees the Rust type system enforces at build time**, reproducible
  in Python only as tests. Hard problems 1 to 3.

The work is weeks and produces no user-visible change.

## [R2] Deployment invariant: exactly one process

**This is a hard constraint the whole benefit rests on, and revision 1 never
stated it.**

The store admits one writer. A merged prober writes, so the serving process is
the writer, so **there is exactly one of it.** Not one container: one *process*.
A `uvicorn --workers 2` or a gunicorn deployment reintroduces the store lock on
day one, inside the architecture that was supposed to remove it.

Two consequences to design for rather than discover:

1. **The app must refuse to start with more than one worker**, loudly, naming
   this invariant. A deployment that silently half-works is worse than one that
   will not boot.
2. **No horizontal scaling, ever, while this shape holds.** No replicas, and no
   rolling deploy without a gap in service.

Measured, against the running server holding the lock:

| | result |
|---|---|
| second read-write open | refused, `IO error ... LOCK` |
| read-only open | **succeeded, 53,836 quads** |

So one writer plus N readers is available. That is the escape hatch if the single
process ever becomes intolerable: serve from read-only replicas and keep one
writer. It is not this design, and taking it would give back the merge's only
operational win, so it belongs in the risks section and not in the plan.

**Related, and cheap to write down now:** asyncio primitives are loop-bound and
not thread-safe. A future "move the sweep to a thread so it stops competing with
serving" refactor silently breaks every politeness lock in this spec.

## Architecture after this lands

```
  registry/*.toml ─┐
  metrics.toml ────┼──▶  ONE Python process, ONE worker
  state/*.toml ────┘        │
                            ├── prober package: probes, judges, emits
                            ├── writes run-<instant>.nq  (kept: see below)
                            ├── loads it into the store IN-PROCESS
                            └── serves HTML and four RDF forms
```

**The `.nq` file stays.** It is no longer an inter-process handoff, and it is
still the source of truth: a sweep observes a changing world, so a lost run is
gone rather than reproducible, whichever language wrote it. Dropping the file
would make the store the only copy of an irreproducible observation.

## Package layout

**[R2]** Revision 1 mapped 11 of 17 source files. The four it omitted each carry
guarantees, and one of them blocks step 2 outright.

| new file | ports | responsibility |
|---|---|---|
| `web/prober/metrics.py` | `metrics.rs` | metric definitions, probe kinds, the `LIMIT` cross-check |
| `web/prober/verdict.py` | `verdict.rs` | the closed verdict vocabulary |
| `web/prober/observe.py` | **[R2]** `observe.rs` | the `Observation` type every probe returns |
| `web/prober/media.py` | **[R2]** `media.rs` | the RDF media-type allowlist, shared so classification and declaration parsing cannot disagree |
| `web/prober/declare.py` | **[R2]** `declare.rs` | service-description parsing into `Declarations`. **`resolve` cannot be tested without it.** |
| `web/prober/resolve.py` | `resolve.rs` | judgment. Pure. Three entry points, not one: see HP4. |
| `web/prober/emit.py` | `emit.rs` | facts to N-Quads. Pure. |
| `web/prober/authority.py` | **[R2]** the pure half of `politeness.rs` | `authority`, `host_key`, `parse_retry_after`. Pure, and needed by `registry`. |
| `web/prober/registry.py` | `registry.rs` | endpoint list, exclusions, credential stripping |
| `web/prober/dormancy.py` | `dormancy.rs` | admission policy. Pure, no clock. |
| `web/prober/state_file.py` | `state_file.rs` | state read/write, O_EXCL locking, fail-closed |
| `web/prober/write.py` | **[R2]** `write.rs` | crash-safe incremental writing. See below. |
| `web/prober/budget.py` | `budget.rs` | the three nested budgets |
| `web/prober/politeness.py` | the impure half of `politeness.rs` | per-host serialisation and gap |
| `web/prober/client.py` | `client.rs` | the six probe operations |
| `web/prober/sweep.py` | `main.rs`, `lib.rs` | orchestration |
| `web/prober/cli.py` | **[R2]** all three binaries | `sweep`, `dormancy` (`init`/`list`/`wake`/`sleep`/`prune`), `seed-registry` |

**[R2] `write.py` is not optional.** `write.rs` names the in-progress file
`<out>.<at>.partial` and not `<out>.partial`, because a fixed suffix means the
next sweep truncates the previous crash's partial on its first write. It refuses
an existing partial with `create_new`, flushes per chunk because a SIGKILL runs
no destructors, and renames atomically onto `--out`. **A Python sweep that just
opens `--out` destroys last night's complete run on its first crash.**

**[R2] `cli.py` is not optional either.** `state_file.rs` documents `dormancy
init` as the ONLY sanctioned creator of the state file, because a sweep that
created its own could not tell "first ever run" from "the volume did not mount".
Delete the Rust without porting the CLI and a fresh deployment cannot be
initialised and the registry cannot be reseeded.
## Hard problem 1: the map lock must not be held across an await

`politeness.rs` holds **two** locks with opposite requirements:

| lock | Rust type | requirement |
|---|---|---|
| the host map | `std::sync::Mutex<HashMap<String, Host>>` | must NOT be held across an await |
| one host's turn | `Arc<tokio::sync::Mutex<Option<Instant>>>` | MUST be held across the sleep |

The type system enforces it: a `std::sync::MutexGuard` is not `Send`, so a future
holding one across an await cannot satisfy a `Send` bound. A never-called function
`_acquire_future_is_send` exists purely to supply that bound, its doc comment
beginning at `politeness.rs:481`. Its reason: "A guarantee that disappears when
somebody deletes a test is not a structural guarantee."

**Decision.** `threading.Lock` for the map, used only for synchronous dict access
and never `await`ed inside. `asyncio.Lock` per host, held across the sleep.

### [R2] Which failure the test is for, because revision 1 tested for the wrong one

Revision 1 said the failure mode was "the sweep serialises behind one slow host,
hours instead of minutes." **That describes the wrong bug.** With a
`threading.Lock` for the map, a second coroutine calling `.acquire()` blocks the
event loop thread, the holder can never resume, and the sweep **deadlocks**. That
is louder than a stall, a hung test rather than a slow one. The "serialises"
description belongs to a different mistake, using an `asyncio.Lock` for the map.

Both are worth a test, and they are different tests.

### [R2] The duration test topology, corrected

Revision 1's test was "two hosts answering after a fixed delay, run concurrently,
assert one delay not two." **It can pass with the bug present.** The only waits
inside `acquire` are the min-gap sleep, which needs a PRIOR release stamp on that
host, and the stand-down wait, which needs a prior `stand_down`. Two fresh hosts
never sleep inside `acquire` at all, so a map lock held across that internal wait
is never exercised. The test only catches the grossest version, the map lock held
across the entire request.

The corrected shape forces the internal sleep:

> Issue one request to host A so it carries a release stamp, or `stand_down` host
> A for about a second. Then, concurrently, acquire host A (which must now sleep
> inside `acquire`) and host B. Assert **host B's acquire returns immediately**.
> With the map lock held across A's internal wait, B cannot proceed.

Verification requires this demonstrated failing with the lock deliberately
misplaced, and passing without. A test that has never failed has not been shown
to test anything.

## Hard problem 2: a cancelled probe must still release its host

`HostGuard` implements `Drop` (`politeness.rs:470`) and stamps the release
instant. It stamps at RELEASE, not acquisition, and lines 474-475 say why:
"stamped at acquisition, a request that took longer than the gap would leave no
pause at all." Budgets cancel by dropping the future, which runs `Drop`.

### [R2] The hole revision 1 left, which is the common case not an edge case

Revision 1's `async with` covers cancellation arriving in the **body**, mid-probe.
It does not cover cancellation arriving during **acquisition**.

In Rust, cancelling an `acquire` that is sleeping drops the `OwnedMutexGuard` and
the host is released; no stamp is written, which is correct because no request was
sent. In Python, if `__aenter__` has taken the `asyncio.Lock` and is then
cancelled during the gap or stand-down sleep, **`__aexit__` never runs** (PEP 343:
exit runs only if enter completed) and the lock leaks. Every later request to that
host waits forever, is budget-cancelled, and reports `indeterminate`. **The host
goes silently dark for the rest of the sweep**, which is a confident wrong answer
by omission.

And this is the designed-in path, not a rarity. `lib.rs`'s own comment on permits
says a stood-down host's acquire IS cancelled by the metric budget by design: it
"keeps its permit until the METRIC budget cancels the wait, then does it again for
the next endpoint in the group." **Every long stand-down takes this path.**

**Decision.** `__aenter__` wraps everything after taking the lock in a
`try/except CancelledError` that releases the lock and re-raises. `__aexit__`
stamps the release and performs no `await`, so it cannot be interrupted twice.
`CancelledError` is only delivered at await points, so a no-await exit is safe.

**[R2] Two cancellation tests, not one:**
1. cancel mid-probe, then assert the next request to that host waits the full gap
   (a lost stamp would let it go out immediately);
2. cancel inside `acquire`'s sleep, then assert the next acquire on that host
   **succeeds rather than hanging**.

**[R2] Pin Python >= 3.12.** Older `asyncio.Lock` had cancellation-during-acquire
wakeup bugs, which is exactly the path above.

## Hard problem 3: purity is a convention, not a type

`resolve`, `dormancy`, `verdict` and `metrics` contain zero clock calls, checkable
by grep across 2,215 lines of `dormancy.rs` alone. Nothing in Python stops
`import time` appearing next year.

### [R2] An allowlist, not a blocklist

Revision 1 proposed banning a list of imports. A blocklist always omits
something, and this one omitted `sys`, `subprocess`, `io`, `urllib`, `aiohttp`,
`requests`, `ctypes`, `importlib`, `uuid` (uuid1 reads the clock), `secrets` and
`threading`. Worse, three holes are not import-shaped at all:

- **Builtins need no import.** `open()`, `__import__("time")`, `eval`, `exec`.
  The test must ban these NAMES as well as import statements.
- **Transitivity.** `emit.py` importing a pure sibling that imports `datetime`
  passes a five-file check.
- **[R2] Hash-order nondeterminism.** Rust `emit` uses `BTreeMap`/`BTreeSet`
  throughout and deliberately preserves input order for samples, because
  "the order is evidence about the endpoint". Python set iteration is
  hash-randomised per process. **One `set` feeding output order makes the
  byte-equality golden test flaky in the worst way: passing most days.**

**Decision.** An **allowlist**, applied recursively to in-package imports:
`typing`, `dataclasses`, `enum`, plus `tomllib` for `metrics.py`, plus the pure
siblings. Any other import fails. The banned builtin names are checked too. And
no `set` or `frozenset` iteration may feed output order, enforced by the same AST
test where detectable and by review where not.

This is the one place the Python version is **stronger** than the Rust, where the
discipline is held by comments and review. **[R2]** Extending it to Rust first is
cheap, since the grep is already the test: a `#[test]` doing `include_str!` plus a
string search. Do that before the Rust goes, so the guarantee is gained rather
than swapped.

## Hard problem 4: replacing 477 tests without pretending they ported

### [R2] The oracle does not exist yet

Revision 1 wrote "the Rust binary as the oracle" as though the interface existed.
**It does not.** The three binaries are `sweep`, `seed-registry` and `dormancy`;
none accepts arbitrary facts or observations. Differential testing therefore
requires **writing new Rust harness code**, a stdin-JSON-to-output shim per pure
module, plus a cross-language fixture encoding. That harness is itself unreviewed
code sitting between the oracle and the check. The plan must budget for it and
review it.

### [R2] `resolve` is not one function

`resolve(def, declared, obs)` is one of **three** judgment surfaces.
`resolve_fetch(defs, obs) -> (Verdict, Option<Level>)` at `resolve.rs:338` has its
own status-code departures (404 and 410 on a description are answers, not
failures), and `Declared::from` is a third. The differential corpus must cover all
three, or the "whole judgment surface" claim is false.

### [R2] The input space is equivalence classes, not enumerable

`Observation` carries unbounded strings: `status`, `allow_origin`,
`allow_methods`, `allow_headers` (parsed by `allows_get` with comma-splitting,
wildcards and case rules), `content_type`, `body_kind`, `bindings`, `boolean`.
Choosing the equivalence classes **is** the judgment being ported, so the class
list is a review artefact in its own right, not a mechanical enumeration.

### Two things that make this cheaper than it looks

- **[R2] `pyoxigraph` wraps the same `oxrdf`** the prober serialises through
  (`prober/Cargo.toml`: `oxrdf = "0.3"`, `oxrdfio = "0.2"`). Building quads in
  Python and serialising with pyoxigraph gives identical escaping and term
  formatting by construction, rather than by careful reimplementation. Literals
  in the output path are integer-only.
- **[R2] But port the percent-encoder, do not substitute it.** Subject IRIs come
  from a hand-rolled unreserved-set encoder at `emit.rs:214` whose behaviour
  `urllib.parse.quote` does not match at its defaults (`safe='/'`).

### [R2] The golden runs need the Rust checked against itself first

Two caveats revision 1 missed. Re-emission needs an inverse parser (N-Quads back
to facts) that does not exist, and a parser written from the same reading of
`emit.rs` as the emitter can hide symmetric misunderstandings: it validates
ordering and rendering, not semantics. And two of the four preserved runs predate
later emitter changes.

**So: demonstrate the CURRENT Rust emitter re-emits all four runs byte-identically
BEFORE holding Python to that bar.** If it does not, the two oracles contradict
each other and the checklist is unsatisfiable as written.
## [R2] Hard problem 5: the client's HTTP semantics are measured decisions with hostile Python defaults

Revision 1 compressed all of this into one migration-table line. Each item below
was paid for in this codebase and each has a plausible-wrong Python default.

- **Timeout shape.** reqwest's `.timeout(30s)` bounds the WHOLE request.
  `httpx.Timeout(30)` is **per phase**: a server dribbling a byte every 29 seconds
  never times out. `aiohttp` needs `ClientTimeout(total=...)`. Get this wrong and
  the request budget stops bounding wall time.
- **Redirects.** `aiohttp` follows redirects **by default**. That silently
  reintroduces the exact defect `Policy::none()` plus `gated_chain` exist to
  prevent: per-hop politeness acquired on THE HOP'S host, which may not be the
  probed host, method never rewritten, query re-sent, 5-hop bound, cycle check
  over canonicalised URLs. **httpx does not follow by default, and that is part of
  why the library choice should be httpx.**
- **`elapsed_ms` deliberately excludes politeness waits.** It is the sum of hop
  request times, because "our politeness delay is not the endpoint's response time
  and must not be published as one." A port measuring wall clock across the chain
  publishes our own 2 s gaps as strangers' latency.
- **`truncate_body` cuts at 256 KiB in BYTES**, backing off to a UTF-8 boundary,
  and truncation happens BEFORE classification so classification and the
  declaration parse read the same bytes. Python's `s[:262144]` slices **code
  points**, keeping up to 4x the bytes, and the classification and declaration
  hash then diverge from the oracle.
- **`extract` accepts `typed-literal` as well as `literal`** (`client.rs:752`),
  which is Virtuoso's nonstandard SPARQL-JSON. A port leaning on a results library
  or on the spec grammar drops it, and the `AskData` literal guard then returns a
  confident false negative on the very guard listed under "what must not change".
- **`parses_as_rdf` requires at least one triple.** An empty clean parse is not
  evidence. And the media-type allowlist is deliberately narrower than a library's
  sniffing: `text/plain`, `application/json` and `application/xml` are rejected on
  purpose, for measured reasons.

## [R2] Hard problem 6: `authority` is pure, high-stakes, and was scheduled too late

`registry.rs:154` calls `crate::politeness::authority(url).host_port` for
credential stripping. Revision 1 put registry at step 4 and politeness at step 6.
**The order was impossible.**

Worse, the natural Python move is `urllib.parse`, which **disagrees** with this
hand-rolled parser on the cases its tests pin: uppercase schemes, a `?` before any
`/`, junk keeping distinct buckets, `/path://weird`, non-http default ports NOT
folded, and the empty-host fallback. The history here is a published-password
defect (`ftp://alice:s3cret@...`).

**Decision.** Split the pure half of politeness into `authority.py` as its own
early step, and verify it **differentially**, not behaviourally. These are the
easiest functions in the project to compare exactly, and revision 1's
"behavioural" label for step 6 gave that up for free.

## [R2] Hard problem 7: per-endpoint isolation, and `TaskGroup` is the wrong primitive

Revision 1's open decision called `asyncio.TaskGroup` "the direct analogue" of
`JoinSet`. **It is closer to the opposite**: TaskGroup's defining behaviour is
cancel-all-on-first-exception, and this project's rule is that one endpoint's
failure must not lose the others.

The analogue is manual tasks, or `gather(return_exceptions=True)`, with the
exception caught **inside** the group task. And isolation here is more than "do not
cancel siblings": `run_sweep` converts a panicked group into published
`NotMeasured { ProberFailed }` facts for exactly the endpoints whose slots are
empty, leaves already-delivered endpoints alone rather than republishing them as
failed, counts them in the footer, and keeps `ProberFailed` distinct from
`Indeterminate` on purpose, because "a reader could not tell a crashed sweep from
a slow endpoint."

Also port intact: concurrency counts **hosts**, not endpoints (endpoints grouped
by `host_key`, one sequential task per group, the permit held for the whole
group); the bounded arrivals channel (`ARRIVALS_IN_FLIGHT`, `lib.rs:192`) that
gives the writer backpressure; chunk-written-on-arrival versus `Sweep`-in-input-
order as two ordering properties that both hold; and the
drop-the-sender-before-draining shape whose failure mode "presents as a hang
rather than as a failure".

## [R2] Hard problem 8: the state file's discipline is not differential-testable

Revision 1's step 5 claimed dormancy and `state_file` together as differential.
**The policy is; the file discipline is not.** `state_file.rs` owns an O_EXCL lock
file, lock-then-read-INSIDE-the-lock (a read before the lock is exactly the
lost-update snapshot), fsync before rename, an explicit lock release so a failure
to remove it is reported, and fail-closed reads with a taxonomy separating
"missing, run `dormancy init`" from "unreadable".

These need **behavioural** tests of their own, including two concurrent merges.
Python note: the equivalent of `create_new` is
`os.open(..., O_CREAT | O_EXCL)`. Plain `open("x")` is not.

## [R2] Hard problem 9: monotonic time

Rust `Instant` is monotonic. A Python port using `time.time()` for release stamps
and stand-down instants inherits NTP steps and can compute a negative gap. Use
`time.monotonic()`.

Same family: Rust's overflow paranoia (`checked_add` on a stranger-controlled
`Retry-After: 9223372036854775807`) is harmless in Python's bignums, but
`parse_retry_after`'s rejection of negatives and floats must be preserved.
`int("12.5")` raising is convenient; `int("-5")` succeeding is the trap.

## What must not change

- **The six-verdict closed vocabulary**, `absent` only where the endpoint itself
  answered, a timeout always `indeterminate`.
- **The published RDF, byte for byte**, for the same facts. Subject IRIs derived
  from (run, endpoint, metric) and reversible. Sparqlwatch-owned predicates stay
  owned, for the `rdfs:domain void:Dataset` reason.
- **`emit`'s section protocol**: summary quads after the values they summarise.
- **Politeness**: 30 s request, 60 s metric, 600 s endpoint, 2 s gap measured
  release-to-start, never two requests in flight to one host, the stand-down wait
  being the LONGER of two conditions and not their sum, exclusions re-read every
  run, dormancy failing closed.
- **The `AskData` literal guard** and its trap: `?c` in `?s a ?c` binds an IRI, so
  a class metric must be `SelectIris`. Rediscovered by measurement twice.
- **No third-party endpoint contacted by CI.**

## Migration order

**[R2]** Corrected: `authority` moves early because `registry` needs it, and
`declare` moves before `resolve` because `resolve` cannot be tested without it.

| step | module | verified by |
|---|---|---|
| 0 | **[R2]** the differential harness itself | reviewed as code; it sits between oracle and check |
| 1 | `verdict`, `metrics` | differential: shipped `metrics.toml`, definitions and revision hash |
| 2 | **[R2]** `authority` | differential: the pinned URL cases, exactly |
| 3 | `observe`, `media`, **[R2]** `declare` | differential: same service descriptions, same `Declarations` |
| 4 | `resolve` **and** `resolve_fetch` **and** `Declared::from` | differential over reviewed equivalence classes |
| 5 | `emit` | differential byte-equality, then the four golden runs (after the Rust self-check) |
| 6 | `registry` | differential: same TOML, same admitted list, same credential stripping |
| 7 | `dormancy` | differential: same state and instant, same plan and next state |
| 8 | **[R2]** `state_file`, `write` | behavioural: O_EXCL, fsync, concurrent merges, kill -9 crash safety |
| 9 | `budget`, `politeness` | behavioural: the corrected duration test and BOTH cancellation tests |
| 10 | `client` | behavioural: six probe operations plus every item in HP5 |
| 11 | `sweep`, `cli` | end to end locally, then one live sweep of a few consenting endpoints |

**The Rust is deleted only after step 11, in its own commit**, so removing 17,000
lines is reviewable and revertible on its own.

Steps 0 to 7 touch no network and carry no risk to anyone's server. Steps 8 to 11
are where a mistake reaches a stranger's endpoint.

## Verification before the Rust is deleted

**[R2]** Revision 1's list would not have justified deletion. Additions marked.

- [ ] Differential tests pass exactly for every pure module, against the Rust.
- [ ] **[R2]** The current Rust emitter re-emits all four preserved runs byte-identically, demonstrated FIRST, so the two oracles are known consistent.
- [ ] All four runs then re-emit byte-identically through the Python emitter.
- [ ] **[R2]** The corrected duration test (HP1) demonstrated failing with the map lock misplaced, and passing without.
- [ ] **[R2]** BOTH cancellation tests (HP2) demonstrated failing, including cancellation inside `acquire`.
- [ ] The AST purity test uses an allowlist, is recursive, covers banned builtins, and fails when an import is deliberately added.
- [ ] **[R2]** Crash safety: `kill -9` a mid-sweep process; the previous `--out` is intact and the partial parses to its last terminator.
- [ ] **[R2]** State-file locking, fsync and stale-lock startup behaviour, including two concurrent merges.
- [ ] **[R2]** `dormancy` CLI parity, `init` especially, and a decision recorded about `seed-registry`.
- [ ] **[R2]** The web tier's own 342 tests and the reader golden pass against a store loaded in-process by the new path.
- [ ] **[R2]** `ruff` and `mypy --strict` configured and running in CI. "The clippy replacement is undecided" and "delete the Rust" cannot both be true.
- [ ] **[R2]** The app refuses to start with more than one worker.
- [ ] **[R2]** A duration sanity check on a real-shaped sweep, since CPU-bound RDF parsing of up to 256 KiB bodies per endpoint now runs on the serving loop.
- [ ] One live sweep produces a run file that loads and a page that renders, with the store lock never released.
- [ ] CI runs all of it and contacts no third-party endpoint.

## Open decisions

1. **[R2] Whether `POST /admin/sweep` exists at all**, and if so its authorisation
   story. Nothing in this project authenticates anything today. The CLI plus a
   scheduler may be the whole answer.
2. **[R2] `seed-registry`'s fate.** Port it, or accept that the registry is
   frozen until someone does.
3. Whether the AST purity test is added to the Rust side first, so the guarantee
   is gained rather than swapped. Recommended in HP3.
4. **[R2] The single-worker refusal's mechanism**: a startup check, a lock probe,
   or documentation only. Documentation only is not sufficient.

## Risks

- **The store-lock win survives only while the sweep runs in the serving
  process.** If stage 4 later runs the sweep as a separate CronJob for isolation,
  the problem returns unchanged and the merge will have bought nothing but one
  language. **Decide before step 11, not after.** The founding spec's Scheduling
  section argued FOR a CronJob, on the grounds that an in-process scheduler
  couples the web tier's uptime to the sweep's and makes a stuck sweep a restart
  of the whole app. That argument was correct and has been knowingly reversed;
  the reversal and its cost are recorded there rather than deleted.

- **[R3] The merge's justification is also void if Oxigraph becomes a service,
  and the founding spec planned exactly that.** Its Deployment section listed
  Oxigraph as its own `Deployment` plus PVC plus `Service`. That is Oxigraph's
  SERVER mode, reached over the SPARQL protocol, not the embedded library this
  project uses (`web/app.py:327` opens a RocksDB directory in process). In server
  mode **the store lock never exists**: the server serialises writers, so any
  number of web replicas and a separate prober can all talk to it.

  So there are two independent roads to "the merge bought only one language", and
  the second one is a deployment decision nobody has consciously taken yet. Both
  founding-spec sites are now corrected to the merged architecture, and the
  question is flagged there too, but it wants an explicit answer before stage 4
  builds anything. Doing it by accident would retire this spec's stated benefit
  without anybody noticing.

  It also changes what the rewrite is worth. If Oxigraph ends up a service, the
  honest ledger for this work is one language, at the cost of 477 tests and three
  build-time guarantees. That may still be the right trade; it should be made
  with the number in view.
- **A long sweep inside the serving process competes with serving.** The
  543-endpoint sweep took 1h26m21s. "It is async so it is fine" is not an
  argument: CPU-bound parsing blocks the loop.
- **The politeness guarantees become tests rather than build errors.** The
  accepted cost of the decision. The mitigation is that every one of those tests
  must be demonstrated failing.
- **[R2] The differential harness is new, unreviewed Rust** sitting between the
  oracle and the check. A bug in it can make a wrong port look right.

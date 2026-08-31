# Review: 2026-08-31-one-webapp-design.md

Reviewer: Fable (adversarial spec review). Date: 2026-08-31.
Method: every claim checked against the tree at `main` (clean, a4f0239 plus 84f1005 which added the spec). `cargo test` run in full. No network request left the machine except cargo's registry.

Verdict: **yes with changes**. The four hard problems are correctly diagnosed as problems, but two of the four proposed mitigations have verifiable holes, the module inventory is incomplete in ways that lose real guarantees, and the differential-oracle mechanism the whole verification bar rests on does not exist yet and is harder than the spec implies.

---

## 1. The four hard problems, checked against the source

### Hard problem 1: two locks. Diagnosis CONFIRMED, proposed test INSUFFICIENT.

The diagnosis is exactly right. `politeness.rs` holds a `std::sync::Mutex<HashMap<String, Host>>` (field comment says hold it "only long enough to clone the `Arc` out or read the stand-down instant beside it") and a per-host `Arc<tokio::sync::Mutex<Option<Instant>>>` taken with `lock_owned` and held across the sleep and the whole request. The never-called Send assertion exists: `_acquire_future_is_send`, doc comment starting at politeness.rs:481, `fn` at line 496, with the quoted sentence about deleted tests verbatim. The spec's line reference points at the comment rather than the `fn`; harmless.

The proposed duration test, as written, can pass by accident. Read `acquire`: the only waits inside it are (a) the min-gap sleep, which requires a PRIOR release stamp on that host, and (b) the stand-down wait, which requires a prior `stand_down`. The spec's test is "two hosts, each answering after a fixed delay, run both concurrently, assert wall clock is about one delay". Both hosts are fresh, so neither acquire ever sleeps; the fixed delay is the server answering, which happens after acquire has returned and only the per-host lock is held. A Python `acquire` that wrongly holds the map lock across its internal wait therefore sails through this test, because the internal wait never runs. The test only fails for the grossest bug, the map lock held across the entire request. The realistic bug (map lock held across the gap sleep or the stand-down wait inside acquire) needs a test shaped like: stand_down host A for ~1s (or issue two back-to-back requests to A so the second acquire sleeps the gap), concurrently acquire host B, assert B's acquire returns immediately.

Second inaccuracy in the failure-mode description: with the spec's own chosen design (a `threading.Lock` for the map), holding it across an await does not produce "hours instead of minutes". A second coroutine calling `lock.acquire()` blocks the event loop thread; the holder can never resume; the sweep deadlocks. That is louder than a stall (a hung test, not a slow one), but the spec should say which failure it is testing for, because the "serialises behind one slow host" description matches the asyncio-Lock-for-the-map mistake, a different bug with a different test.

The threading.Lock / asyncio.Lock split itself is the right mirror of the Rust shape.

### Hard problem 2: release on cancellation. Diagnosis CONFIRMED, decision has a hole the size of the common case.

Confirmed in the source: `impl Drop for HostGuard` (politeness.rs:470) stamps `Some(Instant::now())`, and the stamped-at-release reasoning is quoted accurately (lines 474-475). Budgets cancel by dropping the future (`tokio::time::timeout` in budget.rs, whose own test asserts the work is interrupted, not merely reported late).

The hole: the spec's `async with` decision and its cancellation test cover cancellation arriving in the BODY (mid-probe). They do not cover cancellation arriving inside acquisition. In Rust, when the metric budget cancels an `acquire` that is sleeping (gap or stand-down), dropping the future drops the `OwnedMutexGuard` and the host is released automatically; note `HostGuard` does not exist yet at that point, so no stamp is written, which is correct because no request was sent. In Python, if `__aenter__` has taken the `asyncio.Lock` and is then cancelled during the gap or stand-down sleep, `__aexit__` never runs (PEP 343 semantics: the context manager's exit only runs if enter completed), and the lock leaks. Every later request to that host waits forever, gets budget-cancelled, and reports `indeterminate`. The host goes silently dark for the rest of the sweep, which is precisely the "confident wrong answer by omission" this project exists to avoid.

And this is not an edge case. run_sweep's own comments (lib.rs, permit paragraph) say a stood-down host's acquire IS cancelled by the metric budget by design: "a host that asked for an hour keeps its permit until the METRIC budget cancels the wait, then does it again for the next endpoint in the group." Cancellation during acquire's sleep is the designed-in behaviour of every long stand-down. The Python `__aenter__` must catch `CancelledError` between lock acquisition and return, release the lock, and re-raise; and the verification list needs a second cancellation test: cancel a task inside acquire's sleep, then assert the next acquire on that host succeeds rather than hanging.

Minor: pin Python >= 3.12; older asyncio.Lock had known cancellation-during-acquire wakeup bugs.

The `__aexit__`-stamps-synchronously decision itself is sound: CancelledError is only delivered at await points, so a no-await exit cannot be interrupted.

### Hard problem 3: purity. Diagnosis CONFIRMED, blocklist has holes.

The grep claim reproduces exactly: `grep -c 'Instant::now\|SystemTime\|Utc::now'` over dormancy.rs returns 0, and the file is 2,215 lines. Same grep over resolve.rs, verdict.rs, metrics.rs: 0. emit.rs has one hit, which is a comment (line 1339, explaining why `SystemTime` is NOT used); the architecture table's claim stands.

Holes in the proposed AST test:

- `open()` is a builtin. Reading a file needs no import at all. Same for `__import__("time")`, `eval`, `exec`. The test must also ban these names, not only import statements.
- The blocklist omits `sys`, `subprocess`, `io`, `urllib`, `aiohttp`, `requests`, `ctypes`, `importlib`, `uuid` (uuid1 reads the clock), `secrets`, `threading`. A blocklist always omits something; invert it to an allowlist (say: `typing`, `dataclasses`, `enum`, `tomllib` for metrics.py, and the package's own pure siblings) and the test fails safe when someone adds any import at all.
- Transitivity: `emit.py` importing a sibling helper that imports `datetime` passes the five-file check. The allowlist must be applied recursively to in-package imports, or the five files must be forbidden from importing anything in the package outside the pure set.
- Not import-shaped but same family: Python set iteration order is hash-randomised across processes. Rust emit uses `BTreeMap`/`BTreeSet` throughout and deliberately preserves input order for samples ("the order is evidence about the endpoint", emit.rs:162). One `set` in the Python emit path makes the byte-equality golden test flaky in the worst way, passing most days. Worth a sentence in the spec: no `set`/`frozenset` iteration feeding output order, enforced by the same AST test if possible.

Extending the discipline to Rust first (open decision 3) is cheap: the grep is already the test; committing it as a `#[test]` shelling nothing (just include_str + string search) would do.

### Hard problem 4: differential testing. Right idea, and the spec understates what has to be built.

Feasibility, module by module:

- **metrics**: feasible and genuinely valuable. `definitions_revision` (metrics.rs:469) is FNV-1a over a canonical string, portable by construction ("has to outlive the toolchain"). But the canonical string embeds Rust `{:?}` Debug renderings of the `kind` and `cost` enums; Python must reproduce those exact strings (`AskData`, `Cheap`, ...). The differential test catches a mismatch, so this is fine, but the plan should know the hash is not language-neutral by accident, only by replication.
- **resolve**: "enumerate the input space, which is small and closed" overstates it. `resolve(def, declared, obs)` takes an `Observation` with unbounded string fields: `status: Option<u16>`, `allow_origin/methods/headers: Option<String>` (parsed by `allows_get` with comma-splitting, wildcard, case rules), `content_type`, `body_kind`, `bindings`, `boolean`. What is enumerable is the equivalence classes, and choosing them is exactly the judgment being ported. Also, resolve is not "the single judgment function": `resolve_fetch(defs, obs) -> (Verdict, Option<Level>)` (resolve.rs:338) is a second public judgment surface with its own status-code departures (404/410 on a description is an answer), and `Declared::from` a third. The differential corpus must cover all three or the "whole judgment surface" claim in the migration table is false.
- **emit**: feasible, and there is a concrete way to make byte-equality cheap that the spec misses: emit.rs serialises through oxrdf, and the web tier already ships pyoxigraph, which wraps the same oxrdf. Building quads in Python and serialising them with pyoxigraph gives the same escaping and term formatting by construction. Literals are integer-only (`xsd:INTEGER` throughout; no floats found in the output path), subject IRIs come from a hand-rolled unreserved-set percent-encoder (emit.rs:214) with pinned case/escape behaviour that `urllib.parse.quote`'s defaults do NOT match (`safe='/'`), so port the encoder, do not substitute it.
- **dormancy**: most feasible of the four; the `dormancy` CLI binary already exists as a partial oracle, and the policy takes `now` as a string.
- **The missing mechanism**: no Rust binary can currently be fed arbitrary facts or observations. The three binaries are sweep, seed-registry, and dormancy. Differential testing therefore requires writing NEW Rust harness code (a stdin-JSON-to-output shim per module) plus a cross-language fixture encoding, and that harness is itself unreviewed code sitting between the oracle and the check. The plan must budget for it and review it; the spec presents "the Rust binary as the oracle" as if the interface existed.

Golden runs: two caveats. First, re-emission requires an inverse parser (N-Quads back to facts) that does not exist, and a parser written from the same reading of emit.rs as the emitter can hide symmetric misunderstandings; it validates ordering and rendering, not semantics. Second, two of the four preserved runs predate later emitter stages (the baseline test in emit.rs records output moves on 2026-08-23 and 2026-08-24; the lod-cloud-543 and calibration-54 runs are dated 2026-08-24/25). Before holding Python to byte-identity, demonstrate the CURRENT Rust emitter re-emits all four runs byte-identically; if it does not, the differential oracle and the golden oracle contradict each other and the spec's checklist is unsatisfiable as written. Also confirmed: exactly four `.nq` runs exist in `~/code/sparqlwatch-runs/`, checksummed, including the 543 sweep (README confirms 1h26m21s).

---

## 2. Hard problems the spec misses

Ranked by how silently the port would get them wrong.

**A. The module inventory is incomplete, and the missing modules carry guarantees.** The package-layout table maps 11 modules; the tree has more, and four omissions matter:

- `write.rs` (802 lines) appears nowhere. It owns: the per-run partial file name `<out>.<at>.partial` (a fixed suffix loses the previous crash's data, rule 2 of its header comment), `create_new` refusal of an existing partial, refusal of a directory `--out` at t=0, flush-per-chunk because "a SIGKILL runs no destructors" (rule 3), atomic rename onto `--out`, and the symlink caveat. No step ports it, no verification item checks it. A Python sweep that opens `--out` directly destroys last night's complete run on the first crash.
- `declare.rs` (367 lines) appears nowhere, yet `Declared::from` and `resolve_fetch` take its `Declarations`. Step 2 ("resolve, differential") cannot even be executed without porting declare or its output type, and declare does real RDF graph walking (subtree traversal from `sd:Service` through linking predicates) that has its own wrong-answer modes.
- `media.rs` and `observe.rs` (the Observation type, `RDF_MEDIA_TYPES` allowlist logic shared so "the classification and the declaration parse can never again disagree").
- The other two binaries. `__main__.py` replaces the sweep CLI only. `dormancy init` is documented in state_file.rs as "the ONLY thing that may create" the state file (a sweep must not, because it cannot tell first-run from unmounted-volume); `wake`/`sleep`/`prune` are the operator interface. `seed-registry` builds the registry. Delete the Rust after step 8 and there is no way to initialise a fresh deployment's dormancy state and no way to reseed the registry.

**B. Single-process serving is a hard constraint the spec never states.** The entire benefit ("the question stops existing") holds only while exactly one process serves and sweeps. `web/README.md:94`: only one process can hold the RocksDB store. A uvicorn/gunicorn deployment with workers > 1 reintroduces the lock problem on day one, in the merged architecture that was supposed to remove it. The spec's risk section covers the CronJob variant but not the worker count. State it as a deployment invariant, and decide how the app refuses to start with workers > 1. Related: asyncio primitives are loop-bound and not thread-safe; a future "move the sweep to a thread so it stops competing with serving" refactor silently breaks every politeness lock. Worth one sentence now.

**C. The client's HTTP semantics are a cluster of measured decisions, compressed to one line in step 7.** Each of these was paid for and each has a plausible-wrong Python default:

- Timeout shape: reqwest's `.timeout(30s)` bounds the WHOLE request. `httpx.Timeout(30)` is per-phase (a server dribbling bytes every 29s never times out); `aiohttp` needs `ClientTimeout(total=...)`. Get this wrong and the request budget stops bounding wall time, and only the metric budget saves it, at double the intended cost.
- Redirects: `aiohttp` follows redirects by default; a port on it silently reintroduces exactly the ungated-invisible-hops defect `Policy::none()` plus `gated_chain` exist to prevent (per-hop gate acquisition ON THE HOP'S HOST, which may not be the probed host; method never rewritten; query re-sent; 5-hop bound; cycle check over canonicalised URLs). httpx does not follow by default, which is the right default here; the spec should pin the library partly on this ground.
- `elapsed_ms` is the SUM of hop request times, deliberately excluding politeness waits ("our politeness delay is not the endpoint's response time and must not be published as one"). A port measuring wall clock across the chain publishes our own 2s gaps as strangers' latency.
- `truncate_body` cuts at 256 KiB in BYTES, backing off to a UTF-8 char boundary, and truncation happens BEFORE classification so classification and declaration-parse read the same bytes (fetch_rdf_once comment records the self-contradictory row that ordering bug published). Python's `s[:262144]` slices code points, retaining up to 4x the bytes; classification and the declaration hash diverge from the oracle.
- `extract` accepts `typed-literal` as well as `literal`, which is Virtuoso's nonstandard SPARQL-JSON. Any Python port leaning on a results library, or on the spec grammar, drops it, and the AskData literal guard then reports `Some(false)`: a confident false negative on exactly the guard the spec lists under "what must not change".
- `parses_as_rdf` requires at least one triple (an empty clean parse is not evidence), and the media-type allowlist is deliberately narrower than the format-sniffing a library would do (`text/plain`, `application/json`, `application/xml` are rejected on purpose, with measured reasons).

**D. host_key / authority is pure, high-stakes, and scheduled too late.** registry.rs:154 calls `crate::politeness::authority` for credential stripping; step 4 (registry) precedes step 6 (politeness). The order is broken as written. Worse, the natural Python move is `urllib.parse`, which disagrees with this hand-rolled parser on the measured cases its tests pin (uppercase schemes, `?` before any `/`, junk keeping distinct buckets, `/path://weird`, non-http default ports NOT folded, empty-host fallback). The history here is a published-password defect (`ftp://alice:s3cret@...`). Fix: split the pure half of politeness (authority, host_key, parse_retry_after, honour) into an early step and verify it DIFFERENTIALLY, which step 6's "behavioural" label currently forgoes despite these being the easiest functions in the project to compare exactly.

**E. Per-endpoint isolation is more than "don't cancel siblings".** run_sweep converts a panicked group into published `NotMeasured { ProberFailed }` facts for exactly the endpoints whose slots are empty (already-delivered endpoints stay on disk and must NOT be republished as failed, `endpoints_without_facts`), logs them, counts them in the footer, and distinguishes `ProberFailed` from `Indeterminate` on purpose ("a reader could not tell a crashed sweep from a slow endpoint"). Open decision 1 calls `TaskGroup` "the direct analogue" of `JoinSet`; it is closer to the opposite, since TaskGroup's defining behaviour is cancel-on-first-exception. The analogue is manual tasks (or `gather(return_exceptions=True)`) with the exception caught inside the group task and the slot-based accounting reproduced. The spec flags the question; it should also correct the analogy so the plan does not start from the wrong primitive. Also worth porting intact: concurrency counts HOSTS (endpoints grouped by host_key, one sequential task per group, permit held for the whole group), the bounded arrivals channel (16) for writer backpressure, chunk-written-on-arrival versus Sweep-in-input-order (two ordering properties that both hold), and the drop-the-sender-before-draining shape whose failure mode "presents as a hang rather than as a failure".

**F. state_file's hard parts are not differential-testable, and step 5 claims they are.** The policy (dormancy) is differential; the file discipline is not: O_EXCL lock file, lock-then-read-inside-lock (`merge_state` comment: a read before the lock is exactly the lost-update snapshot), fsync-before-rename, explicit lock release so failure to remove it is reported, fail-closed reads with error taxonomy distinguishing missing (points to `dormancy init`) from unreadable. These need behavioural tests of their own, including two concurrent merges, and none is in the verification list. Python note: `os.open(..., O_CREAT|O_EXCL)` is the equivalent; `open("x")` is not.

**G. Monotonic time.** Rust `Instant` is monotonic. A Python port using `time.time()` for release stamps and stand-down instants inherits NTP steps; `time.monotonic()` is the equivalent. One sentence in the spec prevents it. Same family: `Duration` overflow paranoia (`checked_add` on a stranger-controlled `Retry-After: 9223372036854775807`) becomes harmless in Python's bignums, but the negative/float rejection in `parse_retry_after` must be preserved (`int("12.5")` raising is convenient; `int("-5")` succeeding is the trap).

---

## 3. The verification bar

Passing the listed checks would NOT justify deleting the Rust. Missing:

1. Crash-safety of the run writer (kill -9 a mid-sweep process; assert the previous `--out` intact and the partial parseable to its last terminator). The section protocol is pinned but the file discipline is not.
2. State-file locking and fsync behaviour (F above), including the stale-lock startup check.
3. The second cancellation test: cancel inside acquire's sleep, assert the host is not leaked (the listed test only cancels mid-probe).
4. A corrected duration test topology (see HP1) so the demonstrated failure is the realistic bug, not only the gross one.
5. Dormancy CLI parity (`init` especially, as the only sanctioned creator of the state file) and a decision about seed-registry.
6. The web tier's own 342 tests plus the reader golden against a store loaded in-process by the new path, since load_run semantics (replace-never-merge, current-pointer rules, drift detection) are now invoked by the same process that wrote the file.
7. Rust-emitter self-check on the four golden runs FIRST (see HP4), so the two oracles are known consistent before Python is measured against both.
8. ruff/mypy --strict actually configured and in CI (open decision 4 must close before deletion; "the clippy replacement is undecided" and "delete the Rust" cannot both be true).
9. Some duration sanity on a real-shaped sweep: the event-loop risk the spec itself raises (CPU-bound RDF parsing of up to 256 KiB bodies per endpoint, now on the serving loop) has no corresponding check.

Item 3 in the spec's list (demonstrate the duration test failing both ways) is exactly the right bar; it just needs to be pointed at a test that can fail for the right reason.

---

## 4. Factual claims, spot-checked

- **477 tests over 16 targets: CONFIRMED by running `cargo test`**: 16 "running" lines, 477 passed total, 2 ignored (live_smoke, correctly excluded). Suite is green on this tree.
- **7 wiremock files: CONFIRMED** (binary, client, cors_preflight, fetch, dormancy_sweep, end_to_end, politeness).
- **2,215 lines, zero clock calls in dormancy.rs: CONFIRMED** (wc and the spec's own grep, reproduced).
- **Six probe operations: CONFIRMED** (fetch_rdf, ask, cors, preflight, select_iris, ask_literal; exactly these are `pub async fn` on Client).
- **Budgets 30/60/600 and 2s gap: CONFIRMED** (budget.rs Default; `DEFAULT_MIN_GAP`, measured release-to-start via the Drop stamp; "longer of the two, not the sum" is literally `wait.max(...)`).
- **politeness.rs:481 and :470: CONFIRMED in substance** (481 is the first line of the never-called function's doc comment, `fn` at 496; 470 is `impl Drop`, quote at 474-475).
- **"clippy caught a real defect as recently as 2026-08-30": OVERSTATED.** Commit 9b45860 (2026-08-30) removes a dead `mut` that failed `-D warnings`. That is a lint nit, not a behavioural defect. The ledger's point survives (clippy gates this repo and fires), but "real defect" is the kind of claim this project's culture says to keep honest.
- Package-layout table maps 11 of 17 src files; see finding A for the consequential omissions.

## 5. Things checked and found correct, that a reader might doubt

- The two-locks diagnosis is not spec-writer folklore; the source enforces and documents it exactly as claimed, including the reason the Send assertion lives in src/ rather than tests.
- Stamp-at-release being load-bearing is real and doubly documented (Drop impl and acquire's doc).
- The revision hash is deliberately portable (FNV-1a, canonical rendering, "has to outlive the toolchain"), so step 1's differential check is well-founded.
- Emit is genuinely deterministic: BTree collections throughout, sample order deliberately preserved as evidence, integer-only literals in the output path. Byte-equality is an achievable bar, and pyoxigraph (same oxrdf underneath) makes it cheap.
- The 1h26m / 543-endpoint figure matches the runs README; the four golden runs exist and are checksummed.
- Keeping the `.nq` file, and the argument for it, is consistent with what load_run.py and the runs README actually say.
- The spec's honesty about costs (weeks, no user-visible change, tests-not-types) is accurate against the tree, not rhetorical.

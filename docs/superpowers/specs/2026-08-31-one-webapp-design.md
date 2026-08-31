# One webapp: the prober in Python

**Status:** draft for review
**Date:** 2026-08-31
**Decision:** taken by the owner on 2026-08-31, after being shown the cost and an
alternative that delivered the deployment win without the rewrite. Recorded here
because a spec that hides the tradeoff it was warned about is not reviewable.

**Goal:** One Python web application that probes, judges, stores and serves, with
no separate Rust binary and no `.nq` file handed between processes.

**Supersedes:** the two-tier split described in `docs/architecture.md`, which
must be rewritten when this lands.

## What this buys, stated once and honestly

One language, and one genuine operational fix.

`web/README.md` records that only one process may hold the Oxigraph store open,
so loading a run today means stop the server, load, start the server, and it
calls the deployment version of that "a question this stage does not answer." A
prober running inside the web process writes straight to the store, and the
question stops existing.

Everything else on the ledger is a cost.

## What this costs

- **477 Rust tests over 16 targets**, of which 7 integration files drive a real
  HTTP server through wiremock. They do not port; they are rewritten.
- **`cargo clippy -D warnings`**, which caught a real defect in this codebase as
  recently as 2026-08-30.
- **Three guarantees the Rust type system currently enforces at build time.**
  Each is reproducible in Python only as a test, and a test can be deleted. They
  are enumerated as Hard Problems 1 to 3 below, because they are the parts of
  this port most likely to fail silently.

The work is weeks, and produces no user-visible change. It is worth writing that
down so nobody later mistakes a quiet period for a stall.

## Architecture after this lands

```
  registry/*.toml ─┐
  metrics.toml ────┼──▶  one Python process
  state/*.toml ────┘        │
                            ├── prober package: probes the internet,
                            │     judges, emits facts
                            ├── writes run-<instant>.nq  (kept: see below)
                            ├── loads it into the store IN-PROCESS
                            └── serves HTML and four RDF forms
```

**The `.nq` file stays.** It is not an inter-process handoff any more, and it is
still the source of truth. `~/code/sparqlwatch-runs/README.md` says a sweep
observes a changing world, so a lost run is gone rather than reproducible, and
that is true whichever language wrote it. The store remains derived and
rebuildable from the files alone. Dropping the file to "simplify" would make the
store the only copy of an irreproducible observation, which is the opposite of
simplification.

## Package layout

The prober becomes a package inside the web application, mirroring the Rust
module boundaries so the differential tests in Verification have something to
compare against module by module.

| new file | ports | responsibility |
|---|---|---|
| `web/prober/metrics.py` | `metrics.rs` | metric definitions, probe kinds, the `LIMIT` cross-check |
| `web/prober/verdict.py` | `verdict.rs` | the closed verdict vocabulary |
| `web/prober/resolve.py` | `resolve.rs` | the single judgment function. Pure. |
| `web/prober/emit.py` | `emit.rs` | facts to N-Quads. Pure. |
| `web/prober/registry.py` | `registry.rs` | endpoint list, exclusions, credential stripping |
| `web/prober/dormancy.py` | `dormancy.rs` | admission policy. Pure, no clock. |
| `web/prober/state_file.py` | `state_file.rs` | dormancy state read/write, locking, fail-closed |
| `web/prober/budget.py` | `budget.rs` | the three nested budgets |
| `web/prober/politeness.py` | `politeness.rs` | per-host serialisation and gap |
| `web/prober/client.py` | `client.rs` | the six probe operations |
| `web/prober/sweep.py` | `main.rs` | orchestration |
| `web/prober/__main__.py` | the CLI | `python -m web.prober` for cron and for tests |

The CLI stays. A sweep must be runnable without an HTTP request, or the only way
to test the whole path is through the web tier, and cron in stage 4 needs it too.

## Hard problem 1: the map lock must not be held across an await

This is the sharpest thing in the port, and it is invisible if it goes wrong.

`politeness.rs` holds **two** locks with opposite requirements:

| lock | Rust type | requirement |
|---|---|---|
| the host map | `std::sync::Mutex<HashMap<String, Host>>` | must NOT be held across an await |
| one host's turn | `Arc<tokio::sync::Mutex<Option<Instant>>>` | MUST be held across the sleep |

The Rust type system enforces the distinction. A `std::sync::MutexGuard` is not
`Send`, so a future holding one across an await point does not satisfy a `Send`
bound. `politeness.rs:481` is a never-called function whose entire job is to
assert that `acquire`'s future is `Send`, and its comment says why it lives in
`src/` rather than in a test: "A guarantee that disappears when somebody deletes
a test is not a structural guarantee."

**In Python both are `asyncio.Lock` and nothing distinguishes them.** Hold the
map lock across the sleep and every request in the sweep serialises behind one
slow host. The sweep still completes. Every verdict is still correct. It just
takes hours instead of minutes, and no existing test would notice.

**Decision.** The map is guarded by a plain `threading.Lock` used only for
synchronous dict access, never `await`ed inside, and the per-host turn is an
`asyncio.Lock`. That mirrors the Rust shape, but mirroring is not enforcement, so
it is backed by a test that fails on the stall:

> Two hosts, each answering after a fixed delay. Run both concurrently and
> assert the wall clock is about one delay, not two. With the map lock held
> across the wait, this test fails; with it released, it passes. The test asserts
> a DURATION, which is the only thing that distinguishes the two
> implementations, since both produce identical output.

A duration assertion is a weaker guarantee than a compile error and that is the
cost being accepted here. It is written down so a future reader knows the test is
load-bearing and not a performance nicety.

## Hard problem 2: a cancelled probe must still release its host

`HostGuard` implements `Drop`, so the release stamp is written deterministically
however the probe ends, including a panic. The stamp is written at RELEASE and
not at acquisition, and `politeness.rs:470` says why: "stamped at acquisition, a
request that took longer than the gap would leave no pause at all."

Budgets cancel probes. `Budget::with_metric_budget` wraps a future in
`tokio::time::timeout`, and when it fires the future is dropped, which runs
`Drop` for anything it holds.

**In Python, budget expiry raises `asyncio.CancelledError` inside the coroutine.**
A `finally` block does run, but the release must survive cancellation arriving
while the coroutine is already suspended inside the sleep, and `asyncio.wait_for`
shields nothing by default.

**Decision.** The guard is an `async with` context manager whose `__aexit__`
stamps the release time, and the stamp itself performs no `await`, so it cannot
be interrupted a second time. Backed by a test that cancels mid-probe and then
asserts the next request to that host still waits the full gap: if the stamp were
lost, the next request would go out immediately.

## Hard problem 3: purity is a convention, not a type

`resolve()` and `dormancy` take no clock, no filesystem and no socket, and today
that is checkable mechanically: `grep -c 'Instant::now\|SystemTime\|Utc::now'`
over `dormancy.rs` returns 0 across 2,215 lines. Nothing in Python stops
`import time` appearing in `resolve.py` next year.

**Decision.** A test walks the AST of `resolve.py`, `dormancy.py`, `emit.py`,
`verdict.py` and `metrics.py` and fails if any of them imports `time`,
`datetime`, `os`, `pathlib`, `httpx`, `socket` or `random`, or references
`asyncio`. Not a lint rule in a config somebody can loosen: a test in the suite
CI runs, next to the tests that assert what those modules compute.

This is the one place where the Python version can be made STRONGER than the
Rust, and it should be: the Rust discipline is a convention held by comments and
review, while an AST test is an assertion. Whether to extend it to the Rust side
before deleting it is a question for the plan, not this spec.

## Hard problem 4: replacing 477 tests without pretending they ported

They do not port. The plan must not treat "the Python suite is green" as
equivalent to "the Rust suite was green", because the Python suite will start
tiny.

**Decision.** Two mechanisms, and the first is the important one.

**Differential testing against the Rust binary, module by module.** The pure
modules can be compared exactly, because they are pure: feed the same input to
both and require byte equality. This is available for `metrics` (load a
`metrics.toml`, compare the parsed definitions and the revision hash), `resolve`
(enumerate the input space, which is small and closed, and compare every
verdict), `emit` (build the same facts and compare the N-Quads byte for byte),
and `dormancy` (same state, same instant, same plan).

That is the whole judgment surface of the project, verified against 17,000 lines
of working, reviewed Rust rather than against a re-reading of it. The Rust binary
stays in the tree until this is done, precisely so it can be the oracle.

**Golden runs as an end-to-end oracle.** Four real run files are preserved in
`~/code/sparqlwatch-runs/`, checksummed, including a 543-endpoint sweep. Parse
one, feed its facts to the Python emitter, and require the emitted N-Quads to
match the file. This cannot validate the network layer, since a sweep is not
reproducible, but it validates everything downstream of the network in one
assertion against real data.

**Behavioural tests for the impure modules.** wiremock has no Python equivalent
worth pretending about; `pytest-httpserver` or a hand-rolled `aiohttp` test
server covers the same ground. These are written fresh, and the plan enumerates
them from the six probe operations (`fetch_rdf`, `ask`, `cors`, `preflight`,
`select_iris`, `ask_literal`) plus the redirect, media-type and
literal-guard behaviours in `client.rs`, rather than counting them and hoping.

## What must not change

These are the properties the rewrite is not permitted to alter, and each is
already pinned by a test on the Rust side that the plan must reproduce.

- **The six-verdict closed vocabulary**, and `absent` only where the endpoint
  itself answered. A timeout is `indeterminate`.
- **The published RDF, byte for byte**, for the same facts. Subject IRIs stay
  derived from (run, endpoint, metric) and reversible. Sparqlwatch-owned
  predicates stay owned, for the `rdfs:domain void:Dataset` reason.
- **`emit`'s section protocol**: summary quads after the values they summarise,
  so a truncated write loses a fact rather than misstating one.
- **Politeness**: 30 s request, 60 s metric, 600 s endpoint, 2 s minimum gap
  measured release-to-start, never two requests in flight to one host, the
  stand-down wait being the LONGER of two conditions and not their sum, an
  exclusion list re-read every run, and a dormancy state that fails closed.
- **The `AskData` literal guard**, and the trap it creates: `?c` in `?s a ?c`
  binds an IRI, so a class metric must be `SelectIris`. This has been rediscovered
  by measurement twice.
- **No third-party endpoint is contacted by CI.**
## Migration: incremental, with the Rust binary as the oracle

Not a big bang. The site keeps working throughout, and each module is verified
against its Rust counterpart before the next one starts.

Order is by dependency, pure modules first, effects last:

| step | module | verified by |
|---|---|---|
| 1 | `verdict`, `metrics` | differential: parse the shipped `metrics.toml`, compare definitions and revision hash |
| 2 | `resolve` | differential: enumerate the closed input space, compare every verdict |
| 3 | `emit` | differential: same facts, byte-equal N-Quads. Plus the four golden runs. |
| 4 | `registry` | differential: same TOML and exclusions, same admitted list, same credential stripping |
| 5 | `dormancy`, `state_file` | differential: same state and instant, same plan and same next state |
| 6 | `budget`, `politeness` | behavioural, including the duration test from Hard problem 1 and the cancellation test from Hard problem 2 |
| 7 | `client` | behavioural against a local test server, one case per each of the six probe operations plus redirects, media types and the literal guard |
| 8 | `sweep`, `__main__` | end to end against a local test server, then one live sweep of a handful of consenting endpoints |

**The Rust prober is deleted only after step 8**, and its deletion is its own
commit, so the diff that removes 17,000 lines is reviewable on its own and
revertible on its own.

**Steps 1 to 5 are the whole judgment surface and carry no risk to anyone's
server**, because nothing in them touches the network. Steps 6 to 8 are where the
politeness guarantees live, and where a mistake reaches a stranger's endpoint.

## Verification before the Rust is deleted

All of these, not a selection:

- [ ] Differential tests pass for every pure module, exactly, against the Rust.
- [ ] All four preserved runs re-emit byte-identically through the Python emitter.
- [ ] The duration test from Hard problem 1 fails when the map lock is deliberately held across the wait, and passes when it is not. Demonstrated both ways, in the task report, because a test that has never failed has not been shown to test anything.
- [ ] The cancellation test from Hard problem 2, likewise demonstrated failing.
- [ ] The AST purity test covers all five pure modules and fails when an import is deliberately added.
- [ ] One live sweep against a handful of endpoints produces a run file that loads, and a page that renders, with the store lock never released.
- [ ] CI runs the whole thing, and still contacts no third-party endpoint.

## Open decisions

1. **Concurrency primitive for the sweep.** Rust uses `JoinSet` with a semaphore
   across hosts. `asyncio.TaskGroup` plus `asyncio.Semaphore` is the direct
   analogue and is the assumption here, but the interaction between `TaskGroup`'s
   cancellation-on-first-exception and this project's per-endpoint isolation rule
   (one endpoint's failure must not lose the others) needs settling in the plan.
2. **Whether the sweep is HTTP-triggerable.** A `POST /admin/sweep` is the reason
   the merge was chosen, but it needs an authorisation story, and there is none
   today: nothing in this project authenticates anything.
3. **Whether the AST purity test is extended to the Rust side first**, so the
   guarantee is strictly gained rather than swapped, before the Rust goes.
4. **What happens to `cargo clippy`'s role.** `ruff` and `mypy --strict` are the
   nearest equivalents and neither is configured in this project yet.

## Risks

- **The store lock, which is the reason for the merge, is only solved while the
  sweep runs in the serving process.** If stage 4 ever runs the sweep as a
  separate CronJob for isolation, the problem returns unchanged, and the merge
  will have bought nothing. Decide that before step 8, not after.
- **A long sweep inside the serving process competes with serving.** The
  543-endpoint sweep took 1h26m. Whatever runs it must not hold the event loop,
  and "it is async so it is fine" is not an argument: a CPU-bound emit over
  thousands of facts will block it.
- **The politeness guarantees become tests rather than build errors.** Recorded
  in Hard problems 1 and 2. This is the accepted cost of the decision, and the
  mitigation is that both tests must be demonstrated failing.

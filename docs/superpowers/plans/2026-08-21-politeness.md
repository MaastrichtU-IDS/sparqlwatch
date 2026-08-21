# Stage 1c-b2: Per-Host Politeness

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Never have two requests in flight to one host, space consecutive requests to a host, and honour `Retry-After`, so a sweep of 548 strangers' endpoints is something an operator would tolerate.

**Architecture:** One new module holding a per-host gate, acquired at the outermost request boundary in `Client` and nowhere else. Two pure helper functions (a host identity, and `Retry-After` parsing) tested on their own. No change to judgement, emission, or the metric set.

**Tech Stack:** Rust 1.96, edition 2021. tokio, reqwest 0.13, wiremock 0.6.5. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`, whose risk table already commits us to "per-host concurrency of 1, delays, honest User-Agent with a contact URL, honour `Retry-After`". This slice delivers the first, second and fourth; the User-Agent already carries a resolvable contact URL.

**Supersedes part of:** `docs/superpowers/plans/2026-08-21-safe-at-scale.md`, whose header records why stage 1c-b was split into four slices.

## Why this is the slice that gates seeding

Nothing currently spaces our requests. At the default cost ceiling a sweep makes
seven requests per endpoint as fast as the endpoint answers, and several registry
URLs share a host. Stage 1d seeds 548 endpoint URLs from a real-world dump; run
today, that is a burst against every host in it. This is the difference between
being experienced as a monitor and as a nuisance, and an operator who blocks us
costs us the access the whole project depends on. So the defaults here err
toward slow.

## The two hazards this plan is shaped around

A review of the earlier combined plan found both, and they are why the design
below looks more careful than "add a sleep".

**Reentrancy.** `Client::preflight` resolves a redirect chain by calling
`preflight_once` in a loop, and `ask`, `cors`, `select_iris` and `ask_literal`
all funnel through `get_with_body`. A gate acquired in the inner helper would be
acquired once per redirect hop, and a non-reentrant per-host lock held across
that loop self-deadlocks: the task waits for a lock it already holds. So the gate
is acquired in the **public** methods only, exactly once per logical probe, and
the inner helpers never touch it.

**A lock held across an await.** The obvious shape is a map from host to a
per-host lock. If the map's own lock is held while awaiting the per-host one, the
whole sweep serialises behind one host and no test written against an
uncontended first acquire would notice. The fix is structural rather than
disciplinary: make the map's lock a `std::sync::Mutex`, whose guard is not `Send`,
so holding it across an `await` in a future that must be `Send` **fails to
compile**. The per-host lock, which is meant to be held across the request, is a
`tokio::sync::Mutex`.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.**
  Note that this does not block you: `tokio::sync::Mutex` and
  `tokio::task::JoinSet` already compile with the features `Cargo.toml` enables
  today (`rt-multi-thread` brings `sync` and `rt` with it). I verified that by
  building a throwaway module before writing this plan, so if you find yourself
  reaching for a new dependency or a feature flag, re-read this line first.
- `resolve()` stays a pure function. `src/emit.rs` is a pure function of its
  inputs with **no clock read** and no randomness. `--at` is never read from the
  clock.
- **The politeness gate reads the clock, and that is correct.** It is scheduling,
  not measurement. No clock read may leak into `resolve.rs` or `emit.rs`, and
  nothing the gate observes may reach a published verdict except through the
  existing budget and observation paths.
- `absent` and `verified` are assertive verdicts, publishable only when the
  evidence establishes them. A host we stopped probing did not tell us anything,
  so its remaining metrics are `indeterminate`.
- The probe dispatch in `lib.rs` has no `_` arm and `ProbeKind::ALL` is complete
  by construction. Keep both.
- Every test offline against local `wiremock`. Only `live_smoke` touches real
  endpoints and stays `#[ignore]`d.
- No em-dashes anywhere. The repository is free of them.
- After any experiment that edits source, restore it and run
  `touch src/*.rs tests/*.rs` before the final test run, and **verify a mutation
  actually applied** (print or diff the mutated state) before concluding
  anything from it. Both failure modes have already cost this project real time.
- A test you have not watched fail is not evidence.

## Starting state

`main` at `ef8a841`. 212 tests pass, 2 ignored, clippy clean with
`--all-targets -- -D warnings`.

```rust
// src/client.rs
pub struct Client { http: reqwest::Client, no_redirect: reqwest::Client }

// Public, request-issuing. These are the acquisition points:
pub async fn fetch_rdf(&self, url: &str) -> Observation;                       // own request
pub async fn ask(&self, url: &str, query: &str) -> Observation;                 // via get_with_body
pub async fn cors(&self, url: &str, query: &str) -> Observation;                // via get_with_body
pub async fn select_iris(&self, url: &str, query: &str, var: &str) -> Observation;  // via get_with_body
pub async fn ask_literal(&self, url: &str, query: &str, var: &str) -> Observation;  // via get_with_body
pub async fn preflight(&self, url: &str) -> Observation;                        // loops preflight_once

// Private, must NEVER acquire:
async fn get_with_body(&self, url: &str, query: &str, send_origin: bool) -> (Observation, String);
async fn preflight_once(&self, url: &str) -> (Observation, Option<String>);

// src/budget.rs
pub struct Budget { pub request: Duration, pub metric: Duration, pub endpoint: Duration }
```

---

## Task 1: Host identity, and reading `Retry-After`

Two pure functions, tested on their own before anything holds a lock.

**Files:**
- Create: `prober/src/politeness.rs` (declare it in `lib.rs`)
- Test: unit tests in `prober/src/politeness.rs`

**Interfaces:**
- `pub fn host_key(url: &str) -> String`: the politeness identity of a URL.
  Lowercased host plus port, ignoring scheme, path, query and userinfo. Two URLs
  with the same key are one server and must never be in flight together.
- `pub fn parse_retry_after(value: &str) -> RetryAfter`, where
  `pub enum RetryAfter { Seconds(Duration), HttpDate, Unparseable }`. Three
  variants rather than an `Option`, because the three cases get different
  treatment in Task 3 and collapsing them loses the distinction.
- `pub enum Honour { Wait(Duration), TooLong }` and
  `pub fn honour(requested: Duration, cap: Duration) -> Honour`, so the cap
  decision is pure and testable separately from any waiting.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_host_key_is_the_server_not_the_url() {
    for (a, b) in [
        ("http://example.org/sparql", "https://example.org/other"),
        ("http://Example.ORG/a", "http://example.org/b"),
        ("http://user:pw@example.org/a", "http://example.org/b"),
        ("http://example.org/a?query=x", "http://example.org/b"),
    ] {
        assert_eq!(host_key(a), host_key(b), "{a} and {b} are one server");
    }
    assert_ne!(host_key("http://a.example.org/x"), host_key("http://b.example.org/x"));
    // A port is part of the server: two engines commonly share a host, and
    // serialising them together would halve our throughput for no politeness
    // gain. Note this differs from `declare::same_endpoint`, which normalises a
    // default port away because it is answering a different question (is this
    // the same SERVICE), and say so in a comment.
    assert_ne!(host_key("http://example.org:7878/x"), host_key("http://example.org:7879/x"));
}

#[test]
fn a_url_we_cannot_parse_still_gets_a_key() {
    // The registry is seeded from a real-world dump. A key that panics or
    // collapses every junk URL into one bucket would either crash the sweep or
    // serialise unrelated hosts behind each other.
    let a = host_key("not a url at all");
    let b = host_key("also not a url");
    assert!(!a.is_empty());
    assert_ne!(a, b, "distinct junk must not share a bucket");
}

#[test]
fn retry_after_reads_delta_seconds() {
    assert_eq!(parse_retry_after("120"), RetryAfter::Seconds(Duration::from_secs(120)));
    assert_eq!(parse_retry_after("  30 "), RetryAfter::Seconds(Duration::from_secs(30)));
    assert_eq!(parse_retry_after("0"), RetryAfter::Seconds(Duration::ZERO));
}

#[test]
fn retry_after_distinguishes_a_date_from_junk() {
    // We do not parse the HTTP-date form (see Task 3 for what we do instead),
    // but we must not mistake it for junk: a server that answered precisely
    // deserves a different response from one that sent nonsense.
    assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), RetryAfter::HttpDate);
    assert_eq!(parse_retry_after("banana"), RetryAfter::Unparseable);
    assert_eq!(parse_retry_after(""), RetryAfter::Unparseable);
    assert_eq!(parse_retry_after("-5"), RetryAfter::Unparseable,
               "a negative delay is malformed, not immediate");
    assert_eq!(parse_retry_after("12.5"), RetryAfter::Unparseable,
               "delta-seconds is an integer; a float is malformed, not rounded");
}

#[test]
fn a_delay_beyond_the_cap_is_not_waited_out() {
    // A server asking us back in an hour has told us to go away for this sweep.
    // Waiting would blow the endpoint budget and hold a slot for nothing.
    assert_eq!(honour(Duration::from_secs(3600), Duration::from_secs(120)), Honour::TooLong);
    assert_eq!(honour(Duration::from_secs(5), Duration::from_secs(120)),
               Honour::Wait(Duration::from_secs(5)));
    assert_eq!(honour(Duration::from_secs(120), Duration::from_secs(120)),
               Honour::Wait(Duration::from_secs(120)), "the cap itself is honoured, not refused");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml --lib politeness`
Expected: the module does not exist.

- [ ] **Step 3: Implement**

Recognise the HTTP-date form without parsing it: a value that is not an integer
but contains a comma and ends in `GMT` is a date. That is deliberately a sniff,
not a parser, and the comment should say so.

`host_key` must not use a URL crate (none is a dependency). Hand-roll it: strip
the scheme if present, take everything before the first `/`, drop anything before
an `@`, lowercase, and return that. For an unparseable string, return the trimmed
lowercased input rather than a constant, so two junk URLs do not share a bucket.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations, each alone and each verified applied: lowercase dropped; the `@` strip
dropped; the port included in the "same server" set (make `host_key` strip the
port, which must fail the port assertion); `honour` using `>=` instead of `>` so
the cap itself is refused; the negative-delay guard removed. Restore, `touch`,
re-run.

- [ ] **Step 5: Commit**

```bash
git add prober/src
git commit -m "feat(prober): a host identity and a Retry-After reader, as pure functions"
```

---

## Task 2: The gate

**Files:**
- Modify: `prober/src/politeness.rs`
- Test: `prober/tests/politeness.rs`

**Interfaces:**
- `pub struct Politeness` with `pub fn new(min_gap: Duration) -> Politeness`.
- `pub async fn acquire(&self, url: &str) -> HostGuard<'_>`: waits until the
  host is free **and** the minimum gap since its last release has elapsed, then
  returns a guard. Dropping the guard stamps the release time and frees the host.
- The gap is measured from **release**, not from acquisition, so a slow request
  does not shorten the pause that follows it.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn two_requests_to_one_host_are_spaced_by_the_minimum_gap() {
    let p = Politeness::new(Duration::from_millis(300));
    let t0 = Instant::now();
    drop(p.acquire("http://example.org/a").await);
    drop(p.acquire("http://example.org/b").await);
    assert!(t0.elapsed() >= Duration::from_millis(300),
            "the second request to one host waited for the gap");
}

#[tokio::test]
async fn different_hosts_do_not_wait_for_each_other() {
    let p = Politeness::new(Duration::from_secs(30));
    let t0 = Instant::now();
    let (_a, _b) = tokio::join!(p.acquire("http://a.example.org/x"), p.acquire("http://b.example.org/x"));
    assert!(t0.elapsed() < Duration::from_secs(1),
            "a long gap on one host must not serialise unrelated hosts");
}

#[tokio::test]
async fn one_host_never_has_two_requests_in_flight() {
    // The gap alone does not give this: two tasks could both find the gap
    // elapsed and proceed together. The guard has to exclude.
    let p = std::sync::Arc::new(Politeness::new(Duration::ZERO));
    let live = std::sync::Arc::new(AtomicUsize::new(0));
    let peak = std::sync::Arc::new(AtomicUsize::new(0));
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (p, live, peak) = (p.clone(), live.clone(), peak.clone());
        set.spawn(async move {
            let _g = p.acquire("http://example.org/x").await;
            let n = live.fetch_add(1, SeqCst) + 1;
            peak.fetch_max(n, SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            live.fetch_sub(1, SeqCst);
        });
    }
    while set.join_next().await.is_some() {}
    assert_eq!(peak.load(SeqCst), 1, "requests to one host must not overlap");
}

/// The test the earlier plan could not write: it detects the map's lock being
/// held while awaiting a per-host lock. A first acquire is uncontended and so
/// cannot show it. Hold one host busy, then time an acquire on a DIFFERENT host.
#[tokio::test]
async fn a_busy_host_does_not_block_the_map() {
    let p = std::sync::Arc::new(Politeness::new(Duration::ZERO));
    let held = p.acquire("http://slow.example.org/x").await;
    let t0 = Instant::now();
    let other = p.acquire("http://fast.example.org/x").await;
    assert!(t0.elapsed() < Duration::from_millis(100),
            "acquiring a free host must not wait behind a busy one");
    drop(other);
    drop(held);
}

#[tokio::test]
async fn the_gap_is_measured_from_release_not_from_acquisition() {
    let p = Politeness::new(Duration::from_millis(200));
    let g = p.acquire("http://example.org/a").await;
    tokio::time::sleep(Duration::from_millis(200)).await; // a slow request
    drop(g);
    let t0 = Instant::now();
    drop(p.acquire("http://example.org/b").await);
    assert!(t0.elapsed() >= Duration::from_millis(150),
            "a slow request must not consume the pause that follows it");
}
```

Timing assertions are one-sided (at least the gap, or comfortably under a much
larger bound) so a loaded machine cannot make them flap. Do not assert an upper
bound close to the gap.

- [ ] **Step 2: Run to verify they fail**

Expected: no `Politeness` type.

- [ ] **Step 3: Implement**

```rust
pub struct Politeness {
    /// Host key to per-host state. A `std::sync::Mutex` on purpose: its guard is
    /// not `Send`, so holding it across an `await` fails to compile in a future
    /// that must be `Send`. That makes "the map lock is held while waiting for a
    /// host" a compile error rather than a subtle stall no test would catch.
    /// Hold it only long enough to clone the `Arc`.
    hosts: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Instant>>>>>,
    min_gap: Duration,
}
```

The per-host value is `Option<Instant>`: the release time, `None` before the
first release. `acquire` clones the `Arc` out of the map, drops the map guard,
awaits the per-host lock, then sleeps out any remainder of the gap while holding
it. The guard stamps `Some(Instant::now())` on drop.

Stamping on drop needs care: a `Drop` impl cannot await. Keep the per-host lock
guard inside `HostGuard` and write the release time through it synchronously in
`Drop`, which is possible because the value is a plain `Option<Instant>` behind a
lock we already hold.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations, each verified applied: `acquire` returns immediately without waiting
(the gap tests must fail); the per-host lock removed so only the gap remains (the
overlap test must fail); key on the full URL (the same-host test must fail);
stamp the release time at acquisition instead of at drop (the
measured-from-release test must fail); hold the map guard across the per-host
await (this must **fail to compile**, and say so in your report, since that is
the structural guarantee rather than a test).

- [ ] **Step 5: Commit**

```bash
git add prober/src prober/tests/politeness.rs
git commit -m "feat(prober): a per-host gate that serialises and spaces requests"
```

---

## Task 3: Acquire once per probe, and honour `Retry-After`

**Files:**
- Modify: `prober/src/client.rs`
- Modify: `prober/src/main.rs` (two flags)
- Test: `prober/tests/politeness.rs`

**Interfaces:**
- `Client::new(budget, politeness: Politeness)`. **There are 49 `Client::new`
  call sites** across `src/` and `tests/`; count them yourself first, and expect
  the suite to be red until the last one is updated.

  The parameter is explicit rather than defaulted, and that is a deliberate cost.
  A `Client::new` that quietly means "no politeness" would be exactly the silent
  default this crate refuses everywhere else: an unknown probe kind, an unknown
  cost and a missing `var` are all load errors precisely so that a file cannot
  look like it says something and not say it. A constructor whose default is
  impoliteness, in a project whose thesis is being a tolerable guest, is the same
  defect wearing different clothes.

  So provide `Politeness::unlimited()` for tests, with a doc comment saying it
  exists for tests and must never appear in a sweep. Tests pass it; `main.rs`
  passes the real settings. **Do not give the tests the production gap**: at 2
  seconds per request across 49 call sites the suite would take many minutes and
  somebody would soon delete the politeness rather than the slowness.
- Two flags: `--min-gap-ms` (default **2000**) and `--retry-after-cap-s`
  (default **120**).

- [ ] **Step 1: Write the failing tests**

```rust
/// The reentrancy hazard, as a test rather than a hope. A preflight that
/// resolves a redirect chain acquires ONCE. If the gate were acquired per hop
/// this deadlocks and the test times out rather than failing cleanly, so it runs
/// under an explicit timeout to make the failure legible.
#[tokio::test]
async fn a_preflight_resolving_a_redirect_chain_acquires_once() {
    let server = /* 303 from /a to /b, /b grants */;
    let client = Client::new_with_politeness(Budget::default(), Duration::from_millis(50), CAP).unwrap();
    let fut = client.preflight(&format!("{}/a", server.uri()));
    let o = tokio::time::timeout(Duration::from_secs(5), fut).await
        .expect("acquiring the gate once per hop would deadlock here");
    assert_eq!(o.status, Some(204));
}

#[tokio::test]
async fn every_public_probe_goes_through_the_gate() {
    // One host, one small gap, six probes. If a probe bypassed the gate the
    // elapsed time would fall below the floor. Asserts a FLOOR, so it cannot
    // flap on a slow machine.
    let server = an_endpoint_that_answers_everything().await;
    let client = Client::new_with_politeness(Budget::default(), Duration::from_millis(120), CAP).unwrap();
    let url = format!("{}/sparql", server.uri());
    let t0 = Instant::now();
    client.ask(&url, "ASK{}").await;
    client.cors(&url, "ASK{}").await;
    client.select_iris(&url, "SELECT ?c WHERE{?s a ?c}", "c").await;
    client.ask_literal(&url, "SELECT ?g WHERE{?s ?p ?g}", "g").await;
    client.fetch_rdf(&url).await;
    client.preflight(&url).await;
    assert!(t0.elapsed() >= Duration::from_millis(120 * 5),
            "six gated probes to one host wait five gaps");
}

#[tokio::test]
async fn a_retry_after_within_the_cap_is_waited_out_and_the_request_retried() {
    // 429 with Retry-After: 1 on the first call, 200 on the second.
    let server = /* mock that 429s once then succeeds */;
    let client = Client::new_with_politeness(Budget::default(), Duration::ZERO, Duration::from_secs(120)).unwrap();
    let o = client.ask(&format!("{}/sparql", server.uri()), "ASK{}").await;
    assert_eq!(o.status, Some(200), "the retry happened and the second answer is what we report");
}

#[tokio::test]
async fn a_retry_after_beyond_the_cap_is_not_waited_out() {
    // Retry-After: 3600 with a 2s cap. Must return promptly, reporting the 429.
    let t0 = Instant::now();
    let o = client_with_cap_2s.ask(&url, "ASK{}").await;
    assert!(t0.elapsed() < Duration::from_secs(10), "we did not wait an hour");
    assert_eq!(o.status, Some(429), "and we report what the endpoint actually said");
}

#[tokio::test]
async fn an_unparseable_retry_after_is_not_guessed_at() {
    // We do not know how long to wait, so we do not wait and do not retry. The
    // 429 stands and resolves to `indeterminate`, which is honest: the endpoint
    // told us nothing about the capability.
    let o = client.ask(&url_that_429s_with("banana"), "ASK{}").await;
    assert_eq!(o.status, Some(429));
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement the acquisition rule**

Acquire in `fetch_rdf`, `ask`, `cors`, `select_iris`, `ask_literal` and
`preflight`, and **nowhere else**. `get_with_body` and `preflight_once` must not
acquire: they are called from inside an already-held guard, and acquiring there
would deadlock on the redirect loop.

Write that rule as a comment on both private helpers, naming the deadlock. A
future reader adding a sixth public method needs to know which side of the line
it is on.

- [ ] **Step 4: Implement `Retry-After`**

On a `429` or `503` carrying `Retry-After`, inside the held guard:

- `Seconds(d)` and `honour(d, cap) == Wait(d)`: sleep `d`, retry the request
  **once**, and report the second response. One retry, not a loop: a server that
  throttles twice is telling us to come back later.
- `Seconds(d)` and `TooLong`: do not wait. Report the 429 or 503 as observed.
- `HttpDate`: do not wait and do not retry. Log it at `warn` with the value, so
  we learn whether real endpoints use this form. **Report the response as
  observed.** We are not guessing a delay from a format we do not parse, and
  guessing wrong in the short direction is exactly the impoliteness this slice
  exists to prevent.
- `Unparseable`, or no header: report as observed, no wait, no retry.

In every non-retry case the observation carries the real status, so `resolve()`
maps it to `indeterminate` by the existing rules. Nothing here invents a verdict.

- [ ] **Step 5: Prove the tests are load-bearing**

Mutations, each verified applied: acquire inside `get_with_body` as well (the
preflight test must fail, by deadlock caught as a timeout); remove the acquire
from one public method, `fetch_rdf` say (the all-probes-gated test must fail);
retry regardless of the cap (the beyond-cap test must fail); retry in a loop
instead of once (report what breaks, and if nothing does, add a test that
counts the requests the mock received). Restore, `touch`, re-run.

- [ ] **Step 6: Commit**

```bash
git add prober/src prober/tests
git commit -m "feat(prober): gate every probe once, and honour Retry-After within a cap"
```

---

## Task 4: Say what a sweep costs, and what has not been measured

**Files:**
- Modify: `prober/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: Document the contract**

The defaults and why they are what they are: 2 seconds between requests to one
host, at most one request in flight per host, `Retry-After` honoured up to 120
seconds and otherwise reported as observed, one retry rather than a loop. Say
that the HTTP-date form of `Retry-After` is recognised but not parsed, and what
we do instead.

Say plainly that a host we stopped probing yields `indeterminate` for its
remaining metrics, not `absent`: we stopped asking, which tells us nothing about
the endpoint.

- [ ] **Step 2: Do the arithmetic, and label it arithmetic**

With the defaults, work out what a 548-endpoint sweep costs: requests per
endpoint at the default cost ceiling, the per-host gap, and the resulting
wall-clock for a **sequential** sweep, which is what exists until stage 1c-b3
adds concurrency. Show the working so a reader can check it.

State that this is arithmetic and not a measurement, that no 548-endpoint sweep
has been run, and that the figure is the reason 1c-b3 exists.

- [ ] **Step 3: Spec**

Mark the risk table's probing row as partly delivered, naming which parts: 
per-host concurrency of 1, delays, and `Retry-After` are done; the published
probe schedule is not. Update the delivery sequence to show 1c-b2 done.

- [ ] **Step 4: Commit**

```bash
git add prober/README.md docs/superpowers/specs
git commit -m "docs: what a polite sweep costs, and what we have not measured"
```

---

## Done criteria

- Four tasks committed, suite green, clippy clean with `--all-targets -- -D warnings`.
- A real sweep over the three endpoints in `endpoints.toml` produces the same
  verdicts as before, and takes measurably longer for a reason the README explains.
- Holding the map lock across a per-host await does not compile, and the report
  says so.
- No private helper in `client.rs` acquires the gate.
- The README's sweep estimate is labelled arithmetic, not measurement.

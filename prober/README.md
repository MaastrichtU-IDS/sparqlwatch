# sparqlwatch prober

Probes public SPARQL endpoints and writes what it observed as RDF. One sweep
produces one immutable named graph of DQV quality measurements with PROV
provenance. That graph is the whole output: there is no database, no state
carried between runs, and no score.

## The rule the whole thing serves

**It must never report a confident wrong answer.** Each (endpoint, metric) pair
resolves to exactly one of six verdicts:

| Verdict | Meaning |
| --- | --- |
| `verified` | A probe confirms it works, and where a declaration is possible, the endpoint declares it |
| `undeclared-but-verified` | Works, and the endpoint could have declared it but did not (see the endpoint's `declarationsRead` fact for whether we could read its description) |
| `declared-but-wrong` | Answered, and answered incorrectly |
| `declared-only` | Claimed, not confirmable by probe |
| `absent` | Neither claimed nor observed |
| `indeterminate` | We never got to find out |

The declared/observed axis applies only where a declaration is possible, so
only a metric carrying a `declared_by` in `metrics.toml` can ever produce
`undeclared-but-verified`. `geo-functions` is the only one today. For every
other metric there is no term in the service-description vocabulary that could
advertise the capability (liveness, CORS headers, class counts), so "the
endpoint declares nothing" would say nothing about the endpoint, and a
confirmed probe reads simply `verified`.

`absent` may only be claimed when the evidence actually establishes absence:
the endpoint itself has to have answered the question we asked. For most
probes that means it answered with a 2xx status, in a form we could read. Two
probes have a further exception, in both cases because the status code *is* the
answer to their question rather than a fact about our request:

- the description fetch, where a `404` or `410` counts as absence, because
  those two statuses speak to what is published at the URL itself;
- the CORS preflight, where a `405` or a `501` counts as absence, because a
  browser's `fetch` requires the preflight to answer with an ok status, so
  those statuses are the endpoint saying it will not serve a cross-origin
  query. A redirect is not one of them: the probe resolves the chain and
  judges the response at the end of it, and a chain it could not resolve is
  `indeterminate`.

A timeout, an unreachable host, a 429, a gateway error, an HTML query console,
an unparseable body (anything else we never got to interpret) is
`indeterminate`. There is deliberately no composite score and no ranking, here
or downstream.

All judgement lives in one pure function, `resolve()` in `src/resolve.rs`. The
HTTP client returns evidence and no opinion; the emitter is a pure function of
its inputs, with no clock read and no randomness.

A service description is fetched once per endpoint with a queryless GET request
that asks for RDF (`text/turtle, application/rdf+xml;q=0.9, application/ld+json;q=0.8`),
and its declarations are compared against what the probes observe. Where a
declaration is possible, a confirmed capability usually reports as
`undeclared-but-verified`, because almost no endpoint declares its
capabilities: in the survey behind this project, 18 endpoints evaluate
`geof:sfWithin` and not one declares it. That is the finding this verdict
exists to publish, which is why metrics nothing could declare are kept out of
it.
The `service-description` metric now carries a graded level (0 to 4) rather than
always being indeterminate, grading by informativeness rather than presence.

## Running it

```sh
cargo run -- --at 2026-08-20T12:00:00Z --out run.nq
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--at` | *required* | The run instant, ISO-8601 with an explicit timezone |
| `--endpoints` | `endpoints.toml` | Endpoint list to sweep |
| `--metrics` | `metrics.toml` | Metric definitions to apply |
| `--out` | `run.nq` | Where the finished N-Quads land. The run itself is written to `<out>.<at>.partial` and renamed onto this at the end |
| `--max-cost` | `cheap` | Only run metrics with cost at or below this value (`cheap` or `expensive`) |
| `--min-gap-ms` | `2000` | Minimum pause between two consecutive requests to one host |
| `--retry-after-cap-s` | `20` | Longest `Retry-After` waited out before one retry of a throttled request |
| `--concurrency` | `4` | How many HOSTS to probe at once; one host is never asked two things at once |

`--at` is required and is **not** read from the clock, deliberately. It names
the run graph, it is published as the activity's `prov:generatedAtTime`, and a
scheduled `CronJob` passes the scheduled instant, so a retry of a failed sweep
lands in the same graph rather than inventing a second one. That also makes the
run's IDENTIFIERS reproducible: same `--at`, same graph and same subjects. Not
its contents and not its bytes, because a sweep observes a changing world; see
"Output is not byte-identical" below. It is validated before any probing starts,
because it is interpolated into IRIs and published as an `xsd:dateTime`.

A retry of a failed sweep therefore meets the partial file the failed attempt
left, and it **refuses to start** rather than overwriting it, naming the file and
saying what can be done with it. That is the point of writing to a sibling in the
first place: an attempt that died at endpoint 500 of 548 has 500 endpoints on
disk, a retry has no prior on getting further, and this process is not the one to
decide those 500 are worth less than a fresh start. Load the partial file, or move
it aside, then run again. The same refusal means two invocations sharing an `--at`
cannot interleave into one file.

Every outbound request passes a per-host gate that gives three guarantees: never
two requests in flight to one host, at least `--min-gap-ms` between one request
finishing and the next one to that host starting, and nothing at all to a host
before the instant that host asked us to come back at. The gate is taken
once per outbound **request**, not once per probe, and in one place only. No
probe follows a redirect implicitly: a chain is walked a hop at a time, and each
hop takes the gate for the host that hop actually touches, which is not
necessarily the host the probe was pointed at. A three-hop chain therefore costs
two gaps, and that is the honest price of a promise with no exceptions in it. A
chain longer than five hops, a cycle, or a `Location` we cannot resolve is
`indeterminate`: we never reached an answer.

`--concurrency` does not weaken any of the three guarantees, and it generalises
the first one only as far as the host an endpoint NAMES. Endpoints are grouped
by that host and each group is probed by one task, so two endpoints of one host
are never in flight together and never contend for its gate: raising the flag
adds hosts in flight, never requests to a host it was pointed at. The hop case
just described is the exception, because a redirect is gated on the host the hop
actually touches: an endpoint redirecting into another host of the same sweep
queues at that host's gate, which a sequential sweep never did because nothing
else was running. The gate itself still holds, one request in flight and one
gap; what it costs is that the wait is charged to the redirecting endpoint's
metric budget. See Known limitations.

The default of 4 is politeness rather than throughput. Four hosts in flight,
each of them spacing its own requests by 2 seconds, is roughly two requests per
second in aggregate, which is a defensible load for a service that probes
strangers uninvited. Zero is refused by the parser rather than repaired
downstream, because `Semaphore::new(0)` does not fail, it hangs, and a sweep
that probes nothing and reports nothing is the worst failure this crate has.

One endpoint per host in flight is not only politeness: it is what keeps
`--min-gap-ms`'s startup check the only budget relation a sweep needs. The
per-host guard is held until the request returns, so a second endpoint of the
same host would wait for the guard rather than for the gap alone. That wait is
gap plus the whole first request, and against a throttled host gap plus request
plus the honoured `Retry-After` plus the retry, the same 2 + 30 + 20 + 30 = 82
seconds the retry arithmetic below arrives at, inside a 60 second metric budget.
Grouping on the same `host_key` the gate acquires removes that hold term for the
host an endpoint names, which is why the check compares the gap against the
metric budget and nothing else. Relaxing per-host serialisation would mean
rewriting that check around the 82 second hold instead of around the gap, and
nobody has taken that decision. The one place the hold term survives is the
cross-host redirect priced under Known limitations.

The unit of all three guarantees is `host_key`'s answer, not the server itself.
Two spellings it keys apart (`a.example.` and `a.example`, or `http://x:443/`
and `https://x/`) are two groups, so with concurrency they can carry two request
streams to one server, and a `Retry-After` stand-down recorded under one key
does not defer the other. There is no budget consequence, because the grouping
key and the gate key are the same function and so nothing waits on anything; the
cost is politeness, and the fix is a registry that spells one server one way.

`--min-gap-ms` is validated at startup, before any probing: the gap plus the
30s request budget has to stay under the 60s metric budget, or the pause alone
consumes the budget the measurement needs and every metric reports
`indeterminate` against an endpoint that answered perfectly.

A `429` or `503` carrying a `Retry-After` in delta-seconds defers the **host**
until that instant, whether or not the delay is one we are willing to wait out:
a server that tells us to come back later is not sent a different question in
the meantime. Every later request to that host, from any metric, waits at the
gate until the instant passes. Within `--retry-after-cap-s` the request is also
retried **once** after the wait; a longer delay, an HTTP-date value or junk is
not retried at all, and the throttle is reported as observed rather than turned
into a guess. The retried walk takes the gate again, hop by hop, like a first
one, and it is that acquisition which waits the delay out, so there is exactly
one place in the crate that decides how long we wait.

The deferral is deliberately not bounded. A host that asks for an hour gets an
hour, the metric and endpoint budgets cancel the requests that queue behind it,
and those metrics report `indeterminate`, which is exactly what happened: we
never got to ask. A second bound here would be a second place deciding how long
we wait.

The cap's ceiling is arithmetic, not taste, and the arithmetic is about the
ordinary throttle rather than the worst case. Four things come out of one 60s
metric budget, in this order: the gap, the first request, the honoured wait, and
the retried request. A throttle usually comes back quickly, since refusing a
request is cheap for the server refusing it, so what has to fit is
`gap + cap + request budget < metric budget` (2 + 20 + 30 = 52 < 60). A larger
cap eats that margin, so raising it makes a cancelled retry **more** likely, not
less.

No cap value makes the worst case fit. A first request that runs its full 30s
timeout before the throttle arrives costs 2 + 30 + 30 = 62 > 60 even with a cap
of zero: a gap plus two full-timeout requests is already over the metric budget
on its own. A retry is therefore best-effort inside that budget. When it does
not fit, `tokio` cancels it and the metric reports `indeterminate`, which is
correct, because we never got an answer. A retry guaranteed to fit in every case
would need a metric budget above 82 seconds, and nobody has taken that
decision.

Three nested budgets bound the work, per request (30s), per metric (60s), and per
endpoint (600s), and every one of them cancels the future rather than
reporting afterwards that it took too long. A metric the budget never reached
is `indeterminate` and carries no `elapsedMs`, because nothing was measured.

## The seeded registry

Two endpoint lists live in this crate and they are not interchangeable.

| File | What it is |
| --- | --- |
| `endpoints.toml` | The **development list**: three endpoints, written by hand, and the default `--endpoints`. `registry.rs` asserts it loads exactly three entries with `qlever.dev` first, and none of the three appears in the LOD Cloud dump. |
| `registry/lod-cloud.toml` | The **seeded list**: 543 candidates extracted from a LOD Cloud dump. Generated, so a hand edit is overwritten by the next re-seed and fails the fixed-point test in `src/bin/seed-registry.rs`. |
| `registry/lod-cloud.provenance.toml` | Which dump produced that list, and every count behind it. A parseable file rather than a comment header, so a test can read it back and compare it against the list beside it. Generated too, and a fixed point of its own writer for the same reason the list is: a hand edit or a change to the renderer fails a test rather than standing as the only record of where the list came from. |
| `registry/exclusions.toml` | The **exclusion list**: the hosts this project does not probe, because somebody asked. The one file in `registry/` that is not generated, and the one a re-seed leaves alone. See Asking not to be probed below. |
| `registry/calibration-sample.toml` | 54 candidates cut out of the seeded list to price a sweep before one was attempted. It is a sample and not a registry, and its own header says so. |

No sweep reaches the seeded list by accident. `--endpoints
registry/lod-cloud.toml` is how a run gets it, a plain `cargo run` uses the
three-endpoint development list, and `seed-registry` has no flag that can write
to `endpoints.toml`.

### Asking not to be probed

`registry/exclusions.toml` names the hosts this project does not probe. It is
the one file in `registry/` that is **not generated**: `seed-registry` never
writes it, has no flag naming it, and a re-seed leaves it byte for byte as it
was. That is the whole point of it. Deleting a URL from `lod-cloud.toml` by hand
lasts until the next re-seed, which regenerates the list from the dump and puts
the host straight back.

One table per host, and both fields are required:

```toml
[[exclusion]]
host = "sparql.example.org"
reason = "A person asked, 2026-08-25"
```

`reason` cannot be missing or blank: an entry without one **fails the load**. An
anonymous exclusion is one nobody can maintain, because removing it would be as
unaccountable as adding it, and "a person asked" with a date is a perfectly good
reason. Put no personal data there: the file is public and the reason is copied
into the warning the drop emits, so the fact that a request was made and when is
enough.

`host` is a host and not a URL: no scheme, no port, no path. A value carrying
`/`, `:`, `@` or a space **fails the load** rather than standing as an exclusion
that quietly matches nothing, which is what pasting a URL out of a server log
would otherwise produce. Case and a trailing DNS root label are normalised, so
`Asked.Example.` and `asked.example` are one entry.

The list is applied in two places and needs both:

- `load_endpoints`, so an excluded host never reaches a sweep whatever file
  `--endpoints` names, hand-written or seeded.
- `seed::candidates`, so an excluded host is **absent from a freshly seeded
  registry** rather than merely skipped when one is read.
  `registry/lod-cloud.toml` is committed and public, and leaving a host in it on
  the strength of a filter somewhere else would publish the name of a host that
  asked to be left alone while giving a reader no way to see that the sweep
  skips it.

It is the only rule in `registry.rs` that fails the load instead of dropping an
entry with a warning. The others fail open because the endpoint list is seeded
from real-world dumps and refusing the whole file over one bad string would mean
monitoring nothing. This one cannot fail open: failing open means probing a host
that asked not to be, and a sweep that does not happen is a smaller wrong than a
sweep somebody asked us not to run.

The file carries one worked example, `sparqlwatch-exclusion-worked-example`,
which nobody asked for and whose `reason` says so. A single-label host is
refused by no other rule in `registry.rs`, which makes it the one input that can
show the mechanism is wired in at all:
`the_shipped_exclusion_list_is_subtracted_by_load_endpoints` would prove nothing
against an empty list.

Adding an entry can red two other tests, and both reds are the point. If the
host is in `lod-cloud.toml`, re-seed, or the fixed-point test fails because the
loader no longer reads back what that file holds. If it is in
`calibration-sample.toml`, that sample's own count test fails and the sample has
to be re-cut, because its measured costs then describe a population that no
longer includes the host.

**What this mechanism does not do.** Every line of this belongs on any page that
tells a sysadmin how to ask to be excluded:

- **Nothing watches a mailbox.** An exclusion becomes real when a person adds it
  to the file and commits it, and it takes effect at the next build, because
  `registry.rs` compiles the file in with `include_str!`. A deployment already
  running honours the file it was built from until it is rebuilt. There is no
  automation anywhere between a request and the file.
- **Nothing already published is retracted.** A run graph is immutable and
  append-only and the endpoint URL is part of the subject of every fact about it,
  so an exclusion stops future sweeps and leaves past measurements standing.
- **The granularity is the name.** Every URL on an excluded host goes, whatever
  its scheme, port or path, and one path cannot be excluded while another is
  kept. The match is on the whole host and not a suffix, so `sub.asked.example`
  is not covered by an entry for `asked.example`: hosts under one domain
  routinely belong to different people, and a suffix match would silently remove
  endpoints belonging to parties who never asked. Somebody who runs several
  names names them all.
- **Nothing is resolved.** A second name for the same server, or a server
  renamed, is a second entry, because nothing here asks a resolver what an entry
  points at.
- **An entry publishes the host it excludes**, in a public repository. A hashed
  entry would be checkable by nobody, including the person who asked.
- **This crate carries no contact address**, so nothing here tells anybody where
  to send the request. The page that does is the page that has to carry the
  address.

### Re-seeding

The dump is **not committed**: it is 4 MB of a third party's data, and `[dump]`
in the provenance file says where to get it.

```sh
cargo run --bin seed-registry -- \
  --dump ~/code/umaka-test/lod-data.json \
  --sha256 d94fdb394ed423e305f8dc51040ff01a155023f60dbfcc4b62ff87f9b6a2cc35 \
  --source https://lod-cloud.net/versions/2026-06-15/lod-data.json \
  --dump-version 2026-06-15 \
  --downloaded 2026-08-19
```

`--source` has to be the **versioned** URL, because a hash is only worth
recording if the exact bytes can be fetched again and checked; an unversioned
path is the case where the hash records what was used and nothing recovers it.
`--dump-version` and `--downloaded` are two flags because a versioned dump can
be fetched long after it was published. The tool **computes the digest itself**
and writes nothing if it disagrees with `--sha256`, so the hash in the
provenance file describes the bytes that produced the list beside it rather than
what somebody typed. Nothing here reads a clock, so re-seeding one dump writes
the same two files byte for byte and a diff shows only what the dump or the rule
changed.

Both files are written together or not at all: each text goes to a `.tmp` path
beside its destination and both are renamed only after both writes have
succeeded. Two independent `std::fs::write` calls had a window in which a full
disk or a read-only `registry/` left a regenerated list beside a provenance file
describing the previous dump, which is the state the digest gate exists to
prevent. A crash between the two renames can still leave one new file beside one
old one; that window is two renames wide rather than two file writes wide.

A write that fails partway **leaves its `.tmp` files behind**, because the tool
would rather leave a staged file for an operator to see than delete evidence of
what it was doing when it failed. They are safe to remove, and the next
successful re-seed renames over them. `a_second_write_that_fails_leaves_neither_destination_written`
in `tests/seed_registry.rs` is what holds the rename after both writes: it plants
a directory where the provenance's `.tmp` has to go, which is the only shape that
distinguishes renaming-after-both from renaming-as-you-go, and moving the rename
into the write loop reds it.

**That SHA-256 is hand-written, and it is for provenance only.** No digest crate
is in this project's lock file and a checksum for provenance did not warrant
adding one, so `sha256_hex` in `src/bin/seed-registry.rs` is FIPS 180-4 written
out by hand. It is asserted against four published vectors, including the
56-byte case where the length no longer fits in the block and pads into a second
one, which is the most common place a hand-rolled SHA-256 is wrong, and the
digest it recorded for the real 4,172,695-byte dump agrees with `shasum -a 256`.
That is enough to identify an input. It is **not** the code to use for anything
security-bearing, and nothing in this project asks it to be.

### LOD Cloud only, and why

The seeded list comes from the LOD Cloud dump alone. The design names a second
source, YummyData's curated list, and that one lives in the application's
database rather than in a checked-in file
(`~/code/umakadata/db/migrate/20190904034259_create_endpoints.rb` creates the
table and nothing in that repository carries the rows), so acquiring it needs a
running instance or a dump and is its own slice.

What the 2026-06-15 dump yielded, every count also recorded in the provenance
file:

| Count | Stage |
| --- | --- |
| 1683 | datasets, all of them carrying a `sparql` key, 970 of the arrays empty |
| 713 | datasets naming at least one endpoint |
| 725 | `access_url` entries |
| 548 | distinct URLs, matching the survey behind this project |
| 5 | refused, under the rules below |
| 543 | seeded |

Order in the file is ascending by dataset key, then by position within that
dataset's `sparql` array. That is not document order: `serde_json` without
`preserve_order` backs an object with a `BTreeMap`. What the order has to be is
deterministic and stable, because `run_sweep` dispatches host groups in
first-seen order and two seeds of one dump must diff cleanly, and sorted-by-key
gives both without moving if the dump's serialiser changes how it lays keys out.

### The dump's `status` field is recorded and gates nothing

It reports 72 URLs OK where the survey found 65 answering a query, so it is a
third party's stale judgement, and admitting or refusing on it would be this
project publishing somebody else's confidence as its own. A `FAIL` entry is
still a candidate, and `the_dumps_own_status_field_does_not_gate_a_candidate`
pins that against a `FAIL` entry taken from the real dump.

The measurement adds a second reading that does not change the first: the field
is a bad LIVENESS oracle and a good COST predictor for one bucket, since the 43
candidates it had marked timed-out accounted for 66% of the sweep's serial cost
(see Sweep cost below). Both are true at once, and neither makes the field an
admission rule.

### The refusals

Six rules stand between the dump and the seeded list, and 5 of the 548 distinct
URLs were refused by them: the exclusion list was empty of real entries when
this dump was seeded, so five rules did the refusing. Every one is decidable from the string with nobody
contacted, which is what makes it honest to apply before a sweep rather than
after one, and every one is counted under its own reason, so a difference
between two seeds can be attributed to a rule rather than guessed at.

| Rule | Fired here | Holds | Why |
| --- | --- | --- | --- |
| `dedupe` | 177 (725 entries to 548 distinct) | every path | One `declarationsRead` fact is published per list ENTRY, so a URL listed twice put two of them on one endpoint IRI in one run graph, and two differing fetches made them contradict each other. It also stops the sweep sending one stranger's server two identical sets of requests. |
| `without_credentials` | 0 | every path | The endpoint string is published verbatim inside every subject, in a run graph never rewritten, so a credential admitted here would be permanent. |
| `without_excluded` | 0 | every path, **and** the seeder | Somebody asked. It holds on every path because the request is made to this software, and it is applied by the seeder as well so that the committed list does not name a host that asked to be left alone. It is also the only rule here that fails the load rather than dropping an entry, because failing open means probing that host. See Asking not to be probed above. |
| `without_unroutable_hosts` | 1 (`http://localhost:3030/Dataset/query`, really in the dump) | **the seeder only** | The question is not "may this be probed" but "may a third-party dump nominate it". An operator pointing this tool at their own machine is a legitimate use and this suite does it constantly through wiremock, so the rule sits at the seam where a stranger's list becomes ours. Wiring it into `load_endpoints` was tried and reverted: it emptied the endpoint list of `end_to_end.rs:1142` and `:1183`. |
| `without_reserved_names` | 2 (`example.org`, `www.example.org`) | every path | RFC 2606 reserves these for documentation and `.invalid` never to resolve, so no service can be there and no operator could mean to point at one. |
| `without_unpublishable_iris` | 2 (a `{SPARQL}` template placeholder, a URL with an example query inlined) | every path | `emit_nquads` builds the endpoint term with `NamedNode::new` and drops every fact about it when that fails, so such an entry costs a stranger's bandwidth to produce nothing at all. |

Whitespace is not a rule of its own: an `access_url` is taken verbatim, never
trimmed, because `dqv:computedOn` publishes the registry string as it stands and
a trimmed spelling is a string the registry does not contain. A value carrying
whitespace cannot be a `NamedNode`, so it is refused by the IRI rule under that
reason. This dump carried no blank or whitespace-bearing value.

`https://test-svu/sparql` is in the dump and is **seeded on purpose**. A
single-label host cannot resolve publicly, but it is neither harmful to probe
nor unpublishable, and the refusals here are for what must not be contacted or
cannot be published. It appears in the run as an honest failure, which is the
correct outcome; refusing it would be this project asserting an outcome it had
not measured.

Roughly 37 seeded entries cannot be endpoints at all on inspection (a WordPress
route, a GitHub `.owl` blob, Yandex Disk links). They stay seeded, because that
is decidable only BY PROBE and not from the string, and the admission slice is
where they go.

### Not yet operable on a daily cadence

Saying this plainly rather than leaving it to be inferred: **the seeded registry
is not something to put on a schedule today**, because nothing yet stops the
dead being re-probed on every sweep. 486 of the 543 candidates answered no query
at all, and the 43 candidates the dump had marked timed-out cost two thirds of
the sweep's serial probe time, 2.28 of its 3.46 serial hours. What makes a daily sweep affordable is an admission
policy, which admits responders and keeps the rest as a published
`unreachable-candidates` list that is not re-probed daily, and that is the next
slice. Until it exists, a sweep of `registry/lod-cloud.toml` is a measurement
somebody runs deliberately, not a `CronJob`.

### Where the runs are kept

Both sweeps described below are preserved **outside this repository**, at
`~/code/sparqlwatch-runs/`, with a `SHA256SUMS.txt` to check them against
(`shasum -a 256 -c SHA256SUMS.txt`). They are kept because `web/load_run.py`
states that the .nq files are the source of truth and the Oxigraph store is the
derived artefact, and a sweep observes a changing world: re-running the same
`--at` gives different `elapsedMs` and can give different verdicts, so a lost
run is not reproducible, it is gone. They are not in git because the full sweep
is 6.3 MB, though it gzips to 140 KB, and a compressed copy could be committed
later if that is wanted.

## Sweep cost

At the default `--min-gap-ms` of 2000 and `--max-cost cheap`, one full sweep of
the seeded registry has now been run, so this section states the measurement
first, then the arithmetic that was used to predict it, then what the
measurement did to that arithmetic.

### Measured: the first registry-scale sweep

543 candidates from `registry/lod-cloud.toml`, `--max-cost cheap`,
`--concurrency 4`, `--at 2026-08-24T19:45:03Z`. It ran from 19:45:03Z to
21:11:24Z, **1h26m21s**, and produced 3801 measurements, 543 `declarationsRead`
facts, 543 cost-ceiling declines, 0 content samples (`classes` is `expensive`),
0 failed endpoints and 6.3 MB of N-Quads. The activity carries
`emission=incremental`, `finalised=true` and `failedEndpoints=0`, so stage
1c-b4's protocol held on a real registry-scale run.

Per-endpoint cost from the run's own `elapsedMs`, in seconds, grouped by the
cause the dump's `status` field had recorded for that candidate:

| Bucket | n | median | mean | max | Share of all serial cost |
| --- | --- | --- | --- | --- | --- |
| timed out | 43 | 210.0 | 191.2 | 210.0 | 66.0% |
| an HTTP status | 185 | 2.1 | 12.4 | 210.0 | 18.4% |
| OK | 70 | 0.9 | 11.5 | 210.0 | 6.4% |
| refused or unreachable | 16 | 0.6 | 37.9 | 210.0 | 4.9% |
| cause unrecorded | 229 | 0.0 | 2.3 | 210.0 | 4.3% |
| total, serial | 543 | | | | 3.46 h |

Two things in that table are worth reading twice. **Dead hosts are cheap and
half-alive hosts are expensive**: a host whose DNS is gone refuses in
milliseconds, while a host that answers with a status code is up, so our seven
probes can each hang for the full 30s request budget. And the driver is one of
the smallest buckets: 43 candidates, two thirds of the serial cost.

Liveness and self-description, against the survey this project's metric set came
from. Three quantities, stated separately because they are easy to conflate and
an earlier draft of this paragraph did conflate them, all counted over the 543:

- **57 answered a query**: `availability` `verified`, 10.5% of the 543, against
  the survey's 65 of 548.
- **26 published a parseable service description**: `service-description`
  `verified`. The rest are 508 `indeterminate` and 9 `absent`, and the file
  carries 35 `sw:level` triples, one per non-indeterminate verdict.
- **30 returned some parseable RDF to the queryless GET**: `declarationsRead`
  `true`. This is the weakest of the three. It says at least one triple came
  back and was read, not that it resolved to a service-description verdict.

The figure comparable with the survey's is the description rate among the
living, because both populations were probed once and both are mostly dead: 24
of the 57 that answered a query also published a description, **42.1% of live**,
against the survey's 28 of 65, **43.1% of live**. (Two of the 26 answered no
query, which is the difference between 26 and 24.) So this sweep is **slightly
lower on both** liveness and descriptions, consistent with five more days of
decay since the survey probed on 2026-08-19, and the rate among the living is
almost unchanged.

The 26 descriptions are graded: **23 at level 1, 2 at level 2, and exactly one
at level 4**. Level 1 is the stub the design's level table calls "the Virtuoso
default", so **one endpoint in the whole registry** declares an entailment
regime, example resources or extension functions. That echoes the survey's
finding that 21 of its 28 descriptions were byte-identical 14-triple Virtuoso
stubs: a binary "publishes a service description" credits the engine, and the
graded metric is what separates the publisher from it.

### The estimate, and how badly it missed

The sweep was priced before it ran, from a 54-candidate calibration sample
(`registry/calibration-sample.toml`) drawn to include the largest host groups: 47
hosts, cheap ceiling, concurrency 4, 13m59s, 378 measurements, and only 6 of the
54 endpoints producing any positive verdict. Projected from those measured costs
against the population, at concurrency 4, that gave **2.24 hours with a 90%
interval of 2.05 to 2.43 hours**, comfortably under the 4-hour abort threshold
the plan had set, so the sweep proceeded.

**That projection was wrong, and the outcome fell outside its own interval.**
Measured: 1.44 hours. The cause is recorded here rather than rounded away,
because it is a fault in the estimate and not in the sweep. The projection was
dominated by the HTTP-status bucket, one of the buckets sampled only 8 times,
whose mean was estimated at 62.1s from three slow values and is really 12.4s,
five times lower. The 90% interval was bootstrapped from those same 8 values, so
it could not express any uncertainty about their own mean. The sample had been
enlarged to 30 draws for the bucket believed to matter, leaving the bucket that
actually mattered at 8.

So the measured number is the one to plan a scheduled job around, and it is the
only full-sweep number this project has: **1h26m21s for 543 candidates at
`--concurrency 4`**, against 3.46 hours of serial cost. Everything in the
arithmetic below remains an estimate. It now has exactly one calibration point.

### The floor arithmetic, which is still an estimate

A note on the two sizes in this document. The arithmetic below is sized at
**548**, which is the dump's distinct-URL count; the seeded registry holds
**543**, because five URLs are refused by the rules under The refusals above.
The numbers here are round-number estimates and the 5-endpoint difference does
not change any of them, so they are left as they were rather than restated.

Seven metrics are `cheap` at the default cost ceiling: availability, cors,
cors-preflight, geo-functions, geo-data, service-description, has-classes.

Per endpoint, the prober makes 7 requests. Between consecutive requests to one
host, there is a gap. Endpoints are grouped by host and `--concurrency` bounds
how many of those groups run at once, so concurrency buys parallelism **across
different hosts** and never within one: per-host concurrency stays at one
request, whatever the flag says. So, per host group:

- Per endpoint: 7 requests with some latency (call it L per request) plus 6 gaps.
  An endpoint that redirects costs one more gated request and one more gap per
  hop, since every hop is a request in its own right.
- Gap time per endpoint: 6 gaps × 2 seconds = 12 seconds.
- Request time per endpoint: 7 requests × L.
- With an average request latency of 300 ms (a rough middle ground for
  network round-trip), the per-endpoint floor is roughly (7 × 0.3) + 12 = 14.1
  seconds.
- For 548 endpoints at 14.1 seconds each: 548 × 14.1 = 7,726.8 seconds
  sequentially, or about 2 hours 9 minutes; at the default `--concurrency 4`,
  roughly a quarter of that, about 32 minutes.

Every number above is a **floor**, and the failure path is what actually prices
a sweep. A black-holed endpoint costs its whole 600-second endpoint budget
rather than 14.1 seconds, so the real bound is `sum(per-endpoint cost) /
concurrency`: 548 dead endpoints would be `548 × 600 / 4` = 22.8 hours, and at
a plausible 10% dead it is `(493 × 14.1 + 55 × 600) / 4` = 9,988 seconds, or
about 2 hours 46 minutes. Before the sweep above, that was the number to plan a
scheduled job around rather than the healthy-endpoint floor. Against the
measurement it is high by roughly a factor of two, and the reason is visible in
the bucket table: this model charges a dead endpoint its whole 600-second
budget, while a host whose DNS is gone actually costs milliseconds. It is a
bound, and it held.

It also assumes all endpoints are distinct hosts. Registry URLs that share a
host are one group and are probed one after another, so a host carrying several
endpoints costs the sum of them however high `--concurrency` is set. Request
latencies vary widely too; the 300 ms above is a middle estimate.

**In this population that host-group risk does not exist**, which is worth
recording because the plan behind the sweep was built around it. Two groups
tie for most expensive at 7.0 minutes, `data.gov.uz` and `linked.opendata.cz`,
each two candidates that each burned the full 210-second budget; the largest
group, `api.talis.com` with 27 candidates, costs 0.2 seconds in total because
its DNS is gone. The makespan was set by the aggregate over 436
groups, not by any one of them. A different registry could still be shaped the
other way, so the arithmetic above stays; what changed is that the lever is
measured and small rather than assumed and large.

Measured, not estimated, on this repository's three-endpoint `endpoints.toml`
on 2026-08-24: 14.4 seconds at the default `--concurrency 4` and 38.7 seconds
at `--concurrency 1`. Three endpoints on three hosts is not a registry sweep,
so it says nothing about the 548-endpoint figures above; it is here because it
is the only concurrency measurement this project has actually taken.

## Configuration files

**`endpoints.toml`** is the list to sweep, one array of URLs, and it is the
three-endpoint development list rather than the seeded registry (see The seeded
registry above). Whatever file `--endpoints` names, `load_endpoints` applies five
rules in a fixed order: `dedupe`, `without_credentials`, `without_excluded`,
`without_reserved_names`, `without_unpublishable_iris`. Four of them drop an
entry with a warning rather than failing the load, because the list can be
seeded from a real-world dump known to contain junk, and one bad string must not
discard a whole sweep's work. `without_excluded` is the exception in both
directions: it subtracts the hosts in `registry/exclusions.toml`, and an
exclusion list that does not parse fails the load, because failing open there
means probing a host that asked not to be probed (see Asking not to be probed
above). `without_unroutable_hosts` is deliberately not among the five, and the
table in The seeded registry says why.

The last two arrived with stage 1d-a. `without_reserved_names` drops a host
under an RFC 2606 documentation name (`.example`, `example.com`, `example.net`,
`example.org`, `.invalid`), because nobody serves anything there, and the match
is on whole labels so `notexample.org` and `example.org.uk` are kept.
`without_unpublishable_iris` drops an entry that cannot be an
`oxrdf::NamedNode`, which is what `emit_nquads` builds the endpoint term with:
such an entry would cost a stranger's bandwidth and then have every fact about
it dropped at emission, so refusing it before probing is strictly better. It
calls `NamedNode::new` itself rather than checking a character list, so it cannot
drift from the constraint the emitter actually applies.

The list is deduplicated at load, first-seen order preserved, with a warning
naming each entry dropped. One row per (endpoint, metric) and one
`declarationsRead` fact per endpoint held per list ENTRY, not per endpoint, so a
URL listed twice published two facts about one endpoint IRI in one run graph,
and two differing fetches made them contradict each other with nothing in the
graph to resolve it. Deduplicating at load also stops the sweep sending one
stranger's server two identical sets of requests. The comparison is on the
exact string: `http://x/sparql` and `http://x/sparql/` stay two entries, even
though the declaration scoper treats them as one service, because two spellings
in a registry are a registry problem to see rather than one to collapse
silently.

A URL whose authority carries a **non-empty userinfo** component is dropped
after that, with a warning naming the host it was pointed at and never the URL,
because repeating the URL would copy the credential into the log the drop
exists to keep it out of. This happens after the deduplication and before
anything is swept. `http://alice:s3cret@a.example/sparql` is refused and so is
`ftp://alice:s3cret@a.example/sparql`: the authority is delimited on `//`, so
the check does not care about the scheme. An empty userinfo
(`http://@a.example/sparql`) carries no credential and is kept. A bare `@`
anywhere else in the URL is not touched at all, because
`http://a.example/sparql?contact=x@y.example` is a legitimate endpoint and a
`contains('@')` check would silently remove it from every sweep. This lives in
the registry rather than in the emitter because it changes what gets swept: the
endpoint string ends up inside the subject of every fact about it, in a per-run
graph that is never rewritten, so a credential admitted here would be published
permanently. What it does not cover is a credential in a query string; see
Known limitations.

**`metrics.toml`** is the metric definitions, as *data*. Each names a probe
kind from a closed set (`Liveness`, `Cors`, `CorsPreflight`, `AskFilter`,
`AskData`, `SelectIris`, `FetchWellKnown`) plus its parameters, so adding a metric that
fits an existing kind needs no Rust change. An unknown kind, an unrecognised
`cost` value, a bindings-reading kind with no `var`, a key the loader does not
recognise, and an `id` defined twice are each a loud load error rather than a
silent default. A metric that says nothing about `cost` is `cheap`, which is the
one silent default that remains, so state the cost explicitly.

An unrecognised key matters more than it sounds: before the loader refused them,
`cost_class = "expensive"` loaded as `cheap` and ran a planet-scale scan against
every endpoint in the registry. A duplicate `id` matters because the id is the
metric's published identity: two definitions under one id can land on opposite
sides of the cost ceiling and give the same (endpoint, metric) pair both a
verdict and a not-measured fact. Note the contrast with the endpoint registry
above, which drops a duplicate with a warning: that list is seeded from
real-world dumps whose repeats say the same thing, while `metrics.toml` is
written by hand and its repeats say different things, so there is nothing
honest to guess.

Every metric declares a cost of `cheap` or `expensive`, stating whether
probing it is inexpensive enough to run everywhere or should be opt-in (via `--max-cost expensive`).
The definitions are hashed into a `metricDefinitionRevision` recorded
on every run, a pure function of the definitions themselves, so a measurement
can be read against the definition that produced it.

`FetchWellKnown` probes by fetching the queryless GET described above. It returns
`Verified`, plus a level reflecting the description's informativeness, only when
a **2xx** response arrives under an RDF-specific media type (`text/turtle`,
`application/rdf+xml`, `application/ld+json`, `application/n-triples`,
`application/trig`, `application/n-quads`) and parses to **at least one triple**.
All three conditions are load-bearing. `RdfFormat::from_media_type` also accepts
the generic `text/plain`, `application/json` and `application/xml`, under which
an empty throttle body, a `{"error":"boom"}` page and a SPARQL-results document
all parse cleanly, so a generic media type is not a positive identification of
RDF and neither is a zero-triple parse. A genuine RDF/XML document served as bare
`application/xml` therefore reports `indeterminate`, which is honest, rather than
a confident verdict. The media type is read through one shared function
(`src/media.rs`), which strips the header's parameters (`; charset=utf-8`)
before matching, because that decision has to be identical here and in the
declaration parser: when there were two copies they drifted, and an RDF/XML
description served with a charset parameter classified as RDF, then got
reparsed as Turtle, publishing `verified` with level 0 (which means "none
served"), `declarationsRead false` for a document we had read, and losing every
declaration in it. A `404` or `410` response returns `Absent` with level 0. Any
other status is `Indeterminate`, recording that the request failed rather than
that the description is absent.

The level ladder follows the design doc: 0 none served, 1 a stub, 2 names a
default dataset or graphs, 3 carries VoID class or property partitions, 4
declares an entailment regime, example resources, or extension functions. It is
monotonic: a description that declares more never grades lower.

## Cost ceilings and not measured

When a metric is `expensive`, a sweep with `--max-cost cheap` (the default) skips
it. That skip is published as a fact, not as an omission. Per declined (endpoint, metric)
the run emits a resource of type `urn:sparqlwatch:NotMeasured`, with:

- `urn:sparqlwatch:notMeasuredOn` pointing to the endpoint
- `urn:sparqlwatch:notMeasuredMetric` pointing to the metric definition
- `urn:sparqlwatch:notMeasuredReason` set to `"cost-ceiling"`
- `prov:wasGeneratedBy` pointing to the run's activity, the same link every
  measurement carries, so the fact reaches the `urn:sparqlwatch:maxCost` that
  explains it without anyone having to assume the `activity:{at}` naming
  convention
- **no** `dqv:value`
- **no** level

Those first two are sparqlwatch's own predicates on purpose, and no DQV or Data
Cube predicate appears on the fact at all. `dqv:computedOn` is declared with
domain `dqv:QualityMeasurement` and `dqv:isMeasurementOf` with domain
`qb:Observation`, so either one here would entail, under plain RDFS, that a
quality measurement exists for a pair we deliberately did not measure: a
consumer materialising domains would read a value-less measurement rather than a
declined one. Reusing a predicate whose declared domain is a class you are not is
an assertion, not a convenience. Sparqlwatch declares no domain and no range for
its own two, because an undeclared predicate entails nothing, and endpoint and
metric stay just as joinable.

This is not a seventh verdict. The verdict vocabulary stays at six: this fact says a
measurement did not happen, while a verdict says what was learned about a capability.
Someone querying for verdicts will never encounter a value that is not one of the six.
Someone asking why a verdict is missing gets an answer: either it was declined, or the
budget was exhausted. A second reason, `prober-failed`, says the prober itself
never got to ask: the task probing that endpoint's host did not return, so
there was no observation at all rather than an inconclusive one, and every
metric on that endpoint carries the fact rather than a verdict.

The run's PROV activity records three things about the run itself:
`urn:sparqlwatch:maxCost` naming the ceiling used,
`urn:sparqlwatch:concurrency` naming how many hosts were probed at once, and
`urn:sparqlwatch:failedEndpoints` counting the endpoints the prober failed on.
The last two are `xsd:integer`.

What a consumer may conclude from those two is narrow, and worth stating,
because both invite more. `concurrency` does **not** change what `elapsedMs`
measures: each hop's timer starts after the per-host gate has been acquired and
stops when the response returns, and a redirect chain publishes the sum of its
hops' own durations rather than the wall clock of the walk, precisely so our own
politeness is never published as somebody's response time. What it does explain
is the run's wall-clock duration, and the one case where another host's gate can
sit inside a metric budget: a run at `--concurrency 1` cannot have lost a metric
to the cross-host redirect described under Known limitations, because no other
group was running, and a run at 4 can. `failedEndpoints` counts the endpoints
this run observed nothing at all about. It is published on every run that
finishes, including the ordinary `0`, so "this run failed on nothing" is a fact
a consumer can read rather than an absence it has to interpret. Since the count
moved into the footer, an absent quad has two readings and not one: a run
emitted before this fact existed, or a run that did not finish and never wrote
its footer. `sw:finalised` is what tells those apart, and it is the quad to test
before reading anything into the absence. A non-zero value means that many
endpoints carry `prober-failed` facts in place of verdicts and carry no
`declarationsRead` fact either, so a consumer expecting one row per (endpoint,
metric) has to read this rather than assume it. Such a run also exits non-zero,
after writing its output, so the run is preserved and the scheduler still learns
it was incomplete.

### How a run is written, and what a truncated one says

A run is emitted as three kinds of section: a header of run-level facts, one
self-contained chunk per endpoint, and a footer. Each section ends with its own
terminator, `urn:sparqlwatch:emission "incremental"` for the header,
`urn:sparqlwatch:completedEndpoint <endpoint>` for a chunk and
`urn:sparqlwatch:finalised "true"^^xsd:boolean` for the footer, all three on the
run's activity. N-Quads has no prologue and every line ends in a newline, so any
prefix of the file parses, which means a crash leaves a readable file whose only
risk is that its lines contradict each other. The terminators are what remove
that risk: a reader that holds a section's terminator holds the whole section,
and a reader that does not may drop the fragment.

A consumer reads three cases off facts that were each true when they were
written. `emission` with `finalised` is a complete run: every endpoint the sweep
was given has a chunk, including the ones a panicked group lost, and
`failedEndpoints` counts over all of them. `emission` without `finalised` is a
run that did not finish, so an endpoint with no `completedEndpoint` marker was
never reached rather than measured and found wanting, and there is no
`failedEndpoints` count at all, because that count is in the missing footer: a
summary of chunks is not published until the chunks are. Neither terminator is a
run that makes no claim about sections either way: that is what a run emitted
before this scheme existed looks like, and it promised nothing, so it must not be
reported as unfinished. It is also what a run of THIS scheme cut inside its
header looks like, since `emission` is the header's last quad, and no reader can
tell the two apart from the bytes. `web/load_run.py` separates them on a fact the
file does carry: a run from before this scheme still measured endpoints, so a file
with no terminator and no endpoint fact at all is refused rather than admitted as
the store's newest activity. `finalised` is a boolean rather than
`prov:endedAtTime` because nothing in the prober can produce that instant
soundly: `emit` reads no clock by design,
`std` cannot format a `SystemTime` as `xsd:dateTime`, no date library is in the
lock file, and a flag supplied at launch would publish a predicted future into a
graph that is never rewritten.

Loading a truncated run commits a consumer to nothing, which is the non-obvious
half of this working in our favour. `web/load_run.py` replaces a run's graphs
wholesale: it drops the graphs the incoming file names and then inserts that
file's quads, never merging, so the store holds the most recent file that
claimed a run IRI and never a blend of two. Load a crash's partial file, then
later the same run's complete file, and what remains is the complete run with
none of the partial's quads standing beside it. Load either of them twice and
the store ends up identical, so a re-load is always safe (the non-atomic window
`load_run.py` documents is itself closed by re-running that same load). So a
partial file is worth loading as soon as it appears, and nothing has to be undone
when that run is later completed.

**A run in progress is written to `<out>.<at>.partial`, and renamed onto `--out`
when it finishes.** Never to `--out` directly: that file is the source of truth
for the loaded store, nothing re-creates it, so truncating it at t=0 would mean a
sweep that died at endpoint 500 of 548 had destroyed the previous complete run.
Never to a fixed `<out>.partial` either, because the next scheduled sweep would
truncate the previous crash's file on its first write, which is the same loss one
run later. `--at` is required and validated before anything is opened, so it
names the file uniquely per run, and a partial file that is already there is
refused rather than truncated (see the `--at` paragraph above). `rename` within a
directory is atomic on macOS and Linux, so `--out` is always either the previous
complete run or this one, and a crash leaves the partial file under its own name,
where `web/load_run.py` will load it as far as its last whole section. Renamed
onto is not written through: a `--out` that is a **symlink** is replaced by the
finished file rather than followed, which the `std::fs::write` this replaced did
follow, so a deployment has to point `--out` at a real path. A `--out` that is a
directory is refused before any probing, along with any other reason the partial
file cannot be created.

Each chunk is flushed as it is written, and nothing depends on a destructor
running: a `SIGKILL` runs none. What that protects against is the process
dying, all of it: a panic, a `SIGKILL`, an OOM kill, a cancelled `CronJob`.
Measured on the shipped `endpoints.toml`: a run killed with `SIGKILL` the instant
its second chunk landed left 24,094 bytes and 108 quads on disk, which the loader
took whole, while the previous `--out` was byte-identical. Killed a little later,
the same experiment left a 25,634-byte partial that loaded as 114 quads with 0
bytes discarded, unfinished, one `completedEndpoint` whose 8 verdicts were all
present, and again a byte-identical `--out`; a retry at that `--at` then exited 1
and left the partial byte-identical.

What it does not protect against is the machine losing power, and the boundary is
worth stating exactly. There is no `fsync`, so a flushed chunk can still be lost
in the page cache, and nothing syncs the directory after the rename either, so
the rename onto `--out` is not promised to survive a reboot. Power loss is also a
different SHAPE of damage, and it is where the terminator rule stops: a
delayed-allocation filesystem can leave the file at its full length with blocks
that were never written back reading as zeros. A NUL byte is not valid N-Quads,
and `load_run.py` only ever cuts back to the last terminator LINE, so zeros
anywhere before that line survive the cut, the parse fails, and the whole file is
refused, chunks and all. A truncated tail is recoverable; a hole is not. See the
last of the known limitations for why that is an accepted boundary rather than a
gap to close.

What writing incrementally buys is crash tolerance and not a smaller heap. The
returned `Sweep` holds every endpoint's facts by design, so a 548-endpoint sweep
retains all 548 at its peak; the arrival channel is bounded at 16 to buy
backpressure, so a fast host cannot run arbitrarily far ahead of the writer. A
30-endpoint sweep measured `peak_queued=15, retained_at_end=30`.

`completedEndpoint` is per endpoint and not per run, so "did this run reach this
endpoint" is a fact rather than an inference from absence. Read "completed" as
"this endpoint's chunk is complete": the run wrote everything it will ever say
about the endpoint, which is NOT the same as the prober having succeeded there.
An endpoint whose probing task panicked carries `prober-failed` declines and a
`completedEndpoint` marker together, and `run-prober-failed.nq` shows that
shape, `failedEndpoints "1"` beside a marker naming that very endpoint. Both
readings would be defensible names; this is the one the file means, and it is
the one the read tier needs. Dropping the marker for a failed endpoint would
make it indistinguishable, on a run that later crashed, from an endpoint the run
never got to, so a page would say a later sweep never reached an endpoint whose
failure that sweep had published.

It is written even for an endpoint the run published no other fact about,
because that is the only way "reached but learned nothing" and "never attempted"
can be told apart, and such a chunk still types its endpoint `dcat:DataService`
so the marker names a resource the graph describes.

Two ordering rules hold inside the file because the chunk is what a truncated
one preserves. **A chunk types every endpoint it publishes a fact about**,
rather than a run typing each endpoint once. For a run this emitter writes the
two are the same thing, since each endpoint gets exactly one chunk; they differ
only where one chunk names an endpoint another already typed, and then the
run-scoped version leaves the chunk that holds the facts with no type for their
subject. **And no fact family publishes its own summary before the things it
summarises**, which is why `sampleSize` and `sampleTruncated` come after the
last `sampledValue`, and why `failedEndpoints` sits in the footer. A cut inside
a sample's values then loses the sample, which every consumer already handles,
instead of leaving `sampleSize 200, sampleTruncated false` standing beside three
values, which a page would render as two hundred classes sampled, complete.

The labels in that file state only what was actually measured. `geo-data` and
`has-classes` and `classes` query the default graph AND every named graph, via a `UNION` with a
`GRAPH ?anyg { ... }` branch, so an endpoint holding everything in named
graphs is not reported as holding nothing. The graph variable is never the
metric's own result variable: `GRAPH ?g { ?s geo:asWKT ?g }` would join the
graph name against the geometry literal, match nothing, and publish a silent
false `absent`, which is exactly the failure this widening exists to remove.
What the suite checks about those queries is structural only; see the
named-graph entry under Known limitations for what that does and does not
establish.

The two CORS metrics are deliberately separate facts, and neither subsumes the
other. `cors` observes an `access-control-allow-origin` header on a **simple
GET**: what a `curl` user sees. `cors-preflight` sends the `OPTIONS` preflight a
browser sends before a cross-origin query (`Origin`,
`Access-Control-Request-Method: GET`, `Access-Control-Request-Headers:
content-type`) and is what decides whether an embedded query editor can talk to
the endpoint at all. An endpoint that sets the header on GET and refuses
`OPTIONS` is common, and it reports `verified` on the first and `absent` on the
second, which is the honest pair of answers, and the shape
`the_two_cors_metrics_are_not_the_same_probe` pins end to end. The preflight
probe never follows a redirect implicitly: a `303` would rewrite the `OPTIONS`
into a `GET` and hand back exactly the simple-GET header we already have,
publishing a grant for that endpoint. It resolves the chain deliberately
instead, re-issuing the same `OPTIONS` at each `Location` for up to 5 hops, and
draws the verdict from the response at the end. A chain with no usable
`Location`, a cycle, or more hops than that is `indeterminate`: we never
reached a preflight answer. Minting a redirect itself as `absent` published
"does not answer a browser preflight" for services that answer one one hop
away, contradicting the `cors` row in the same run, which had followed the very
same redirect. `verified` on `cors-preflight` requires a 2xx
whose `access-control-allow-origin` is `*` or our own origin
(`https://sparqlwatch.dev.k8s.semanticscience.org`, the same host the
`User-Agent` names) and whose `access-control-allow-methods`, if it sends one,
lists GET. A header naming somebody else's origin is a grant to somebody else.

The two class metrics (`has-classes` and `classes`) are also deliberately separate,
for a reason worth recording: they answer different questions at vastly different costs.
On qlever.dev/api/osm-planet (planet-scale OpenStreetMap), `SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 200`
timed out past 45 seconds, while `SELECT ?c WHERE { ?s a ?c } LIMIT 1` answered in 0.166 seconds.
The cost is `DISTINCT` scanning every class name in the endpoint, not the named-graph
`UNION`. So `has-classes` (cheap, "this endpoint holds typed resources") runs everywhere,
while `classes` (expensive, "here are up to 200 distinct resource types") is opt-in.
`has-classes` uses probe kind `SelectIris`, never `AskData`, and this choice matters:
`AskData` extracts bindings with a literal guard, and since `?c` in `?s a ?c` binds an IRI,
the guard would find no literal and the metric would publish `absent` for an endpoint full of
typed resources. `SelectIris` has no such guard and returns `verified` when any type is found.

## Content samples

`classes` already ran `SELECT DISTINCT ?c ... LIMIT 200` to answer "does this
endpoint publish a bounded list of types"; it now publishes the bindings it
reads instead of discarding them once the verdict is drawn. A metric that
uses probe kind `SelectIris` may declare `sample_limit` in `metrics.toml`,
checked at load against the `LIMIT` its own query actually carries (SPARQL
comments stripped first, so a comment mentioning a different number cannot
stand in for the real one), so the two numbers cannot drift apart. `classes` is
the only metric that declares one, at 200, matching its query's `LIMIT 200`.
`has-classes` runs `SELECT ?c WHERE { ?s a ?c } LIMIT 1` and deliberately
declares none: its single binding is whichever type the endpoint happened to
return first, and publishing that as a "sample" would suggest it says something
about the endpoint's vocabulary, when it says only that at least one typed
resource exists.

`AskData` may not declare a `sample_limit` either, even though it does read
bindings: it reads the lexical forms of literals, and every sampled value is
published as an IRI, so an `AskData` sample would drop most values and
republish any whose lexical form happens to parse as an IRI as a resource the
endpoint never mentioned, losing the datatype either way. Lifting that
restriction needs a sample that carries, per value, whether it is an IRI or a
literal with its datatype, and an emitter that emits accordingly. Until both
exist the path stays closed, because a half-supported path publishes a wrong
fact about somebody's data.

Per sample, the run graph carries only sparqlwatch's own predicates plus
`rdf:type` and `prov:wasGeneratedBy`:

- `rdf:type urn:sparqlwatch:ContentSample`
- `urn:sparqlwatch:sampledFrom` the endpoint
- `urn:sparqlwatch:sampledBy` the metric definition
- `urn:sparqlwatch:sampledValue`, one per IRI published, repeated
- `urn:sparqlwatch:sampleTruncated` an `xsd:boolean`
- `urn:sparqlwatch:sampleSize` the count of values published, an `xsd:integer`
- `prov:wasGeneratedBy` the run's activity, the same link every other fact in
  this graph carries

Written in that order, with the two summarising quads after the values they
describe, so that a file cut inside the value list loses the sample rather than
misstating its size. See How a run is written above.

Values are published in the order the endpoint returned them: not sorted, not
deduplicated beyond what `SELECT DISTINCT` already did, because reordering
would discard evidence about the endpoint for a tidiness nobody asked for.

`sampleSize` counts the values actually published, so it is always verifiable
against the `sampledValue` quads beside it. A value that cannot be written as
an IRI is dropped and not counted, and the drop is logged with the bound and
published counts, because the graph has no way to say "there was one more and
we could not name it". A sample whose values are all unwritable publishes no
node at all, since `sampleSize 0` would read as "this endpoint has no classes".
Against the three endpoints in this project's own registry, nothing was dropped
(kadaster 59 of 59, ontop 50 of 50, qlever no sample).

A sample is published only where the measurement for that same metric came out
positive, `verified` or `undeclared-but-verified`. This is gated on the verdict
rather than on the response status deliberately: `resolve()` already encodes
every status rule this project has, so the sample and the measurement cannot
disagree, and a change to those rules carries the sample with it. Without that
gate, a `429` or a `503` carrying a parseable SPARQL-results body published the
endpoint's full class list, marked complete, in the same graph whose
measurement for that metric read `indeterminate`.

`sampleTruncated` is `true` when the number of values reached the metric's
declared `sample_limit`, using `>=` rather than `==`: an endpoint that ignores
its own `LIMIT` and returns more than asked is still not called complete,
because the query still bounded what we could see, not because the count came
out exactly right. A list a reader believes is complete when it is not is the
content equivalent of a confident wrong answer, so this is published rather
than left for a consumer to infer from the count alone.

Measured with `--max-cost expensive` against this project's own registry:
`data.kkg.kadaster.nl` returned 59 distinct classes, not truncated, mostly the
schema vocabulary its data is built from (`owl:Class`, `owl:Restriction`,
`rdfs:Class`, `rdf:Property`). `ontop.certain.ai.ustp.at` returned 50, not
truncated, real domain vocabulary from its own namespace
(`https://w3id.org/aidoc-ap#AISystemCapability`,
`https://w3id.org/aidoc-ap#ComputationalResource`, and others). `qlever.dev/api/osm-planet`
produced no sample at all: its class enumeration exceeds the 30s request
budget, so `classes` there is `indeterminate`, there are no bindings to
publish, and correctly nothing is published as an empty sample either. The
expensive sweep of these three endpoints took 1m18s, against roughly 41s at
the default cost ceiling.

A content sample is deliberately **not** a dataset description, and this
project deliberately does **not** publish VoID from it. What a sample holds is
an observation from one bounded query, not a description of a dataset: the
thing behind a SPARQL endpoint may be several datasets, or a virtual graph over
a relational store rather than a dataset at all (`ontop`, in this project's own
registry, is exactly that). `void:classPartition` and `void:class` both carry
`rdfs:domain void:Dataset`, so reusing either predicate here would entail,
under plain RDFS, that a content sample IS a dataset, which is not something
this project can honestly assert about an arbitrary endpoint. This project has
already shipped that class of defect once: the `NotMeasured` fact originally
reused `dqv:computedOn`, entailing that 548 deliberately declined pairs were
quality measurements that never happened, and nothing was visibly wrong with
it until a consumer ran inference. `ContentSample` uses sparqlwatch's own
predicates for exactly that reason.

Samples appear only under `--max-cost expensive`, because `classes` is the
only metric that produces one and it is `expensive`. A default sweep therefore
publishes no samples at all. That absence is not silence: the `NotMeasured`
fact already published for `classes` under the default ceiling (see [Cost
ceilings and not measured](#cost-ceilings-and-not-measured) above) is what
tells a reader "we did not look" rather than "there is nothing there".

## Fact identity, and why order is not part of it

Two properties are easy to confuse here, and they do not have the same
standing. Identity is a property of the graph. Order is a property of one
emitted file.

**Identity.** Every run-scoped fact's subject is a pure function of (run,
endpoint, metric):

```
urn:sparqlwatch:<kind>:<run>:<percent-encoded endpoint>:<metric id>
```

`<kind>` is `measurement`, `not-measured` or `content-sample`, the three fact
families scoped to a run, and it is in the IRI so that a measurement and a
not-measured fact about one pair can never land on one node that both has and
has not a verdict. `<run>` is the `--at` instant verbatim. The endpoint is
percent-encoded keeping RFC 3986's unreserved set (`ALPHA / DIGIT / "-" / "." /
"_" / "~"`) over UTF-8 bytes with uppercase hex, so it carries no `:` of its
own, and the metric id is checked against `[a-z0-9][a-z0-9-]*` both by the
metrics loader and again by the subject builder, so it carries none either.
Those two facts together are what make the mapping injective. One real subject,
from this repository's own fixtures:

```
urn:sparqlwatch:measurement:2026-08-22T20:00:00Z:https%3A%2F%2Fqlever.dev%2Fapi%2Fosm-planet:availability
```

Nothing positional is left in it. Reordering `endpoints.toml` renames no
subject, two runs at the same `--at` name the same nodes, and one endpoint's
facts can be written on their own, which is what stage 1c-b4 needs. No
normalisation happens on the way in: `registry::dedupe` treats two spellings of
one endpoint as two entries, so they are two subjects here too, and the endpoint
a subject encodes is the string the registry actually contains. The fourth
published fact, `declarationsRead`, is deliberately outside this scheme: its
subject is the endpoint IRI itself, because it says something about the endpoint
rather than about an (endpoint, metric) pair.

**A subject must never be parsed.** Two reasons, and the first alone is
sufficient.

- Every fact already carries what it is about, as triples. A measurement has
  `dqv:computedOn` and `dqv:isMeasurementOf`; a content sample has
  `sw:sampledFrom` and `sw:sampledBy`; a not-measured fact has
  `sw:notMeasuredOn` and `sw:notMeasuredMetric`. Joining on those is always
  available, and it is the only access path this project supports.
- A store holds every run ever loaded, and this scheme has already changed once.
  Before this branch the tail of a subject was the row's index in the emitted
  file (`urn:sparqlwatch:measurement:<run>:0`). Those runs are still valid
  history, a run graph is never rewritten, and history is kept, so a store holds
  **two subject shapes** for as long as it holds history. A consumer that splits
  on `:` reads one of them wrong. It would also read the new one wrong, since the
  run segment is an unencoded `xsd:dateTime` and carries colons of its own.

**Order.** A file is the header, then one chunk per endpoint, then the footer.
A sweep writes those chunks in COMPLETION order, one as each endpoint finishes,
which is the whole point of writing incrementally; the `Sweep` it returns is in
INPUT order, built from the slots after the last chunk was written. Those are two
different properties and both hold. The chunks a panicked group never delivered
come after every real chunk and before the footer, because a footer certifies a
run whose endpoints are all in the file. `emit_nquads` is the same three sections
composed in one call for a caller that holds a whole run in memory; the prober is
no longer such a caller, so its only callers today are the emitter's own tests. It
derives its endpoint sequence from the union of all four fact lists in
first-appearance order, because
an endpoint can appear in one list only: a prober-failed endpoint is in the
not-measured list alone, and an endpoint whose description was read but whose
metrics produced no row is in the `declarationsRead` list alone. For lists
already grouped by endpoint that reproduces input order. Within one chunk the
order is measurements, then not-measured
facts, then content samples, then the `declarationsRead` fact, then the chunk's
terminator. Within one family it is the order of the definition list the facts
came from, which is not `metrics.toml` order in general: `within_cost`
partitions that file into the metrics that run and the metrics the ceiling
declines, and `assemble_endpoint` writes an endpoint's not-measured facts as the
metrics that would have run and then the metrics the ceiling declined, so a cheap
metric
listed after an expensive one comes out first. That is worth having for diffing
two files by eye, and it is all it is worth. Beyond the section terminators and
the two rules above, it is **not** a property of the data:

- N-Quads serialises a set. No consumer may read meaning from the order of lines
  in one.
- `web/load_run.py` parses the file and inserts the quads into Oxigraph, which is
  order-blind, so the order is gone before any query sees it.
- Writing each endpoint's chunk as it completes is what makes the chunk sequence
  completion order rather than input order, and nothing downstream lost anything
  when that changed. What a consumer may read from the order is only what the
  terminators say, and those say it as facts rather than as position.

**Output is not byte-identical between two runs of one `--at`.** `emit_nquads`
reads no clock, no environment and no global, so it is a pure function of its
inputs. Its inputs are not: `elapsedMs` comes from an `Instant::now()` taken
around each request, so two runs of one endpoint differ in it, and a verdict can
differ too because the endpoint can. Do not write a test that diffs two runs'
bytes. Compare subjects, verdicts and levels.

## Proxy environment

The deployment target has no direct egress, so a proxy is mandatory there:

```sh
export HTTP_PROXY=http://egress-proxy.platform.svc.cluster.local:3128
export HTTPS_PROXY=$HTTP_PROXY
export http_proxy=$HTTP_PROXY
export https_proxy=$HTTP_PROXY
export NO_PROXY=localhost,127.0.0.1,.svc,.cluster.local
```

`reqwest`'s `system-proxy` feature is load-bearing rather than a convenience:
without a proxy every endpoint would fail identically, which looks exactly like
a dead registry.

**Correction to the spec's stage-0 findings.** The finding that "uppercase
`HTTP_PROXY` is ignored for `http://` URLs" is **curl-specific**: curl ignores
the uppercase form there because of a CGI variable collision. This client uses
reqwest, and hyper-util resolves the proxy with
`get_first_env(&["HTTP_PROXY", "http_proxy"])` (`matcher.rs:232`, hyper-util
0.1.20), reading both cases with uppercase first, so that constraint does not
apply to this code. Setting both cases, as above, is harmless
belt-and-braces and worth keeping for any sidecar or shell tooling that does
follow curl's rule, but the spec's note should not be read as binding here.

Two caveats on that correction, neither of which weakens it:

- `get_first_env` decides presence with `std::env::var(name).is_ok()`, so a
  correctly-set lowercase `http_proxy` is silently shadowed by an uppercase
  `HTTP_PROXY` that is merely set to an empty string. That is exactly the
  registry-wide silent-failure shape the spec's own stage-0 finding warns
  about, and the four-line export block above is precisely what a templated
  Helm values file could leave empty for one case while filling in the
  other.
- hyper-util disables environment-variable proxying entirely, uppercase and
  lowercase both, when `REQUEST_METHOD` is set (`matcher.rs:230`, with the
  early return at `matcher.rs:305`). The CGI collision curl guards against
  therefore exists here too, in a stronger form: it drops the proxy outright
  rather than merely picking the wrong case. "That constraint does not apply
  to this code" above is true only for the uppercase-versus-lowercase
  question, not for the CGI collision itself.

## Known limitations

The following are deferred deliberately, not oversights:

- The fetch is **unconditional**: an endpoint pays one queryless GET even if no
  configured metric actually needs the result, because the probe kind doesn't know
  which metrics use it. This is a small cost traded for simpler logic.

- A description larger than the 256 KiB body cap is never graded: it reports
  `indeterminate`, because we did not read it. Classification and the
  declaration join now read the same truncated bytes, which is what makes that
  answer coherent. They used not to: classification read the whole body while
  the join read the truncated one, so a description whose only triples sat past
  the cut classified as `Rdf` (licensing `verified`) and then graded `Level(0)`,
  which means "none served". That row asserted both that a description is
  published and that it says nothing, and its level was indistinguishable from
  an `absent` row's.

- One cost of that cap remains, in the conservative direction. Declarations are
  read from the same truncated body, so a declaration sitting past the cut is
  lost, and a metric that should read `verified` reports
  `undeclared-but-verified` instead. Measured with a 300 KiB Turtle description
  whose `sd:extensionFunction geof:sfWithin` sits past 256 KiB, `geo-functions`
  reported `undeclared-but-verified`. That understates a real endpoint rather
  than asserting something false about it, which is the direction this project
  errs in on purpose. Whether to raise the cap, stream the parse, or leave it is
  stage 1c's call; real descriptions are typically hundreds of bytes, not
  hundreds of kilobytes.

- The **named-graph half of the `geo-data` and `classes` queries is unverified
  by execution.** The suite contains no SPARQL engine, so what it can check
  about those queries is structural: `the_content_metrics_reach_named_graphs_without_colliding_variables`
  asserts that each query has exactly one `GRAPH ?g { ... }` branch, that the
  graph variable is not the metric's result variable, and that the block binds
  that result variable, so a branch incapable of contributing a row fails the
  test. That is correctness by reading, not by execution. `endpoints.toml` holds
  no endpoint known to keep its data in named graphs (the live test's
  `data.kkg.kadaster.nl` answers both branches from its default graph, so it
  would pass with the `GRAPH` branch deleted), so nothing here demonstrates that
  a partitioned endpoint is actually reached. Closing that gap needs an endpoint
  that holds its data that way.

  Stage 1d-a seeded 543 real candidates and swept them, and **the deferral still
  stands**. Two reasons, either sufficient. `classes` is `expensive`, so the
  `--max-cost cheap` sweep never ran it: that run published 543 cost-ceiling
  declines and 0 content samples. And `live_smoke.rs:52-62` records the shipped
  query passing with the `GRAPH` branch deleted, so a passing query against a
  seeded endpoint would prove nothing about that branch either. What the sweep
  produced is a list to verify against rather than a verification: 18 candidates
  among the responders, six of them named in the stage's ledger, `dbpedia.org`,
  `data.bnf.fr`, `data.cervantesvirtual.com`, `dati.camera.it`, `ldf.fi/warsa`
  and `ldf.fi/ww1lod`. **That 18 cannot be re-derived from the preserved run**:
  `classes` was declined 543 times, so no measurement in the file speaks to
  named graphs. It is a pointer to the six endpoints named above, not a
  measurement to build on. Retiring the deferral needs a dedicated run at
  `--max-cost expensive` against an endpoint from that list, plus a check that
  the result actually depends on the `GRAPH` branch and not on the endpoint's
  default graph.

- **An API key in an endpoint's query string is published, permanently.** Every
  run-scoped fact's subject is `urn:sparqlwatch:<kind>:<run>:<percent-encoded
  endpoint>:<metric>`, so the endpoint URL is a reversible part of the identifier
  of everything we say about it, and the `declarationsRead` fact names the
  endpoint IRI itself. All of it sits in a per-run named graph that is immutable
  and append-only: a published identifier can never be corrected. `registry.rs`
  refuses a URL whose authority carries a non-empty userinfo component, whatever
  the scheme, so both `http://user:secret@host/sparql` and
  `ftp://user:secret@host/sparql` are dropped at load. It does nothing about
  `https://host/sparql?apikey=...`, because a query parameter's meaning is the
  operator's, not ours, and a name-based blocklist (`key`, `token`, `apikey`, ...)
  would be a confident wrong answer in both directions: it would drop legitimate
  endpoints whose query carries a dataset selector, and admit a credential under
  a name nobody guessed. Closing it properly needs the registry to distinguish a
  public URL from a credentialed one, which is the question stage 1d was to
  answer.

  Stage 1d-a answers it for one source, narrowly: **the LOD Cloud dump contains
  no credentialed URL at all.** `without_credentials` refused 0 of its 548
  distinct URLs, recorded as `refused_credentials = 0` in
  `registry/lod-cloud.provenance.toml`. So nothing in this dump forced the
  seeder to tell a public URL from a credentialed one, and that is a property of
  this dump rather than of seeding in general. The refusal stays: it costs
  nothing here, and stage 5 accepts public submissions. None of it touches the
  query-string case above, so the standing advice is unchanged: do not put a
  secret in `endpoints.toml`.

- **An exclusion is honoured by the next build, not by the request.**
  `registry/exclusions.toml` is the only removal path this project has, nothing
  watches a mailbox, and nothing retracts what an earlier run already published.
  Asking not to be probed, above, states every limit of the mechanism, and any
  page that offers a sysadmin a way out has to state them too rather than imply
  a promise the file cannot keep.

- **A cross-host redirect can wait at another endpoint's gate, and that wait is
  charged to the metric budget.** `--concurrency` groups endpoints by the host
  they name, but a redirect is gated on the host each hop actually touches (see
  `gated_hop`, and the test `a_probe_redirected_to_another_host_gates_the_new_host`),
  so an endpoint that redirects into another host being swept at the same moment
  queues at that host's gate. The review of stage 1c-b3 verified it with mocks:
  two endpoints at `--concurrency 2`, the first redirecting to the second's
  host, produced arrivals `a, b, b, a, b, b` with consecutive arrivals at the
  shared host 456 ms apart against a `delay + gap` of 450 ms, so one guard was
  serving two groups. At production values that wait is up to
  `gap + request` = 32 s, or `2 + 30 + 20 + 30` = 82 s if the shared host is
  throttled, inside the 60 s metric budget; a cancelled metric budget is silent,
  so the endpoint would read `indeterminate` with nothing saying why. Grouping
  cannot close it, because the redirect target is only knowable by following the
  redirect, and following one without the gate is exactly what the per-hop gate
  exists to prevent. The common case is unaffected: `host_key` ignores the
  scheme, so an `http` to `https` redirect on one host stays inside one group and
  costs another gap and nothing else. How often the cross-host case arises in a
  real registry is **unquantified**. Of the three endpoints in the shipped
  `endpoints.toml`, measured on 2026-08-24, none redirects at all: a queryless
  GET returns 404, 200 and 500 respectively, with no `Location`. So the shipped
  list does not exercise this.

  The 543-candidate sweep **cannot answer it either**, and the reason is our own
  logging rather than the registry. The hop-level line, "re-issuing the request
  at a redirect target", is a `tracing::debug!` (`client.rs:353`) and that run
  was at `info`, so the run's count of cross-host hops is zero for the wrong
  reason and **no rate may be inferred from it**. What the run does show is a
  different fact: 14 redirect chains looped and were not resolved. That line is a
  `tracing::warn!` (`client.rs:350`), so it was visible at `info` and the count
  is real, unlike the DEBUG hop line above; but it was counted from the run's
  console output, which was not kept beside the `.nq` in
  `~/code/sparqlwatch-runs/`, so it cannot be re-derived either. Answering the
  question properly needs a run at `RUST_LOG=debug` or a counter on the client,
  and neither is worth another 1.5 hours of strangers' traffic on its own, so
  the frequency stays unquantified.

- **Two sweeps with different `--at` values and the same `--out` lose one of
  them, silently.** The partial file is named for the run, so each gets its own
  and neither refuses the other, and the second `rename` replaces the first's
  `--out`. Rule 1's promise is technically kept, `--out` holds *a* complete run,
  but a run that finished and reported success is gone from the only place that
  holds it, with nothing said. This is not new to the incremental write:
  `fs::write` behaved the same way. It is worth naming because overlapping
  `CronJob`s are exactly how it happens, and because the `O_EXCL` refusal that
  protects a retry does not protect this case: the refusal only fires for
  invocations that SHARE an `--at`.

- **A flushed chunk is not a synced chunk.** Both halves of the design's
  per-endpoint isolation rule are now met: no endpoint's slowness delays another
  endpoint's probing, because hosts are grouped into separate tasks, and no
  endpoint's slowness delays another endpoint's OUTPUT, because each chunk is
  written and flushed as that endpoint finishes. What that buys is bounded
  precisely: a process that dies keeps every chunk already flushed, since the
  bytes are with the kernel and no destructor is needed. A machine that loses
  power can still lose them, because there is no `fsync` and none is planned.
  Three reasons were given, in order of weight, and **the third is now retired
  by measurement**. A lost run is a **gap in history rather than a false fact**:
  a run graph is written once and never corrected, so losing one leaves a
  missing sweep, not a wrong verdict standing in the store as current. Coverage
  returns on its own: the next scheduled sweep probes the same registry under
  its own `--at`, into its own graph.

  The third reason, which this file used to make, was that 548 `fsync` calls per
  sweep, one per endpoint, cost something on the deployment's volume that nobody
  here had measured, so paying it would buy an unquantified amount of durability
  with an unquantified amount of latency. **That cost is now measured, and it is
  negligible.** The 543-candidate sweep wrote 543 chunks over 5181 seconds, 11.6
  KB per chunk on average, so even at a pessimistic 10 ms per `fsync` the whole
  sweep would pay 5.4 seconds, about 0.1% of its wall clock. Cost is therefore
  no longer an argument in either direction, and whether to `fsync` is now
  purely a durability-versus-simplicity call, to be argued on that basis. The
  first two reasons are untouched and nothing in the code has changed.

  What a re-run does NOT do is recover the lost run: the same `--at` names the
  same graph, but a sweep observes a changing world, so what it writes is a new
  observation and not the old file back. The other half of the boundary, the zeroed tail a power loss
  can leave and no terminator rule can rescue, is under "How a run is written".

## Tests

```sh
cargo test                                  # unit + integration, all offline
cargo test --test live_smoke -- --ignored   # hits real third-party endpoints
cargo clippy --all-targets -- -D warnings
```

Everything but `live_smoke` runs against a local `wiremock` server, so the
suite is deterministic and CI never touches a stranger's endpoint. Rust 1.96,
edition 2021, no nightly features.

## Looking at a run

The web tier is stage 3 and does not exist yet. Until it does, render a run as a
standalone local page:

```
cargo run -q -- --at 2026-08-20T12:00:00Z --out run.nq
node ../tools/render-run.mjs run.nq run.html
```

It is a read-only viewer over the emitted N-Quads, using the same verdict encoding
as the design: dashed borders mark "works but not declared" and "indeterminate",
and `absent` has no border at all, because it is the only verdict that claims a
negative. The real web tier will query Oxigraph rather than parse a file.

A run containing content samples (that is, one from an `--max-cost expensive`
sweep) also gets a "Content samples" panel, one detail block per (endpoint,
metric) sample: the endpoint, the metric, the value count, and whether the
list is truncated, with the truncation state carried by its own badge rather
than left to be inferred from the count. The value list itself sits behind a
disclosure toggle rather than always on screen, since 59 or more IRIs is too
much to put in a table row.

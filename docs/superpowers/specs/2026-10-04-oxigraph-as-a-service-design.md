# Oxigraph as a service: publishing without a restart

**Status:** design, 2026-10-04. Nothing built.

## The problem, measured

The site is unavailable for roughly 60–90 seconds every hour. On the 15:00
restart of 2026-10-04: the old pod terminated, the `build-store` init container
ran 15:05:37 → 15:06:14 (37s, to load ONE run file out of 521 already in the
store), the containers started, and readiness passed at 15:06:52. The operator
hit it mid-window and reported the server down.

That is about 2% of every hour, by design, and the design is explicit about why.

## Why it happens

Four facts compose:

1. **A read-only handle is a snapshot.** `app.py` opens the store with
   `Store.read_only(path)` and `_opened_store` caches that handle for the life of
   the process. RocksDB takes its view at open and never sees a later write.
2. **So publishing means replacing the process.** The sweep writes a run file and
   then `rollout restart`s the site. `app.py` states it: "THE STORE CANNOT CHANGE
   UNDER THIS PROCESS ... restarting is the only thing that publishes."
3. **The replacement cannot overlap itself.** `replicas: 1`,
   `strategy: Recreate`, an init container that opens the store READ-WRITE, and a
   ReadWriteOnce PVC. Two pods alive at once means two processes contending for
   the RocksDB write lock — the failure that took the site down for fourteen
   hours on 2026-10-02.
4. **`Recreate` is therefore load-bearing**, and `Recreate` is exactly what
   guarantees a gap.

## What was ruled out, and why

**Re-open the read-only handle on a TTL.** This is what ontoexplorer's API does
(`_RO_STORE_TTL = 60.0` in `clients/oxigraph.py`), and it was the first
candidate. pyoxigraph 0.5.9's own documentation forbids it:

> Opening as read-only while having an other process writing the database is
> undefined behavior.

There is no `Store.secondary` in 0.5.9 either. RocksDB has a secondary mode that
catches up with a primary; pyoxigraph does not expose it. ontoexplorer's own
comment names this trap and points at its HTTP mode as the way around it, so the
pattern is not an endorsement of the embedded one.

**Shrink the gap instead.** Attacking the 37-second init is cheap and does not
meet the requirement, which is that the site should never be down.

## The design

**One process opens the store. Everything else speaks HTTP to it.**

An `oxigraph` Deployment owns the store PVC as sole opener and serves
SPARQL Query and Update. `site` and `sparql` lose their volume mounts entirely.
The loader stops being an init container: the sweep POSTs its run to `/update`,
and nothing restarts.

This is the mode ontoexplorer reaches for when it wants no secondary at all:
"Server owns the volume (sole opener): every process reads via the proxy and
writes via the routed write functions — none opens embedded, avoiding the
'read-only alongside a writer is undefined behaviour' trap."

### What the proxy must implement

Everything this codebase asks of a `Store` has an HTTP equivalent but one:

| call | sites | over HTTP |
| --- | --- | --- |
| `query` | 37 | `/query` |
| `update` | 15 | `/update` |
| `contains_named_graph` | 5 | `ASK { GRAPH <g> { ?s ?p ?o } }` |
| `remove_graph` | 2 | `DROP GRAPH` |
| `quads_for_pattern` | 2 | `CONSTRUCT` |
| `named_graphs` | 1 | `SELECT DISTINCT ?g` |
| `add`, `extend` | 2 | `INSERT DATA` / the graph store protocol |
| **`optimize`** | **2** | **none** |

`optimize` is the one that does not survive the move, and it is not a detail.
Pruning without compacting made the store HEAVIER on 2026-10-03 and put the site
into an OOM crashloop; `load_run` compacts after a load for the same reason, its
comment recording the store keeping four times its compacted size. Under a
server, compaction becomes the server's own and we lose the ability to demand
it. **This needs an answer before anything is built** — either the server
compacts adequately on its own, which has to be measured rather than assumed, or
the maintenance jobs keep a privileged path to the volume for compaction alone.

### Substitutions have to go

Nine call sites pass `substitutions=` (pyoxigraph's SEP-0007), across
`endpoint_measurements` (4), `endpoint_content`, `endpoint_history`,
`void_document`, `app`, and `load_run`. Substitution is a pyoxigraph API and is
not part of SPARQL-over-HTTP.

Each binds a single variable, almost always `?endpoint` to a validated IRI, so
each becomes a `VALUES ?endpoint { <iri> }` clause. The rewrite REMOVES a
constraint rather than adding one: seven query files carry comments explaining
that a variable sits in the projection only because substitution requires it
there, and that stops being true.

### The caching doctrine inverts, and this is the delicate part

Three `lru_cache`s — `endpoint_index`, `fleet_history`, `build_payload` — are
keyed on the store HANDLE with no invalidation, and `app.py` argues at length
that this is the only honest cache available:

> So the cache key is the store HANDLE and there is no invalidation ... nothing
> here guesses how long an answer stays true, and no request can ever be served
> an answer the handle it was asked of would not give. A TTL would be a guess,
> and on a site whose doctrine is that a stale qualifier is a positive claim, a
> guess is the wrong instrument.

A live store makes the premise false. A proxy is a stable object, so those caches
would serve their first answer forever.

**The fix keeps the doctrine rather than trading it away: key the caches on the
newest run.** One cheap query per request yields the newest run's IRI; the
expensive fleet-wide passes are cached against it and rebuild exactly when new
data lands. No TTL, no guess — the key changes when and only when the answer
could change. `SnapshotCache`, the whole-response byte cache, takes the same key.

This is the part worth reviewing hardest. It is the point where a storage change
touches a stated principle, and getting it wrong means the site quietly serves
yesterday's answers while claiming to be live.

## Sequence

Ordered so that nothing is removed before its replacement works. Two sequencing
mistakes on 2026-10-03 — pruning before the reader shipped, pruning before
compaction was wired in — were each individually-verified steps in the wrong
order, which is a failure no per-step check catches.

1. **`HttpStore` proxy plus the substitution rewrite.** Nothing deployed changes:
   the proxy is tested against a local `oxigraph serve`, and the rewritten
   queries keep running against the embedded store. Both paths pass the same
   suite.
2. **Re-key the caches on the newest run.** Still embedded, still correct — the
   key simply never changes while the store cannot.
3. **Answer the compaction question.** Measure what `oxigraph serve` does on its
   own after a large delete. Decide before, not after.
4. **Stand the server up beside the current pod**, reading the same volume, and
   compare: every page rendered both ways must match.
5. **Cut over reads**, leaving the init container and the restart in place. The
   outage is unchanged; the risk is contained to reads.
6. **Move writes to `/update` and delete the restart.** This is the step that
   removes the outage, and it is last.

## What this costs

The `sparql` container currently exists for one reason: its own memory ceiling,
so "a query that balloons is not the page server's problem". Under a server, a
ballooning query is the SERVER's problem, and the server is now a single point of
failure for every reader. That is a real trade — today a heavy SPARQL query can
only hurt `/sparql`; afterwards it can hurt everything.

A second trade: the store becomes reachable only through one process, so the
maintenance jobs under `ops/` (summarise, prune, rebuild, recon) either learn to
speak HTTP or keep a privileged direct path, which reintroduces the two-opener
question they were meant to escape.

Neither is a reason not to do it. Both are reasons to write them down now.

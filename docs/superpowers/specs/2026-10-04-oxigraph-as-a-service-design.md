# Publishing without a restart

**Status:** design, revised 2026-10-04 after review. Nothing built.

The first draft of this proposed `oxigraph serve` behind an HTTP proxy. Review
found the cache key unsound and the sequence unsafe, and raised two cheaper
designs the draft never considered. This version replaces it. What the draft got
right — the problem, and why the obvious fix is forbidden — is kept.

## The problem, measured

The site is unavailable 60–90 seconds every hour. On the 15:00 restart of
2026-10-04 the old pod terminated, `build-store` ran 15:05:37 → 15:06:14 (37s,
to load ONE run file out of 521 already in the store), and readiness passed at
15:06:52. The operator hit it mid-window and reported the server down.

## Why

1. **A read-only handle is a snapshot.** `app.py` opens with
   `Store.read_only(path)`; `_opened_store` caches it for the process's life.
2. **So publishing means replacing the process**, which `app.py` states outright:
   "restarting is the only thing that publishes."
3. **The replacement cannot overlap itself**: `replicas: 1`, `Recreate`, a
   read-write init container, an RWO PVC. Two pods means two writers contending
   for the RocksDB lock — the failure that cost fourteen hours on 2026-10-02.

## What is forbidden, and what is merely unsupported

**Re-opening the read-only handle on a timer is undefined behaviour.**
pyoxigraph 0.5.9: "Opening as read-only while having an other process writing
the database is undefined behavior." There is no `Store.secondary`.

Measured on this machine, pyoxigraph 0.5.9:

```
read_only open ALONGSIDE a writer:  SUCCEEDED, saw the writer's quad
second read-write open:             REFUSED — lock held
one process, 1 writer + 2 reader threads, 300 writes:  0 errors
```

So read-only openers do not contend for the lock, and the undefined case "works"
— which is the trap. A reader keeps deleted SSTs alive through its open file
descriptors and sees a stale but consistent view, until it needs a file
compaction removed. It fails rarely and unpredictably. **This codebase refuses
stale qualifiers as positive claims; it should not build its storage layer on
behaviour its own dependency declines to define.**

The last line is the important measurement: **one process may read and write
concurrently, from many threads, and sees its own writes immediately.** That is
supported, and it is the foundation of the recommended design.

## D — immutable snapshots (recommended after the spike of 2026-10-04)

Readers cannot safely share a store with a writer. A SNAPSHOT has no writer, so
readers sharing a frozen checkpoint is fully defined.

The writer owns the live store and calls `Store.backup()` after each load.
Readers open `snapshots/gen-N` read-only. Nothing will ever write to it.

**Measured against the deployed store, 2,950,651 quads and 1.07 GB:**

```
backup():                       0.013-0.015s
four snapshots, actual disk:    81,920 bytes   (hard links)
snapshot cold open:             ~0s
build_payload on a snapshot:    2.4s
```

So a snapshot is free in time and very nearly free in disk.

**What it buys that A and B cannot.** Readers hold no exclusive resource, so
deploys roll with `maxUnavailable: 0` — and a reader can re-open a NEWER
snapshot in place, because that one is immutable too. Both restart causes go.
It also keeps everything the other designs spend: the `ops/` jobs still open the
live store exactly as they do now, the `sparql` container keeps its own memory
ceiling, and `substitutions=`, `optimize` and the four integrity refusals all
stay.

### THE SNAPSHOT MUST BE TAKEN FROM A READ-WRITE HANDLE

This is the spike's most important result and it fails SILENTLY.

```
read-only handle sees:             8000
snapshot taken from that handle:   5000   <- the WAL is missing
snapshot taken from a read-write:  8000
```

`backup()` from a read-only handle omits whatever is still in the write-ahead
log. It does not error. It produces a valid database quietly missing the most
recent writes — in production, readers permanently a run behind with nothing to
show for it. The spike saw exactly this against the deployment: the handle read
2,950,651 quads and its snapshot held 2,940,244, the 10,407 difference being the
17:00 run still in the WAL after the init container exited.

A read-write handle's checkpoint flushes first and is correct. So the snapshot
belongs INSIDE the loader, immediately after the load, with one process doing
both.

A second hazard from the same spike: a read-only backup taken while a writer is
active can fail outright, hard-linking an SST that compaction removed
underneath it (`FileNotFoundError: while link file to ... 000009.sst`). Loud
rather than silent, and another reason the writer owns the snapshot.

### What is still unknown

Four snapshots of a store nobody is writing to is the easy case. The question
the spike cannot answer is what a week of snapshot-and-reap does while the live
store compacts underneath: hard links keep superseded SSTs alive, so disk is
held by whatever the oldest retained snapshot references. Build the producer,
watch disk for several days, and only then change how readers open the store.

## Three designs



### A — `oxigraph serve` behind an HTTP proxy

A server owns the volume; `site` and `sparql` speak SPARQL over HTTP.

Costs: nine `substitutions=` call sites rewritten to `VALUES` (SEP-0007 is a
pyoxigraph API, not SPARQL-over-HTTP); `store.optimize` has **no HTTP
equivalent**, and pruning without compacting already caused an OOM crashloop on
2026-10-03; `_opened_store`'s four integrity refusals lose their home; every
query crosses HTTP with result serialisation, and `build_payload` is seconds of
queries.

### B — one process owns the store (recommended)

The site process opens the store **read-write** and loads new run files on a
background thread. It sees its own writes immediately, so nothing restarts.
There is no second opener, so there is no undefined behaviour.

Costs, and they are real:
- **The `sparql` container cannot open the store.** It exists for one reason —
  its own memory ceiling, so "a query that balloons is not the page server's
  problem". It must proxy to the site process or be dropped, and that isolation
  is lost.
- **Every `ops/` job loses its direct path**: summarise, prune, rebuild, recon
  all open the store read-write today and could not. They need an authenticated
  admin surface on the site process, which is a new attack surface and a new
  thing to get wrong.
- **`optimize` survives**, which A cannot offer, because we own the process.

### C — move the load into the sweep job, site pods read-only, `RollingUpdate`

Smallest change: keeps the caches, the doctrine, the substitutions and
`optimize`. The loader already has the runs volume.

**Rejected.** A rolling update means the old pod serves from a read-only handle
while the sweep job writes — exactly the undefined case above. It would work
almost always. The draft rejected the TTL re-open for this precise reason, and
accepting it here would be applying a different standard to a cheaper option.

## The caches, which is where the draft was wrong

Three `lru_cache`s and `SnapshotCache` are keyed on the store handle with no
invalidation, and `app.py` argues this is the only honest cache available
*because the store cannot change*. Any of these designs falsifies that premise.

The draft proposed keying on "the newest run" and claimed "the key changes when
and only when the answer could change". **That is false in both directions:**

- **A load is not one write.** `load_run.py` has twelve `store.update` sites;
  `_maintain_current` issues one per endpoint, about 126 of them; `rebuild_current`
  may follow. The key would flip at the FIRST write, so a request arriving
  mid-load computes from a half-advanced `current` and caches it under the new
  key until the next sweep.
- **`rebuild_current` removes `current` wholesale** (`load_run.py:1430`) and
  rewrites it over minutes. Readers would see an empty `current` throughout.
- **Writes with no new run.** `daily.py` summarises, prunes and compacts; the
  newest run does not move, so nothing invalidates.
- **Same key, different content.** Re-loading a run IRI is the documented
  recovery for a truncated file. Same newest run, different data.

**Instead: a generation quad.** `<urn:sparqlwatch:store> sw:generation N`, bumped
as the LAST statement of every writer — the loader, `rebuild_current`, and every
job in `daily.py`. Readers key the caches on it. A request arriving mid-write
hits the cached answer for generation N: older than the store, and internally
consistent, which is the property that matters. After a cold build, re-read the
generation and discard the result if it moved.

This is still no TTL and no guess. It is the handle-key doctrine with a handle
that can advance.

## Writers must be serialised

Today the RocksDB lock serialises the loader against `rebuild_current` and
`daily.py` — crudely, by making them fail, which is how the 2026-10-02 outage
presented. Under B they are calls into one process and can interleave:
`_maintain_current` reads its pointers then writes; `rebuild_current` reads the
graph list then drops `current`; `daily.prune_day` computes `pinned_runs` then
drops over many statements. A rebuild straddling a load silently loses the new
run's facts.

So B needs an explicit single-writer lock in-process, held for the whole of a
load, a rebuild or a daily job. This is not optional and is cheap to get right;
it is expensive to discover later.

## Compaction

Both `optimize` calls justify themselves by the process exiting before RocksDB's
background threads run. A long-lived process removes that reason for ordinary
loads. What remains is the prune's mass tombstones, which background compaction
does not target — level-triggered, not tombstone-triggered. Under B the admin
surface exposes `optimize` and `daily.py` calls it as it does today.

## Sequence

Each step is safe to stop at, and the step that removes the outage is last.

1. **The generation quad**, with the embedded store unchanged. Every writer bumps
   it; readers key on it. Verifiable now: the key cannot change while the store
   cannot, so this is a no-op in production and a real test in CI.
2. **The in-process writer lock**, still with the init container doing the
   loading. Again a no-op in production, because there is still one writer.
3. **The admin surface**, with the `ops/` jobs moved onto it one at a time,
   while the site still opens read-only and the jobs still have the volume.
   Each job proven against the new path before the old one goes.
4. **The site opens read-write and loads on a thread.** The init container and
   the hourly restart are deleted in the same change, because they cannot
   coexist with it. This is the step that removes the outage, and it is the only
   one that cannot be half-done.
5. **Decide the `sparql` container's fate** — proxy or drop — on measurements
   taken after step 4, not before.

Step 4 is irreversible in the sense that it changes the publish mechanism in one
go. Steps 1–3 are what make it a small change rather than a rewrite.

## What is not yet known

- Whether loading on a background thread inside the serving process costs page
  latency enough to matter. Measure before step 4.
- What the admin surface authenticates with. The namespace is dev and the
  cluster is private, which is a reason to keep it simple, not a reason to skip
  it.
- Whether dropping the `sparql` container's memory ceiling is acceptable. Today
  a ballooning query hurts only `/sparql`.

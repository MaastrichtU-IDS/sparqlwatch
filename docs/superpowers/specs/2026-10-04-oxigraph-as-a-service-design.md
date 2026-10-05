# Publishing without a restart

**Status:** built and deployed 2026-10-05. See *What shipped* at the end for
what was measured and what was left out. The body below is the design as it
was argued, kept unedited -- its predictions are worth more against the
outcome than they would be quietly corrected.

Two reviews and one spike changed this twice. Draft 1 proposed `oxigraph serve`
behind an HTTP proxy; its cache key was unsound and its sequence unsafe. Draft 2
proposed the site process owning the store read-write; it removed the hourly
restart but not deploy restarts, which the operator then said were not
acceptable: people are going to come to this site to explore endpoints.

This draft proposes **immutable snapshots**, and the rest of the file is written
for that. Where an earlier recommendation survives it is marked as rejected and
why, because the reasons are the useful part.

## The requirement

The site should not be down. It currently is, for 60–90 seconds every hour. On
the 15:00 restart of 2026-10-04 the old pod terminated, `build-store` ran
15:05:37 → 15:06:14 (37s, to load ONE run file out of 521 already in the store),
and readiness passed at 15:06:52.

This is a dev cluster and a move to prod with its hardening is expected later.
That is a reason to pick a design that makes replicas possible, not a reason to
accept downtime now.

## Why it is down

A read-only handle is a snapshot: `app.py` opens with `Store.read_only(path)`
and `_opened_store` caches it for the process's life, so new data reaches a
reader only when the process is replaced. `app.py` says so: "restarting is the
only thing that publishes." And the replacement cannot overlap itself —
`replicas: 1`, `Recreate`, a read-write init container, and a ReadWriteOnce
store PVC — because two writers contend for the RocksDB lock, which is the
failure that cost fourteen hours on 2026-10-02.

## The design: immutable snapshots

Readers cannot safely share a store with a writer. A snapshot has no writer, so
readers sharing a frozen checkpoint is fully defined.

The loader owns the live store and, after each load, calls `Store.backup()` into
a new generation. Readers open a generation read-only. Nothing will ever write to
one.

**Measured against the deployed store, 2,950,651 quads and 1.07 GB:**

```
backup():                       0.013-0.015s
four snapshots, actual disk:    81,920 bytes   (hard links)
snapshot cold open:             ~0s
build_payload on a snapshot:    2.4s
```

A snapshot is free in time and nearly free in disk. Readers hold no exclusive
resource, so deploys roll, and a reader can take a newer generation in place —
both restart causes go. It also keeps what the other designs spend: the `ops/`
jobs still open the live store as they do now, the `sparql` container keeps its
own memory ceiling, and `substitutions=`, `optimize` and the four integrity
refusals all stay.

### The snapshot must be taken from a read-write handle

The spike's most important result, and it fails SILENTLY:

```
read-only handle sees             8000
snapshot taken from that handle   5000   <- the write-ahead log is missing
snapshot taken from a read-write  8000
```

`backup()` from a read-only handle omits whatever is still in the WAL. It does
not error; it produces a valid database quietly missing the newest writes.
Against the deployment the handle read 2,950,651 quads and its snapshot held
2,940,244 — the 10,407 difference being the 17:00 run, still in the WAL after
the init container exited. Readers would have sat permanently one run behind
with nothing anywhere saying so.

This is structural rather than luck: a checkpoint flushes memtables before
linking, and a read-only database cannot flush. The loader must still
`Store.flush()` before `backup()` and then verify (below), because a structural
argument is not a test.

A second hazard from the same spike: a read-only backup taken while a writer is
active can fail outright, hard-linking an SST that compaction removed underneath
(`FileNotFoundError: while link file to ... 000009.sst`). Loud rather than
silent, and another reason the writer owns the snapshot.

### The publish protocol

Naming a directory is not publishing it. `backup()` creates its target and
raises if it exists, so a reader listing `snapshots/` can open one that is half
written.

1. Loader loads, `flush()`es, `optimize()`s when due, then `backup()` to
   `gen-N.tmp`.
2. Loader opens `gen-N.tmp` read-only and **verifies**: its generation quad is
   the one just written, and its quad count matches the live store's. A snapshot
   that fails verification is deleted and the old `CURRENT` stands.
3. `rename(gen-N.tmp, gen-N)` — atomic within a filesystem.
4. Atomically rewrite a `CURRENT` file naming `gen-N`.

Snapshot AFTER `optimize`, never before: a snapshot of the pre-compaction store
pins the bloated SST set.

### How a reader follows, and it is a poll

Readers poll `CURRENT`. **This is a poll, and the doc should not pretend
otherwise** — draft 2 claimed "no TTL, no guess" and that claim does not survive
here. What it buys over a TTL on data freshness is that each generation is
internally consistent and named, so a reader never serves a half-advanced store;
staleness is bounded by the poll interval and is reportable.

On seeing a new generation a reader opens it, **pre-warms `build_payload` on a
background thread**, and only then swaps one handle object. Every request pins
the handle it started with, so a page is never assembled from two generations.
Readers publish which generation they served in an `X-Generation` header.

### Readers will leak every generation unless this is fixed

`_opened_store` is `@lru_cache(maxsize=None)` keyed on path (`web/app.py:405`).
A new generation path adds a handle that is never released. The three
`maxsize=4` caches and `SnapshotCache` hold strong references to old handles
too, and `sparql_pool.py:101` opens a handle per worker.

An open RocksDB handle keeps unlinked files alive, so **the reaper would free
nothing while `du` reported success** — watch `df`, not `du`. Worse, a reader
still on a reaped generation opens SSTs lazily and hits `FileNotFoundError`
mid-query, which is the failure this design rejects alternatives for.

So: bound `_opened_store`, release the previous handle on swap, recycle the
`sparql_pool` workers on swap, and have the reaper delete only generations no
live reader reports in `X-Generation`, with a floor of keeping two.

### Disk, worst case

The store is rewritten twice a day — `OPTIMIZE_EVERY` is one day
(`loaded_manifest.py:68`) and `daily.py` compacts after a prune. A snapshot
straddling a rewrite pins a whole distinct SST set.

With hourly snapshots and a retention of 24, roughly three full copies (~3.3 GB)
plus the live store at its post-write size (the code records four times the
compacted size) is about 8 GB against a 20Gi volume. Survivable, but there is no
reason to carry it: **retain two or three generations by count**, and alert on
`df`. If the reaper stalls — loader crash, or the leaked handles above — growth
is about a gigabyte per compaction and unbounded.

## What this replaces, and why those were rejected

**A — `oxigraph serve` behind an HTTP proxy.** Nine `substitutions=` call sites
rewritten, the four integrity refusals homeless, every query across HTTP, and
`store.optimize` has no HTTP equivalent — which matters because pruning without
compacting already caused an OOM crashloop on 2026-10-03.

**B — the site process owns the store read-write.** Simple and sound, and it
removes the hourly restart. It cannot remove deploy restarts: one writer means
two pods cannot coexist, so `Recreate` stays. Rejected on the requirement.

**C — loader in the sweep job, read-only site pods, `RollingUpdate`.** Rejected
for undefined behaviour: a rolling update serves from a read-only handle while
the sweep writes. D is C plus snapshots, which removes exactly that objection.

**A correction on the record.** The second review called D undeployable because
the store is an `emptyDir`. That is true of
`/home/svcaccount/services/.../app.yaml`, a checkout last modified 24 September.
The live cluster has `sparqlwatch-store` as a bound ReadWriteOnce 20Gi PVC and
the deployment mounts it; the spike read 2.95M quads through it. The stale
checkout is worth knowing about because it will mislead the next reader too.

The RWO store does pin every pod that mounts it to one node, which bounds how
far `maxUnavailable: 0` goes: readers can roll, but only on that node. Spreading
across nodes is a prod question and needs RWX or per-reader copies.

## First step

Not "build the producer and watch disk", which tests the easy half: with no
readers, nothing pins deleted files and the failure that matters never fires.

Build in one step, locally first:

- the producer with `flush` → `optimize` when due → `backup` to `.tmp` → verify
  → rename → `CURRENT`;
- reaping to a retention of two;
- and a harness holding a reader on `gen-N` while the reaper deletes it and
  compaction runs, measuring `df` rather than `du`.

Only once that harness is boring does any of it reach the cluster, and readers
move after the producer has run for days.

## What shipped

Deployed to dev on 2026-10-05 across sparqlwatch #58-#63 and services #486/#488.

**The hourly outage is gone**, which was the requirement. The 14:00 sweep of
2026-10-05 is the proof: it wrote `run-2026-10-05T14:00:30Z.nq`, loaded it,
published generation 9, and the site was serving that generation by 14:07:41 --
same pod, zero restarts, 115 requests across the window and not one non-200.
That is the first time in this deployment's life that new data reached readers
without the process being destroyed. Deploys roll the same way: `Recreate` is
gone, `maxUnavailable: 0`.

**Measured on the deployment**, against roughly three million quads:

```
publish (flush, backup, verify, rename, reap):  0.5 s
disk after nine generations, retention of two:  18.4 GiB free of 19.5
swap picked up after a publish:                 inside one poll (30 s)
```

Disk is the number the design was least sure of, and it is the one to keep
watching: `load_run.py` now prints free space on every publish, so the evidence
accumulates hourly without anyone mounting the PVC. One figure is not a
retention measurement -- what matters is a figure from either side of a
compaction, and that takes a day.

**What was built differently.** The loader moved out of the site pod entirely,
into the sweep's own job, rather than staying an init container -- which is what
made `RollingUpdate` safe and removed the last writer a reader could share a
store with. The site pod and both CronJobs carry a `podAffinity`, because the
store PVC is ReadWriteOnce and that is per node, not per pod; read-only mounts
do not soften it.

**What was NOT built.** The reaper deletes by count, with a floor of two. It
does not consult live readers, and no `X-Generation` header is served. The
design's argument for that still stands -- a reader that has just read `CURRENT`
has not opened it yet -- but the floor of two covers the same window at a
fraction of the machinery, and disk is nowhere near the pressure that would
justify the rest. Revisit it when the free-space figures say to.

**A failure got quieter, and was then given a voice.** Before, a loader that
could not open the store crashlooped the pod: impossible to miss. Afterwards it
is a failed job and a site serving its last generation indefinitely -- correct
data, arbitrarily old, nothing saying so. The follower therefore reports a
`CURRENT` that has not moved in two hours (`snapshots.Follower.check_stale`),
since it is the one process always running and already reading that file. It
never refuses to serve: a stale site beats no site, which is the same
requirement this whole document is about.

**Three things this design reasoned wrong**, each settled by the cluster:

- `RollingUpdate` alone would not have prevented the 2026-10-05 morning outage;
  `Recreate` was load-bearing for correctness until the writer moved out.
- The snapshot has to be taken **after** `store.optimize()`. A checkpoint pins
  the SST set as it finds it, so one taken before compaction pins the bloated
  version -- about four times the compacted size -- for that generation's life.
- A fully loaded store that had never published would never publish, because
  `load_run.py` returns before opening the store when there is nothing to load.
  The bootstrap in `--skip-loaded` exists for exactly that state.

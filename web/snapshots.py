"""Immutable generations of the store, so a reader never shares one with a writer.

WHY. The site is unavailable 60-90 seconds every hour because a read-only handle
is a snapshot taken at open: new data reaches a reader only when the process is
replaced, and the replacement cannot overlap itself while a read-write init
container and a single store exist. A reader cannot simply re-open the live
store instead -- pyoxigraph is explicit that "opening as read-only while having
an other process writing the database is undefined behavior", and 0.5.9 exposes
no secondary mode.

A SNAPSHOT HAS NO WRITER, so readers sharing a frozen checkpoint is defined.
`Store.backup` is a RocksDB checkpoint: hard links, not a copy. Measured against
the deployed store on 2026-10-04, 2,950,651 quads and 1.07 GB: 0.013s per
snapshot, and four of them cost 81,920 bytes of real disk.

THE SNAPSHOT MUST COME FROM A READ-WRITE HANDLE. A checkpoint flushes memtables
before linking and a read-only database cannot flush, so `backup()` from a
read-only handle silently omits the write-ahead log -- a valid database quietly
missing the newest writes. Measured: a read-only handle saw 8,000 quads and its
snapshot held 5,000; against the deployment, 2,950,651 seen and 2,940,244
snapshotted, the difference being one run still in the WAL. Nothing errors. This
module therefore takes a `Store` opened read-write, and verifies afterwards
rather than trusting the argument above.
"""

from __future__ import annotations

import os
import re
import shutil
import threading
import time
from pathlib import Path

from pyoxigraph import NamedNode, Quad, Store

# The marker a snapshot is verified against. Written into the LIVE store
# immediately before the checkpoint, so finding it in the snapshot proves the
# snapshot carries writes made moments earlier -- which is exactly what a
# read-only backup would have dropped.
GENERATION = NamedNode("urn:sparqlwatch:generation")
STORE = NamedNode("urn:sparqlwatch:store")
META = NamedNode("urn:sparqlwatch:meta")

CURRENT = "CURRENT"
_GEN = re.compile(r"^gen-(\d{6})$")

# When a store that is no longer being republished stops being a slow hour and
# starts being a stopped pipeline. The sweep publishes once an hour, so one
# missed hour is a long run or a late node; two in a row is not.
STALE_AFTER = 2 * 60 * 60
# And once it HAS stopped, repeating it every poll is 120 lines an hour, which
# reads the same as saying nothing.
_REPEAT_AFTER = 60 * 60


def _generations(root: Path) -> list[int]:
    """Every published generation, oldest first. Half-written ones are ignored."""
    out = []
    for entry in root.iterdir() if root.is_dir() else []:
        m = _GEN.match(entry.name)
        if m and entry.is_dir():
            out.append(int(m.group(1)))
    return sorted(out)


def published(root: Path) -> int | None:
    """The generation `CURRENT` names, or None before the first publish.

    Readers follow this file and nothing else. A generation directory that
    exists but is not named here is either half-written or not yet published,
    and either way is not theirs to open.
    """
    try:
        name = (root / CURRENT).read_text().strip()
    except OSError:
        return None
    m = _GEN.match(name)
    return int(m.group(1)) if m else None


def path_of(root: Path, generation: int) -> Path:
    return root / f"gen-{generation:06d}"


def published_at(root: Path) -> float | None:
    """When CURRENT last moved, as a POSIX timestamp, or None if it cannot be read.

    This is the publish time of the generation being served, and `publish` gets
    it right for free: the pointer is written to a temporary file and renamed,
    and rename carries the mtime across. It is deliberately NOT the time this
    process took the generation -- a pod that starts after the pipeline has
    already been dead for a day would otherwise call its day-old data fresh,
    which is the exact case worth catching.
    """
    try:
        return (root / CURRENT).stat().st_mtime
    except OSError:
        return None


def _stamp(store: Store, generation: int) -> None:
    """Record the generation in the live store, in its own graph."""
    store.update(
        "DELETE WHERE { GRAPH <%s> { <%s> <%s> ?o } }" % (META.value, STORE.value, GENERATION.value)
    )
    store.add(
        Quad(STORE, GENERATION, NamedNode(f"urn:sparqlwatch:generation:{generation}"), META)
    )


def _generation_in(path: Path) -> int | None:
    """The generation a snapshot claims, read back from the snapshot itself."""
    snap = Store.read_only(str(path))
    for row in snap.query(
        "SELECT ?o WHERE { GRAPH <%s> { <%s> <%s> ?o } }" % (META.value, STORE.value, GENERATION.value)
    ):
        return int(row["o"].value.rsplit(":", 1)[-1])
    return None


def publish(store: Store, root: Path, *, keep: int = 2, in_use: set[int] | None = None) -> int:
    """Checkpoint `store` as the next generation and point `CURRENT` at it.

    `store` MUST be open read-write; see the module docstring for the silent
    failure otherwise. Returns the generation published.

    The order is deliberate. A directory is not published by being named: a
    reader listing the root could otherwise open one mid-checkpoint, and
    `backup()` refuses an existing target so it cannot be written in place
    either. So the checkpoint goes to `.tmp`, is verified by reading the marker
    back out of it, and only then becomes `gen-N` and the content of `CURRENT`,
    each by `rename`, which is atomic within a filesystem.
    """
    root.mkdir(parents=True, exist_ok=True)
    generation = (max(_generations(root), default=0)) + 1
    _stamp(store, generation)
    # The flush is belt and braces: a read-write checkpoint flushes memtables on
    # its own. It costs nothing next to a load and removes a dependency on that
    # staying true.
    store.flush()

    tmp = root / f"gen-{generation:06d}.tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    store.backup(str(tmp))

    seen = _generation_in(tmp)
    if seen != generation:
        # The snapshot is short of writes made moments ago, which is the exact
        # shape of the read-only-backup bug. Destroy it and leave CURRENT alone:
        # an older generation that is whole beats a newer one that is missing a
        # run with nothing to say so.
        shutil.rmtree(tmp, ignore_errors=True)
        raise AssertionError(
            f"snapshot claims generation {seen}, expected {generation}; not published"
        )

    tmp.rename(path_of(root, generation))
    pointer = root / (CURRENT + ".tmp")
    pointer.write_text(f"gen-{generation:06d}\n")
    pointer.rename(root / CURRENT)

    reap(root, keep=keep, in_use=in_use)
    return generation


def reap(root: Path, *, keep: int = 2, in_use: set[int] | None = None) -> list[int]:
    """Delete generations nobody is reading. Returns what was deleted.

    NEVER the published one, never one a live reader reports, and never the
    newest `keep`. The floor matters because a reader that has just read
    `CURRENT` has not opened it yet, so "no reader reports it" is not the same
    as "no reader is about to".

    Deleting a directory a reader still holds does not free the space: an open
    RocksDB handle keeps unlinked files alive, so the only visible effect is
    that `du` starts lying. Watch `df`.
    """
    in_use = set(in_use or ())
    now = published(root)
    if now is not None:
        in_use.add(now)
    generations = _generations(root)
    protected = set(generations[-keep:]) | in_use
    gone = []
    for generation in generations:
        if generation in protected:
            continue
        shutil.rmtree(path_of(root, generation), ignore_errors=True)
        gone.append(generation)
    return gone


class Follower:
    """One read-only handle on the published generation, swapped when CURRENT moves.

    THIS IS A POLL, and nothing here pretends otherwise. What it buys over a TTL
    on freshness is not that it avoids a guess -- the interval is a guess -- but
    that each generation is internally consistent and named. A reader is never
    part-way through taking one, so a page is never assembled from two. Staleness
    is bounded by the interval and is reportable, which a TTL over a mutating
    store cannot offer.

    THE HANDOVER IS ONE ASSIGNMENT, and that is the whole concurrency design.
    `pinned()` reads a single tuple, so a request that has started holds its
    generation and its store together; rebinding `_pinned` cannot tear that pair
    apart or change what an in-flight request sees. The previous generation then
    survives exactly as long as some request still refers to it, and is released
    by refcounting the moment the last one returns -- which is what lets the
    reaper's unlinked files actually come back as free blocks. A cache that
    holds store handles defeats that, which is why `on_swap` exists: callers
    with such caches clear them there.
    """

    def __init__(self, root, *, open_store=None, prewarm=None, on_swap=None,
                 on_stale=None, stale_after: float = STALE_AFTER):
        self._root = Path(root)
        self._open = open_store or Store.read_only
        self._prewarm = prewarm
        self._on_swap = on_swap
        self._on_stale = on_stale
        self._stale_after = stale_after
        self._lock = threading.Lock()
        self._pinned: tuple[int, Store] | None = None
        self._thread: threading.Thread | None = None
        self._stop = threading.Event()
        self._said_at: float | None = None
        self.last_error: BaseException | None = None

        generation = published(self._root)
        if generation is None:
            raise RuntimeError(
                f"{self._root} holds no published generation. Opening a store "
                f"here would create an empty one, and the site would answer "
                f"every question as though no sweep had ever run. Load a run "
                f"with web/load_run.py --skip-loaded first."
            )
        # Deliberately NOT guarded the way `refresh` guards a later open: there
        # is no older generation to fall back to, so a failure here is fatal and
        # says so rather than coming back as an empty page.
        #
        # AND NOT WARMED. The warm exists so that a NEW generation is not served
        # cold while a warm one is still available; on the first open there is no
        # older generation and nothing is being served yet. Warming here would
        # also run inside whatever lock the caller built this under -- in the
        # site that is a 25-30s pass with every arriving request queued behind
        # it, which is the startup stall this is meant to prevent, not cause.
        self._take(generation, warm=False)

    @property
    def running(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    def pinned(self) -> tuple[int, Store]:
        """The generation being served and its handle, as one indivisible pair."""
        return self._pinned

    def _take(self, generation: int, *, warm: bool = True) -> None:
        store = self._open(str(path_of(self._root, generation)))
        # WARM BEFORE THE SWAP, never after: the first requests against a cold
        # handle pay the whole cost of building the page, and that is every hour
        # for whoever arrives first. While this runs, `pinned()` still returns
        # the previous generation and requests keep being served from it.
        if warm and self._prewarm is not None:
            self._prewarm(store)
        self._pinned = (generation, store)
        if self._on_swap is not None:
            self._on_swap(generation)

    def refresh(self) -> int | None:
        """Take the published generation if it has moved. Returns it, or None.

        Under the lock end to end, so two threads that notice the same new
        CURRENT do not both open it. The loser would not merely waste an open:
        its swap would land second and release a generation the winner's
        requests had already pinned.
        """
        with self._lock:
            generation = published(self._root)
            if generation is None or generation == self._pinned[0]:
                return None
            try:
                self._take(generation)
            except Exception as exc:  # noqa: BLE001 - the site keeps serving
                # A generation can be reaped between reading CURRENT and opening
                # it. Serving the previous one is the correct outcome; taking the
                # site down to report that a NEWER store exists is not.
                self.last_error = exc
                return None
            self.last_error = None
            return generation

    def published_age(self) -> float | None:
        """How long ago the generation being served was published, in seconds."""
        at = published_at(self._root)
        return None if at is None else time.time() - at

    def check_stale(self) -> float | None:
        """Report a store that has stopped being republished. Returns what it said.

        WHY THE FOLLOWER CARRIES THIS. The loader runs in its own job now, so a
        loader that cannot open the store is a failed job and a site that goes
        on serving the last generation it took -- correct data, indefinitely
        old, and nothing anywhere saying so. This process is the only one always
        running and already looking at CURRENT every poll, so the stall costs a
        `stat` to notice here and a cron-watcher to notice anywhere else.

        It never touches what is being served. Refusing to answer because the
        data is old would turn a stale site into no site, and a stale site is
        the better of those two by a wide margin.
        """
        if self._on_stale is None:
            return None
        age = self.published_age()
        if age is None:
            # Unreadable is a different fault from old, and the age that would
            # have to be invented to report it is the whole content of the
            # report. `refresh` already carries CURRENT going missing.
            return None
        if age < self._stale_after:
            self._said_at = None
            return None
        # `monotonic` for the repeat clock, wall time for the age: the age is
        # measured against a filesystem timestamp and has to be, while a clock
        # that steps backwards should not be able to silence the repeat.
        now = time.monotonic()
        if self._said_at is not None and now - self._said_at < _REPEAT_AFTER:
            return None
        self._said_at = now
        self._on_stale(self._pinned[0], age)
        return age

    def start(self, interval: float = 30.0) -> None:
        if self.running:
            return
        self._stop.clear()

        def poll():
            # `wait` rather than `sleep` so `stop()` returns promptly instead of
            # after up to one interval.
            while not self._stop.wait(interval):
                # NOTHING GETS OUT OF THIS LOOP. A poll thread that dies leaves a
                # site serving one generation for the rest of the process with no
                # swap and no complaint -- strictly worse than every failure it
                # could be dying of, including a reporting callback that throws.
                try:
                    self.refresh()
                    self.check_stale()
                except Exception as exc:  # noqa: BLE001 - see above
                    self.last_error = exc

        self._thread = threading.Thread(target=poll, name="snapshot-follower", daemon=True)
        self._thread.start()

    def stop(self, timeout: float = 5.0) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=timeout)
            self._thread = None

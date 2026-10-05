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

    def __init__(self, root, *, open_store=None, prewarm=None, on_swap=None):
        self._root = Path(root)
        self._open = open_store or Store.read_only
        self._prewarm = prewarm
        self._on_swap = on_swap
        self._lock = threading.Lock()
        self._pinned: tuple[int, Store] | None = None
        self._thread: threading.Thread | None = None
        self._stop = threading.Event()
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
        self._take(generation)

    @property
    def running(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    def pinned(self) -> tuple[int, Store]:
        """The generation being served and its handle, as one indivisible pair."""
        return self._pinned

    def _take(self, generation: int) -> None:
        store = self._open(str(path_of(self._root, generation)))
        # WARM BEFORE THE SWAP, never after: the first requests against a cold
        # handle pay the whole cost of building the page, and that is every hour
        # for whoever arrives first. While this runs, `pinned()` still returns
        # the previous generation and requests keep being served from it.
        if self._prewarm is not None:
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

    def start(self, interval: float = 30.0) -> None:
        if self.running:
            return
        self._stop.clear()

        def poll():
            # `wait` rather than `sleep` so `stop()` returns promptly instead of
            # after up to one interval.
            while not self._stop.wait(interval):
                self.refresh()

        self._thread = threading.Thread(target=poll, name="snapshot-follower", daemon=True)
        self._thread.start()

    def stop(self, timeout: float = 5.0) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=timeout)
            self._thread = None

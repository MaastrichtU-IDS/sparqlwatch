"""Which run files a store has already absorbed, so a restart is not a rebuild.

THE PROBLEM THIS SOLVES IS AN OUTAGE, not a slow script. The site publishes new
sweep data by restarting -- the store is opened read-only and cached per
process, so a restart is the only way new facts reach a reader -- and the
prober restarts it every hour. While the store lived on an emptyDir, every one
of those restarts replayed the whole archive from nothing: 294 files and 1.4M
quads, measured at ten minutes on 2026-09-25, during which the site served 503.
That is seventeen percent of every hour, and it grew by about fifty seconds a
day as the archive grew, so the arithmetic ended with a rebuild longer than the
hour between rebuilds and a site that was never up at all.

With the store on a volume that survives the pod, the replay is unnecessary for
every file the store already holds. This module is the record of which those
are.

A SIDECAR FILE, NOT TRIPLES IN THE STORE. "These bytes have been loaded" is a
fact about this deployment's filesystem, not about a SPARQL endpoint. Put in
the graph it would be answerable by the site's own queries, would show up in
the RDF this service publishes about itself, and would have to be excluded by
name from every query that scans graphs. It lives beside the store instead,
where it is exactly as durable as the store it describes and reachable by
nothing else.

KEYED BY CONTENT HASH, NOT BY FILENAME OR RUN IRI. This module sits directly on
top of load_run's central invariant: the same run IRI seen again with CHANGED
content must replace what is stored, never merge with it. A manifest keyed by
name would skip exactly the file that invariant exists to catch -- a re-run of
the same instant, which is the case load_run's docstring records as having
actually happened here. Hashing costs a read of a few megabytes across the
whole archive and removes the question.

WRITTEN AFTER EACH FILE, AND ATOMICALLY. A manifest that names a file the store
does not hold is worse than no manifest, because the missing run is then
permanently skipped and nothing reports it. So an entry is added only once its
file has loaded, and the write is a temp-file rename, which is atomic within a
directory: a crash leaves either the old manifest or the new one, never a
half-written one that parses as neither.
"""

from __future__ import annotations

import gzip
import hashlib
import json
import os
from datetime import datetime, timedelta, timezone
from pathlib import Path

# Beside the store directory rather than inside it. Inside, it would be a
# stray file in a RocksDB directory, where every other name belongs to RocksDB
# and a future version is entitled to sweep what it does not recognise.
MANIFEST_SUFFIX = ".loaded.json"

# HOW OFTEN THE STORE IS COMPACTED, and why it is not every restart.
#
# Nothing compacts an Oxigraph store on its own here: the init container exits
# the moment loading finishes, so RocksDB's background compaction never gets to
# run, and the store sits at its post-write size. Measured 2026-09-25: 1,292
# bytes per quad uncompacted against 317 compacted, a factor of four, which is
# the difference between filling 20Gi in five months and in seventeen.
#
# But compaction is not free and it happens on the RESTART path, which is
# downtime -- ~2.5s on a 35 MiB store, so ~30s on the deployed one. Paying that
# every hour costs twelve minutes of downtime a day to avoid a day's worth of
# slack, which is about 110 MB against a 20Gi volume. Once a day is the trade
# that takes the space win and leaves the downtime where it was.
OPTIMIZE_EVERY = timedelta(days=1)

# Bumped if the meaning of an entry ever changes. An unrecognised version is
# treated as no manifest at all, which costs one full rebuild and is always
# safe; guessing at an older layout is not.
VERSION = 1


def manifest_path(store_path: str | Path) -> Path:
    store = Path(store_path)
    return store.parent / (store.name + MANIFEST_SUFFIX)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# A gzip member starts with these two bytes. Sniffed rather than trusted to the
# filename, because the question being asked is "are these bytes compressed",
# and a `.nq` that is in fact gzipped should load rather than fail on a parse
# error thirty lines into a binary blob.
GZIP_MAGIC = b"\x1f\x8b"


def decompress(data: bytes) -> bytes:
    """The run's N-Quads, whether or not the file on disk was compressed.

    Run files are ~96% redundant -- 65% of a run is its subject and graph IRIs
    written out longhand, because N-Quads has no prefixes -- so they gzip to
    about 4%. The digest above is taken over the DECOMPRESSED bytes, so a run
    keeps its identity across being compressed: compressing the archive must
    not look like every run changing at once.
    """
    return gzip.decompress(data) if data[:2] == GZIP_MAGIC else data


def read(store_path: str | Path) -> dict[str, str]:
    """The recorded digests, or an empty mapping.

    Every failure answers "nothing is known to be loaded". A corrupt, truncated
    or unreadable manifest must not be interpreted optimistically: the cost of
    being wrong in that direction is a run silently missing from the site, and
    the cost of being wrong in this one is a single slow start.
    """
    path = manifest_path(store_path)
    try:
        raw = json.loads(path.read_text())
    except (OSError, ValueError):
        return {}
    if not isinstance(raw, dict) or raw.get("version") != VERSION:
        return {}
    files = raw.get("files")
    if not isinstance(files, dict):
        return {}
    return {k: v for k, v in files.items() if isinstance(k, str) and isinstance(v, str)}


def optimized_at(store_path: str | Path) -> datetime | None:
    """When the store was last compacted, as this manifest records it."""
    path = manifest_path(store_path)
    try:
        raw = json.loads(path.read_text())
        return datetime.fromisoformat(raw["optimized"])
    except (OSError, ValueError, KeyError, TypeError):
        return None


def due_for_optimize(store_path: str | Path, now: datetime | None = None) -> bool:
    """Whether a compaction is owed.

    An unreadable or missing timestamp means yes, for the same reason an
    unreadable manifest means "nothing is loaded": the cost of being wrong that
    way is one slow start, and the cost of the other way is a store that
    silently never compacts again.
    """
    last = optimized_at(store_path)
    if last is None:
        return True
    now = now or datetime.now(timezone.utc)
    if last.tzinfo is None:
        last = last.replace(tzinfo=timezone.utc)
    # A timestamp in the future is a clock that moved backwards, not a
    # compaction that has not happened yet. Treated as due, so a bad clock
    # cannot switch compaction off until it catches up.
    return not (timedelta(0) <= now - last < OPTIMIZE_EVERY)


def write(
    store_path: str | Path, files: dict[str, str], optimized: datetime | None = None
) -> None:
    """Replace the manifest atomically."""
    path = manifest_path(store_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    body: dict = {"version": VERSION, "files": files}
    # Carried forward when this write is not itself recording a compaction, so
    # an ordinary hourly load does not erase the clock and make every restart
    # think a compaction is owed.
    keep = optimized or optimized_at(store_path)
    if keep is not None:
        body["optimized"] = keep.isoformat()
    temp = path.with_name(path.name + ".tmp")
    temp.write_text(json.dumps(body, indent=1, sort_keys=True))
    # flush to disk before the rename, so a power loss cannot leave the rename
    # visible while the bytes behind it are not.
    with open(temp, "rb") as handle:
        os.fsync(handle.fileno())
    temp.replace(path)


def key_for(path: str | Path) -> str:
    """The manifest key for a run file: its name, without any `.gz`.

    COMPRESSING THE ARCHIVE MUST NOT LOOK LIKE NEW RUNS. `run-X.nq` becoming
    `run-X.nq.gz` is the same run in a smaller box, and a key that included the
    suffix would make the whole archive load again on the restart after it was
    compressed -- which is precisely the full rebuild this module exists to
    stop. The digest is taken over the decompressed bytes for the same reason;
    together they make compression invisible here.
    """
    name = Path(path).name
    return name[:-3] if name.endswith(".gz") else name


def partition(
    run_paths: list[str], contents: list[bytes], loaded: dict[str, str]
) -> tuple[list[int], list[int]]:
    """Split the run files into (skip, load) index lists.

    A file is skipped only when the manifest records THIS run with THESE
    contents. Bytes that have changed are loaded again, which is what makes a
    re-run of one instant reach the store instead of being silently dropped.
    """
    skip, load = [], []
    for i, (path, data) in enumerate(zip(run_paths, contents)):
        (skip if loaded.get(key_for(path)) == digest(data) else load).append(i)
    return skip, load

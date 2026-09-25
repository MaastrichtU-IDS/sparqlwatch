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

import hashlib
import json
import os
from pathlib import Path

# Beside the store directory rather than inside it. Inside, it would be a
# stray file in a RocksDB directory, where every other name belongs to RocksDB
# and a future version is entitled to sweep what it does not recognise.
MANIFEST_SUFFIX = ".loaded.json"

# Bumped if the meaning of an entry ever changes. An unrecognised version is
# treated as no manifest at all, which costs one full rebuild and is always
# safe; guessing at an older layout is not.
VERSION = 1


def manifest_path(store_path: str | Path) -> Path:
    store = Path(store_path)
    return store.parent / (store.name + MANIFEST_SUFFIX)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


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


def write(store_path: str | Path, files: dict[str, str]) -> None:
    """Replace the manifest atomically."""
    path = manifest_path(store_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + ".tmp")
    temp.write_text(json.dumps({"version": VERSION, "files": files}, indent=1, sort_keys=True))
    # flush to disk before the rename, so a power loss cannot leave the rename
    # visible while the bytes behind it are not.
    with open(temp, "rb") as handle:
        os.fsync(handle.fileno())
    temp.replace(path)


def partition(
    run_paths: list[str], contents: list[bytes], loaded: dict[str, str]
) -> tuple[list[int], list[int]]:
    """Split the run files into (skip, load) index lists.

    A file is skipped only when the manifest records THIS path with THIS
    file's digest. A path whose bytes have changed is loaded again, which is
    what makes a re-run of one instant reach the store instead of being
    silently dropped.
    """
    skip, load = [], []
    for i, (path, data) in enumerate(zip(run_paths, contents)):
        key = str(Path(path).name)
        (skip if loaded.get(key) == digest(data) else load).append(i)
    return skip, load

"""Worker processes that run SPARQL, so a bad query cannot take the site down.

WHY THIS EXISTS, in one measurement. `sparql_service.stream` enforces a
deadline and a row cap while pulling solutions, which works only because
Oxigraph yields lazily -- and a large class of query yields nothing at all
until it has finished. `ORDER BY` over the whole store, a three-way cross
join, a property path, a VALUES blow-up: each computes inside the engine
first. Measured on 2026-09-25, a three-way cross join and an ORDER BY over
every triple both ran past every per-row check until killed from outside. On a
single replica with one memory limit, that is the site going down.

So a query runs in another process, and a process can be killed.

RLIMIT_AS IS NOT USED, and that is deliberate rather than an omission. Capping
address space does stop a runaway allocation -- verified, a 2 GiB bytearray
raises MemoryError under a 512 MiB cap. But RocksDB creates threads and maps
files, and under the same cap it fails to create a thread, which C++ raises as
`std::system_error` and which calls `terminate`: the worker aborts instead of
answering, on ordinary queries, not just bombs. A limit that breaks the normal
path to bound the abnormal one is worse than no limit. Memory is bounded by
the row cap, by the worker being a separate process the kernel can choose as
its OOM victim ahead of the one serving pages, and by the container limit.

SPAWN, NOT FORK. The parent has the store open for rendering pages; a forked
child would inherit RocksDB's state and its background threads, which is
undefined at best. A spawned worker opens its own read-only handle and shares
nothing.

WORKERS PERSIST. Opening the store costs about half a second, which is
tolerable once per worker and not once per query.

THE POOL SIZE IS THE CONCURRENCY LIMIT. There is no queue: when every worker
is busy the endpoint says so immediately. Queuing would convert "a few
expensive queries" into "every later request waits behind them", which is the
outage this module exists to prevent, arriving more slowly.
"""

from __future__ import annotations

import multiprocessing as mp
import os
import threading
from dataclasses import dataclass

# How many queries may run at once. Each is a process holding its own store
# handle, so this is also how many cores a hostile caller can occupy.
DEFAULT_WORKERS = 2

# Wall clock per query, enforced by killing the worker. Lower than the
# in-process deadline in sparql_service, which only ever sees queries that
# stream; this is the one that stops the ones that do not.
DEFAULT_BUDGET_SECONDS = 15.0

WORKERS_VARIABLE = "SPARQLWATCH_SPARQL_WORKERS"
BUDGET_VARIABLE = "SPARQLWATCH_SPARQL_BUDGET_SECONDS"


def _worker(conn, store_path: str) -> None:
    """One worker: open the store once, then answer queries until told to stop.

    Imports happen here rather than at module scope because this runs in a
    fresh interpreter under spawn, and because the parent should not pay for
    pyoxigraph twice.
    """
    from pyoxigraph import Store

    import sparql_service
    from load_run import CURRENT_GRAPH

    store = Store.read_only(store_path)
    conn.send(("ready", None))
    while True:
        message = conn.recv()
        if message is None:
            return
        query, accept = message
        try:
            answer = sparql_service.execute(
                store, query, default_graph=CURRENT_GRAPH, accept=accept
            )
            conn.send(("ok", (answer.body, answer.media_type, answer.status, answer.complete)))
        except Exception as exc:  # noqa: BLE001 - the worker must not die on one query
            conn.send(("error", f"{type(exc).__name__}: {exc}"))


@dataclass
class _Worker:
    process: object
    conn: object
    busy: bool = False


class Pool:
    """A fixed set of query workers, replaced when one has to be killed."""

    def __init__(self, store_path: str, workers: int | None = None, budget: float | None = None):
        self.store_path = store_path
        self.size = workers if workers is not None else int(
            os.environ.get(WORKERS_VARIABLE, DEFAULT_WORKERS)
        )
        self.budget = budget if budget is not None else float(
            os.environ.get(BUDGET_VARIABLE, DEFAULT_BUDGET_SECONDS)
        )
        self._ctx = mp.get_context("spawn")
        self._lock = threading.Lock()
        self._workers: list[_Worker] = []

    # -- lifecycle ---------------------------------------------------------
    def _spawn(self) -> _Worker:
        parent, child = self._ctx.Pipe()
        process = self._ctx.Process(
            target=_worker, args=(child, self.store_path), daemon=True
        )
        process.start()
        child.close()
        # Wait for the store to open before the worker counts as available, so
        # the first query does not pay for it and then look slow.
        if parent.poll(60) and parent.recv()[0] == "ready":
            return _Worker(process, parent)
        process.kill()
        raise RuntimeError("a SPARQL worker did not start")

    def start(self) -> None:
        with self._lock:
            while len(self._workers) < self.size:
                self._workers.append(self._spawn())

    def stop(self) -> None:
        with self._lock:
            for worker in self._workers:
                try:
                    worker.conn.send(None)
                except (BrokenPipeError, OSError):
                    pass
                worker.process.join(timeout=2)
                if worker.process.is_alive():
                    worker.process.kill()
            self._workers = []

    # -- dispatch ----------------------------------------------------------
    def _claim(self) -> _Worker | None:
        with self._lock:
            for worker in self._workers:
                if not worker.busy:
                    worker.busy = True
                    return worker
        return None

    def _release(self, worker: _Worker, replace: bool) -> None:
        with self._lock:
            if not replace:
                worker.busy = False
                return
            # A killed worker cannot be reused: its pipe is dead and its store
            # handle went with it. It is dropped and a fresh one takes its
            # place, so the pool's capacity recovers rather than eroding with
            # every hostile query.
            if worker in self._workers:
                self._workers.remove(worker)
        try:
            self._workers.append(self._spawn())
        except RuntimeError:
            pass

    def execute(self, query: str, accept: str = "") -> tuple:
        """(body, media_type, status, complete). Never raises for a bad query."""
        worker = self._claim()
        if worker is None:
            return (
                b"too many queries are running; try again shortly\n",
                "text/plain; charset=utf-8",
                503,
                False,
            )
        try:
            worker.conn.send((query, accept))
            if not worker.conn.poll(self.budget):
                # THE POINT OF THIS MODULE. The query is still inside the
                # engine and no amount of asking will get it out.
                worker.process.kill()
                worker.process.join(timeout=5)
                self._release(worker, replace=True)
                return (
                    f"query exceeded {self.budget:g}s and was stopped\n".encode("utf-8"),
                    "text/plain; charset=utf-8",
                    504,
                    False,
                )
            kind, payload = worker.conn.recv()
        except (EOFError, BrokenPipeError, OSError):
            # The worker died on its own -- an OOM kill, most likely.
            self._release(worker, replace=True)
            return (
                b"the query could not be completed\n",
                "text/plain; charset=utf-8",
                500,
                False,
            )
        self._release(worker, replace=False)
        if kind == "ok":
            return payload
        return (
            b"the query could not be completed\n",
            "text/plain; charset=utf-8",
            500,
            False,
        )


class DirectPool:
    """The same interface, run in this process. For tests, and for a store
    handed over as an object rather than a path.

    THE GUARDS ARE NOT WEAKER HERE, because they are not reimplemented here:
    `Pool`'s worker calls exactly this `sparql_service.execute`, so every
    federation, size, deadline and row-cap test over a DirectPool is testing
    the same code a worker runs. What this does not provide is the isolation --
    a query that blocks inside the engine blocks the caller. That is precisely
    what `Pool` exists for and what `Pool`'s own tests cover, and it is why
    nothing should hand a DirectPool to the public route.
    """

    def __init__(self, store):
        self.store = store

    def start(self) -> None:
        return None

    def stop(self) -> None:
        return None

    def execute(self, query: str, accept: str = "") -> tuple:
        import sparql_service
        from load_run import CURRENT_GRAPH

        answer = sparql_service.execute(
            self.store, query, default_graph=CURRENT_GRAPH, accept=accept
        )
        return (answer.body, answer.media_type, answer.status, answer.complete)

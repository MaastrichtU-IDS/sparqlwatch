"""Loads a prober run into the Oxigraph store, replacing rather than merging.

A run is an N-Quads file in which every quad names a graph: the run IRI
(``urn:sparqlwatch:run:<timestamp>``). This module gets those quads into the
store the same way every time a run with that IRI is seen again: drop the
graphs the file names, then insert the file's quads, never merge into an
existing graph.

Why replace at all, since RDF is a set and loading the same content twice is
already a no-op: the hazard is the same run IRI with *changed* content. This
happened during this project's own development, when the same ``--at`` was
re-run several times while one endpoint's DNS flapped. Merging two versions
of the same run produces a graph where one measurement carries two
dqv:value triples (say, both "verified" and "indeterminate"), which is worse
than either answer alone, because a run graph is supposed to describe one
sweep's outcome, not the union of several. Replacing keeps that invariant:
a run graph, once loaded, always reflects the most recent file claiming
that run IRI, never a blend of two.

The order matters: parse the incoming bytes completely with pyoxigraph.parse
(which never touches the store), derive the graph names from the parsed
quads, and only then drop and insert. Dropping the named graphs before
parsing looks like the obvious implementation, and it is wrong: a truncated
or otherwise malformed .nq file (exactly what a crashed or interrupted
prober writes) would then destroy the existing run before the parse failure
is ever noticed, leaving the store with neither the old run nor the new one.
Parsing first means a malformed file is rejected before the store is
touched at all.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass, field
from pathlib import Path

from pyoxigraph import DefaultGraph, NamedNode, RdfFormat, Store, parse


@dataclass
class LoadResult:
    """What one load_run call did.

    ``replaced`` lists the graph IRIs (as strings) that already existed in
    the store and were dropped before this load's quads were inserted. An
    empty list means every graph this file names was new to the store, so a
    caller can tell the destructive case (something was overwritten) from
    the additive one.
    """

    replaced: list[str] = field(default_factory=list)
    quad_count: int = 0


def load_run(store: Store, nquads: bytes) -> LoadResult:
    """Load an N-Quads run into ``store``, replacing any graph it names.

    Raises ValueError if the input is not valid N-Quads, or if it does not
    name at least one graph (see the module docstring on ordering for why
    parsing happens before any store mutation).
    """
    # Parse everything before destroying anything: this list() call forces
    # the whole file to be read and validated. Only after it succeeds do we
    # know which graphs to drop, and only then do we drop them. Reordering
    # this so the store is touched first is the mutation this module exists
    # to prevent; see the module docstring.
    try:
        quads = list(parse(nquads, format=RdfFormat.N_QUADS))
    except SyntaxError as error:
        raise ValueError(f"not valid N-Quads: {error}") from error

    graph_names: set[NamedNode] = set()
    for quad in quads:
        if isinstance(quad.graph_name, DefaultGraph):
            # Every run this project emits is a named graph (the run IRI).
            # A file with default-graph triples is not a run, whatever else
            # it might be, so refuse it rather than silently absorbing those
            # triples into the store's default graph where no run graph
            # could ever be dropped to get rid of them again.
            raise ValueError(
                "input has default-graph triples; a sparqlwatch run is "
                "always a named graph, so this is not a run file"
            )
        graph_names.add(quad.graph_name)

    if not graph_names:
        raise ValueError("input names no graphs; nothing to load")

    replaced = sorted(
        graph.value for graph in graph_names if store.contains_named_graph(graph)
    )

    for graph in graph_names:
        store.remove_graph(graph)
    store.extend(quads)

    return LoadResult(replaced=replaced, quad_count=len(quads))


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) < 2:
        print("usage: load_run.py STORE_PATH RUN.nq [RUN.nq ...]", file=sys.stderr)
        return 2

    store = Store(args[0])
    for path in args[1:]:
        result = load_run(store, Path(path).read_bytes())
        if result.replaced:
            print(f"{path}: loaded {result.quad_count} quads, replaced {result.replaced}")
        else:
            print(f"{path}: loaded {result.quad_count} quads, no existing graph replaced")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

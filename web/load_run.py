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

That closes the loss driven by the input, and nothing else. The replacement
is NOT atomic. Dropping the graphs and inserting the quads are two separate
store operations, because pyoxigraph 0.5.9's Store has no transaction API at
all: it offers extend, bulk_extend and bulk_load, and nothing that groups
them into one unit of work. So anything that stops the insert from
completing (a full disk, an OOM kill, a power loss) leaves the graphs
dropped and the new quads not inserted. Read the paragraph above as exactly
what it says and no more: it is not a promise that the store can never be
left half-updated, because it can be.

Two things make that bounded window acceptable rather than a hole in the
design. First, the store is a derived artefact. The .nq files the prober
writes are the source of truth, and a run graph is immutable, so an
interrupted load is a re-loadable state rather than lost data: re-running
this loader with the same file restores the run exactly. Second, an
interruption must not be silent, so load_run counts what the store actually
holds for the graphs it has just written and raises if that is not the
number of quads parsed. An operator told "loaded 0 of 278" re-runs the load;
one left with a silently empty graph does not know there is anything to
re-run.
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


def _stored_count(store: Store, graph_names: set[NamedNode]) -> int:
    """How many quads ``store`` holds in ``graph_names`` right now."""
    return sum(
        len(list(store.quads_for_pattern(None, None, None, graph)))
        for graph in graph_names
    )


def _incomplete_load(stored: int, expected: int, graph_names: set[NamedNode]) -> str:
    return (
        f"loaded {stored} of {expected} quads into "
        f"{sorted(graph.value for graph in graph_names)}: the graphs were "
        "dropped and the insert did not complete, so the store now holds a "
        "partial run. The .nq file is the source of truth and a run graph is "
        "immutable, so re-running this load with the same file restores it "
        "exactly."
    )


def _parsed_graphs(nquads: bytes) -> tuple[list, set[NamedNode]]:
    """Parse ``nquads`` and return its quads and the named graphs it names.

    Raises ValueError under exactly the conditions load_run() documents: not
    valid N-Quads, default-graph triples present, or no named graph at all.
    Split out of load_run() so main() can run the same validation, below,
    against every input file before Store() is called on any of them: this
    is where a mistyped run path, an unreadable file, or a file that fails
    one of these checks gets discovered, and it must happen before the store
    exists at all.
    """
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

    return quads, graph_names


def load_run(store: Store, nquads: bytes) -> LoadResult:
    """Load an N-Quads run into ``store``, replacing any graph it names.

    Raises ValueError if the input is not valid N-Quads, or if it does not
    name at least one graph (see the module docstring on ordering for why
    parsing happens before any store mutation).

    Raises RuntimeError if, after the insert, the store does not hold every
    quad that was parsed. The replacement is not atomic (again, see the
    module docstring), so this is the load saying out loud that it left a
    partial run behind and must be re-run.
    """
    # Parse and validate everything before destroying anything. Only after
    # it succeeds do we know which graphs to drop, and only then do we drop
    # them. Reordering this so the store is touched first is the mutation
    # this module exists to prevent; see the module docstring.
    quads, graph_names = _parsed_graphs(nquads)

    replaced = sorted(
        graph.value for graph in graph_names if store.contains_named_graph(graph)
    )

    # What the store must hold afterwards. Counted over the distinct quads,
    # not len(quads): RDF is a set, so a file that repeats a line parses to
    # two Quad objects and stores as one.
    expected = len(set(quads))

    for graph in graph_names:
        store.remove_graph(graph)
    try:
        store.extend(quads)
    except Exception as error:
        # The drop has already happened, so an operator needs to be told what
        # is left rather than only what went wrong.
        stored = _stored_count(store, graph_names)
        if stored != expected:
            raise RuntimeError(
                _incomplete_load(stored, expected, graph_names)
            ) from error
        raise

    # An insert that neither completed nor raised (a killed process resumed
    # elsewhere, a store that accepted less than it was given) must not pass
    # for a successful load.
    stored = _stored_count(store, graph_names)
    if stored != expected:
        raise RuntimeError(_incomplete_load(stored, expected, graph_names))

    return LoadResult(replaced=replaced, quad_count=len(quads))


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) < 2:
        print("usage: load_run.py STORE_PATH RUN.nq [RUN.nq ...]", file=sys.stderr)
        return 2

    run_paths = args[1:]
    # Read and validate every run file before Store(args[0]) below, which is
    # what creates the store's on-disk directory if it does not exist yet.
    # A mistyped run path, an unreadable file, or one _parsed_graphs refuses
    # must be discovered here, before that directory exists, not after: this
    # module's own docstring explains why a truncated file must not touch an
    # existing run, and creating the store directory for a run that then
    # fails to load is the same mistake pointed at a store that never
    # existed before this invocation.
    contents = []
    for path in run_paths:
        data = Path(path).read_bytes()
        _parsed_graphs(data)
        contents.append(data)

    store = Store(args[0])
    for path, data in zip(run_paths, contents):
        result = load_run(store, data)
        if result.replaced:
            print(f"{path}: loaded {result.quad_count} quads, replaced {result.replaced}")
        else:
            print(f"{path}: loaded {result.quad_count} quads, no existing graph replaced")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

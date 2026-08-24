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

One tolerance sits on top of that, for the file a crashed prober actually
leaves. prober/src/emit.rs writes a run as a header, one self-contained
chunk per endpoint, and a footer, each section ending in a terminator quad:
sw:emission closes the header, sw:completedEndpoint closes a chunk,
sw:finalised closes the footer. A crash leaves a prefix of that sequence, so
the last section in the file may be a fragment. Refusing the whole file then
loses every endpoint that did finish, which is the case the incremental
write exists for; loading it whole publishes a fragment of a section as a
whole one. So the bytes are cut back to the end of the last terminator line
and everything after it is dropped. A file whose last statement already is a
terminator loads exactly as before, and that includes every complete run,
whose last line is the footer's sw:finalised and not a chunk marker.

Two things this tolerance is not. It does not accept a file with no
terminator anywhere and a parse error: there is nothing to cut back to, so
that file is refused with its parse error rather than loaded as zero quads
and then rediagnosed as naming no graphs. And it does not rescue a corrupt
file: a syntax error before the last terminator is still in the bytes after
the cut, so such a file is refused whole, exactly as it was.

What it cannot tell apart, and does tolerate: a hand-edited file whose last
chunk was corrupted on purpose looks exactly like one a crash truncated, and
its last chunk is dropped rather than the file refused. Nothing in the bytes
distinguishes the two, and dropping one chunk of a file nobody should have
edited is the cheaper error than refusing every run a real crash produces.
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

    ``discarded_bytes`` is how many trailing bytes were cut off as an
    incomplete final section (see the module docstring). Zero for every
    complete run and for every file that needed no tolerance. A field rather
    than a warning because this module has no logging: main() prints it from
    here, beside the quad count.
    """

    replaced: list[str] = field(default_factory=list)
    quad_count: int = 0
    discarded_bytes: int = 0


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


# The three predicates that close a section, read off prober/src/emit.rs:
# sw:emission is the last quad emit_header writes, sw:completedEndpoint the
# last quad emit_endpoint writes, sw:finalised the last quad emit_footer
# writes. All three are needed. A complete run ends at sw:finalised, a run
# killed between endpoints ends at sw:completedEndpoint, and a run killed
# before its first endpoint ends at sw:emission, so a set missing any one of
# them would truncate away a section that was written whole. Leaving out
# sw:finalised is the worst of the three: every complete run would lose its
# footer, and a reader testing for the footer would then report that no sweep
# this project publishes ever finished.
#
# These spellings are a wire format shared with the emitter (see emit.rs's
# module docstring), so neither side may change them alone.
_TERMINATOR_PREDICATES = frozenset(
    {
        "urn:sparqlwatch:emission",
        "urn:sparqlwatch:completedEndpoint",
        "urn:sparqlwatch:finalised",
    }
)
_TERMINATOR_TERMS = frozenset(
    f"<{predicate}>".encode() for predicate in _TERMINATOR_PREDICATES
)
# Every terminator's subject is the run's activity, which emit.rs builds as
# urn:sparqlwatch:activity:<run>. A second anchor beside the predicate.
_ACTIVITY_PREFIX = b"<urn:sparqlwatch:activity:"


def _try_parse(nquads: bytes) -> tuple[list | None, SyntaxError | None]:
    """The quads ``nquads`` holds, or the SyntaxError it raised. Never both."""
    try:
        return list(parse(nquads, format=RdfFormat.N_QUADS)), None
    except SyntaxError as error:
        return None, error


def _is_terminator_line(line: bytes) -> bool:
    """Whether ``line`` is one statement whose predicate closes a section.

    Matched in predicate position, not by searching the line for the marker
    spelling. Sampled class IRIs come from strangers' endpoints, so a
    sw:sampledValue line may legitimately carry an object IRI spelled
    ``urn:sparqlwatch:completedEndpoint``, and a byte search would read that
    line as a chunk boundary and cut in the middle of a chunk. An IRI cannot
    contain a space (N-Quads forbids #x00-#x20 inside IRIREF), so splitting on
    whitespace puts the subject in the first field and the predicate in the
    second whatever the object turns out to be, and a class IRI can only ever
    reach the third.
    """
    terms = line.split(None, 2)
    return (
        len(terms) == 3
        and terms[0].startswith(_ACTIVITY_PREFIX)
        and terms[0].endswith(b">")
        and terms[1] in _TERMINATOR_TERMS
    )


def _ends_at_terminator(quads: list) -> bool:
    """Whether the last statement parsed is a section terminator.

    Read off the parsed quad rather than the bytes, so the predicate position
    is structural here and needs no anchoring.
    """
    return bool(quads) and quads[-1].predicate.value in _TERMINATOR_PREDICATES


def _last_terminator_end(nquads: bytes) -> int | None:
    """Where the last terminator line in ``nquads`` ends, or None if there is
    no terminator line at all.

    Only newline-terminated lines are considered, which loses nothing: a file
    whose final line is a terminator with no trailing newline parses, and its
    last statement is that terminator, so it never reaches this function.
    """
    line_end = len(nquads)
    while True:
        newline = nquads.rfind(b"\n", 0, line_end)
        if newline == -1:
            return None
        start = nquads.rfind(b"\n", 0, newline) + 1
        if _is_terminator_line(nquads[start:newline]):
            return newline + 1
        line_end = newline


def _quads_to_the_last_terminator(nquads: bytes) -> tuple[list, int]:
    """``nquads``' quads up to the end of its last complete section, and how
    many trailing bytes that dropped.

    See the module docstring for why the cut exists and what it deliberately
    cannot tell apart.
    """
    quads, error = _try_parse(nquads)
    if quads is not None and _ends_at_terminator(quads):
        # The whole file is whole sections, complete runs included. Nothing to
        # cut, and no behaviour change for any file that was already loadable.
        return quads, 0

    cut = _last_terminator_end(nquads)
    if cut is None:
        if error is not None:
            # No terminator to fall back to, so there is no prefix that is
            # known to be whole sections. Refusing with the parse error keeps
            # the diagnosis the file's own: cutting to nothing would report a
            # zero-quad load and then fail as "names no graphs", which is true
            # of the empty prefix and says nothing about the real fault.
            raise ValueError(f"not valid N-Quads: {error}")
        # Parses, and carries no terminator anywhere: a run written before
        # this format existed (every captured fixture in tests/fixtures is
        # one). It promised nothing about sections, so there is nothing to
        # truncate and it loads whole.
        return quads, 0

    kept, cut_error = _try_parse(nquads[:cut])
    if kept is None:
        # A prefix ending at a line boundary parses whenever the whole file
        # does, so reaching here means ``error`` is set: the corruption is
        # before the last terminator and the cut does not remove it. That is a
        # corrupt file rather than a writer that stopped, and it is refused
        # whole exactly as before.
        raise ValueError(f"not valid N-Quads: {error or cut_error}")
    return kept, len(nquads) - cut


def _parsed_graphs(nquads: bytes) -> tuple[list, set[NamedNode], int]:
    """Parse ``nquads`` and return its quads, the named graphs it names, and
    how many trailing bytes were dropped as an incomplete final section.

    Raises ValueError under exactly the conditions load_run() documents: not
    valid N-Quads, default-graph triples present, or no named graph at all.
    Split out of load_run() so main() can run the same validation, below,
    against every input file before Store() is called on any of them: this
    is where a mistyped run path, an unreadable file, or a file that fails
    one of these checks gets discovered, and it must happen before the store
    exists at all.

    The incomplete-section tolerance lives here and not in load_run() for the
    same reason: main() validates every input with this function before
    Store() is constructed, so a tolerance in load_run() would never be
    reached from the command line, and the crashed-prober file this stage
    exists for would still be refused before the store was even opened.
    """
    quads, discarded = _quads_to_the_last_terminator(nquads)

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

    return quads, graph_names, discarded


def load_run(store: Store, nquads: bytes) -> LoadResult:
    """Load an N-Quads run into ``store``, replacing any graph it names.

    Raises ValueError if the input is not valid N-Quads, or if it does not
    name at least one graph (see the module docstring on ordering for why
    parsing happens before any store mutation).

    An incomplete final section is cut off first and counted in
    LoadResult.discarded_bytes, so a file a crash truncated loads as far as
    its last whole section (see the module docstring for the rule, and
    _parsed_graphs for why the cut lives there).

    Raises RuntimeError if, after the insert, the store does not hold every
    quad that was parsed. The replacement is not atomic (again, see the
    module docstring), so this is the load saying out loud that it left a
    partial run behind and must be re-run.
    """
    # Parse and validate everything before destroying anything. Only after
    # it succeeds do we know which graphs to drop, and only then do we drop
    # them. Reordering this so the store is touched first is the mutation
    # this module exists to prevent; see the module docstring.
    quads, graph_names, discarded = _parsed_graphs(nquads)

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

    return LoadResult(
        replaced=replaced, quad_count=len(quads), discarded_bytes=discarded
    )


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
        # Reported from the result of the load, not from the validation pass
        # above, which parses every file a second time: one file, one line
        # about what it discarded.
        dropped = (
            f", discarded {result.discarded_bytes} trailing bytes "
            "(an incomplete final section, so the run did not finish)"
            if result.discarded_bytes
            else ""
        )
        if result.replaced:
            print(
                f"{path}: loaded {result.quad_count} quads, "
                f"replaced {result.replaced}{dropped}"
            )
        else:
            print(
                f"{path}: loaded {result.quad_count} quads, "
                f"no existing graph replaced{dropped}"
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

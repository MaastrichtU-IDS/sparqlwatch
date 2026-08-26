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
store operations, so anything that stops the insert from completing (a full
disk, an OOM kill, a power loss) leaves the graphs dropped and the new quads
not inserted. Read the paragraph above as exactly what it says and no more: it
is not a promise that the store can never be left half-updated, because it can
be.

An earlier version of this paragraph said the reason was that pyoxigraph 0.5.9
has no transaction API at all. That was false, and it shaped this project's
design from stage 2-1 onward. Store.update's own documentation says "Updates
are applied in a transactional manner: either the full operation succeeds, or
nothing is written to the database", and that holds across ';'-separated
operations: running "DROP GRAPH <urn:g> ; DROP GRAPH <urn:missing>" raises on
the second operation and leaves urn:g in place, so the first was rolled back.
What pyoxigraph does not offer is an explicit transaction HANDLE, something
that would let this module group its own remove_graph and extend calls. The
real objection to writing the run-graph replacement as one update is the size
of the INSERT DATA body it would need: a run of the 543-endpoint registry is
27,194 quads, and serialising them into SPARQL text to be re-parsed is a
different cost from handing parsed Quad objects to extend. So the window above
stays, and is reported rather than hidden.

The derived graph this module also maintains DOES use that transactionality,
one update per endpoint. See "The derived urn:sparqlwatch:current graph" below.

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
leaves. prober/src/emit.rs writes a run as a header, the endpoints the sweep
declined to ask, one self-contained chunk per endpoint, and a footer, each
section ending in a terminator quad: sw:emission closes the header,
sw:dormantCount closes the dormancy section, sw:completedEndpoint closes a
chunk, sw:finalised closes the footer. A crash leaves a prefix of that
sequence, so the last section in the file may be a fragment. Refusing the whole file then
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

A file that parses and carries no terminator anywhere makes no claim about
sections either way, and the bytes cannot say why. That is what a run from
before this format looks like, and it is also what a run of this format cut
inside its HEADER looks like, since sw:emission is the header's last quad. So
provenance is not asserted, and the two are separated by a fact the file does
carry instead: a run from before this format still measured endpoints, and a
header holds only the activity's own metadata. A file with no terminator and
no endpoint fact at all is therefore refused, the same way an empty file is,
because loading it would put the store's greatest prov:generatedAtTime on a
graph that says nothing about finishing, which wins the newest-run aggregate
in both read queries and silences the unfinished-run detection for every
endpoint on the site. A file with no terminator and endpoint facts in it
loads whole, exactly as before.

What it cannot tell apart, and does tolerate: a hand-edited file whose last
chunk was corrupted on purpose looks exactly like one a crash truncated, and
its last chunk is dropped rather than the file refused. Nothing in the bytes
distinguishes the two, and dropping one chunk of a file nobody should have
edited is the cheaper error than refusing every run a real crash produces.

THE DERIVED urn:sparqlwatch:current GRAPH
=========================================

Beside the run graphs, this module maintains one more named graph, and all
three read queries read it instead of deciding recency themselves. The reason
is measured. endpoint_measurements.rq deciding "the newest run that recorded
anything for this endpoint" at query time cost 10.0 ms over one run graph of
the 543-endpoint registry, 309.7 ms over seven and 5,801.8 ms over thirty, so
one month of daily sweeps made the endpoint page a 5.8 second load.
endpoint_content.rq is 6,488.5 ms and endpoint_description.rq, the whole RDF
representation, is 11,704.3 ms at thirty runs once there is one content sample
per endpoint per run. A flat scan of this graph over the same 30-run store is
3.1 ms for all 3,801 rows.

THE QUAD SHAPE, so a reader does not have to infer it. For each endpoint E
whose facts current holds, in graph urn:sparqlwatch:current:

  E sw:currentRun <run>          the newest run that recorded a measurement,
                                 a decline or a sw:declarationsRead for E
  E sw:currentSampleRun <run>    the newest run that published a
                                 sw:metric:classes sample of E
  E sw:declarationsRead <bool>   copied verbatim from sw:currentRun's graph
  <measurement> ?p ?o            every quad of every dqv:QualityMeasurement
                                 whose dqv:computedOn is E, copied verbatim
                                 from sw:currentRun's graph
  <notMeasured> ?p ?o            every quad of every sw:NotMeasured whose
                                 sw:notMeasuredOn is E, copied verbatim

and NOTHING ELSE. In particular:

  - NO rdf:type prov:Activity and NO prov:generatedAtTime, ever. All three read
    queries select their run as GRAPH ?run { ?a a prov:Activity ;
    prov:generatedAtTime ?t } with no restriction on which graph, and the
    newest-run aggregate is unrestricted too. A current graph holding a typed
    activity with a timestamp therefore IS a run to every one of them, and the
    newest run by construction, so both readers raise "runs tied as most
    recent" and every page and every RDF representation becomes a 500. Measured
    on a committed fixture.
  - NO run-level fact. sw:emission, sw:finalised and sw:completedEndpoint are
    properties of a RUN, and the readers reach them through the pointer, in one
    hop into the named run's graph. Copying them onto the endpoint would put
    triples in current that no run graph holds, which breaks the rule that
    current is reconstructible from the run graphs alone, and it would force
    endpoint_description.rq to publish <endpoint> sw:finalised true, a wrong
    fact because finishing is something a run does.
  - NO sample quads. The class sample stays in its run graph and is read through
    sw:currentSampleRun, because the index reads current for verdicts and never
    for sample values, so copying several hundred sw:sampledValue triples per
    endpoint would grow the graph the index scans and buy nothing.

NOT AN INPUT. A run file naming urn:sparqlwatch:current as its graph is
refused, in _parsed_graphs, before any store is opened. Everything above says
this graph is derived from the run graphs and reconstructible from them alone,
and one hand-written line naming it used to wipe it, insert that line's own
triples into it, and return a LoadResult with drifted=[]: the one detector for
a broken current graph asks which pointers name a run that no longer states
their facts, and an emptied graph holds no pointers to ask about.

TWO POINTERS, which is the subtle half. The newest run that MEASURED an
endpoint and the newest run that SAMPLED it are different runs the moment a
cheap sweep declines sw:metric:classes, and that is the steady state: the
543-endpoint registry sweep declined classes for every one of them. One pointer
with one notion of recency loses the class sample outright.

THE UPDATE RULE. For every endpoint the incoming run mentions, what current
holds for that endpoint is replaced, in one store.update() so the endpoint is
never half-updated. The advance is refused only when the run current already
points at is STRICTLY newer, so re-loading the same run IRI does refresh, which
is the documented recovery, while an out-of-order older run is still refused. A
TIE between two different runs is refused rather than resolved by load order,
naming both runs, because both readers refuse such a store and deciding it here
would be an unannounced behaviour change.

THREE CASES THE RULE CANNOT FIX, and they are detected rather than left to a
reader. A run graph that SHRINKS (the full sweep, then the truncated file a
crashed prober leaves under the same run IRI) and a run graph DROPPED wholesale
both leave current attributing facts to a run that no longer states them; both
are found by noticing an endpoint whose pointer names a run whose graph no
longer mentions it, reported in LoadResult.drifted, and repaired by
rebuild_current. The FINISHED/UNFINISHED flip needs nothing: the run-level
facts are reached through the pointer, so a footer arriving changes what the
page says without a quad of current moving.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass, field
from datetime import datetime
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
    advanced: list[str] = field(default_factory=list)
    advanced_samples: list[str] = field(default_factory=list)
    kept_newer: list[str] = field(default_factory=list)
    drifted: list[str] = field(default_factory=list)


# ---------------------------------------------------------------------------
# The derived urn:sparqlwatch:current graph
# ---------------------------------------------------------------------------
CURRENT_GRAPH_IRI = "urn:sparqlwatch:current"
CURRENT_GRAPH = NamedNode(CURRENT_GRAPH_IRI)

# The predicates the three read queries and this module share. Spelled once,
# because a rename on one side alone would leave the readers looking for a
# pointer nothing writes and every page answering "we know nothing about this
# endpoint".
CURRENT_RUN = "urn:sparqlwatch:currentRun"
CURRENT_SAMPLE_RUN = "urn:sparqlwatch:currentSampleRun"

# Every update and query below is parameterised through pyoxigraph's
# ``prefixes`` argument rather than by formatting an IRI into the text.
# Store.query() also takes ``substitutions`` (SEP-0007) and the three .rq files
# use it, but Store.update() in pyoxigraph 0.5.9 does NOT: its signature is
# (update, base_iri, prefixes, custom_functions, custom_aggregate_functions),
# and passing substitutions= raises TypeError. So the endpoint and the run
# reach an update as prefix declarations, used as ``endpoint:`` and ``run:``
# with an empty local name. pyoxigraph validates a prefix IRI before parsing
# (a value holding '>' is refused as "Invalid prefix IRI ... Invalid IRI code
# point"), so the value cannot escape into the query text the way an
# interpolated string could.
_ENDPOINT_PREFIX = "endpoint"
_RUN_PREFIX = "run"

_PREAMBLE = """
PREFIX dqv: <http://www.w3.org/ns/dqv#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX sw: <urn:sparqlwatch:>
"""

# The instant a run's activity carries. MAX rather than a plain binding so a
# graph is one row whatever it holds, and the caller decides what an absent
# instant means.
_RUN_INSTANT = _PREAMBLE + """
SELECT (MAX(?instant) AS ?newest) WHERE {
  GRAPH run: { ?activity a prov:Activity ; prov:generatedAtTime ?instant }
}
"""

# Which endpoints a run recorded a measurement, a decline or a declarations
# fact for. This is the set sw:currentRun governs, and it is deliberately the
# same three shapes endpoint_measurements.rq reads, plus sw:declarationsRead so
# that fact is never left in current without a pointer to the run that stated
# it.
_MEASURED_ENDPOINTS = _PREAMBLE + """
SELECT DISTINCT ?endpoint WHERE {
  GRAPH run: {
    { ?thing dqv:computedOn ?endpoint }
    UNION
    { ?thing sw:notMeasuredOn ?endpoint }
    UNION
    { ?endpoint sw:declarationsRead ?read }
  }
}
"""

# Which endpoints a run published a CLASS sample for. Pinned to
# sw:metric:classes because endpoint_content.rq is: a sample from another
# metric is not this endpoint's classes, and web/tests/fixtures/
# run-properties-sample.nq exists to prove that pin holds.
_SAMPLED_ENDPOINTS = _PREAMBLE + """
SELECT DISTINCT ?endpoint WHERE {
  GRAPH run: {
    ?sample sw:sampledFrom ?endpoint ; sw:sampledBy sw:metric:classes .
  }
}
"""

# What current currently points each endpoint at, and when that run ran. One
# query for the whole graph rather than one per endpoint: at 543 endpoints the
# per-endpoint form is 543 round trips to answer a question one scan answers.
# ?instant is OPTIONAL so a pointer naming a graph that has been dropped comes
# back with the pointer and no instant, which is how the caller tells "older"
# from "gone".
_POINTERS = _PREAMBLE + """
SELECT ?endpoint ?which ?run ?instant WHERE {
  GRAPH sw:current { ?endpoint ?which ?run }
  FILTER (?which = sw:currentRun || ?which = sw:currentSampleRun)
  OPTIONAL {
    GRAPH ?run { ?activity a prov:Activity ; prov:generatedAtTime ?instant }
  }
}
"""

# An endpoint whose pointer names a run whose graph no longer mentions it.
# Two things produce it and neither is an ordering mistake the advance rule
# could fix: a run graph that shrank (the full sweep, then the truncated file a
# crashed prober left under the same run IRI) and a run graph dropped
# wholesale, which the spec calls a feature of one graph per run.
_DRIFTED_RUN_POINTERS = _PREAMBLE + """
SELECT ?endpoint WHERE {
  GRAPH sw:current { ?endpoint sw:currentRun ?run }
  FILTER NOT EXISTS {
    GRAPH ?run {
      { ?thing dqv:computedOn ?endpoint }
      UNION
      { ?thing sw:notMeasuredOn ?endpoint }
      UNION
      { ?endpoint sw:declarationsRead ?read }
    }
  }
}
"""

_DRIFTED_SAMPLE_POINTERS = _PREAMBLE + """
SELECT ?endpoint WHERE {
  GRAPH sw:current { ?endpoint sw:currentSampleRun ?run }
  FILTER NOT EXISTS {
    GRAPH ?run {
      ?sample sw:sampledFrom ?endpoint ; sw:sampledBy sw:metric:classes .
    }
  }
}
"""

# One endpoint's measurement, decline and declarations facts, replaced. Both
# halves of the delete are needed and they cover different mistakes. Deleting
# by endpoint clears what current holds ABOUT this endpoint. Deleting by the
# incoming run's subjects clears any earlier use of those same subject IRIs,
# which matters for a run written in the row-index subject scheme (stage
# 1c-b3 replaced it with a scheme deriving the subject from the run, the
# endpoint and the metric, so nothing the current prober writes can reuse a
# subject for a different endpoint, but captured historical runs can). Without
# it, a subject that was endpoint Y's in an older run and is endpoint X's in
# this one would end up in current carrying both dqv:computedOn triples, which
# is the self-contradicting graph load_run exists to prevent, one level up.
#
# The whole block is ONE store.update() call. Store.update is documented as
# transactional ("either the full operation succeeds, or nothing is written")
# and that holds across ';'-separated operations: verified by running
# "DROP GRAPH <urn:g> ; DROP GRAPH <urn:missing>", which raises on the second
# operation and leaves urn:g in place. So an endpoint is never half-updated:
# never cleared without being rewritten, never two runs' facts at once.
# Written without its own PREFIX prologue, because pyoxigraph 0.5.9 accepts a
# prologue only before the FIRST operation of an update: "PREFIX a: <...>
# INSERT DATA {...} ; PREFIX b: <...> INSERT DATA {...}" is refused with
# "expected one of CREATE, DELETE, INSERT". So the two units below are bodies
# and _update_text prepends one prologue for however many are combined.
_REPLACE_MEASURED = """
DELETE { GRAPH sw:current { ?thing ?p ?o } }
WHERE  { GRAPH sw:current { ?thing dqv:computedOn endpoint: . ?thing ?p ?o } } ;
DELETE { GRAPH sw:current { ?thing ?p ?o } }
WHERE  { GRAPH sw:current { ?thing sw:notMeasuredOn endpoint: . ?thing ?p ?o } } ;
DELETE { GRAPH sw:current { ?thing ?p ?o } }
WHERE  {
  GRAPH run: { ?thing dqv:computedOn endpoint: }
  GRAPH sw:current { ?thing ?p ?o }
} ;
DELETE { GRAPH sw:current { ?thing ?p ?o } }
WHERE  {
  GRAPH run: { ?thing sw:notMeasuredOn endpoint: }
  GRAPH sw:current { ?thing ?p ?o }
} ;
DELETE WHERE { GRAPH sw:current { endpoint: sw:declarationsRead ?read } } ;
DELETE WHERE { GRAPH sw:current { endpoint: sw:currentRun ?run } } ;
INSERT { GRAPH sw:current { ?thing ?p ?o } }
WHERE  { GRAPH run: { ?thing dqv:computedOn endpoint: . ?thing ?p ?o } } ;
INSERT { GRAPH sw:current { ?thing ?p ?o } }
WHERE  { GRAPH run: { ?thing sw:notMeasuredOn endpoint: . ?thing ?p ?o } } ;
INSERT { GRAPH sw:current { endpoint: sw:declarationsRead ?read } }
WHERE  { GRAPH run: { endpoint: sw:declarationsRead ?read } } ;
INSERT DATA { GRAPH sw:current { endpoint: sw:currentRun run: } }
"""

# One endpoint's class-sample pointer, replaced. The sample's own quads stay in
# their run graph and are read through this pointer, because the index this
# stage builds scans current for verdicts and never for sample values, so
# copying several hundred sw:sampledValue triples per endpoint into current
# would buy nothing and would grow the graph the index scans.
_REPLACE_SAMPLED = """
DELETE WHERE { GRAPH sw:current { endpoint: sw:currentSampleRun ?run } } ;
INSERT DATA { GRAPH sw:current { endpoint: sw:currentSampleRun run: } }
"""

# What one graph says about one endpoint, in the exact shape the loader copies:
# the quads of every measurement and every not-measured resource computed on
# it, and its sw:declarationsRead fact. ONE template, read against current with
# graph: bound to urn:sparqlwatch:current and against a run graph with graph:
# bound to the run, so the check cannot compare two different shapes and call
# the difference drift.
_FACTS_FOR_ENDPOINT = _PREAMBLE + """
SELECT ?thing ?p ?o WHERE {
  GRAPH graph: {
    {
      { ?thing dqv:computedOn endpoint: }
      UNION
      { ?thing sw:notMeasuredOn endpoint: }
      ?thing ?p ?o
    }
    UNION
    {
      endpoint: sw:declarationsRead ?o
      BIND (endpoint: AS ?thing)
      BIND (sw:declarationsRead AS ?p)
    }
  }
}
"""


def _endpoint_run(endpoint: str, run: str) -> dict[str, str]:
    return {_ENDPOINT_PREFIX: endpoint, _RUN_PREFIX: run}


def _update_text(*bodies: str) -> str:
    """One update out of one or more unit bodies, with a single prologue."""
    return _PREAMBLE + " ;\n".join(bodies)


def _instant(store: Store, run: str) -> str | None:
    """The prov:generatedAtTime of ``run``'s activity, or None if it has none.

    None means the graph is gone or holds no activity, which is not the same
    thing as an older run and must not be compared as if it were.
    """
    for row in store.query(_RUN_INSTANT, prefixes={_RUN_PREFIX: run}):
        return None if row["newest"] is None else row["newest"].value
    return None


def _as_datetime(instant: str) -> datetime:
    """``instant`` as a datetime, for comparing two runs' recency.

    Not a string comparison. endpoint_content.rq's header argues at length
    that recency must be decided by the typed xsd:dateTime the data models and
    not by ordering the run IRI as a string, because today's run IRIs happen to
    embed an ISO-8601 instant so the two agree by coincidence. The same
    argument applies here, so the lexical form is parsed rather than compared.
    A form this cannot parse raises rather than silently mis-ordering two runs.
    """
    try:
        return datetime.fromisoformat(instant)
    except ValueError as error:
        raise ValueError(
            f"{instant!r} is not a parseable xsd:dateTime, so this run's "
            f"recency cannot be compared: {error}"
        ) from error


def _run_endpoints(store: Store, run: str, query: str) -> set[str]:
    return {
        row["endpoint"].value
        for row in store.query(query, prefixes={_RUN_PREFIX: run})
    }


def _pointers(store: Store) -> dict[tuple[str, str], tuple[str, str | None]]:
    """What current points every endpoint at: (endpoint, pointer) -> (run, instant).

    ``instant`` is None where the named run's graph holds no activity, which is
    what a dropped run graph looks like from here.
    """
    found: dict[tuple[str, str], tuple[str, str | None]] = {}
    for row in store.query(_POINTERS):
        key = (row["endpoint"].value, row["which"].value)
        instant = None if row["instant"] is None else row["instant"].value
        found[key] = (row["run"].value, instant)
    return found


def _advance(
    existing: tuple[str, str | None] | None, run: str, instant: str
) -> bool:
    """Whether ``run`` at ``instant`` may replace what ``existing`` names.

    Five cases, and the wording of the rule is what keeps the monotone trap
    shut. There is no existing pointer, so advance. The pointer already names
    THIS run, so advance: re-loading a run is the documented recovery from a
    current graph left wrong, and a rule that refused a tie would make the
    recovery the one operation that cannot repair anything. The pointer names a
    run with no instant, so it is gone or says nothing and cannot be compared;
    advance, and the drift report says so. The pointer's run is strictly newer,
    so keep it: an out-of-order older run must not move current backwards. And
    a tie between two DIFFERENT runs is not resolved here at all; see
    _tie_message.
    """
    if existing is None:
        return True
    existing_run, existing_instant = existing
    if existing_run == run:
        return True
    if existing_instant is None:
        return True
    return _as_datetime(existing_instant) < _as_datetime(instant)


def _tied(existing: tuple[str, str | None] | None, run: str, instant: str) -> bool:
    """Whether ``existing`` and ``run`` are two different runs at one instant."""
    if existing is None:
        return False
    existing_run, existing_instant = existing
    if existing_run == run or existing_instant is None:
        return False
    return _as_datetime(existing_instant) == _as_datetime(instant)


def _tie_message(endpoint: str, first: str, second: str, instant: str) -> str:
    return (
        f"{endpoint} has 2 runs tied as most recent ({sorted([first, second])}) "
        f"at {instant}: two run graphs share a prov:generatedAtTime, so which "
        f"of them urn:sparqlwatch:current should point at has no answer. Both "
        f"read paths refuse such a store rather than blending two sweeps under "
        f"one run's name, and advancing by load order would decide it silently. "
        f"Drop one of the two run graphs, then rebuild with "
        f"'python web/load_run.py --rebuild STORE_PATH'."
    )


def _drifted(store: Store) -> list[str]:
    """Endpoints whose pointer names a run whose graph no longer mentions them."""
    drifted = set()
    for query in (_DRIFTED_RUN_POINTERS, _DRIFTED_SAMPLE_POINTERS):
        drifted |= {row["endpoint"].value for row in store.query(query)}
    return sorted(drifted)


def _drift_advice(drifted: list[str], path: str) -> str:
    """What an operator is told when a load leaves current attributing facts to
    a run that no longer states them.

    ``path`` is the store, so the command in the last sentence can be run as
    printed rather than after a substitution.
    """
    return (
        f"urn:sparqlwatch:current attributes facts to a run that no longer "
        f"states them, for {len(drifted)} endpoint(s): {drifted}. A run graph "
        f"has shrunk or been dropped since current was written, which no "
        f"ordering rule can repair because the facts are gone rather than "
        f"stale. Those endpoints are still publishing that run's verdicts, "
        f"including any assertive 'verified' or 'absent' among them. Rebuild "
        f"with 'python web/load_run.py --rebuild {path}'."
    )


def pointers_to_missing_runs(store: Store) -> list[tuple[str, str, str]]:
    """Every (endpoint, pointer, run) in current whose run graph is not in the store.

    A different question from _drifted, and it has a different caller. _drifted
    asks whether the graph a pointer names still states this endpoint's facts,
    and it is computed inside load_run, so it is only ever asked when something
    is loaded. Dropping a run graph is an out-of-band store operation with no
    load after it, so nothing computes _drifted and nothing notices. This asks
    the cruder question a reader cannot recover from at all, whether the graph
    is there, and it is cheap enough to ask when a server opens a store: one
    query over current plus one contains_named_graph per distinct run named.

    Both pointers are checked. endpoint_measurements.rq and index.rq read the
    run sw:currentRun names and endpoint_content.rq reads the run
    sw:currentSampleRun names, and each of them drops a solution whose run graph
    is gone, so either pointer left dangling makes a page state a negative about
    an endpoint the store still holds facts about.
    """
    held: dict[str, bool] = {}
    missing = []
    for (endpoint, pointer), (run, _instant) in _pointers(store).items():
        if run not in held:
            held[run] = store.contains_named_graph(NamedNode(run))
        if not held[run]:
            missing.append((endpoint, pointer, run))
    return sorted(missing)


def _maintain_current(
    store: Store, graph_names: set[NamedNode]
) -> tuple[list[str], list[str], list[str]]:
    """Bring current up to date for every endpoint the loaded graphs mention.

    The graphs are taken oldest first, so a file carrying two sweeps (which
    web/tests/fixtures/run-two-sweeps.nq does) leaves current holding the newer
    one's facts rather than whichever the iteration order reached last.

    Every tie is found before anything is written. A tie is refused rather than
    resolved, and refusing halfway through would leave current holding some
    endpoints' new facts and some endpoints' old ones, which is the blend the
    refusal exists to prevent.
    """
    # A graph with no activity instant is skipped rather than ordered by
    # guesswork. It was already invisible to all three read queries, which
    # require GRAPH ?run { ?a a prov:Activity ; prov:generatedAtTime ?t }, so
    # skipping it here changes nothing about what a reader can see; putting it
    # in current would.
    runs: list[tuple[str, str]] = []
    for graph in graph_names:
        instant = _instant(store, graph.value)
        if instant is not None:
            runs.append((graph.value, instant))
    runs.sort(key=lambda pair: _as_datetime(pair[1]))

    work: list[tuple[str, str, set[str], set[str]]] = []
    pointers = _pointers(store)
    for run, instant in runs:
        measured = _run_endpoints(store, run, _MEASURED_ENDPOINTS)
        sampled = _run_endpoints(store, run, _SAMPLED_ENDPOINTS)
        for endpoint in sorted(measured):
            existing = pointers.get((endpoint, CURRENT_RUN))
            if _tied(existing, run, instant):
                raise ValueError(
                    _tie_message(endpoint, existing[0], run, instant)
                )
        for endpoint in sorted(sampled):
            existing = pointers.get((endpoint, CURRENT_SAMPLE_RUN))
            if _tied(existing, run, instant):
                raise ValueError(
                    _tie_message(endpoint, existing[0], run, instant)
                )
        work.append((run, instant, measured, sampled))
        # The pointers this run will move, so a second graph in the same file
        # is compared against what the first one leaves behind.
        for endpoint in measured:
            if _advance(pointers.get((endpoint, CURRENT_RUN)), run, instant):
                pointers[(endpoint, CURRENT_RUN)] = (run, instant)
        for endpoint in sampled:
            if _advance(pointers.get((endpoint, CURRENT_SAMPLE_RUN)), run, instant):
                pointers[(endpoint, CURRENT_SAMPLE_RUN)] = (run, instant)

    advanced: set[str] = set()
    advanced_samples: set[str] = set()
    kept_newer: set[str] = set()
    # Read again rather than reusing the dict above: that one was advanced in
    # simulation so a second graph in the same file could be checked for a tie
    # against what the first one leaves, and the write loop needs the state the
    # store is actually in.
    live = _pointers(store)
    for run, instant, measured, sampled in work:
        for endpoint in sorted(measured | sampled):
            bodies = []
            if endpoint in measured:
                if _advance(live.get((endpoint, CURRENT_RUN)), run, instant):
                    bodies.append(_REPLACE_MEASURED)
                    advanced.add(endpoint)
                    live[(endpoint, CURRENT_RUN)] = (run, instant)
                else:
                    kept_newer.add(endpoint)
            if endpoint in sampled:
                if _advance(live.get((endpoint, CURRENT_SAMPLE_RUN)), run, instant):
                    bodies.append(_REPLACE_SAMPLED)
                    advanced_samples.add(endpoint)
                    live[(endpoint, CURRENT_SAMPLE_RUN)] = (run, instant)
                else:
                    kept_newer.add(endpoint)
            if bodies:
                # One call, so this endpoint's measurements, declines,
                # declarations fact and both pointers move together or not at
                # all. See _REPLACE_MEASURED on why that is available.
                store.update(
                    _update_text(*bodies),
                    prefixes=_endpoint_run(endpoint, run),
                )

    return sorted(advanced), sorted(advanced_samples), sorted(kept_newer)


@dataclass
class RebuildResult:
    """What a rebuild of current did.

    ``endpoints`` is how many endpoints it wrote, ``runs`` how many run graphs
    it read to decide what to write.
    """

    endpoints: int = 0
    runs: int = 0


@dataclass
class CheckResult:
    """What a check of current found.

    ``drifted`` maps an endpoint to the reasons current disagrees with the run
    graphs about it, one string per reason. ``endpoints`` is how many endpoints
    were compared, so "nothing drifted" can be told from "nothing was looked
    at".
    """

    drifted: dict[str, list[str]] = field(default_factory=dict)
    endpoints: int = 0

    @property
    def ok(self) -> bool:
        return not self.drifted


def _run_graphs(store: Store) -> list[tuple[str, str]]:
    """Every run graph in the store with its instant, oldest first.

    A run graph is a named graph other than current that holds an activity with
    a prov:generatedAtTime. current holds no typed activity by construction, so
    it cannot be mistaken for one here, and the assertion that it holds none is
    tested (test_current_holds_no_typed_activity).
    """
    runs = []
    for graph in store.named_graphs():
        if graph == CURRENT_GRAPH:
            continue
        instant = _instant(store, graph.value)
        if instant is not None:
            runs.append((graph.value, instant))
    runs.sort(key=lambda pair: _as_datetime(pair[1]))
    return runs


def _newest_per_endpoint(
    store: Store, runs: list[tuple[str, str]]
) -> tuple[dict[str, str], dict[str, str]]:
    """The newest run that measured each endpoint, and that sampled each one.

    This is the computation the three read queries used to do at query time,
    and moving it here is what this stage is for: over a 30-run store of the
    543-endpoint registry it cost 5,801.8 ms per page load, against 3.1 ms for
    a flat scan of current. Done once per rebuild or check rather than once per
    request.

    Raises ValueError on a tie, naming both runs, for the same reason
    _maintain_current does: two run graphs sharing a prov:generatedAtTime make
    "the newest run" a question with no answer.
    """
    measured: dict[str, str] = {}
    sampled: dict[str, str] = {}
    instants: dict[str, str] = {}
    for run, instant in runs:
        for endpoint in _run_endpoints(store, run, _MEASURED_ENDPOINTS):
            _keep_newest(measured, instants, endpoint, run, instant, CURRENT_RUN)
        for endpoint in _run_endpoints(store, run, _SAMPLED_ENDPOINTS):
            _keep_newest(
                sampled, instants, endpoint, run, instant, CURRENT_SAMPLE_RUN
            )
    return measured, sampled


def _keep_newest(
    chosen: dict[str, str],
    instants: dict[str, str],
    endpoint: str,
    run: str,
    instant: str,
    pointer: str,
) -> None:
    key = f"{pointer}\n{endpoint}"
    held = chosen.get(endpoint)
    if held is None:
        chosen[endpoint] = run
        instants[key] = instant
        return
    if _as_datetime(instants[key]) == _as_datetime(instant):
        raise ValueError(_tie_message(endpoint, held, run, instant))
    if _as_datetime(instants[key]) < _as_datetime(instant):
        chosen[endpoint] = run
        instants[key] = instant


def rebuild_current(store: Store) -> RebuildResult:
    """Rebuild current from the run graphs alone.

    THE ALGORITHM, and why it is not one SPARQL update. Deriving the whole
    graph in one update means every endpoint's recency is decided by scanning
    the whole history, which is exactly the 5.8 second page this stage exists
    to remove, run once for every endpoint: measured at 43.5 s over the 30-run
    store of the 543-endpoint registry. So this walks the run graphs once,
    oldest first, to learn the newest run that measured each endpoint and the
    newest that sampled each one, and then writes each endpoint with the same
    per-endpoint update the load path uses. That is O(run graphs) queries plus
    exactly one update per endpoint, not one per endpoint per run, and it
    shares its writing code with the maintenance path so the two cannot derive
    different graphs.

    current is dropped first, so an endpoint the run graphs no longer mention
    is removed rather than left behind. Every write after that is one
    transactional update, and the whole rebuild is re-runnable, which is what
    makes an interrupted rebuild a state to repeat rather than a state to
    diagnose.
    """
    runs = _run_graphs(store)
    measured, sampled = _newest_per_endpoint(store, runs)

    if store.contains_named_graph(CURRENT_GRAPH):
        store.remove_graph(CURRENT_GRAPH)

    for endpoint in sorted(set(measured) | set(sampled)):
        # The two units can name different runs, which is the whole point of
        # two pointers, so they cannot share one prefix binding and so they are
        # two calls here rather than one. Each is transactional on its own, and
        # a rebuild interrupted between them is repaired by running it again.
        if endpoint in measured:
            store.update(
                _update_text(_REPLACE_MEASURED),
                prefixes=_endpoint_run(endpoint, measured[endpoint]),
            )
        if endpoint in sampled:
            store.update(
                _update_text(_REPLACE_SAMPLED),
                prefixes=_endpoint_run(endpoint, sampled[endpoint]),
            )

    return RebuildResult(
        endpoints=len(set(measured) | set(sampled)), runs=len(runs)
    )


def _facts(store: Store, query: str, prefixes: dict[str, str]) -> set[tuple[str, str, str]]:
    return {
        (row["thing"].value, row["p"].value, str(row["o"]))
        for row in store.query(query, prefixes=prefixes)
    }


def check_current(store: Store) -> CheckResult:
    """Compare current against the run graphs and name every endpoint that drifted.

    A derived graph that cannot be verified is a liability: the index this
    stage builds asserts things over current that a reader cannot cross-check
    by hand. So this recomputes, from the run graphs alone, which run each
    endpoint's facts should come from and what those facts are, and reports
    every disagreement with what current holds. It is deliberately a second
    derivation and not a call into the writing path, because a check that used
    the writer's own answer could only ever agree with it.

    It is the expensive shape by design: it does the per-endpoint recency scan
    the read queries no longer do. That is the cost of verifying, paid by an
    operator running a check, not by a page load.
    """
    runs = _run_graphs(store)
    measured, sampled = _newest_per_endpoint(store, runs)
    reasons: dict[str, list[str]] = {}

    def note(endpoint: str, reason: str) -> None:
        reasons.setdefault(endpoint, []).append(reason)

    pointers = _pointers(store)
    expected = set(measured) | set(sampled)
    for endpoint in sorted(expected):
        if endpoint in measured:
            held = pointers.get((endpoint, CURRENT_RUN))
            if held is None:
                note(endpoint, f"no sw:currentRun; expected {measured[endpoint]}")
            elif held[0] != measured[endpoint]:
                note(
                    endpoint,
                    f"sw:currentRun is {held[0]}, expected {measured[endpoint]}",
                )
            else:
                in_current = _facts(
                    store,
                    _FACTS_FOR_ENDPOINT,
                    {_ENDPOINT_PREFIX: endpoint, "graph": CURRENT_GRAPH_IRI},
                )
                in_run = _facts(
                    store,
                    _FACTS_FOR_ENDPOINT,
                    {_ENDPOINT_PREFIX: endpoint, "graph": measured[endpoint]},
                )
                missing = in_run - in_current
                extra = in_current - in_run
                if missing:
                    note(endpoint, f"{len(missing)} fact(s) missing from current")
                if extra:
                    note(endpoint, f"{len(extra)} fact(s) in current no run states")
        elif (endpoint, CURRENT_RUN) in pointers:
            note(endpoint, "sw:currentRun names a run that measured nothing here")
        if endpoint in sampled:
            held = pointers.get((endpoint, CURRENT_SAMPLE_RUN))
            if held is None:
                note(
                    endpoint,
                    f"no sw:currentSampleRun; expected {sampled[endpoint]}",
                )
            elif held[0] != sampled[endpoint]:
                note(
                    endpoint,
                    f"sw:currentSampleRun is {held[0]}, expected "
                    f"{sampled[endpoint]}",
                )

    for (endpoint, pointer), (run, _) in sorted(pointers.items()):
        if endpoint not in expected:
            note(endpoint, f"{pointer} names {run}, but no run graph mentions it")

    # Every endpoint this compared, which is the run graphs' set PLUS the
    # endpoints only current names. The union and not len(expected): the loop
    # just above compares an endpoint the run graphs have stopped mentioning,
    # and counting only the run graphs' set made "3 of 0 endpoints drifted" of a
    # dropped run graph and "7 of 2" of a shrunk one, a numerator outside its own
    # denominator.
    compared = expected | {endpoint for endpoint, _ in pointers}
    return CheckResult(drifted=reasons, endpoints=len(compared))


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


# The four predicates that close a section, read off prober/src/emit.rs:
# sw:emission is the last quad emit_header writes, sw:dormantCount the last
# quad emit_dormancy writes, sw:completedEndpoint the last quad emit_endpoint
# writes, sw:finalised the last quad emit_footer writes. All four are needed. A
# complete run ends at sw:finalised, a run killed between endpoints ends at
# sw:completedEndpoint, a run killed before its first endpoint ends at
# sw:dormantCount, and a run killed inside the dormancy section ends at
# sw:emission, so a set missing any one of them would truncate away a section
# that was written whole. Leaving out sw:finalised is the worst of the four:
# every complete run would lose its footer, and a reader testing for the footer
# would then report that no sweep this project publishes ever finished.
#
# sw:dormantCount and NOT sw:dormantEndpoint, which is one character away from
# it and is the per-endpoint predicate rather than the section's terminator.
# The convention points both ways -- sw:completedEndpoint IS a terminator and
# is singular -- so the trap is real: recognising the per-endpoint spelling
# here would make every dormancy line a cut point, and a truncation would land
# in the middle of the section rather than after it.
#
# These spellings are a wire format shared with the emitter (see emit.rs's
# module docstring), so neither side may change them alone.
_TERMINATOR_PREDICATES = frozenset(
    {
        "urn:sparqlwatch:emission",
        "urn:sparqlwatch:dormantCount",
        "urn:sparqlwatch:completedEndpoint",
        "urn:sparqlwatch:finalised",
    }
)
_TERMINATOR_TERMS = frozenset(
    f"<{predicate}>".encode() for predicate in _TERMINATOR_PREDICATES
)
# Every terminator's subject is the run's activity, which emit.rs builds as
# urn:sparqlwatch:activity:<run>.
_ACTIVITY_IRI_PREFIX = "urn:sparqlwatch:activity:"
# The same prefix in a line's subject position, as a second anchor beside the
# predicate in _is_terminator_line.
_ACTIVITY_PREFIX = f"<{_ACTIVITY_IRI_PREFIX}".encode()


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

    The predicate position is the load-bearing half, and it is the half
    test_a_sampled_value_spelled_like_a_chunk_marker_is_not_a_boundary
    exercises. The subject anchor below is belt-and-braces against a line no
    emitter writes: every terminator emit.rs emits has the run's activity for
    a subject, so a terminator predicate on any other subject is not a
    section boundary this loader put there.
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


def _holds_endpoint_facts(quads: list) -> bool:
    """Whether ``quads`` says anything about an endpoint at all.

    Every quad emit_header and emit_footer write has the run's activity for a
    subject; every quad a chunk writes has the endpoint, a measurement, a
    declined-metric or a content-sample node for a subject (see emit.rs's
    emit_header, emit_footer and emit_endpoint). So a document whose every
    subject is an activity carries the run's own metadata and no facts about
    any endpoint.

    The dormancy section DOES carry endpoint subjects, and it is written after
    the header's terminator for that reason: this function is only ever reached
    by a file with no terminator anywhere, which is a fragment cut inside the
    header, and a fragment holding endpoint facts would load whole, carry the
    store's greatest prov:generatedAtTime with no sw:emission beside it, and
    silence unfinished-run detection for every endpoint on the site.
    emit.rs's a_header_truncated_file_still_holds_no_endpoint_facts pins that
    from the writer's side.
    """
    return any(
        not quad.subject.value.startswith(_ACTIVITY_IRI_PREFIX) for quad in quads
    )


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
        # Parses, and carries no terminator anywhere. Two different files
        # look like this and the bytes do not say which: a run written before
        # this format existed (every captured fixture in tests/fixtures is
        # one), and a run of this format cut inside its HEADER, since
        # sw:emission is the header's last quad so any earlier line boundary
        # leaves no terminator behind. Provenance is not recoverable, so it is
        # not asserted; what is available is a second discriminator. A run
        # from before this format still measured endpoints, and a header holds
        # only the activity's own metadata, so a file with no endpoint facts
        # at all is a fragment of a header or a run that recorded nothing.
        # Neither may load: the graph would carry the store's greatest
        # prov:generatedAtTime with no sw:emission beside it, win the
        # newest-run aggregate in both read queries, and silence the
        # unfinished-run detection for every endpoint on the site.
        #
        # ``quads`` empty is left to the "names no graphs" refusal below, which
        # is where an empty or comment-only file has always been diagnosed.
        if quads and not _holds_endpoint_facts(quads):
            raise ValueError(
                "input holds activity metadata and no endpoint facts, and no "
                "section terminator: it is either a fragment of a run whose "
                "header was cut short or an empty run, and neither can be the "
                "newest activity in the store"
            )
        # A run that promised nothing about sections, with facts in it. There
        # is nothing to truncate and it loads whole.
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

    if CURRENT_GRAPH in graph_names:
        # current is derived from the run graphs and reconstructible from them
        # alone, which is the rule the whole second half of this module's
        # docstring rests on. A file naming it as its graph breaks that rule and
        # reports a clean load while doing it: load_run's remove_graph loop
        # would wipe the graph all three read queries trust, insert the file's
        # own triples into it, and return drifted=[] because _DRIFTED_RUN_
        # POINTERS asks which pointers name a run that no longer states their
        # facts and an emptied current graph holds no pointers to ask about.
        raise ValueError(
            f"input names {CURRENT_GRAPH_IRI} as a graph. That graph is derived "
            f"from the run graphs by this module, not loaded from a file: "
            f"accepting it would replace what every read query trusts with the "
            f"file's own triples, and report a successful load. Name the run "
            f"IRI the prober writes, or rebuild with "
            f"'python web/load_run.py --rebuild STORE_PATH'."
        )

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

    # The run graphs are in and immutable; now the derived graph the three read
    # queries read. This happens after the insert and not before, because it
    # reads the facts it copies out of the graphs that were just written.
    advanced, advanced_samples, kept_newer = _maintain_current(store, graph_names)
    drifted = _drifted(store)

    return LoadResult(
        replaced=replaced,
        quad_count=len(quads),
        discarded_bytes=discarded,
        advanced=advanced,
        advanced_samples=advanced_samples,
        kept_newer=kept_newer,
        drifted=drifted,
    )


_USAGE = (
    "usage: load_run.py STORE_PATH RUN.nq [RUN.nq ...]\n"
    "       load_run.py --rebuild STORE_PATH\n"
    "       load_run.py --check STORE_PATH"
)


def _rebuild_mode(path: str) -> int:
    """The repair path, and the migration path for a store built before current.

    web/app.py refuses to serve a store that holds run graphs and no current
    graph, and names this invocation, because such a store answers "we know
    nothing about this endpoint" for every endpoint it fully describes.
    """
    result = rebuild_current(Store(path))
    print(
        f"{path}: rebuilt {CURRENT_GRAPH_IRI} from {result.runs} run graph(s): "
        f"{result.endpoints} endpoints"
    )
    return 0


def _check_mode(path: str) -> int:
    """Compare current against the run graphs and name what disagrees.

    Exits non-zero when anything drifted, so a cron job or a deploy step can
    tell without reading the output.
    """
    result = check_current(Store(path))
    if result.ok:
        print(
            f"{path}: {CURRENT_GRAPH_IRI} agrees with the run graphs for all "
            f"{result.endpoints} endpoints, no endpoint drifted"
        )
        return 0
    print(
        f"{path}: {len(result.drifted)} of {result.endpoints} endpoints drifted "
        f"from what the run graphs say. Rebuild with "
        f"'python web/load_run.py --rebuild {path}'."
    )
    for endpoint, why in sorted(result.drifted.items()):
        print(f"  {endpoint}: {'; '.join(why)}")
    return 1


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) == 2 and args[0] in ("--rebuild", "--check"):
        # Both modes open an existing store and neither takes a run file, so
        # the path is required to be a store already: Store() would create an
        # empty RocksDB directory at a mistyped path and then rebuild current
        # from no run graphs at all, which is the empty-store mistake
        # web/app.py's _opened_store exists to refuse.
        if not Path(args[1]).is_dir():
            print(
                f"{args[1]} is not an existing store directory; "
                f"{args[0]} reads a store rather than creating one",
                file=sys.stderr,
            )
            return 2
        return (
            _rebuild_mode(args[1])
            if args[0] == "--rebuild"
            else _check_mode(args[1])
        )
    if len(args) < 2 or args[0].startswith("--"):
        print(_USAGE, file=sys.stderr)
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
    # Whether any file left current attributing facts to a run that no longer
    # states them. It decides the exit status, below, and it is deliberately
    # sticky across the loop: a later clean file does not repair an earlier
    # file's drift.
    drifted = False
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
        if result.kept_newer:
            # An out-of-order load: current already pointed at a strictly newer
            # run, so the pointer was left alone and the site shows something
            # other than the file just named. That is the right behaviour and it
            # is not an error, so it is said and the exit status is unaffected;
            # saying nothing left an operator believing they had just published
            # this file.
            print(
                f"{path}: current already pointed at a newer run for "
                f"{len(result.kept_newer)} endpoint(s), so what the site shows "
                f"for them is unchanged by this file: {result.kept_newer}"
            )
        if result.drifted:
            # The one case a load can report that the load cannot fix, and the
            # reason --check exists. On stderr and non-zero because the store is
            # now stating facts no run graph holds, which is the failure this
            # whole module is written against; printing it on stdout at exit 0
            # left it in a log nobody reads.
            drifted = True
            print(f"{path}: {_drift_advice(result.drifted, args[0])}", file=sys.stderr)
    return 1 if drifted else 0


if __name__ == "__main__":
    raise SystemExit(main())

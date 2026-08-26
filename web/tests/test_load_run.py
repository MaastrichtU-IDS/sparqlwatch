"""Tests for load_run: replacing a run's graph rather than merging into it.

See web/load_run.py's module docstring for the two things this module
exists to prevent: a changed re-run silently merging into the old graph
(producing a measurement with two dqv:value triples), and a malformed file
destroying an existing run before the parse failure is noticed.
"""

from pathlib import Path

import pytest
from pyoxigraph import DefaultGraph, NamedNode, RdfFormat, Store, parse

from conftest import run_graph_names, run_quad_count
from endpoint_content import endpoint_content
from endpoint_measurements import endpoint_measurements
from load_run import (
    _TERMINATOR_PREDICATES,
    LoadResult,
    check_current,
    load_run,
    main,
    rebuild_current,
)

FIXTURE = Path(__file__).parent / "fixtures" / "run-with-samples.nq"
TWO_SWEEPS_FIXTURE = Path(__file__).parent / "fixtures" / "run-two-sweeps.nq"

def test_loading_the_same_run_twice_leaves_one_graph(tmp_path):
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    first = len(store)
    load_run(store, FIXTURE.read_bytes())
    assert len(store) == first
    assert len(run_graph_names(store)) == 1


def test_a_changed_rerun_replaces_rather_than_merges(tmp_path):
    """The hazard this function exists to prevent. The same --at re-run after an
    endpoint's DNS recovers produces the same run IRI with different verdicts.
    Merging leaves one measurement carrying two of them, which is a graph that
    contradicts itself, and the run graph is meant to be immutable."""
    store = Store(str(tmp_path / "s"))
    original = FIXTURE.read_text()
    changed = original.replace('"verified"', '"indeterminate"', 1)
    load_run(store, original.encode())
    load_run(store, changed.encode())
    rows = list(store.query("""
        SELECT ?m (COUNT(DISTINCT ?v) AS ?n) WHERE {
          GRAPH ?g { ?m <http://www.w3.org/ns/dqv#value> ?v }
        } GROUP BY ?m HAVING (COUNT(DISTINCT ?v) > 1)"""))
    assert rows == [], f"no measurement may carry two verdicts, got {len(rows)}"


def test_the_result_says_what_it_replaced(tmp_path):
    store = Store(str(tmp_path / "s"))
    first = load_run(store, FIXTURE.read_bytes())
    assert first.replaced == [], "nothing was there to replace"
    second = load_run(store, FIXTURE.read_bytes())
    assert len(second.replaced) == 1, "the second load replaced the run's graph"


def test_two_different_runs_coexist(tmp_path):
    """Runs are per-sweep named graphs, so loading a second run must not disturb
    the first. Without this, 'replace' could be implemented as 'clear the store'
    and every test above would still pass."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    other = FIXTURE.read_text().replace("2026-08-22T16:00:00Z", "2026-08-22T17:00:00Z")
    load_run(store, other.encode())
    assert len(run_graph_names(store)) == 2


def test_a_malformed_file_leaves_an_existing_run_untouched(tmp_path):
    """The most important test in this task. A truncated .nq is not adversarial
    input, it is exactly what a crashed or interrupted prober writes. Dropping
    the named graphs before parsing would destroy the existing run before the
    parse failure is ever noticed: a real run measured going from 278 quads to
    0 under that order. Parsing first must reject the file before the store is
    touched at all."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    before = len(store)
    before_graphs = set(store.named_graphs())

    # Cut the file off mid-quad, the way a crash mid-write would.
    malformed = FIXTURE.read_bytes()[:1000]
    with pytest.raises(ValueError):
        load_run(store, malformed)

    assert len(store) == before, "a rejected load must not touch the store"
    assert set(store.named_graphs()) == before_graphs
    # Not just "the count didn't change": the actual content must still be
    # there, not some other 278 quads.
    still_present = store.query(
        'ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#value> "verified" } }'
    )
    assert bool(still_present), "the original run's content must survive"


def test_a_file_with_no_named_graphs_is_refused(tmp_path):
    """Every run this project emits is a named graph (the run IRI). A file of
    bare default-graph triples is a different thing than it claims to be, so
    refuse it rather than silently loading it into the store's default graph,
    where no run-replacement logic could ever find it again."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    before = len(store)

    default_graph_only = (
        b"<http://example.com/s> <http://example.com/p> <http://example.com/o> .\n"
    )
    # Match the reason, not merely the exception type. Replacing this branch's
    # raise with a continue also makes the load fail, but through the "names no
    # graphs" guard below it, so a bare pytest.raises(ValueError) passes over a
    # loader that no longer refuses default-graph triples at all.
    with pytest.raises(ValueError, match="default-graph triples"):
        load_run(store, default_graph_only)

    assert len(store) == before, "a refused load must not touch the store"


def test_several_named_graphs_are_all_replaced_and_nothing_else_is(tmp_path):
    """The two-sweeps fixture carries two named graphs in one file. Loading it
    must replace both graphs it names on a second load, and must not disturb
    an unrelated third graph already in the store."""
    store = Store(str(tmp_path / "s"))
    other_run = FIXTURE.read_text().replace("2026-08-22T16:00:00Z", "2026-08-22T10:00:00Z")
    load_run(store, other_run.encode())
    other_graph = NamedNode("urn:sparqlwatch:run:2026-08-22T10:00:00Z")
    other_count = len(list(store.quads_for_pattern(None, None, None, other_graph)))

    first = load_run(store, TWO_SWEEPS_FIXTURE.read_bytes())
    assert first.replaced == [], "neither sweep graph existed yet"
    assert len(run_graph_names(store)) == 3, "the unrelated graph plus both sweep graphs"

    second = load_run(store, TWO_SWEEPS_FIXTURE.read_bytes())
    assert len(second.replaced) == 2, "both named graphs were replaced"
    assert len(run_graph_names(store)) == 3, "no graph was added or lost"
    assert len(list(store.quads_for_pattern(None, None, None, other_graph))) == other_count, (
        "the unrelated graph must be untouched by a load that does not name it"
    )


def test_load_result_reports_the_quad_count(tmp_path):
    store = Store(str(tmp_path / "s"))
    result = load_run(store, FIXTURE.read_bytes())
    assert result.quad_count == 278
    assert isinstance(result, LoadResult)


def test_an_interrupted_insert_says_what_the_store_is_left_holding(tmp_path, monkeypatch):
    """The window web/load_run.py admits it cannot close.

    remove_graph and extend are two store operations, so an insert stopped by a
    full disk or an OOM kill leaves the graphs dropped and nothing inserted:
    measured at 278 quads going to zero. Store.update IS transactional (see
    load_run's module docstring), and the reason the run-graph replacement is
    not written as one update is the size of the INSERT DATA body it would
    need, not the absence of a transaction. So this window is loud instead of
    closed. The count is what makes it actionable, because the .nq file is the
    source of truth and a run graph is immutable: an operator told '0 of 278'
    re-runs the load and gets the run back exactly, as the end of this test
    does.

    urn:sparqlwatch:current outlives the dropped run graph, which is the second
    thing this state needs saying about it: current then attributes facts to a
    run the store no longer holds, and that is exactly what the drift check
    reports. Re-loading clears it.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    run = NamedNode("urn:sparqlwatch:run:2026-08-22T16:00:00Z")

    def full_disk(self, quads):
        raise OSError("simulated full disk during insert")

    monkeypatch.setattr(Store, "extend", full_disk)
    with pytest.raises(RuntimeError, match="loaded 0 of 278 quads") as raised:
        load_run(store, FIXTURE.read_bytes())
    assert isinstance(raised.value.__cause__, OSError), (
        "the underlying failure must still be reachable, not swallowed"
    )
    assert run_quad_count(store) == 0, (
        "the window is real: the run is gone until it is re-loaded"
    )
    assert not check_current(store).ok, (
        "and current still names the run that was dropped"
    )

    monkeypatch.undo()
    again = load_run(store, FIXTURE.read_bytes())
    assert again.quad_count == 278
    assert run_quad_count(store) == 278, (
        "re-loading the same file restores the run exactly"
    )
    assert check_current(store).ok, "and clears the drift it left behind"
    assert bool(store.query(
        'ASK { GRAPH ?g { ?m <http://www.w3.org/ns/dqv#value> "verified" } }'
    )), "and restores its content, not merely its quad count"


def test_an_insert_that_silently_stores_nothing_is_not_a_successful_load(tmp_path, monkeypatch):
    """Worse than the crash above, because nothing anywhere would say the run is
    missing. A LoadResult reporting 278 quads over an empty graph is the store
    lying about what it holds, so the count is verified against the store rather
    than against the parse."""
    monkeypatch.setattr(Store, "extend", lambda self, quads: None)
    store = Store(str(tmp_path / "s"))
    with pytest.raises(RuntimeError, match="loaded 0 of 278 quads"):
        load_run(store, FIXTURE.read_bytes())


def test_a_named_graph_file_with_one_default_graph_triple_is_refused_whole(tmp_path):
    """The case that tells this refusal apart from the empty-input guard. This
    file names a run graph, so skipping the default-graph triple instead of
    refusing would leave the load looking successful while that triple settles
    in the store's default graph, where, as load_run's comment says, no run
    graph could ever be dropped to get rid of it again. All or nothing: the
    whole file is refused and the store is untouched."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    before = len(store)

    mixed = FIXTURE.read_bytes() + (
        b"<http://example.com/s> <http://example.com/p> <http://example.com/o> .\n"
    )
    with pytest.raises(ValueError, match="default-graph triples"):
        load_run(store, mixed)

    assert len(store) == before, "a refused load must not touch the store"
    assert not list(store.quads_for_pattern(None, None, None, DefaultGraph())), (
        "nothing may reach the store's default graph"
    )


def test_a_file_naming_no_graph_at_all_is_refused(tmp_path):
    """An empty or comment-only .nq parses cleanly and names nothing, so it is
    not a run: accepting it would report a successful load of zero quads and
    tell an operator whose prober died before writing anything that the run is
    in the store. Its own guard, and its own test: deleting that guard leaves
    every other test here green."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    before = len(store)

    for name, payload in (("empty", b""), ("comments only", b"# nothing here\n")):
        with pytest.raises(ValueError, match="names no graphs"):
            load_run(store, payload)
        assert len(store) == before, f"a refused load ({name}) must not touch the store"


def test_a_bad_run_path_leaves_no_store_directory_behind(tmp_path):
    """The root cause N1's second variant needs: main() used to do
    ``store = Store(args[0])`` before reading any run file, so a run file
    that fails to load left a freshly created, empty store directory behind
    (Store() creates that directory on construction, whether or not
    anything is ever inserted into it). That directory is then exactly the
    "opens as a valid but empty store" case app.py's _opened_store refuses,
    except it was this loader's own failed run that created it, not an
    operator's typo. Every input file must be read and validated before the
    store is opened at all."""
    store_path = tmp_path / "store"
    bad_run = tmp_path / "empty.nq"
    bad_run.write_bytes(b"")

    with pytest.raises(ValueError, match="names no graphs"):
        main([str(store_path), str(bad_run)])

    assert not store_path.exists(), "a rejected run file must not create the store"


# The one committed fixture that is current emitter output: emit_nquads' own
# 51 quads, so its section boundaries and terminator spellings are the wire
# format itself rather than a restatement of it.
TERMINATED_FIXTURE = Path(__file__).parent / "fixtures" / "run-prober-failed.nq"


def _sections() -> tuple[str, str, str]:
    """TERMINATED_FIXTURE split into the three sections emit_nquads writes:
    the header, one endpoint's chunk, and the footer.

    Boundaries are found by the terminator predicates rather than by line
    number, so a regenerated fixture that gains or loses a quad moves them
    with it instead of handing these tests bytes that cut somewhere else.
    """
    lines = TERMINATED_FIXTURE.read_text().splitlines(keepends=True)
    ends: dict[str, int] = {}
    for index, line in enumerate(lines):
        for predicate in ("emission", "completedEndpoint", "finalised"):
            if f"> <urn:sparqlwatch:{predicate}> " in line:
                ends[predicate] = index
    assert set(ends) == {"emission", "completedEndpoint", "finalised"}, (
        f"the fixture must carry all three terminators, found {sorted(ends)}"
    )
    header = "".join(lines[: ends["emission"] + 1])
    chunk = "".join(lines[ends["emission"] + 1 : ends["completedEndpoint"] + 1])
    footer = "".join(lines[ends["completedEndpoint"] + 1 : ends["finalised"] + 1])
    return header, chunk, footer


def _second_chunk() -> bytes:
    """The fixture's chunk rewritten for a second endpoint, so a run can be
    built with one whole chunk followed by a partial one."""
    _, chunk, _ = _sections()
    return chunk.replace("data.kkg.kadaster.nl", "second.example").encode()


FINALISED = (
    'ASK { GRAPH ?g { ?a <urn:sparqlwatch:finalised> '
    '"true"^^<http://www.w3.org/2001/XMLSchema#boolean> } }'
)
FAILED_ENDPOINTS = "ASK { GRAPH ?g { ?a <urn:sparqlwatch:failedEndpoints> ?n } }"
SECOND_ENDPOINT = "ASK { GRAPH ?g { ?s ?p <https://second.example/query> } }"
A_DATA_SERVICE = "ASK { GRAPH ?g { ?s ?p <http://www.w3.org/ns/dcat#DataService> } }"
MARKER_IN_OBJECT = "ASK { GRAPH ?g { ?s ?p <urn:sparqlwatch:completedEndpoint> } }"


def test_a_complete_run_loads_with_its_footer_and_the_same_quad_count(tmp_path):
    """First on the list because getting the terminator set wrong breaks every
    complete run and no other test here catches it. A complete file's last line
    is the footer's sw:finalised, not a chunk marker, and sw:failedEndpoints
    sits between the last marker and it. A tolerance that truncated back to the
    last chunk marker would strip both footer quads from every finished sweep,
    and Task 4's read tier would then tell every visitor that every sweep did
    not finish."""
    store = Store(str(tmp_path / "s"))
    header, chunk, footer = _sections()
    assert header + chunk + footer == TERMINATED_FIXTURE.read_text(), (
        "the three sections must account for the whole file, or every test "
        "below is cutting the wrong bytes"
    )

    result = load_run(store, TERMINATED_FIXTURE.read_bytes())

    assert result.quad_count == 51, "the fixture's own count, footer included"
    assert result.discarded_bytes == 0, "nothing may be dropped from a complete run"
    assert bool(store.query(FINALISED)), "the footer's terminator must be in the store"
    assert bool(store.query(FAILED_ENDPOINTS)), (
        "and the quad between the last marker and it, which a truncate-to-the-"
        "last-marker rule would also lose"
    )


def test_a_chunk_truncated_mid_line_loads_the_whole_chunks_before_it(tmp_path):
    """What a crash mid-write leaves. The partial chunk cannot be parsed at all,
    so today the whole file is refused and the finished endpoints are lost with
    it."""
    store = Store(str(tmp_path / "s"))
    header, chunk, _ = _sections()
    second = _second_chunk().splitlines(keepends=True)
    partial = second[0] + second[1][:20]
    assert not partial.endswith(b"\n"), "this cut must land mid-line"

    result = load_run(store, header.encode() + chunk.encode() + partial)

    assert result.quad_count == 49, "the header's 7 quads and the whole first chunk's 42"
    assert result.discarded_bytes == len(partial)
    assert not bool(store.query(SECOND_ENDPOINT)), (
        "an unfinished chunk's facts must not reach the store"
    )
    assert not bool(store.query(FINALISED)), "this run did not finish and must not say so"


def test_a_chunk_truncated_at_a_line_boundary_is_still_dropped(tmp_path):
    """The case a line-level rule cannot even see. These bytes are complete,
    parseable, mutually inconsistent lines: an endpoint with some of its facts
    and no completion marker. Loading them would publish a partial answer as a
    whole one."""
    store = Store(str(tmp_path / "s"))
    header, chunk, _ = _sections()
    partial = b"".join(_second_chunk().splitlines(keepends=True)[:-1])
    data = header.encode() + chunk.encode() + partial
    assert len(list(parse(data, format=RdfFormat.N_QUADS))) == 90, (
        "these bytes parse, which is exactly why the rule cannot be about "
        "whether they parse"
    )

    result = load_run(store, data)

    assert result.quad_count == 49, "the chunk with no marker is dropped whole"
    assert result.discarded_bytes == len(partial)
    assert not bool(store.query(SECOND_ENDPOINT))


def test_a_run_that_died_before_its_first_endpoint_loads_just_the_header(tmp_path):
    """The third terminator, and why the set has three members: with no chunk
    written and no footer, the header's sw:emission is the only terminator in
    the file. A two-member set would refuse this file whole."""
    store = Store(str(tmp_path / "s"))
    header, chunk, _ = _sections()
    first = chunk.encode().splitlines(keepends=True)
    partial = first[0] + first[1][:30]

    result = load_run(store, header.encode() + partial)

    assert result.quad_count == 7, "the header's run-level facts and nothing else"
    assert result.discarded_bytes == len(partial)
    assert not bool(store.query(A_DATA_SERVICE)), "no endpoint was reached"
    assert bool(store.query("ASK { GRAPH ?g { ?a <urn:sparqlwatch:emission> ?e } }"))


def test_a_sampled_value_spelled_like_a_chunk_marker_is_not_a_boundary(tmp_path):
    """Sampled class IRIs come from strangers' endpoints, so a sample may
    legitimately contain a value IRI spelled urn:sparqlwatch:completedEndpoint.
    A byte scan for that spelling would treat this sw:sampledValue line as a
    chunk boundary and cut in the middle of a chunk, loading a fragment of a
    sample. The marker must be recognised in predicate position, where a class
    IRI can never appear."""
    store = Store(str(tmp_path / "s"))
    header, _, _ = _sections()
    sample = (
        "<urn:sparqlwatch:content-sample:2026-08-23T02:00:00Z:0> "
        "<urn:sparqlwatch:sampledValue> <urn:sparqlwatch:completedEndpoint> "
        "<urn:sparqlwatch:run:2026-08-23T02:00:00Z> .\n"
    ).encode()
    tail = b"<urn:sparqlwatch:content-sample:2026-08-23T02:00:00Z:0> <urn:sparqlwa"

    result = load_run(store, header.encode() + sample + tail)

    assert result.quad_count == 7, "the header only: the sample's chunk has no marker"
    assert result.discarded_bytes == len(sample) + len(tail)
    assert not bool(store.query(MARKER_IN_OBJECT)), (
        "the spoofing value must not have been read as a boundary and kept"
    )


def test_a_syntax_error_in_the_middle_still_refuses_the_whole_file(tmp_path):
    """A corrupt file is not a crashed writer and the two must not be
    conflated. The corruption is before the last terminator, so truncating to
    that terminator still contains it: the file is refused exactly as today,
    and the run already in the store survives."""
    store = Store(str(tmp_path / "s"))
    load_run(store, TERMINATED_FIXTURE.read_bytes())
    before = len(store)
    header, chunk, footer = _sections()
    lines = chunk.splitlines(keepends=True)
    corrupt = "".join(lines[:3]) + "<urn:sparqlwatch:broken> not-a-term .\n" + "".join(lines[3:])

    with pytest.raises(ValueError, match="not valid N-Quads"):
        load_run(store, (header + corrupt + footer).encode())

    assert len(store) == before, "a refused load must not touch the store"
    assert bool(store.query(FINALISED)), "the complete run already loaded must survive"


def test_a_file_with_no_terminator_at_all_is_refused_with_the_parse_error(tmp_path):
    """Never a zero-quad success path. A file that is entirely one truncated
    line has no terminator to fall back to, so it must fail with the parse
    error, not with 'discarded 68 bytes, loaded 0 quads' followed by 'input
    names no graphs', which is a true sentence that misdiagnoses the file and
    buries the real error."""
    store = Store(str(tmp_path / "s"))
    one_truncated_line = (
        b"<urn:sparqlwatch:activity:2026-08-23T02:00:00Z> <urn:sparqlwatch:emis"
    )

    with pytest.raises(ValueError, match="not valid N-Quads") as raised:
        load_run(store, one_truncated_line)

    assert "names no graphs" not in str(raised.value), (
        "the parse error is the diagnosis, not the empty-input guard"
    )


def test_the_result_reports_the_discarded_byte_count(tmp_path):
    """load_run.py has no logging and reports through print in main, so the
    count an operator needs is a field on LoadResult rather than a log line."""
    header, chunk, _ = _sections()
    tail = chunk.encode()[:57]
    assert b"\n" not in tail, "this cut must land in the chunk's first line"

    dropped = load_run(Store(str(tmp_path / "a")), header.encode() + tail)
    assert dropped.discarded_bytes == 57

    whole = load_run(Store(str(tmp_path / "b")), TERMINATED_FIXTURE.read_bytes())
    assert whole.discarded_bytes == 0


def test_a_run_from_before_terminators_existed_loads_whole(tmp_path):
    """The captured sweeps carry no terminator at all, which the emitter's own
    docstring calls the third case: a run from before the scheme existed, which
    promised nothing about sections. There is no terminator to truncate to and
    the bytes parse, so the file loads whole. A rule that refused a file for
    carrying no terminator would reject every historical run in the store."""
    data = FIXTURE.read_bytes()
    assert b"urn:sparqlwatch:finalised" not in data, "this fixture predates the footer"

    result = load_run(Store(str(tmp_path / "s")), data)

    assert result.quad_count == 278
    assert result.discarded_bytes == 0


def test_an_empty_or_comment_only_file_is_still_refused_as_naming_no_graphs(tmp_path):
    """Both parse cleanly and carry no terminator, so the tolerance must leave
    them to the existing guard rather than turn them into a parse error or into
    a successful load of nothing."""
    store = Store(str(tmp_path / "s"))
    for name, payload in (("empty", b""), ("comments only", b"# nothing here\n")):
        with pytest.raises(ValueError, match="names no graphs"):
            load_run(store, payload)
        assert not list(store.named_graphs()), f"a refused load ({name}) stores nothing"


def test_a_header_cut_short_of_its_terminator_is_refused(tmp_path):
    """The third case's other half, and the one that used to load silently.

    sw:emission is the header's LAST quad, so a file cut anywhere before it,
    at a line boundary, parses cleanly and carries no terminator either. It
    then looked exactly like a run from before this format existed, loaded
    with discarded_bytes 0, and carried the store's greatest
    prov:generatedAtTime, which is what both read queries pick the newest run
    by. One truncated header would silence the unfinished-run detection for
    every endpoint on the site.

    Nothing in the bytes says which file this is, but there is a second thing
    the loader already holds: a run from before this format still measured
    endpoints, and a header holds only the activity's own metadata. So a file
    that parses, carries no terminator and says nothing about any endpoint is
    either a fragment or a run with no content, and neither may be admitted
    as the store's newest activity.
    """
    header, _, _ = _sections()
    lines = header.splitlines(keepends=True)
    assert "<urn:sparqlwatch:emission>" in lines[-1], (
        "the cut has to drop the header's terminator and nothing else"
    )
    cut = "".join(lines[:-1]).encode()
    assert list(parse(cut, format=RdfFormat.N_QUADS)), (
        "a cut at a line boundary parses cleanly: that is the whole hazard"
    )

    store = Store(str(tmp_path / "s"))
    with pytest.raises(ValueError, match="no endpoint facts"):
        load_run(store, cut)
    assert not list(store.named_graphs()), "a refused load stores nothing"


def test_the_refusal_says_the_file_is_a_fragment_or_an_empty_run(tmp_path):
    """What the message has to carry, because it is the only thing an operator
    loading a partial file by hand gets: that the file holds activity metadata
    and no endpoint facts, and that both readings are possible."""
    header, _, _ = _sections()
    cut = "".join(header.splitlines(keepends=True)[:-1]).encode()

    with pytest.raises(ValueError) as raised:
        load_run(Store(str(tmp_path / "s")), cut)
    message = str(raised.value)
    assert "activity metadata" in message
    assert "no endpoint facts" in message
    assert "fragment" in message and "empty run" in message


def test_a_whole_header_with_its_terminator_still_loads(tmp_path):
    """The discriminator is the terminator, not the endpoint facts.

    A run killed immediately after its header holds no endpoint facts either,
    and it must still load: its sw:emission is the fact that tells the read
    tier the run died. So this file and the one above differ by one quad, and
    that quad is the whole difference between a run that says it stopped and
    a file that says nothing.
    """
    header, _, _ = _sections()

    result = load_run(Store(str(tmp_path / "s")), header.encode())

    assert result.quad_count == 7, "the header's run-level facts"
    assert result.discarded_bytes == 0


def test_a_truncated_header_cannot_silence_the_newest_run_detection(tmp_path):
    """Why the refusal matters, stated as the consequence it prevents.

    Both read queries pick the newest run in the store by MAX of the
    activity's prov:generatedAtTime. A truncated header carries that
    timestamp and nothing else, so admitting it would make the newest
    activity in the store a run that says nothing about finishing, and the
    unfinished-run sentence would disappear from every endpoint's page. The
    store keeps the run it had.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    header, _, _ = _sections()
    later = "".join(header.splitlines(keepends=True)[:-1]).replace(
        "2026-08-23T02:00:00Z", "2026-08-25T02:00:00Z"
    )

    with pytest.raises(ValueError, match="no endpoint facts"):
        load_run(store, later.encode())

    newest = "SELECT (MAX(?t) AS ?newest) WHERE { GRAPH ?g { ?a <http://www.w3.org/ns/prov#generatedAtTime> ?t } }"
    assert [row["newest"].value for row in store.query(newest)] == [
        "2026-08-22T16:00:00Z"
    ], "the refused fragment must not have become the store's newest activity"


# ---------------------------------------------------------------------------
# The wire format, pinned to one checked-in file
# ---------------------------------------------------------------------------
# The three terminator predicates are written in Rust and recognised here, and
# nothing in either language connects the two spellings. docs/design/
# section-terminators.md is the single source of truth both sides read:
# prober/src/emit.rs's tests assert the emitter's constants against it, and the
# test below asserts this loader's against it. Neither suite invokes the other.
WIRE_FORMAT = (
    Path(__file__).resolve().parents[2] / "docs" / "design" / "section-terminators.md"
)


def _wire_format_table() -> dict[str, str]:
    """The section-to-predicate table in WIRE_FORMAT's fenced block.

    Parsed rather than restated, because a restatement here would be a third
    copy of the table and the point of the file is that there are two.
    """
    lines = WIRE_FORMAT.read_text().splitlines()
    fences = [i for i, line in enumerate(lines) if line.startswith("```")]
    assert len(fences) == 2, f"one fenced block, found {len(fences) // 2}"
    table = {}
    for line in lines[fences[0] + 1 : fences[1]]:
        section, predicate = line.split()
        table[section] = predicate
    return table


def test_the_loader_recognises_exactly_the_documented_terminators():
    """The Python half of the wire format.

    A rename on either side used to leave both suites green while every
    partial run cut back to its header, discarding every endpoint the crash
    preserved. Both sets are asserted whole rather than by membership, so a
    fourth predicate added on one side alone reds this too.
    """
    table = _wire_format_table()
    assert set(table) == {"header", "chunk", "footer"}, (
        f"the three sections emit.rs writes, found {sorted(table)}"
    )
    assert _TERMINATOR_PREDICATES == frozenset(table.values()), (
        f"the loader recognises {sorted(_TERMINATOR_PREDICATES)}, "
        f"{WIRE_FORMAT.name} names {sorted(table.values())}"
    )


# ---------------------------------------------------------------------------
# The derived urn:sparqlwatch:current graph
# ---------------------------------------------------------------------------
# Everything below is about the graph load_run maintains beside the run
# graphs, and the three read queries read instead of computing recency at
# query time. See web/load_run.py's module docstring for the quad shape and
# the measurement that made it necessary.
NEW_SUBJECTS_FIXTURE = Path(__file__).parent / "fixtures" / "run-new-subjects.nq"
DECLINED_FIXTURE = Path(__file__).parent / "fixtures" / "run-declined.nq"
CRASHED_FIXTURE = Path(__file__).parent / "fixtures" / "run-crashed-partway.nq"
ZERO_CLASSES_FIXTURE = Path(__file__).parent / "fixtures" / "run-zero-classes.nq"
PROPERTIES_FIXTURE = Path(__file__).parent / "fixtures" / "run-properties-sample.nq"

NEW_SUBJECTS_INSTANT = "2026-08-22T20:00:00Z"
KADASTER = "https://data.kkg.kadaster.nl/query"
QLEVER = "https://qlever.dev/api/osm-planet"
ONTOP = "https://ontop.certain.ai.ustp.at/sparql"

PROV = "http://www.w3.org/ns/prov#"
DQV = "http://www.w3.org/ns/dqv#"
SW = "urn:sparqlwatch:"


def _replayed(nquads: bytes, source: str, target: str) -> bytes:
    """``nquads`` with the run instant ``source`` rewritten to ``target``.

    A byte rewrite of the instant, not of the graph name alone, because
    prober/src/emit.rs derives the graph IRI, the activity IRI, every
    measurement and not-measured subject and the prov:generatedAtTime literal
    from the one --at value. Rewriting all of them together is what makes the
    result a run the prober could have written, and rewriting only the graph
    name would leave two graphs claiming one activity.
    """
    return nquads.replace(source.encode(), target.encode())


def _current(store: Store) -> set[tuple[str, str, str]]:
    """The urn:sparqlwatch:current graph, as comparable triples."""
    return {
        (quad.subject.value, quad.predicate.value, str(quad.object))
        for quad in store.quads_for_pattern(
            None, None, None, NamedNode(SW + "current")
        )
    }


def _pointer(store: Store, endpoint: str, predicate: str) -> str | None:
    """What ``endpoint``'s ``predicate`` pointer in current names, or None."""
    found = [
        quad.object.value
        for quad in store.quads_for_pattern(
            NamedNode(endpoint),
            NamedNode(SW + predicate),
            None,
            NamedNode(SW + "current"),
        )
    ]
    assert len(found) <= 1, f"{endpoint} has {len(found)} {predicate} pointers"
    return found[0] if found else None


def _verdict_in_current(store: Store, endpoint: str, metric: str) -> list[str]:
    """``endpoint``'s dqv:value for ``metric``, read out of current alone."""
    return sorted(
        row["v"].value
        for row in store.query(
            f"""
            SELECT ?v WHERE {{ GRAPH <{SW}current> {{
              ?m <{DQV}computedOn> <{endpoint}> ;
                 <{DQV}isMeasurementOf> <{SW}metric:{metric}> ;
                 <{DQV}value> ?v .
            }} }}"""
        )
    )


def test_current_holds_no_typed_activity(tmp_path):
    """The defect that 500s every page, guarded first.

    All three read queries select their run as
    GRAPH ?run { ?activity a prov:Activity ; prov:generatedAtTime ?t } with no
    restriction on which graph, and the newest-run aggregate is unrestricted
    too. So a current graph holding a typed activity with a timestamp IS a run
    to every one of them, and it carries the newest timestamp by construction:
    both readers then raise "runs tied as most recent" and every page and every
    RDF representation becomes a 500.

    A store, not a fixture, so this holds for the shape the loader actually
    writes. The non-emptiness assertion is half the test: a current graph that
    stayed empty would satisfy the absence trivially.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    assert _current(store), "current must not be empty"
    assert not bool(store.query(
        f"ASK {{ GRAPH <{SW}current> {{ ?a a <{PROV}Activity> }} }}"
    )), "current may hold no rdf:type prov:Activity triple, ever"
    assert not bool(store.query(
        f"ASK {{ GRAPH <{SW}current> {{ ?a <{PROV}generatedAtTime> ?t }} }}"
    )), (
        "and no prov:generatedAtTime either. The aggregate needs both the type "
        "and the timestamp, so the type's absence alone already excludes "
        "current from it; keeping the timestamp out too means no question about "
        "activities, present or future, can reach into this graph by accident"
    )


def test_current_holds_the_newest_runs_facts_for_each_endpoint(tmp_path):
    """Two runs where a verdict CHANGES, so counting rows cannot pass this.

    The later run is the earlier one replayed at a later instant with
    kadaster's availability rewritten from "verified" to "absent". Both runs
    hold the same number of quads for the same three endpoints, so a current
    graph that kept the older run's facts, or merged the two, has the same size
    as the right one.
    """
    store = Store(str(tmp_path / "s"))
    earlier = NEW_SUBJECTS_FIXTURE.read_bytes()
    load_run(store, earlier)
    assert _verdict_in_current(store, KADASTER, "availability") == ["verified"]

    later_instant = "2026-08-26T00:00:00Z"
    later = _replayed(earlier, NEW_SUBJECTS_INSTANT, later_instant).replace(
        b'availability> <http://www.w3.org/ns/dqv#value> "verified"',
        b'availability> <http://www.w3.org/ns/dqv#value> "absent"',
    )
    assert later != _replayed(earlier, NEW_SUBJECTS_INSTANT, later_instant), (
        "the replay must really have changed a verdict"
    )
    result = load_run(store, later)

    assert _verdict_in_current(store, KADASTER, "availability") == ["absent"], (
        "current must hold the newer run's verdict and only it"
    )
    assert _pointer(store, KADASTER, "currentRun") == (
        f"{SW}run:{later_instant}"
    )
    assert sorted(result.advanced) == sorted([KADASTER, ONTOP, QLEVER])


def test_an_endpoint_only_in_the_older_run_keeps_its_facts(tmp_path):
    """Why current is per-endpoint rather than per-store.

    run-crashed-partway.nq wrote kadaster's chunk and died before it reached
    the other two endpoints. A sweep that never got to an endpoint must not
    erase what the last one learned about it, and at 543 endpoints that is
    every endpoint after the one a crash died on.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    result = load_run(store, CRASHED_FIXTURE.read_bytes())

    crashed_run = f"{SW}run:2026-08-23T04:00:00Z"
    assert _pointer(store, KADASTER, "currentRun") == crashed_run
    assert result.advanced == [KADASTER], (
        "only the endpoint the crashed run reached may advance"
    )
    for endpoint in (QLEVER, ONTOP):
        assert _pointer(store, endpoint, "currentRun") == (
            f"{SW}run:2026-08-22T16:00:00Z"
        ), f"{endpoint} keeps the last run that recorded anything for it"
        assert _verdict_in_current(store, endpoint, "availability"), (
            f"{endpoint}'s verdicts must still be in current"
        )


def test_reloading_the_same_run_refreshes_current(tmp_path):
    """The monotone trap, which the first draft of this design walked into.

    Under a "newer or equal is refused" rule a re-load of the same run IRI is a
    tie and does nothing, so the documented recovery from a half-written
    current graph is precisely the operation that cannot repair it. The rule is
    "refuse only a strictly newer current", so this refreshes.

    Mutating current by hand first is what makes the test bite: without it,
    a loader that skipped the whole second load would leave current already
    correct and pass.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, NEW_SUBJECTS_FIXTURE.read_bytes())
    expected = _current(store)

    store.update(f"""
        DELETE WHERE {{ GRAPH <{SW}current> {{
          ?m <{DQV}value> ?v
        }} }}""")
    assert _current(store) != expected, "the hand mutation must have changed current"

    result = load_run(store, NEW_SUBJECTS_FIXTURE.read_bytes())
    assert _current(store) == expected, (
        "re-loading the same run must restore what current holds for it"
    )
    assert sorted(result.advanced) == sorted([KADASTER, ONTOP, QLEVER])


def test_loading_an_older_run_does_not_move_current_backwards(tmp_path):
    """The other direction of the same rule, and the two pointers moving apart.

    run-declined.nq is the 18:00 sweep that measured all three endpoints and
    declined sw:metric:classes, so it published no sample. run-with-samples.nq
    is the 16:00 sweep that measured them and sampled two of them. Loading them
    in that order leaves the measurement pointer on 18:00, because 16:00 is
    older, and moves the sample pointer to 16:00, because no run had sampled
    anything yet. One notion of recency cannot express that, which is why there
    are two pointers.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, DECLINED_FIXTURE.read_bytes())
    result = load_run(store, FIXTURE.read_bytes())

    declined_run = f"{SW}run:2026-08-22T18:00:00Z"
    samples_run = f"{SW}run:2026-08-22T16:00:00Z"
    assert _pointer(store, KADASTER, "currentRun") == declined_run, (
        "an older run may not move the measurement pointer backwards"
    )
    assert _pointer(store, KADASTER, "currentSampleRun") == samples_run, (
        "and the sample pointer moves on its own, because 18:00 sampled nothing"
    )
    assert sorted(result.kept_newer) == sorted([KADASTER, ONTOP, QLEVER])
    assert result.advanced == [], "no endpoint's measurements may have advanced"
    assert sorted(result.advanced_samples) == sorted([KADASTER, ONTOP])
    assert _verdict_in_current(store, KADASTER, "classes") == [], (
        "the 18:00 sweep declined classes, so current holds no verdict for it"
    )


def test_a_newer_sweep_that_declined_classes_keeps_the_older_sample(tmp_path):
    """Stage 3-1's Critical, pinned at the loader and at both readers.

    The registry sweep declined sw:metric:classes for all 543 endpoints, so
    "the newest run that measured this endpoint" and "the newest run that
    sampled it" being different runs is the steady state and not an edge case.
    A single pointer with a single notion of recency loses the sample outright.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    load_run(store, DECLINED_FIXTURE.read_bytes())

    assert _pointer(store, KADASTER, "currentRun") == f"{SW}run:2026-08-22T18:00:00Z"
    assert _pointer(store, KADASTER, "currentSampleRun") == (
        f"{SW}run:2026-08-22T16:00:00Z"
    )

    content = endpoint_content(store, KADASTER)
    assert content.sampled is True, "the class sample must survive the newer sweep"
    assert len(content.classes) == 59, "all 59 classes, not a truncated list"
    assert content.run == f"{SW}run:2026-08-22T16:00:00Z", (
        "and it must be attributed to the sweep that took it"
    )
    measurements = endpoint_measurements(store, KADASTER)
    assert measurements.run == f"{SW}run:2026-08-22T18:00:00Z"
    assert measurements.generated_at == "2026-08-22T18:00:00Z", (
        "the run-level facts are reached through the pointer, not copied"
    )


def _shrunk(nquads: bytes, *drop: str) -> bytes:
    """``nquads`` with every line naming one of ``drop`` removed.

    What a crashed prober leaves is a prefix of the file, and the endpoints
    after the crash point are simply absent from it. Dropping their lines
    produces the same graph under the same run IRI, which is the shape that
    matters: a re-load replaces the run graph, so current is left attributing
    facts to a run that no longer states them.
    """
    kept = [
        line
        for line in nquads.splitlines(keepends=True)
        if not any(name.encode() in line for name in drop)
    ]
    return b"".join(kept)


def test_a_run_graph_that_shrinks_is_detected_and_named(tmp_path):
    """The first case the update rule cannot fix, so it must be reported.

    Load the full sweep, then the truncated file a crashed prober left under
    the same run IRI. The endpoints the truncated file dropped keep a
    sw:currentRun naming a run whose graph no longer mentions them, and no
    ordering rule can repair that: the run is not older, and the facts are
    simply gone. So the loader says which endpoints, and says to rebuild.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, NEW_SUBJECTS_FIXTURE.read_bytes())
    result = load_run(
        store, _shrunk(NEW_SUBJECTS_FIXTURE.read_bytes(), "qlever", "ontop")
    )

    assert sorted(result.drifted) == sorted([ONTOP, QLEVER]), (
        "the two endpoints the truncated file dropped must be named"
    )
    assert KADASTER not in result.drifted, "kadaster is still in the run"
    assert result.advanced == [KADASTER]


def test_a_dropped_run_graph_is_detected(tmp_path):
    """The second case, and the spec calls dropping a bad run a feature.

    One graph per run exists so that a bad run can be removed wholesale, and
    remove_graph is used in this suite today. Doing it leaves current pointing
    every endpoint of that run at a graph that is gone.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, NEW_SUBJECTS_FIXTURE.read_bytes())
    store.remove_graph(NamedNode(f"{SW}run:{NEW_SUBJECTS_INSTANT}"))

    checked = check_current(store)
    assert sorted(checked.drifted) == sorted([KADASTER, ONTOP, QLEVER]), (
        "every endpoint of the dropped run must be named"
    )
    assert not checked.ok

    # And a later load of an unrelated run reports it too, so an operator who
    # never runs the check still hears about it.
    result = load_run(store, ZERO_CLASSES_FIXTURE.read_bytes())
    assert sorted(result.drifted) == sorted([KADASTER, ONTOP, QLEVER])


def test_the_unfinished_flip_refreshes(tmp_path):
    """The third case: a run that did not finish, then the same run finished.

    This is the case the pointer design answers for free, and the test says so
    rather than asserting a copy. current names the run and the readers join to
    that run's graph for sw:emission and sw:finalised, so the footer arriving
    changes what the page says without a single quad of current moving. Copying
    those run-level facts onto the endpoint would have needed a refresh here,
    and would have put a triple in current that no run graph holds.
    """
    store = Store(str(tmp_path / "s"))
    crashed = CRASHED_FIXTURE.read_bytes()
    load_run(store, crashed)
    before = endpoint_measurements(store, KADASTER)
    assert before.run_did_not_finish is True, "the crashed run wrote no footer"

    activity = f"<{SW}activity:2026-08-23T04:00:00Z>"
    run = f"<{SW}run:2026-08-23T04:00:00Z>"
    footer = (
        f'{activity} <{SW}finalised> "true"^^'
        f"<http://www.w3.org/2001/XMLSchema#boolean> {run} .\n"
    ).encode()
    load_run(store, crashed + footer)

    after = endpoint_measurements(store, KADASTER)
    assert after.run_did_not_finish is False, (
        "the page must stop saying the sweep did not finish"
    )
    assert after.run == before.run, "and it is still the same run"


def _tied_measuring_runs() -> bytes:
    """Two run graphs measuring the same endpoint at the SAME instant.

    Hand-built rather than a fixture pair, because --at is both the run IRI and
    the timestamp, so no two files the prober writes can tie.
    """
    lines = []
    for run in ("a", "b"):
        graph = f"<{SW}test:run:{run}>"
        activity = f"<{SW}test:activity:{run}>"
        measurement = f"<{SW}test:measurement:{run}>"
        lines += [
            f"{activity} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
            f"<{PROV}Activity> {graph} .",
            f"{activity} <{PROV}generatedAtTime> "
            f'"2026-08-27T00:00:00Z"^^'
            f"<http://www.w3.org/2001/XMLSchema#dateTime> {graph} .",
            f"{measurement} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
            f"<{DQV}QualityMeasurement> {graph} .",
            f"{measurement} <{DQV}computedOn> <https://tied.example/sparql> {graph} .",
            f"{measurement} <{DQV}isMeasurementOf> <{SW}metric:availability> {graph} .",
            f'{measurement} <{DQV}value> "verified" {graph} .',
            f"{measurement} <{PROV}wasGeneratedBy> {activity} {graph} .",
        ]
    return ("\n".join(lines) + "\n").encode()


def test_the_loader_refuses_to_advance_current_past_a_tie(tmp_path):
    """Where the readers' tied-run refusal goes.

    Both readers refuse a store with two runs tied as most recent, because
    picking one silently publishes a blend of two sweeps under one run's name.
    A "strictly newer" advance rule resolves such a tie by load order instead,
    which is a behaviour change nobody asked for. So the loader refuses, and
    names both runs, so the condition is still reported rather than decided by
    whichever file happened to be second.
    """
    store = Store(str(tmp_path / "s"))
    with pytest.raises(ValueError, match="tied as most recent") as raised:
        load_run(store, _tied_measuring_runs())
    message = str(raised.value)
    assert f"{SW}test:run:a" in message, "name both runs, so the store is fixable"
    assert f"{SW}test:run:b" in message
    assert "https://tied.example/sparql" in message, "and name the endpoint"


def test_a_rebuild_from_the_run_graphs_alone_reproduces_current(tmp_path):
    """current is derived, so it must be reconstructible from the run graphs.

    Mutated by hand first, in both directions: a fact removed and a fact
    invented. A rebuild that only inserted what was missing would pass on the
    first half alone.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    load_run(store, DECLINED_FIXTURE.read_bytes())
    load_run(store, ZERO_CLASSES_FIXTURE.read_bytes())
    expected = _current(store)
    assert expected, "there must be something to rebuild"

    store.update(f"""
        DELETE {{ GRAPH <{SW}current> {{ <{KADASTER}> <{SW}currentSampleRun> ?r }} }}
        WHERE  {{ GRAPH <{SW}current> {{ <{KADASTER}> <{SW}currentSampleRun> ?r }} }} ;
        INSERT DATA {{ GRAPH <{SW}current> {{
          <{QLEVER}> <{DQV}value> "invented"
        }} }}""")
    assert _current(store) != expected

    rebuilt = rebuild_current(store)
    assert _current(store) == expected, (
        "a rebuild must reproduce current exactly, from the run graphs alone"
    )
    assert rebuilt.endpoints == 4, (
        "three endpoints from the sweeps plus the zero-classes one, "
        f"rebuilt {rebuilt.endpoints}"
    )
    assert rebuilt.runs == 3


def test_the_check_names_every_endpoint_that_drifted_and_no_others(tmp_path):
    """A derived graph that cannot be verified is a liability.

    The index this stage builds asserts things over current that a reader
    cannot cross-check by hand, so there has to be a mode that compares current
    against the run graphs and names what disagrees. Both halves are asserted:
    a clean store must come back clean, or a check that named everything would
    pass the first half.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    load_run(store, DECLINED_FIXTURE.read_bytes())

    clean = check_current(store)
    assert clean.ok, f"a freshly loaded store must be clean, got {clean.drifted}"
    assert clean.endpoints == 3

    store.update(f"""
        DELETE {{ GRAPH <{SW}current> {{ ?m <{DQV}value> ?v }} }}
        WHERE  {{ GRAPH <{SW}current> {{
          ?m <{DQV}computedOn> <{QLEVER}> ; <{DQV}value> ?v
        }} }} ;
        DELETE {{ GRAPH <{SW}current> {{ <{ONTOP}> <{SW}currentSampleRun> ?r }} }}
        WHERE  {{ GRAPH <{SW}current> {{ <{ONTOP}> <{SW}currentSampleRun> ?r }} }}""")

    drifted = check_current(store)
    assert sorted(drifted.drifted) == sorted([ONTOP, QLEVER]), (
        f"expected exactly those two, got {sorted(drifted.drifted)}"
    )
    assert not drifted.ok
    for endpoint in (ONTOP, QLEVER):
        assert drifted.drifted[endpoint], f"{endpoint} must be told what drifted"


def test_the_rebuild_and_the_check_are_reachable_from_the_command_line(
    tmp_path, capsys
):
    """Both modes get a test, because a repair path nobody can run is not one.

    The migration this stage needs is exactly this invocation: a store built
    before current existed holds run graphs and no current graph, and
    web/app.py refuses to open it until this has been run.
    """
    path = str(tmp_path / "s")
    store = Store(path)
    load_run(store, FIXTURE.read_bytes())
    store.remove_graph(NamedNode(f"{SW}current"))
    assert not store.contains_named_graph(NamedNode(f"{SW}current"))
    del store

    assert main(["--rebuild", path]) == 0
    out = capsys.readouterr().out
    assert "3 endpoints" in out, f"the rebuild must say what it did, said {out!r}"

    store = Store(path)
    assert store.contains_named_graph(NamedNode(f"{SW}current"))
    del store

    assert main(["--check", path]) == 0
    assert "no endpoint" in capsys.readouterr().out.lower()

    store = Store(path)
    store.update(f"""
        DELETE {{ GRAPH <{SW}current> {{ <{QLEVER}> <{SW}currentRun> ?r }} }}
        WHERE  {{ GRAPH <{SW}current> {{ <{QLEVER}> <{SW}currentRun> ?r }} }}""")
    del store

    assert main(["--check", path]) == 1, "a drifted store must exit non-zero"
    assert QLEVER in capsys.readouterr().out


def test_a_run_file_naming_the_derived_graph_is_refused(tmp_path):
    """current is derived and reconstructible from the run graphs alone, so it
    is never an input.

    One hand-written line naming urn:sparqlwatch:current as its graph used to
    be accepted: _parsed_graphs collected that IRI like any other, load_run
    called remove_graph on it, and the file's own triples landed in the graph
    all three read queries trust. The load then reported success with an EMPTY
    drifted list, because drifted asks which pointers name a run that no longer
    states their facts and an emptied current graph holds no pointers at all.
    So the one detector for a broken current graph reported nothing about the
    one input that breaks it.

    Refused in _parsed_graphs, which is where main() validates every file
    before Store() is opened, so the refusal happens before the store exists.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    before = _current(store)
    assert before, "the derived graph must be there to be attacked"

    injection = (
        b"<https://evil.example/sparql> <http://www.w3.org/ns/dqv#computedOn> "
        b"<https://evil.example/sparql> <urn:sparqlwatch:current> .\n"
    )
    with pytest.raises(ValueError, match="urn:sparqlwatch:current"):
        load_run(store, injection)

    assert _current(store) == before, "a refused load must not touch current"

    # And from the command line, before the store directory is created at all.
    store_path = tmp_path / "fresh"
    run_path = tmp_path / "inject.nq"
    run_path.write_bytes(injection)
    with pytest.raises(ValueError, match="urn:sparqlwatch:current"):
        main([str(store_path), str(run_path)])
    assert not store_path.exists(), (
        "the refusal must come before Store() creates the directory"
    )


def test_the_command_line_reports_drift_and_exits_non_zero(tmp_path, capsys):
    """LoadResult.drifted reaches the operator, or it may as well not exist.

    load_run() computes drifted correctly and 41 tests exercise it, and main()
    used to print the quad count, the replaced graphs and the discarded bytes
    and nothing else, at exit 0. So loading the truncated file a crashed prober
    leaves under the same run IRI printed one cheerful line while the endpoints
    that file dropped went on publishing verdicts attributed to a run that no
    longer states them, including the two ASSERTIVE values, "verified" and
    "absent". Only a separate --check found it, and nothing told anyone to run
    one.

    Non-zero, and not merely printed. The load itself succeeded, but the store
    it leaves cannot be served as it stands: the facts on the site are
    attributed to a run that no longer states them, and --check exits 1 on
    exactly this condition. A deploy step that reads the exit status is the
    reader this is for.
    """
    path = str(tmp_path / "s")
    full = tmp_path / "full.nq"
    full.write_bytes(NEW_SUBJECTS_FIXTURE.read_bytes())
    shrunk = tmp_path / "shrunk.nq"
    shrunk.write_bytes(_shrunk(NEW_SUBJECTS_FIXTURE.read_bytes(), "qlever", "ontop"))

    assert main([path, str(full)]) == 0
    assert "drifted" not in capsys.readouterr().out.lower(), (
        "a clean load must not mention drift"
    )

    assert main([path, str(shrunk)]) == 1, (
        "a load that leaves current attributing facts to a run that no longer "
        "states them must not exit 0"
    )
    printed = capsys.readouterr()
    said = printed.out + printed.err
    for endpoint in (ONTOP, QLEVER):
        assert endpoint in said, f"{endpoint} drifted and was not named"
    assert KADASTER not in said, "kadaster is still in the run"
    assert "--rebuild" in said, "and the repair must be named"


def test_the_command_line_says_when_it_refused_to_move_current_backwards(
    tmp_path, capsys
):
    """kept_newer, the other field main() threw away.

    An out-of-order load is a real operator mistake: the runs are files in a
    directory and a shell glob orders them by name, so a re-load of an older
    run after a newer one refuses to move the pointer and used to say nothing
    at all about having refused. Printed, but exit 0: the store is correct,
    nothing needs repairing, and the operator only needs to know that the file
    they just named is not what the site is showing.
    """
    path = str(tmp_path / "s")
    older = tmp_path / "older.nq"
    older.write_bytes(FIXTURE.read_bytes())
    newer = tmp_path / "newer.nq"
    newer.write_bytes(NEW_SUBJECTS_FIXTURE.read_bytes())

    assert main([path, str(newer)]) == 0
    capsys.readouterr()
    assert main([path, str(older)]) == 0, "refusing to go backwards is not a failure"
    said = capsys.readouterr().out
    assert KADASTER in said, "the endpoint whose pointer was left alone"
    assert "newer" in said.lower(), f"say why it was left alone, said {said!r}"


def test_the_check_counts_the_endpoints_it_actually_compared(tmp_path):
    """The denominator has to be a number the numerator can sit inside.

    CheckResult.endpoints used to be the count the RUN GRAPHS expect, and the
    drifted set includes endpoints only current knows about, so a dropped run
    graph made --check print "3 of 0 endpoints drifted" and a shrunk one "7 of
    2". Its own docstring says the field is how many endpoints were compared, so
    "nothing drifted" can be told from "nothing was looked at", and an endpoint
    whose pointer is dangling was compared: that is how it came to be named.
    """
    store = Store(str(tmp_path / "s"))
    load_run(store, NEW_SUBJECTS_FIXTURE.read_bytes())
    store.remove_graph(NamedNode(f"{SW}run:{NEW_SUBJECTS_INSTANT}"))

    checked = check_current(store)
    assert len(checked.drifted) == 3, sorted(checked.drifted)
    assert checked.endpoints == 3, (
        "no run graph mentions any of them any more, and all three were "
        "compared and named"
    )
    assert len(checked.drifted) <= checked.endpoints

"""Tests for load_run: replacing a run's graph rather than merging into it.

See web/load_run.py's module docstring for the two things this module
exists to prevent: a changed re-run silently merging into the old graph
(producing a measurement with two dqv:value triples), and a malformed file
destroying an existing run before the parse failure is noticed.
"""

from pathlib import Path

import pytest
from pyoxigraph import DefaultGraph, NamedNode, RdfFormat, Store, parse

from load_run import LoadResult, load_run, main

FIXTURE = Path(__file__).parent / "fixtures" / "run-with-samples.nq"
TWO_SWEEPS_FIXTURE = Path(__file__).parent / "fixtures" / "run-two-sweeps.nq"


def test_loading_the_same_run_twice_leaves_one_graph(tmp_path):
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    first = len(store)
    load_run(store, FIXTURE.read_bytes())
    assert len(store) == first
    assert len(list(store.named_graphs())) == 1


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
    assert len(list(store.named_graphs())) == 2


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
    assert len(list(store.named_graphs())) == 3, "the unrelated graph plus both sweep graphs"

    second = load_run(store, TWO_SWEEPS_FIXTURE.read_bytes())
    assert len(second.replaced) == 2, "both named graphs were replaced"
    assert len(list(store.named_graphs())) == 3, "no graph was added or lost"
    assert len(list(store.quads_for_pattern(None, None, None, other_graph))) == other_count, (
        "the unrelated graph must be untouched by a load that does not name it"
    )


def test_load_result_reports_the_quad_count(tmp_path):
    store = Store(str(tmp_path / "s"))
    result = load_run(store, FIXTURE.read_bytes())
    assert result.quad_count == 278
    assert isinstance(result, LoadResult)


def test_an_interrupted_insert_says_what_the_store_is_left_holding(tmp_path, monkeypatch):
    """The window web/load_run.py admits it cannot close. remove_graph and extend
    are two store operations and pyoxigraph 0.5.9 has no transaction API, so an
    insert stopped by a full disk or an OOM kill leaves the graphs dropped and
    nothing inserted: measured at 278 quads going to zero. It cannot be made
    atomic, so it must be loud instead. The count is what makes it actionable,
    because the .nq file is the source of truth and a run graph is immutable: an
    operator told '0 of 278' re-runs the load and gets the run back exactly, as
    the end of this test does."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())

    def full_disk(self, quads):
        raise OSError("simulated full disk during insert")

    monkeypatch.setattr(Store, "extend", full_disk)
    with pytest.raises(RuntimeError, match="loaded 0 of 278 quads") as raised:
        load_run(store, FIXTURE.read_bytes())
    assert isinstance(raised.value.__cause__, OSError), (
        "the underlying failure must still be reachable, not swallowed"
    )
    assert len(store) == 0, "the window is real: the run is gone until it is re-loaded"

    monkeypatch.undo()
    again = load_run(store, FIXTURE.read_bytes())
    assert again.quad_count == 278
    assert len(store) == 278, "re-loading the same file restores the run exactly"
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

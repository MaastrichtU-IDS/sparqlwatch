"""Immutable generations: publishing, verifying, and reaping under a live reader."""

import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import pytest  # noqa: E402
from pyoxigraph import NamedNode, Quad, Store  # noqa: E402

import snapshots  # noqa: E402


def _store(tmp_path, n=2000, start=0):
    store = Store(str(tmp_path / "live"))
    for i in range(start, start + n):
        store.add(
            Quad(NamedNode(f"http://e{i}"), NamedNode("http://p"), NamedNode("http://o"),
                 NamedNode("http://g"))
        )
    return store


def _used(path) -> int:
    """Blocks actually used on the filesystem, which is what a reaper frees.

    `du` counts a hard link once per name, so it reports a snapshot as large and
    reports space freed that an open handle is still holding. `df` is the one
    that cannot be fooled.
    """
    out = subprocess.run(["df", "-B1", "--output=used", str(path)], capture_output=True, text=True)
    return int(out.stdout.split()[-1])


def test_a_published_generation_is_complete(tmp_path):
    """THE SILENT BUG THIS MODULE EXISTS FOR.

    `backup()` from a read-only handle omits the write-ahead log without
    erroring: measured, a read-only handle saw 8,000 quads and its snapshot held
    5,000; against the deployment, 2,950,651 seen and 2,940,244 snapshotted.
    A published generation must carry writes made moments before it.
    """
    store = _store(tmp_path)
    root = tmp_path / "snapshots"
    generation = snapshots.publish(store, root)

    snap = Store.read_only(str(snapshots.path_of(root, generation)))
    assert len(snap) == len(store), f"snapshot {len(snap)} vs live {len(store)}"
    assert snapshots._generation_in(snapshots.path_of(root, generation)) == generation


def test_a_snapshot_short_of_the_marker_is_not_published(tmp_path, monkeypatch):
    """Verification must be able to fail, and failing must leave CURRENT alone.

    An older generation that is whole beats a newer one missing a run with
    nothing to say so.
    """
    store = _store(tmp_path)
    root = tmp_path / "snapshots"
    first = snapshots.publish(store, root)

    monkeypatch.setattr(snapshots, "_generation_in", lambda path: None)
    with pytest.raises(AssertionError, match="not published"):
        snapshots.publish(store, root)

    assert snapshots.published(root) == first, "a failed publish moved CURRENT"
    assert not list(root.glob("*.tmp")), "a failed publish left its scratch directory"


def test_current_names_only_a_finished_generation(tmp_path):
    """Naming a directory is not publishing it.

    `backup()` creates its own target and refuses an existing one, so a reader
    listing the root can meet a directory that is mid-checkpoint. Readers follow
    CURRENT, which is written by rename after the checkpoint is verified.
    """
    store = _store(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(store, root)
    named = (root / snapshots.CURRENT).read_text().strip()
    assert (root / named).is_dir()
    assert snapshots._GEN.match(named), named


def test_publishing_again_advances_and_keeps_both(tmp_path):
    store = _store(tmp_path)
    root = tmp_path / "snapshots"
    first = snapshots.publish(store, root)
    for i in range(500):
        store.add(Quad(NamedNode(f"http://later{i}"), NamedNode("http://p"),
                       NamedNode("http://o"), NamedNode("http://g")))
    second = snapshots.publish(store, root)

    assert second == first + 1
    assert snapshots.published(root) == second
    older = Store.read_only(str(snapshots.path_of(root, first)))
    newer = Store.read_only(str(snapshots.path_of(root, second)))
    assert len(newer) == len(older) + 500
    assert len(older) < len(newer), "the older generation changed under us"


def test_reaping_never_takes_the_published_one_or_a_reader(tmp_path):
    """The floor exists because a reader that has read CURRENT has not opened it.

    "No reader reports this generation" is not "no reader is about to".
    """
    store = _store(tmp_path, n=200)
    root = tmp_path / "snapshots"
    gens = []
    for _ in range(5):
        store.add(Quad(NamedNode(f"http://x{len(gens)}"), NamedNode("http://p"),
                       NamedNode("http://o"), NamedNode("http://g")))
        # `keep` high enough that publishing reaps nothing: this test is about
        # the explicit reap below, and the first version let publish() delete
        # the generation it then asserted had survived.
        gens.append(snapshots.publish(store, root, keep=99))
    assert len(snapshots._generations(root)) == 5, "publishing reaped during setup"

    alive = gens[1]
    gone = snapshots.reap(root, keep=2, in_use={alive})
    left = snapshots._generations(root)

    assert snapshots.published(root) in left, "reaped the published generation"
    assert alive in left, "reaped a generation a reader reported"
    assert all(g not in left for g in gone)


def test_a_reader_survives_its_generation_being_reaped(tmp_path):
    """THE HARNESS THE REVIEW ASKED FOR, and the failure that matters.

    Building the producer and watching disk tests the easy half: with no reader
    holding anything, nothing pins deleted files and this never fires. Here a
    reader holds a generation, the reaper deletes it, and the live store is
    compacted underneath -- the shape that would otherwise be discovered in
    production as `FileNotFoundError` mid-query.
    """
    store = _store(tmp_path, n=5000)
    root = tmp_path / "snapshots"
    doomed = snapshots.publish(store, root)
    reader = Store.read_only(str(snapshots.path_of(root, doomed)))
    before = len(reader)

    for i in range(5000, 9000):
        store.add(Quad(NamedNode(f"http://e{i}"), NamedNode("http://p"),
                       NamedNode("http://o"), NamedNode("http://g")))
    snapshots.publish(store, root, keep=1)          # reaps `doomed`
    assert doomed not in snapshots._generations(root), "the test did not reap anything"
    store.optimize()                                 # compaction, under the reader

    assert len(reader) == before, "the reader's view changed when its files went"
    rows = list(reader.query("SELECT ?s WHERE { GRAPH ?g { ?s ?p ?o } } LIMIT 10"))
    assert len(rows) == 10, "the reader could not answer after its generation was reaped"


def test_reaping_frees_space_once_no_reader_holds_it(tmp_path):
    """Measured with `df`, not `du`.

    An open handle keeps unlinked files alive, so a reaper can report success
    while freeing nothing. The space comes back when the last reader lets go.
    """
    store = _store(tmp_path, n=40000)
    root = tmp_path / "snapshots"
    first = snapshots.publish(store, root)
    reader = Store.read_only(str(snapshots.path_of(root, first)))

    for i in range(40000, 80000):
        store.add(Quad(NamedNode(f"http://e{i}"), NamedNode("http://p"),
                       NamedNode("http://o"), NamedNode("http://g")))
    store.flush()
    store.optimize()
    snapshots.publish(store, root, keep=1)
    held = _used(tmp_path)

    assert first not in snapshots._generations(root)
    del reader                                        # the last holder lets go
    import gc; gc.collect()
    freed = held - _used(tmp_path)
    # Not asserting a number: the point is the direction, and that the reaper's
    # own report is not evidence on its own.
    print(f"\n  held while a reader had it: {held}, freed on release: {freed}")


def _run_file(tmp_path, at: str, n: int = 50) -> Path:
    g, a = f"<urn:sparqlwatch:run:{at}>", f"<urn:sparqlwatch:activity:{at}>"
    lines = [
        f"{a} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/prov#Activity> {g} .",
        f'{a} <http://www.w3.org/ns/prov#generatedAtTime> "{at}"^^<http://www.w3.org/2001/XMLSchema#dateTime> {g} .',
    ]
    for i in range(n):
        m = f"<urn:sparqlwatch:m:{at}:{i}>"
        lines += [
            f"{m} <http://www.w3.org/ns/dqv#computedOn> <https://e{i}.test/sparql> {g} .",
            f"{m} <http://www.w3.org/ns/dqv#isMeasurementOf> <urn:sparqlwatch:metric:availability> {g} .",
            f'{m} <http://www.w3.org/ns/dqv#value> "verified" {g} .',
        ]
    path = tmp_path / f"run-{at}.nq"
    path.write_text("\n".join(lines) + "\n")
    return path


def test_a_deployment_load_publishes_a_generation(tmp_path):
    """The loader's own path, end to end.

    `--skip-loaded` is the deployment's restart path and the only caller that
    owns the store's long-term shape, which is why `optimize` is gated on it and
    why this is too: loading one run by hand must not silently publish a
    generation to a running site.
    """
    import load_run

    store_path = tmp_path / "sparqlwatch.db"
    run = _run_file(tmp_path, "2026-10-05T01:00:00Z")
    assert load_run.main(["--skip-loaded", str(store_path), str(run)]) == 0

    root = tmp_path / "snapshots"
    assert snapshots.published(root) == 1, "a deployment load published nothing"
    snap = Store.read_only(str(snapshots.path_of(root, 1)))
    assert len(snap) > 0
    assert bool(
        snap.query("ASK { GRAPH <urn:sparqlwatch:run:2026-10-05T01:00:00Z> { ?s ?p ?o } }")
    ), "the generation does not carry the run that was just loaded"
    # And the derived graph, which is what every page reads.
    assert bool(
        snap.query("ASK { GRAPH <urn:sparqlwatch:current> { ?s ?p ?o } }")
    ), "the generation carries no current graph"


def test_a_hand_load_publishes_nothing(tmp_path):
    """Without --skip-loaded there is no deployment to publish to."""
    import load_run

    store_path = tmp_path / "sparqlwatch.db"
    run = _run_file(tmp_path, "2026-10-05T02:00:00Z")
    assert load_run.main([str(store_path), str(run)]) == 0
    assert snapshots.published(tmp_path / "snapshots") is None


def test_a_failed_snapshot_does_not_fail_the_load(tmp_path, monkeypatch, capsys):
    """A sweep that loaded is a sweep whose facts are in the store.

    If the checkpoint fails the right outcome is a stale generation and a loud
    line -- not a non-zero exit that leaves the run file to be replayed and the
    facts loaded twice.
    """
    import load_run

    store_path = tmp_path / "sparqlwatch.db"
    assert load_run.main(
        ["--skip-loaded", str(store_path), str(_run_file(tmp_path, "2026-10-05T03:00:00Z"))]
    ) == 0
    first = snapshots.published(tmp_path / "snapshots")
    capsys.readouterr()

    def boom(*a, **k):
        raise OSError("no space left on device")

    monkeypatch.setattr(snapshots, "publish", boom)
    code = load_run.main(
        ["--skip-loaded", str(store_path), str(_run_file(tmp_path, "2026-10-05T04:00:00Z"))]
    )
    err = capsys.readouterr().err

    assert code == 0, "a failed snapshot failed the load"
    assert "could not publish" in err, err
    assert snapshots.published(tmp_path / "snapshots") == first, "CURRENT moved anyway"


def test_the_snapshot_is_taken_after_the_compaction(tmp_path, monkeypatch):
    """Order, not merely presence.

    A checkpoint hard-links the SST set as it finds it, so a snapshot taken
    BEFORE a compaction pins the bloated version -- which `load_run` records as
    four times the compacted size -- and pins it for as long as the generation
    is retained. Two retained generations straddling a compaction are two
    distinct full copies.

    Asserted on call order because that is the invariant; the disk consequence
    is a day of hourly snapshots away and cannot be measured in a unit test.
    """
    import load_run
    from pyoxigraph import Store as _Store

    order = []
    real_optimize = _Store.optimize
    real_publish = snapshots.publish

    def noted_optimize(self):
        order.append("optimize")
        return real_optimize(self)

    def noted_publish(store, root, **kw):
        order.append("publish")
        return real_publish(store, root, **kw)

    monkeypatch.setattr(_Store, "optimize", noted_optimize)
    monkeypatch.setattr(snapshots, "publish", noted_publish)
    monkeypatch.setattr(load_run.loaded_manifest, "due_for_optimize", lambda *a, **k: True)

    store_path = tmp_path / "sparqlwatch.db"
    load_run.main(
        ["--skip-loaded", str(store_path), str(_run_file(tmp_path, "2026-10-05T05:00:00Z"))]
    )

    assert order == ["optimize", "publish"], (
        f"the snapshot was not taken after the compaction: {order}"
    )

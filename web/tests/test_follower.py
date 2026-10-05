"""Following CURRENT: taking a new generation without ever serving half of one."""

import gc
import logging
import os
import shutil
import sys
import threading
import time
import weakref
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import pytest  # noqa: E402
from pyoxigraph import NamedNode, Quad, Store  # noqa: E402

import snapshots  # noqa: E402


def _live(tmp_path, n=50, start=0):
    store = Store(str(tmp_path / "live"))
    _add(store, n, start)
    return store


def _add(store, n, start):
    for i in range(start, start + n):
        store.add(
            Quad(NamedNode(f"http://e{i}"), NamedNode("http://p"), NamedNode("http://o"),
                 NamedNode("http://g"))
        )


def _count(store) -> int:
    return len(list(store.quads_for_pattern(None, NamedNode("http://p"), None, None)))


def test_a_follower_serves_the_published_generation(tmp_path):
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    published = snapshots.publish(live, root)

    follower = snapshots.Follower(root)
    generation, store = follower.pinned()

    assert generation == published
    assert _count(store) == 50


def test_a_follower_with_nothing_published_says_so(tmp_path):
    """Not an empty store. An empty store answers every question wrongly."""
    with pytest.raises(RuntimeError, match="no published generation"):
        snapshots.Follower(tmp_path / "snapshots")


def test_a_follower_takes_the_newer_generation(tmp_path):
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)
    follower = snapshots.Follower(root)

    _add(live, 25, 50)
    second = snapshots.publish(live, root)

    assert follower.refresh() == second
    generation, store = follower.pinned()
    assert generation == second
    assert _count(store) == 75


def test_refresh_is_a_no_op_when_current_has_not_moved(tmp_path):
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)
    follower = snapshots.Follower(root)
    before = follower.pinned()

    assert follower.refresh() is None
    assert follower.pinned() is before, "an unchanged CURRENT must not reopen the store"


def test_a_pinned_generation_survives_a_swap_underneath_it(tmp_path):
    """THE INVARIANT A PAGE DEPENDS ON.

    A request reads the handle once and assembles its whole page from that one
    generation. If a swap could change what an in-flight request sees, a page
    could carry an index from one generation and a chart from the next -- two
    internally consistent halves that disagree.
    """
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)
    follower = snapshots.Follower(root)

    generation, pinned = follower.pinned()      # a request starts here

    _add(live, 25, 50)
    snapshots.publish(live, root)
    follower.refresh()                          # and the world moves on

    assert _count(pinned) == 50, "the in-flight request saw the swap"
    assert follower.pinned()[0] == generation + 1


def test_the_old_handle_is_released_on_a_swap(tmp_path):
    """The leak the design names: a handle kept alive pins unlinked SST files.

    `du` reports the reap as a success while `df` does not move. This is the
    follower's half -- that it keeps no reference of its own once the swap is
    done. The disk half, that released handles actually give the blocks back,
    is `test_reaping_frees_space_once_no_reader_holds_it`, which measures `df`
    against a real store.

    Asserted against an injected handle because `pyoxigraph.Store` cannot be
    weak-referenced, which is also why the disk test has to exist separately.
    """
    class Handle:
        def __init__(self, path):
            self.path = path

    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)
    follower = snapshots.Follower(root, open_store=Handle)

    dead = weakref.ref(follower.pinned()[1])
    assert dead() is not None

    _add(live, 25, 50)
    snapshots.publish(live, root)
    follower.refresh()
    gc.collect()

    assert dead() is None, "the follower is still holding the previous generation"


def test_the_new_generation_is_warmed_before_it_is_served(tmp_path):
    """Order, and the reason the swap is one assignment.

    Warming after the swap would serve the first requests of every hour out of
    a cold cache -- measured at 4.7-30.3s for a cold `build_payload`. So the
    warm runs on the new handle while the old one is still the one being
    served, and the handover is the single assignment that follows.
    """
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    first = snapshots.publish(live, root)

    holder = {}
    seen = []

    def warm(store):
        serving = holder["follower"].pinned() if "follower" in holder else None
        seen.append((_count(store), serving[0] if serving else None))

    follower = snapshots.Follower(root, prewarm=warm)
    holder["follower"] = follower

    _add(live, 25, 50)
    second = snapshots.publish(live, root)
    follower.refresh()

    assert seen == [(75, first)], (
        "the warm must run on the NEW generation while the OLD one is still "
        "served, and must not run on the first open, where there is no older "
        "generation and the caller is still starting up"
    )
    assert follower.pinned()[0] == second


def test_a_generation_that_cannot_be_opened_leaves_the_old_one_serving(tmp_path):
    """A generation can be reaped between reading CURRENT and opening it.

    Serving the previous generation is correct and the site stays up; failing
    the refresh and taking the site down to report it is not.
    """
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    first = snapshots.publish(live, root)
    follower = snapshots.Follower(root)

    _add(live, 25, 50)
    snapshots.publish(live, root)

    def gone(path):
        raise FileNotFoundError(path)

    follower._open = gone
    assert follower.refresh() is None
    assert follower.pinned()[0] == first
    assert isinstance(follower.last_error, FileNotFoundError)


def test_a_first_open_that_fails_is_not_swallowed(tmp_path):
    """With nothing already serving there is no 'keep the old one' to fall back on."""
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    def gone(path):
        raise FileNotFoundError(path)

    with pytest.raises(FileNotFoundError):
        snapshots.Follower(root, open_store=gone)


def test_the_poll_thread_follows_and_stops(tmp_path):
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    swaps = []
    followed = threading.Event()

    def swapped(generation):
        swaps.append(generation)
        if len(swaps) >= 2:          # the first is the open in __init__
            followed.set()

    follower = snapshots.Follower(root, on_swap=swapped)
    follower.start(interval=0.01)
    try:
        _add(live, 25, 50)
        second = snapshots.publish(live, root)
        assert followed.wait(timeout=20), "the poll thread never took the new generation"
        assert follower.pinned()[0] == second
    finally:
        follower.stop()

    assert not follower.running


def test_two_refreshes_at_once_open_the_generation_once(tmp_path):
    """Two threads seeing the same new CURRENT must not both open it.

    The loser of that race would drop its handle immediately, which is a wasted
    open; the real cost is that the swap would run twice and the second would
    release a generation a request had just pinned.
    """
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    opens = []
    lock = threading.Lock()
    real = Store.read_only

    def counted(path):
        with lock:
            opens.append(path)
        return real(path)

    follower = snapshots.Follower(root, open_store=counted)
    _add(live, 25, 50)
    snapshots.publish(live, root)
    opens.clear()

    start = threading.Barrier(4)

    def go():
        start.wait()
        follower.refresh()

    threads = [threading.Thread(target=go) for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=20)
        assert not t.is_alive(), "a refresh never returned"

    assert len(opens) == 1, f"opened the same generation {len(opens)} times"


# ---------------------------------------------------------------------------
# The site wired to a follower
# ---------------------------------------------------------------------------
# These drive `app.get_store` for real, which almost nothing else does: every
# other test replaces it through `app.dependency_overrides`. What is under test
# here is the wiring, not the pages.


def _loaded(tmp_path, at="2026-10-05T14:00:00Z"):
    """A store built the way the deployment builds one, generations included."""
    import load_run

    g, a = f"<urn:sparqlwatch:run:{at}>", f"<urn:sparqlwatch:activity:{at}>"
    lines = [
        f"{a} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/prov#Activity> {g} .",
        f'{a} <http://www.w3.org/ns/prov#generatedAtTime> "{at}"^^<http://www.w3.org/2001/XMLSchema#dateTime> {g} .',
    ]
    for i in range(5):
        m = f"<urn:sparqlwatch:m:{at}:{i}>"
        lines += [
            f"{m} <http://www.w3.org/ns/dqv#computedOn> <https://e{i}.test/sparql> {g} .",
            f"{m} <http://www.w3.org/ns/dqv#isMeasurementOf> <urn:sparqlwatch:metric:availability> {g} .",
            f'{m} <http://www.w3.org/ns/dqv#value> "verified" {g} .',
        ]
    run = tmp_path / f"run-{at}.nq"
    run.write_text("\n".join(lines) + "\n")
    store_path = tmp_path / "sparqlwatch.db"
    load_run.main(["--skip-loaded", str(store_path), str(run)])
    return store_path, run


def test_the_site_serves_from_a_generation_not_the_live_store(tmp_path, monkeypatch):
    import app

    store_path, _ = _loaded(tmp_path)
    monkeypatch.setenv(app.STORE_PATH_VARIABLE, str(store_path))
    app._FOLLOWERS.clear()

    store = app.get_store()
    follower = app._follower()
    try:
        assert follower is not None, "the site opened the live store despite a generation"
        assert store is follower.pinned()[1]
    finally:
        if follower is not None:
            follower.stop()
        app._FOLLOWERS.clear()


def test_a_store_with_no_generations_still_serves(tmp_path, monkeypatch):
    """The local case. `load_run.py` publishes only on --skip-loaded, so a store
    built by hand has no generations and must still produce a site."""
    import app

    store_path, _ = _loaded(tmp_path)
    shutil.rmtree(tmp_path / "snapshots")
    monkeypatch.setenv(app.STORE_PATH_VARIABLE, str(store_path))
    app._FOLLOWERS.clear()

    try:
        assert app._follower() is None
        assert app.get_store() is not None
    finally:
        app._FOLLOWERS.clear()


def test_the_site_follows_a_newly_published_generation(tmp_path, monkeypatch):
    import app
    import load_run

    store_path, _ = _loaded(tmp_path)
    monkeypatch.setenv(app.STORE_PATH_VARIABLE, str(store_path))
    app._FOLLOWERS.clear()

    follower = app._follower()
    try:
        before = follower.pinned()[0]
        _, run = _loaded(tmp_path, at="2026-10-05T15:00:00Z")
        assert load_run.main(["--skip-loaded", str(store_path), str(run)]) == 0

        assert follower.refresh() == before + 1
        assert app.get_store() is follower.pinned()[1]
    finally:
        follower.stop()
        app._FOLLOWERS.clear()


def test_the_store_keyed_caches_hold_no_more_than_the_generations_kept(tmp_path):
    """The leak, as a number rather than a comment.

    These caches key on the store HANDLE and so keep it alive. A handle kept
    alive pins the unlinked files of a reaped generation, which is why the
    bound has to match what `load_run._SNAPSHOT_KEEP` retains rather than being
    whatever it was when the handle could never change.
    """
    import app
    import load_run

    assert app.endpoint_index.cache_info().maxsize == load_run._SNAPSHOT_KEEP
    assert app.fleet_history.cache_info().maxsize == load_run._SNAPSHOT_KEEP
    assert app._cached_build_payload.cache_info().maxsize == load_run._SNAPSHOT_KEEP
    assert app._opened_store.cache_info().maxsize == 1


# --- a pipeline that has stopped ---------------------------------------------
#
# These are about the failure that got quieter when the loader moved off the site
# pod. Before, a loader that could not open the store crashlooped the pod and was
# impossible to miss. Now it is a failed CronJob and a site that goes on serving
# the last generation it took, indefinitely and without complaint. The follower
# is the one process always running and always looking at CURRENT, so it is where
# the stall gets noticed.


def _stall(root, hours):
    """Backdate CURRENT, which is how old the generation being served is."""
    at = time.time() - hours * 3600
    os.utime(root / snapshots.CURRENT, (at, at))


def test_a_generation_published_just_now_is_not_stale(tmp_path):
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    said = []
    follower = snapshots.Follower(root, on_stale=lambda g, age: said.append((g, age)))

    assert follower.published_age() < 60
    assert follower.check_stale() is None
    assert said == []


def test_a_store_that_stopped_being_republished_is_reported(tmp_path):
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    generation = snapshots.publish(live, root)

    said = []
    follower = snapshots.Follower(root, on_stale=lambda g, age: said.append((g, age)))
    _stall(root, hours=3)

    assert follower.check_stale() == pytest.approx(3 * 3600, abs=60)
    assert len(said) == 1
    assert said[0][0] == generation
    assert said[0][1] == pytest.approx(3 * 3600, abs=60)


def test_a_stall_just_short_of_the_threshold_is_not_reported(tmp_path):
    """The hourly sweep plus a slow run is not yet a pipeline that has stopped."""
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    said = []
    follower = snapshots.Follower(root, on_stale=lambda g, age: said.append(g))
    _stall(root, hours=1.9)

    assert follower.check_stale() is None
    assert said == []


def test_a_stall_is_said_once_and_not_on_every_poll(tmp_path):
    """At a 30s poll, saying it every time is 120 lines an hour and no signal."""
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    said = []
    follower = snapshots.Follower(root, on_stale=lambda g, age: said.append(g))
    _stall(root, hours=3)

    for _ in range(20):
        follower.check_stale()

    assert len(said) == 1


def test_a_stall_still_going_an_hour_later_is_said_again(tmp_path, monkeypatch):
    """Saying it once and never again is a line nobody will be looking at yet.

    The repeat is what `kubectl logs --tail` can still see hours later, and the
    age it carries is how the reader learns whether this is new or has been
    going all night.
    """
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    clock = [1000.0]
    monkeypatch.setattr(snapshots.time, "monotonic", lambda: clock[0])

    said = []
    follower = snapshots.Follower(root, on_stale=lambda g, age: said.append(age))
    _stall(root, hours=3)

    assert follower.check_stale() is not None
    clock[0] += 59 * 60
    assert follower.check_stale() is None, "said twice inside the hour"
    clock[0] += 2 * 60
    assert follower.check_stale() is not None, "an hour on, and still silent"
    assert len(said) == 2


def test_a_new_generation_arms_the_report_again(tmp_path):
    """A stall that was reported, recovered, and came back is news a second time."""
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    said = []
    follower = snapshots.Follower(root, on_stale=lambda g, age: said.append(g))
    _stall(root, hours=3)
    follower.check_stale()
    assert len(said) == 1

    _add(live, 25, 50)
    second = snapshots.publish(live, root)
    assert follower.refresh() == second
    assert follower.check_stale() is None     # fresh again, and nothing said

    _stall(root, hours=3)
    assert follower.check_stale() is not None
    assert said == [1, second]


def test_a_current_that_cannot_be_read_is_not_reported_as_a_stall(tmp_path):
    """Unreadable is a different fault, and guessing an age would be inventing one."""
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    said = []
    follower = snapshots.Follower(root, on_stale=lambda g, age: said.append(g))
    (root / snapshots.CURRENT).unlink()

    assert follower.published_age() is None
    assert follower.check_stale() is None
    assert said == []


def test_a_follower_with_nowhere_to_report_does_not_try(tmp_path):
    """The local case: no reporter wired, and a stall must not become a crash."""
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    follower = snapshots.Follower(root)
    _stall(root, hours=9)

    assert follower.check_stale() is None


def test_a_report_that_raises_does_not_stop_the_follower(tmp_path):
    """The poll thread dying is worse than the stall it was trying to report:
    the site would serve one generation forever with nothing to say so."""
    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    def boom(generation, age):
        raise RuntimeError("the logger is on fire")

    follower = snapshots.Follower(root, on_stale=boom, stale_after=0)
    follower.start(interval=0.01)
    try:
        _add(live, 25, 50)
        second = snapshots.publish(live, root)
        deadline = time.time() + 5
        while follower.pinned()[0] != second and time.time() < deadline:
            time.sleep(0.01)
        assert follower.pinned()[0] == second, "the poll thread stopped following"
        assert follower.running
    finally:
        follower.stop()


def test_the_site_says_so_when_the_store_stops_being_republished(tmp_path, monkeypatch, caplog):
    import app

    live = _live(tmp_path)
    root = tmp_path / "snapshots"
    snapshots.publish(live, root)

    monkeypatch.setenv("SPARQLWATCH_STORE", str(tmp_path / "live"))
    app._FOLLOWERS.clear()
    app._opened_store.cache_clear()
    follower = app._follower()
    try:
        _stall(root, hours=5)
        with caplog.at_level(logging.WARNING, logger="uvicorn.error"):
            assert follower.check_stale() is not None
        assert "5.0 hours" in caplog.text
        assert "sweep" in caplog.text
    finally:
        follower.stop()
        app._FOLLOWERS.clear()

"""What the three read paths answered before ``current`` existed, frozen.

Stage 3-2 moves all three read queries off "compute recency at query time"
and onto the derived urn:sparqlwatch:current graph. The failure that has to be
caught is not a wrong row. It is a page that answers "we know nothing about
this endpoint" for an endpoint it fully describes, or a 500 from a reader
finding two runs tied as most recent because ``current`` looked like a run.
Neither shows up in a test that asserts one endpoint's one field.

So web/tools/capture_reader_golden.py froze all three readers' answers, over
every store web/tests/conftest.py builds and every endpoint any run graph in
those stores mentions, into web/tests/fixtures/reader-golden.json. This
asserts them. A change in any answer fails here with the store, the endpoint
and the field named, and the golden is regenerated only to record a change
that is intended.

The absent endpoint in every store is part of the freeze, not filler: "no run
mentions this one" is exactly what a reader over a missing ``current`` graph
would say about every endpoint, so it has to stay the answer for this one and
only this one.
"""

from __future__ import annotations

import json
from dataclasses import asdict
from pathlib import Path

import pytest
from pyoxigraph import NamedNode, RdfFormat, Store, Variable, serialize

from endpoint_content import endpoint_content
from endpoint_measurements import endpoint_measurements
from load_run import load_run
from queries import read_query

import conftest

GOLDEN_FILE = Path(__file__).parent / "fixtures" / "reader-golden.json"
GOLDEN = json.loads(GOLDEN_FILE.read_text(encoding="utf-8"))

# The same mapping web/tools/capture_reader_golden.py captured through, spelled
# out again here rather than imported from it: the golden is a record of what
# the readers said over these exact stores, and a test that took its inputs
# from the generator could not notice the generator losing one.
STORES: dict[str, tuple[Path, ...]] = {
    "store": (conftest.RUN_WITH_SAMPLES,),
    "store_truncated": (conftest.RUN_TRUNCATED,),
    "store_two_sweeps": (conftest.RUN_TWO_SWEEPS,),
    "store_zero_classes": (conftest.RUN_ZERO_CLASSES,),
    "store_properties_sample": (conftest.RUN_PROPERTIES_SAMPLE,),
    "store_declined": (conftest.RUN_DECLINED,),
    "store_classes_absent": (conftest.RUN_CLASSES_ABSENT,),
    "store_stale_sample": (conftest.RUN_WITH_SAMPLES, conftest.RUN_DECLINED),
    "store_later_sample": (conftest.RUN_WITH_SAMPLES, conftest.RUN_LATER_SAMPLE_ONLY),
    "store_hostile_literals": (conftest.RUN_HOSTILE_LITERALS,),
    "store_new_subjects": (conftest.RUN_WITH_SAMPLES, conftest.RUN_NEW_SUBJECTS),
    "store_prober_failed": (conftest.RUN_PROBER_FAILED,),
    "store_crashed_partway": (conftest.RUN_WITH_SAMPLES, conftest.RUN_CRASHED_PARTWAY),
}

_DESCRIPTION = read_query("endpoint_description")


def _description(store: Store, endpoint: str) -> list[str]:
    """The CONSTRUCT's triples as sorted N-Triples lines.

    Sorted for the same reason the capture sorts them: a CONSTRUCT's solution
    order is not specified, so an unsorted comparison would fail on a
    reordering that changed no fact.
    """
    triples = store.query(
        _DESCRIPTION, substitutions={Variable("endpoint"): NamedNode(endpoint)}
    )
    text = serialize(triples, format=RdfFormat.N_TRIPLES).decode("utf-8")
    return sorted(line for line in text.splitlines() if line.strip())


@pytest.mark.parametrize("name", sorted(STORES))
def test_the_three_readers_return_what_they_returned_before_current_existed(
    name, tmp_path
):
    """One test per store, so a failure names the store rather than the suite.

    Every endpoint in the store is checked in all three representations,
    because the two the HTML page is built from and the one the RDF
    representation is may not disagree about which run is newest, and a store
    where only one of the three broke is the store where nothing notices.
    """
    assert name in GOLDEN, (
        f"{name} has no frozen answers; regenerate with "
        "python tools/capture_reader_golden.py"
    )
    store = Store(str(tmp_path / name))
    for fixture in STORES[name]:
        load_run(store, fixture.read_bytes())

    for endpoint, expected in sorted(GOLDEN[name].items()):
        assert asdict(endpoint_measurements(store, endpoint)) == expected[
            "measurements"
        ], f"{name}: endpoint_measurements changed for {endpoint}"
        assert asdict(endpoint_content(store, endpoint)) == expected["content"], (
            f"{name}: endpoint_content changed for {endpoint}"
        )
        assert _description(store, endpoint) == expected["description"], (
            f"{name}: the RDF representation changed for {endpoint}"
        )


def test_the_golden_covers_every_store_the_suite_builds():
    """A golden that lost a store would pass every test above.

    The store names come from conftest's fixture functions, so a fixture added
    there without being captured fails here rather than going unmeasured.
    """
    fixtures = {
        name
        for name in dir(conftest)
        if name == "store" or name.startswith("store_")
    }
    assert fixtures == set(STORES), (
        "conftest's fixture stores and the golden's stores differ: "
        f"{sorted(fixtures ^ set(STORES))}"
    )
    assert set(GOLDEN) == set(STORES)


def test_the_golden_holds_answers_that_are_not_all_empty():
    """The one way this file could pass while testing nothing.

    A golden captured against a broken reader would freeze "nothing known" for
    every endpoint and then assert it forever. So at least one endpoint per
    store must have been assessed, and the store the page's own tests are
    built on must carry a class sample.
    """
    for name, answers in GOLDEN.items():
        assessed = [
            endpoint
            for endpoint, answer in answers.items()
            if answer["measurements"]["assessed"]
            or answer["content"]["sampled"]
        ]
        if name == "store_properties_sample":
            # The one store whose only endpoint is deliberately unassessed and
            # unsampled. run-properties-sample.nq holds one sample carrying
            # sw:sampledBy sw:metric:properties and no measurement at all, and
            # both read queries pin sw:metric:classes, so "nothing to show" IS
            # the answer this fixture exists to prove. Named here rather than
            # skipped silently, so the exemption is one fixture and not a hole
            # in the check.
            assert assessed == [], (
                "run-properties-sample.nq must stay a sample no reader shows"
            )
            continue
        assert assessed, f"{name} froze nothing but empty answers"
    kadaster = GOLDEN["store"]["https://data.kkg.kadaster.nl/query"]
    assert kadaster["content"]["sampled"] is True
    assert len(kadaster["content"]["classes"]) == 59, (
        "the real sweep sampled 59 classes from kadaster"
    )
    assert kadaster["measurements"]["assessed"] is True
    assert kadaster["description"], "the RDF representation froze no triples"

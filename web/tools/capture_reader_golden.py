"""Freezes what the three read paths answer, so a change to them is visible.

Run from web/:  python tools/capture_reader_golden.py

It writes web/tests/fixtures/reader-golden.json: for every store the suite's
conftest builds, and every endpoint any run graph in it mentions, what
endpoint_measurements(), endpoint_content() and the endpoint_description
CONSTRUCT return. web/tests/test_reader_golden.py asserts it.

Why a frozen file rather than assertions written by hand. Stage 3-2 moves all
three read queries onto the derived urn:sparqlwatch:current graph, and the
failure that has to be caught is not a wrong row but a page that returns
nothing at all, over stores no existing test looked at from all three
directions at once. A golden captured before the move and asserted after it
is the only thing that compares the two derivations.

Regenerate it only to record a change that is intended, and say in the commit
message which answers changed and why.
"""

from __future__ import annotations

import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tests"))

from dataclasses import asdict

from pyoxigraph import NamedNode, RdfFormat, Store, Variable, serialize

import conftest
from endpoint_content import endpoint_content
from endpoint_measurements import endpoint_measurements
from load_run import load_run
from queries import read_query

GOLDEN = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "reader-golden.json"

# Mirrors web/tests/conftest.py's fixtures, by name, so a reader can line the
# two up. Kept as a literal rather than introspected out of conftest: a
# golden whose inputs are computed can silently start covering less.
STORES: dict[str, tuple[Path, ...]] = {
    "store": (conftest.RUN_WITH_SAMPLES,),
    "store_truncated": (conftest.RUN_TRUNCATED,),
    "store_two_sweeps": (conftest.RUN_TWO_SWEEPS,),
    "store_zero_classes": (conftest.RUN_ZERO_CLASSES,),
    "store_properties_sample": (conftest.RUN_PROPERTIES_SAMPLE,),
    "store_content_profiles": (conftest.RUN_CONTENT_PROFILES,),
    # Added 2026-09-03 with the per-metric sample pointer. The first two are the
    # cases the old shape could not express, so the golden should hold what the
    # readers say about them.
    "store_two_metrics": (conftest.RUN_TWO_METRICS_SAMPLED,),
    "store_metrics_diverged": (
        conftest.RUN_TWO_METRICS_SAMPLED,
        conftest.RUN_PROPERTIES_LATER,
    ),
    "store_two_metrics_reloaded": (
        conftest.RUN_TWO_METRICS_SAMPLED,
        conftest.RUN_TWO_METRICS_SAMPLED,
    ),
    "store_declined": (conftest.RUN_DECLINED,),
    "store_classes_absent": (conftest.RUN_CLASSES_ABSENT,),
    "store_stale_sample": (conftest.RUN_WITH_SAMPLES, conftest.RUN_DECLINED),
    "store_later_sample": (conftest.RUN_WITH_SAMPLES, conftest.RUN_LATER_SAMPLE_ONLY),
    "store_hostile_literals": (conftest.RUN_HOSTILE_LITERALS,),
    "store_new_subjects": (conftest.RUN_WITH_SAMPLES, conftest.RUN_NEW_SUBJECTS),
    "store_prober_failed": (conftest.RUN_PROBER_FAILED,),
    "store_crashed_partway": (conftest.RUN_WITH_SAMPLES, conftest.RUN_CRASHED_PARTWAY),
    "store_registry_sample": (conftest.RUN_REGISTRY_SAMPLE,),
    "store_registry_and_failure": (
        conftest.RUN_REGISTRY_SAMPLE,
        conftest.RUN_PROBER_FAILED,
    ),
    "store_two_metric_sets": (
        conftest.RUN_REGISTRY_SAMPLE,
        conftest.RUN_CLASSES_ABSENT,
    ),
    "store_no_availability_two_ways": (
        conftest.RUN_PROBER_FAILED,
        conftest.RUN_NO_AVAILABILITY,
    ),
    "store_dormant_newest": (
        conftest.RUN_WITH_SAMPLES,
        conftest.RUN_WITH_DORMANCY,
    ),
    "store_dormant_automatic": (
        conftest.RUN_WITH_SAMPLES,
        conftest.RUN_DORMANCY_AUTOMATIC,
    ),
    "store_dormancy_then_crash": (
        conftest.RUN_WITH_SAMPLES,
        conftest.RUN_DORMANCY_THEN_CRASH,
    ),
    "store_dormancy_alone": (conftest.RUN_WITH_DORMANCY,),
}

# An endpoint no run mentions, so "we know nothing about this one" is frozen
# too: that is the answer a store with no current graph would give for every
# endpoint, and it must stay wrong for these.
ABSENT_ENDPOINT = "https://absent.example/sparql"

# EVERY WAY A RUN GRAPH CAN MENTION AN ENDPOINT, and the list is the point: an
# endpoint this misses is silently absent from the golden, and nothing notices,
# because test_the_golden_holds_answers_that_are_not_all_empty asks only that
# SOME endpoint per store was assessed.
#
# The dormancy arm is the fifth and it was added with the dormancy stores. A
# dormant endpoint gets no chunk at all: no measurement, no decline, no sample
# and no sw:declarationsRead, so it matches none of the four arms above it. In
# store_dormancy_alone that is the ONLY way the store mentions kadaster, and
# what the golden is there to freeze is that both readers answer "nothing
# known" for it and the CONSTRUCT emits nothing about it. Without this arm that
# endpoint would not be in the file and the claim would be untested.
#
# Keyed on the DECLARATION and not on sw:dormancyReason, which is what the plan
# for this task named. The reason is optional in the vocabulary (see
# EndpointMeasurements.newest_declared_this_endpoint_dormant on why the read
# tier turns on the declaration and not the reason), so an endpoint declared
# dormant with no reason beside it would match a reason arm and not be
# enumerated. Every committed fixture carries both, so the two arms select the
# same endpoints today; this one keeps doing so if a run graph ever carries a
# declaration alone.
_ENDPOINTS_QUERY = """
SELECT DISTINCT ?endpoint WHERE {
  GRAPH ?g {
    { ?s <http://www.w3.org/ns/dqv#computedOn> ?endpoint }
    UNION { ?s <urn:sparqlwatch:notMeasuredOn> ?endpoint }
    UNION { ?s <urn:sparqlwatch:sampledFrom> ?endpoint }
    UNION { ?endpoint <urn:sparqlwatch:declarationsRead> ?read }
    UNION { ?activity <urn:sparqlwatch:dormantEndpoint> ?endpoint }
  }
}
"""

_DESCRIPTION = read_query("endpoint_description")


def endpoints_in(store: Store) -> list[str]:
    return sorted(row["endpoint"].value for row in store.query(_ENDPOINTS_QUERY))


def description_of(store: Store, endpoint: str) -> list[str]:
    """The CONSTRUCT's triples as sorted N-Triples lines.

    Sorted because a CONSTRUCT's solution order is not specified, and an
    unsorted list would make the golden fail on a reordering that changed no
    fact.
    """
    triples = store.query(
        _DESCRIPTION, substitutions={Variable("endpoint"): NamedNode(endpoint)}
    )
    text = serialize(triples, format=RdfFormat.N_TRIPLES).decode("utf-8")
    return sorted(line for line in text.splitlines() if line.strip())


def answers(store: Store, endpoint: str) -> dict:
    return {
        "measurements": asdict(endpoint_measurements(store, endpoint)),
        "content": asdict(endpoint_content(store, endpoint)),
        "description": description_of(store, endpoint),
    }


def capture() -> dict:
    golden: dict[str, dict] = {}
    with tempfile.TemporaryDirectory() as tmp:
        for name, fixtures in STORES.items():
            store = Store(str(Path(tmp) / name))
            for fixture in fixtures:
                load_run(store, fixture.read_bytes())
            golden[name] = {
                endpoint: answers(store, endpoint)
                for endpoint in endpoints_in(store) + [ABSENT_ENDPOINT]
            }
            del store
    return golden


def main() -> int:
    GOLDEN.write_text(
        json.dumps(capture(), indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"wrote {GOLDEN}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

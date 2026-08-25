"""Times the three read queries over a store of N replayed runs.

Run from web/:

    python tools/time_read_queries.py RUN.nq --runs 1 7 30 [--samples] [--before DIR]

The preserved run this project measures against is
~/code/sparqlwatch-runs/run-2026-08-24T19-45-03Z-lod-cloud-543.nq: 27,194
quads, 543 endpoints, and ZERO content samples, because that sweep ran at the
cheap cost ceiling and declined sw:metric:classes for every endpoint. It is
replayed under N run IRIs by rewriting the run instant everywhere it appears,
so the graph IRI, the activity IRI, every measurement and not-measured subject
and the prov:generatedAtTime move together and each replay is a run the prober
could have written.

--samples adds one sw:metric:classes sample per endpoint per run, because the
content and description queries over a store with no samples are queries with
nothing to match, and their cost is a statement about an empty result set
rather than about the shape.

--before DIR times the .rq files in DIR as well as the ones in web/queries,
which is how the before-and-after pair is produced from one store: point it at
a checkout of the previous commit's queries.
"""

from __future__ import annotations

import argparse
import statistics
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from pyoxigraph import (
    NamedNode,
    QueryTriples,
    RdfFormat,
    Store,
    Variable,
    serialize,
)

from load_run import load_run, rebuild_current

QUERY_NAMES = (
    "endpoint_measurements",
    "endpoint_content",
    "endpoint_description",
)
SOURCE_INSTANT = "2026-08-24T19:45:03Z"
ENDPOINT = Variable("endpoint")

# Instants for the replays, one per run, ascending. Distinct days so no two
# replays can tie, which the loader refuses.
def _instants(count: int) -> list[str]:
    return [f"2026-07-{day:02d}T19:45:03Z" for day in range(1, count + 1)]


def _sample_quads(endpoint: str, instant: str, size: int) -> str:
    """One sw:metric:classes sample for ``endpoint`` in the replay at ``instant``.

    Shaped like the ones prober/src/emit.rs writes: a typed sw:ContentSample
    with sw:sampledFrom, sw:sampledBy, sw:sampleSize, sw:sampleTruncated and
    ``size`` sw:sampledValue triples.
    """
    graph = f"<urn:sparqlwatch:run:{instant}>"
    activity = f"<urn:sparqlwatch:activity:{instant}>"
    subject = f"<urn:sparqlwatch:content-sample:{instant}:{endpoint}:classes>"
    xsd = "http://www.w3.org/2001/XMLSchema#"
    lines = [
        f"{subject} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> "
        f"<urn:sparqlwatch:ContentSample> {graph} .",
        f"{subject} <urn:sparqlwatch:sampledFrom> <{endpoint}> {graph} .",
        f"{subject} <urn:sparqlwatch:sampledBy> <urn:sparqlwatch:metric:classes> {graph} .",
        f'{subject} <urn:sparqlwatch:sampleSize> "{size}"^^<{xsd}integer> {graph} .',
        f'{subject} <urn:sparqlwatch:sampleTruncated> "false"^^<{xsd}boolean> {graph} .',
        f"{subject} <http://www.w3.org/ns/prov#wasGeneratedBy> {activity} {graph} .",
    ]
    lines += [
        f"{subject} <urn:sparqlwatch:sampledValue> "
        f"<https://example.org/vocab#Class{n}> {graph} ."
        for n in range(size)
    ]
    return "\n".join(lines) + "\n"


def _endpoints(nquads: bytes) -> list[str]:
    found = set()
    for line in nquads.decode("utf-8").splitlines():
        terms = line.split(None, 3)
        if len(terms) >= 3 and terms[1] in (
            "<http://www.w3.org/ns/dqv#computedOn>",
            "<urn:sparqlwatch:notMeasuredOn>",
        ):
            found.add(terms[2].strip("<>"))
    return sorted(found)


def build(source: Path, runs: int, samples: int) -> tuple[Store, Path, list[str], float]:
    """A store holding ``runs`` replays of ``source``, plus the seconds it took."""
    original = source.read_bytes()
    endpoints = _endpoints(original)
    directory = Path(tempfile.mkdtemp())
    store = Store(str(directory / "s"))
    started = time.perf_counter()
    for instant in _instants(runs):
        replay = original.replace(SOURCE_INSTANT.encode(), instant.encode())
        if samples:
            # Spliced in BEFORE the footer, not appended after it. load_run cuts
            # a file back to its last section terminator, and a complete run's
            # last line is the footer's sw:finalised, so anything appended after
            # it is discarded as an incomplete final section. Appending was the
            # first attempt here and it produced a store with no samples at all
            # and timings that looked identical to the sample-free ones.
            lines = replay.splitlines(keepends=True)
            body = "".join(
                _sample_quads(endpoint, instant, samples) for endpoint in endpoints
            ).encode()
            replay = b"".join(lines[:-1]) + body + lines[-1]
        load_run(store, replay)
    elapsed = time.perf_counter() - started
    return store, directory, endpoints, elapsed


def time_query(store: Store, text: str, endpoints: list[str], repeats: int) -> float:
    """Median milliseconds for one endpoint's answer, over ``repeats`` endpoints.

    The result is drained, because pyoxigraph returns a lazy iterator and a
    timing that never reads it measures the parse and nothing else.
    """
    times = []
    for endpoint in endpoints[:repeats]:
        started = time.perf_counter()
        result = store.query(
            text, substitutions={ENDPOINT: NamedNode(endpoint)}
        )
        if isinstance(result, QueryTriples):
            # A CONSTRUCT yields triples, and serialising them is what the RDF
            # representation actually does, so it is inside the timing.
            serialize(result, format=RdfFormat.N_TRIPLES)
        else:
            list(result)
        times.append((time.perf_counter() - started) * 1000)
    return statistics.median(times)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("--runs", type=int, nargs="+", default=[1, 7, 30])
    parser.add_argument("--samples", type=int, default=0)
    parser.add_argument("--before", type=Path, default=None)
    parser.add_argument("--endpoints", type=int, default=20)
    args = parser.parse_args()

    here = Path(__file__).resolve().parent.parent / "queries"
    for runs in args.runs:
        store, directory, endpoints, load_seconds = build(
            args.source, runs, args.samples
        )
        quads = len(store)
        current = len(
            list(
                store.quads_for_pattern(
                    None, None, None, NamedNode("urn:sparqlwatch:current")
                )
            )
        )
        row = [
            f"{runs} run(s)",
            f"{quads - current} run quads + {current} current quads",
            f"load {load_seconds:.1f} s",
        ]
        after = {
            name: time_query(
                store, (here / f"{name}.rq").read_text(), endpoints, args.endpoints
            )
            for name in QUERY_NAMES
        }
        # The before-and-after pair is two different stores, not two queries
        # over one. Before this stage the store held run graphs and nothing
        # else, so timing the old queries beside a current graph they never
        # read would charge them for quads that did not exist. current is
        # dropped first, and the rebuild below puts it back.
        before = {}
        if args.before is not None:
            store.remove_graph(NamedNode("urn:sparqlwatch:current"))
            before = {
                name: time_query(
                    store,
                    (args.before / f"{name}.rq").read_text(),
                    endpoints,
                    args.endpoints,
                )
                for name in QUERY_NAMES
            }
        for name in QUERY_NAMES:
            cell = f"{name} after {after[name]:.2f} ms"
            if name in before:
                cell += f" / before {before[name]:.2f} ms"
            row.append(cell)
        started = time.perf_counter()
        rebuilt = rebuild_current(store)
        row.append(
            f"rebuild {(time.perf_counter() - started):.1f} s "
            f"({rebuilt.endpoints} endpoints, {rebuilt.runs} runs)"
        )
        print(" | ".join(row), flush=True)
        del store
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

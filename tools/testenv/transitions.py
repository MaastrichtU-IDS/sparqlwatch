"""What changed, and when. The reason this environment exists.

Reads every run graph in the test store, in timestamp order, and reports each
point where an (endpoint, metric) pair's verdict differs from the previous run's.
This is deliberately the shape the history display would need: O(changes), not
O(runs x pairs).

It queries the run graphs directly rather than `urn:sparqlwatch:current`, which
is exactly what the site cannot do today. Every shipped read query goes through
`current` for a measured reason (1.2 ms at one run, 6,488.5 ms at thirty), so
this file is also a demonstration of the cost that a real history feature has to
design around.
"""
import sys
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "web"))
from pyoxigraph import Store  # noqa: E402

STORE = Path(__file__).resolve().parent / "run" / "store.db"

RUNS = """
PREFIX prov: <http://www.w3.org/ns/prov#>
SELECT ?run ?at WHERE {
  GRAPH ?run { ?a a prov:Activity ; prov:generatedAtTime ?at }
} ORDER BY ?at
"""

VERDICTS = """
PREFIX dqv: <http://www.w3.org/ns/dqv#>
SELECT ?endpoint ?metric ?value WHERE {
  GRAPH <%s> {
    ?m dqv:computedOn ?endpoint ;
       dqv:isMeasurementOf ?metric ;
       dqv:value ?value .
  }
}
"""

# Fakes are named by their PORT, not by their path: every endpoint URL here ends
# in /sparql, so trimming to the last path segment labelled all eight of them
# "sparql" and made the report unreadable.
PORT_NAMES = {
    "9001": "steady", "9002": "flaky", "9003": "gains-cors", "9004": "loses-sd",
    "9005": "html-console", "9006": "garbage", "9007": "no-cors", "9008": "newcomer",
}


def short_endpoint(iri: str) -> str:
    port = iri.split(":")[-1].split("/")[0]
    return f"{PORT_NAMES.get(port, port):<13}"


def short(iri: str) -> str:
    return iri.rstrip("/").rsplit("/", 1)[-1].rsplit(":", 1)[-1]

def main():
    if not STORE.exists():
        raise SystemExit(f"no test store at {STORE}; run build.py first")
    st = Store.read_only(str(STORE))
    runs = [(r["run"].value, r["at"].value) for r in st.query(RUNS)]
    print(f"{len(runs)} runs in the store\n")

    previous: dict[tuple[str, str], str] = {}
    changes = defaultdict(list)
    for run, at in runs:
        day = at[:10]
        now = {}
        for row in st.query(VERDICTS % run):
            key = (row["endpoint"].value, row["metric"].value)
            now[key] = row["value"].value
        for key, value in sorted(now.items()):
            was = previous.get(key)
            if was is None:
                changes[key].append((day, None, value))
            elif was != value:
                changes[key].append((day, was, value))
        # A pair that vanishes between runs is a change too: it says the sweep
        # stopped answering that question, which a history view must not draw as
        # continuity. Recorded so the display cannot silently smooth over it.
        for key in set(previous) - set(now):
            changes[key].append((day, previous[key], "(not measured)"))
        previous = now

    interesting = {k: v for k, v in changes.items() if len(v) > 1}
    print(f"{len(changes)} (endpoint, metric) pairs seen, "
          f"{len(interesting)} of them changed at least once after first sight\n")
    for (endpoint, metric), events in sorted(interesting.items(),
                                             key=lambda kv: (short_endpoint(kv[0][0]), short(kv[0][1]))):
        print(f"  {short_endpoint(endpoint)} {short(metric)}")
        for day, was, now_ in events:
            arrow = f"{was} -> {now_}" if was else f"first seen: {now_}"
            print(f"          {day}  {arrow}")
    if not interesting:
        print("  nothing changed, which means the fakes did not misbehave")

if __name__ == "__main__":
    main()

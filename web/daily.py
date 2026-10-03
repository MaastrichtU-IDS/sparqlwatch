"""One summary per (endpoint, metric, day), so a closed day costs ~6 quads.

WHY. A day's 23 hourly runs are about 233,000 quads, and once that day is over
nothing reads them individually. `app._daily_series` buckets the readings by day
and asks only for aggregates; the one reader that wants per-sweep detail,
`fleet.fleet_history`, keeps the newest 40 sweeps -- under two days. Measured
2026-10-01: an hourly run is 10,143 quads of which 34% are `notMeasured` facts
repeating the previous hour's unchanged, 34% is `rdf:type` and provenance
overhead, and 19% is measurements.

COUNTS PER VERDICT, NOT "UP" AND "DOWN". What counts as up is
`app._POSITIVE_VERDICTS`, and the endpoint page is built so that word is read in
one place. Storing a decided up/down would freeze that judgement into the data,
where a later change to the table would disagree with history silently and
nothing would say which was right. Keeping the verdicts lets the reader go on
applying the same table to a summarised day as to a live one.

`median_ms` and `p95_ms` are the exception and are precomputed, because they
cannot be recovered from counts. They are over the sweeps that SUCCEEDED -- not
merely the ones that answered -- which is the rule `_daily_series` applies: a
probe that timed out records its full 30s budget, so a median over everything
spikes to thirty seconds on exactly the days uptime drops and reports as typical
a duration no working request ever took. An endpoint that answered 404 in 240ms
is not a 240ms endpoint either.

That one statistic therefore DOES freeze the up/down table, and it is the only
thing here that does. It is unavoidable: a median has to be taken over some
set, and the set is "the sweeps that worked". The counts above do not freeze it,
and the counts are what uptime is computed from -- so a later change to
`POSITIVE` restates every past day's uptime correctly and leaves only its
durations answering the older question. `test_daily` pins the two tables
together so the drift is caught rather than discovered.

THIS MODULE ONLY WRITES. Nothing is deleted here and no reader prefers a summary
yet: a summary is checked against the raw day it came from before either
happens. See `agrees_with_raw`.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from pyoxigraph import Literal, NamedNode, Quad, Store

SW = "urn:sparqlwatch:"

# WHAT COUNTS AS A WORKING SWEEP, for the duration statistics only. Must equal
# `app._POSITIVE_VERDICTS`; `test_daily` asserts it does. Duplicated rather than
# imported because `app` builds a FastAPI application at import time and this
# module is meant to be usable from a maintenance job that serves nothing.
POSITIVE = ("verified", "undeclared-but-verified")
XSD_INT = NamedNode("http://www.w3.org/2001/XMLSchema#integer")
XSD_DATE = NamedNode("http://www.w3.org/2001/XMLSchema#date")

# Every reading of every metric, with the day its run belongs to. Readings only:
# a decline has no verdict and no duration, and a day's declines are exactly the
# bookkeeping this summary exists to stop storing.
# ONE RUN GRAPH AT A TIME, EACH NAMED. Two earlier shapes were both too slow to
# use, and for the same reason: an unbound `GRAPH ?run` makes the engine consider
# every graph in the store, and a `STRSTARTS` over a timestamp literal cannot use
# an index to narrow it.
#
#   selecting every reading and filtering the day in Python
#       -- a full scan of the archive per day, twice over (`agrees_with_raw`
#          reads it again). The job ran 108 minutes without finishing one day.
#   scoping the day inside one query, `?run` still unbound
#       -- 8.8s for a day of a 36,000-quad store, which is minutes against 4.2
#          million.
#
# So the run graphs for the day are found first -- that query is bound and costs
# 0.01s -- and the readings are then asked of each one BY NAME, which is the
# shape `day_quads` already used.
_RUNS_ON = """
PREFIX prov: <http://www.w3.org/ns/prov#>
SELECT ?g WHERE {
  GRAPH ?g { ?a a prov:Activity ; prov:generatedAtTime ?at }
  FILTER(STRSTARTS(STR(?at), "%s"))
}
"""

_READINGS_IN = """
PREFIX sw: <urn:sparqlwatch:>
PREFIX dqv: <http://www.w3.org/ns/dqv#>
SELECT ?endpoint ?metric ?verdict ?elapsed WHERE {
  GRAPH <%s> {
    ?m dqv:computedOn ?endpoint ;
       dqv:isMeasurementOf ?metric ;
       dqv:value ?verdict .
    OPTIONAL { ?m sw:elapsedMs ?elapsed }
  }
}
"""


def encode(s: str) -> str:
    """Percent-encode keeping RFC 3986's unreserved set, as the prober does.

    The same rule as `emit::encode_unreserved`, and for the same reason: the
    endpoint goes into a subject IRI verbatim and reversibly, and the output
    holds no `:` so the IRI's own separators stay unambiguous.
    """
    out = []
    for b in s.encode():
        c = chr(b)
        if c.isalnum() and b < 128 or c in "-._~":
            out.append(c)
        else:
            out.append(f"%{b:02X}")
    return "".join(out)


@dataclass
class DayCell:
    """One (endpoint, metric) on one day."""

    sweeps: int = 0
    verdicts: dict[str, int] = field(default_factory=dict)
    answered_ms: list[int] = field(default_factory=list)


def _percentile(values: list[int], share: float) -> int:
    """Nearest-rank percentile over a sorted copy.

    Nearest-rank rather than interpolated: the value reported is one a request
    actually took, which is what a reader comparing it against a timeout budget
    needs it to be.
    """
    ordered = sorted(values)
    rank = max(1, min(len(ordered), round(share * len(ordered) + 0.5)))
    return ordered[rank - 1]


def runs_on(store: Store, day: str) -> list[str]:
    """The run graphs `day` covers, named so everything else can bind them."""
    return [row["g"].value for row in store.query(_RUNS_ON % day)]


def read_day(store: Store, day: str) -> dict[tuple[str, str], DayCell]:
    """What the raw runs of `day` say, keyed by (endpoint, metric)."""
    cells: dict[tuple[str, str], DayCell] = {}
    for run in runs_on(store, day):
        for row in store.query(_READINGS_IN % run):
            key = (row["endpoint"].value, row["metric"].value)
            cell = cells.setdefault(key, DayCell())
            cell.sweeps += 1
            verdict = row["verdict"].value
            cell.verdicts[verdict] = cell.verdicts.get(verdict, 0) + 1
            # SUCCESSFUL sweeps only: see POSITIVE. An endpoint that answered
            # `absent` in 240ms did answer, and is not a 240ms endpoint.
            if row["elapsed"] is not None and verdict in POSITIVE:
                cell.answered_ms.append(int(row["elapsed"].value))
    return cells


def graph_iri(day: str) -> str:
    """The graph one day's summary lives in.

    Its own graph per day, never a run graph: this is derived from observations
    rather than being one, and a consumer reading a run must not find our
    arithmetic mixed in with what a sweep saw.
    """
    return f"{SW}daily:{day}"


def summarise_day(
    store: Store, day: str, cells: dict[tuple[str, str], DayCell] | None = None
) -> int:
    """Write `day`'s summary, replacing any earlier one. Returns pairs written.

    Idempotent: subject IRIs are a function of (day, endpoint, metric, verdict),
    so re-running after a late-arriving run rewrites the same subjects rather
    than accumulating a second opinion.
    """
    # `cells` lets a caller that has already read the day hand it over. Reading
    # it twice is the difference between one scan of a day's runs and two, and
    # the job does this for every closed day.
    cells = read_day(store, day) if cells is None else cells
    graph = NamedNode(graph_iri(day))
    store.update(f"DROP SILENT GRAPH <{graph_iri(day)}>")
    for (endpoint, metric), cell in cells.items():
        base = f"{SW}daily:{day}:{encode(endpoint)}:{encode(metric)}"
        subject = NamedNode(base)
        quads = [
            Quad(subject, NamedNode(f"{SW}dailyOf"), NamedNode(endpoint), graph),
            Quad(subject, NamedNode(f"{SW}dailyMetric"), NamedNode(metric), graph),
            Quad(subject, NamedNode(f"{SW}dailyDate"), Literal(day, datatype=XSD_DATE), graph),
            Quad(
                subject,
                NamedNode(f"{SW}dailySweeps"),
                Literal(str(cell.sweeps), datatype=XSD_INT),
                graph,
            ),
        ]
        for verdict, n in sorted(cell.verdicts.items()):
            node = NamedNode(f"{base}:{encode(verdict)}")
            quads += [
                Quad(subject, NamedNode(f"{SW}dailyReading"), node, graph),
                Quad(node, NamedNode(f"{SW}verdict"), Literal(verdict), graph),
                Quad(node, NamedNode(f"{SW}readings"), Literal(str(n), datatype=XSD_INT), graph),
            ]
        # Only where something answered. A day whose every sweep timed out has no
        # typical duration, and writing a zero or a 30,000 would both be claims
        # nothing observed.
        if cell.answered_ms:
            quads += [
                Quad(
                    subject,
                    NamedNode(f"{SW}dailyMedianMs"),
                    Literal(str(_percentile(cell.answered_ms, 0.5)), datatype=XSD_INT),
                    graph,
                ),
                Quad(
                    subject,
                    NamedNode(f"{SW}dailyP95Ms"),
                    Literal(str(_percentile(cell.answered_ms, 0.95)), datatype=XSD_INT),
                    graph,
                ),
            ]
        for q in quads:
            store.add(q)
    return len(cells)


def read_summary(store: Store, day: str) -> dict[tuple[str, str], DayCell]:
    """A written summary, back in the shape `read_day` returns.

    The durations do not survive this round trip and are not meant to: the
    summary keeps the median and the p95, not the sample they came from. What
    `agrees_with_raw` compares is the counts, which do.
    """
    out: dict[tuple[str, str], DayCell] = {}
    rows = store.query(
        """
        PREFIX sw: <urn:sparqlwatch:>
        SELECT ?endpoint ?metric ?sweeps ?verdict ?readings WHERE {
          GRAPH ?g {
            ?s sw:dailyOf ?endpoint ; sw:dailyMetric ?metric ; sw:dailySweeps ?sweeps ;
               sw:dailyReading ?r .
            ?r sw:verdict ?verdict ; sw:readings ?readings .
          }
          FILTER(STRSTARTS(STR(?g), "%s"))
        }
        """
        % graph_iri(day)
    )
    for row in rows:
        key = (row["endpoint"].value, row["metric"].value)
        cell = out.setdefault(key, DayCell())
        cell.sweeps = int(row["sweeps"].value)
        cell.verdicts[row["verdict"].value] = int(row["readings"].value)
    return out


def agrees_with_raw(
    store: Store, day: str, raw: dict[tuple[str, str], DayCell] | None = None
) -> list[str]:
    """Every way the summary of `day` differs from the runs it came from.

    THE GATE BEFORE ANYTHING IS DELETED. A summary is only worth having if the
    page drawn from it is the page drawn from the raw day, and the only way to
    know is to compute both from real data and compare. An empty list means they
    agree; each string names one disagreement, in a form that says which pair.
    """
    raw = read_day(store, day) if raw is None else raw
    summary = read_summary(store, day)
    problems: list[str] = []
    for key in sorted(set(raw) | set(summary)):
        endpoint, metric = key
        where = f"{endpoint} {metric.rsplit(':', 1)[-1]} on {day}"
        if key not in summary:
            problems.append(f"{where}: in the runs, missing from the summary")
            continue
        if key not in raw:
            problems.append(f"{where}: in the summary, missing from the runs")
            continue
        if raw[key].sweeps != summary[key].sweeps:
            problems.append(
                f"{where}: {raw[key].sweeps} sweeps in the runs, "
                f"{summary[key].sweeps} in the summary"
            )
        if raw[key].verdicts != summary[key].verdicts:
            problems.append(
                f"{where}: verdicts {raw[key].verdicts} in the runs, "
                f"{summary[key].verdicts} in the summary"
            )
    return problems


def day_from_summary(store: Store, endpoint: str, metric: str, day: str) -> dict | None:
    """One day of the endpoint chart, read from the summary instead of the runs.

    THE OTHER HALF OF THE GATE. `agrees_with_raw` compares the stored counts
    against the stored readings; this compares what a READER would draw. The two
    are different questions -- a summary can hold the right counts and still be
    read wrongly -- and it is the second that decides whether a day's runs can
    ever be deleted.

    `None` when the summary knows nothing of that pair on that day, which the
    chart draws as a gap rather than as zero, for the reason `_daily_series`
    gives: a day nobody swept is not a day something was down.

    The up/down split is NOT stored and is applied here from the caller's own
    table, so the summarised past and the live present keep reading the same
    word the same way.
    """
    cell = read_summary(store, day).get((endpoint, metric))
    if cell is None:
        return None
    durations = {
        str(row["p"].value).rsplit(":", 1)[-1]: int(row["o"].value)
        for row in store.query(
            """
            PREFIX sw: <urn:sparqlwatch:>
            SELECT ?p ?o WHERE {
              GRAPH <%s> {
                ?s sw:dailyOf <%s> ; sw:dailyMetric <%s> ; ?p ?o .
                FILTER(?p IN (sw:dailyMedianMs, sw:dailyP95Ms))
              }
            }
            """
            % (graph_iri(day), endpoint, metric)
        )
    }
    return {
        "sweeps": cell.sweeps,
        "verdicts": dict(cell.verdicts),
        "median_ms": durations.get("dailyMedianMs"),
        "p95_ms": durations.get("dailyP95Ms"),
    }


def days_in_store(store: Store) -> list[str]:
    """Every day the run archive covers, oldest first."""
    return sorted(
        {
            row["at"].value[:10]
            for row in store.query(
                """
                PREFIX prov: <http://www.w3.org/ns/prov#>
                SELECT ?at WHERE { GRAPH ?g { ?a a prov:Activity ; prov:generatedAtTime ?at } }
                """
            )
        }
    )


def day_quads(store: Store, day: str) -> int:
    """How many quads `day`'s run graphs hold, for the report."""
    total = 0
    for run in runs_on(store, day):
        for row in store.query("SELECT (COUNT(*) AS ?n) WHERE { GRAPH <%s> { ?s ?p ?o } }" % run):
            total += int(row["n"].value)
    return total


def main(argv: list[str] | None = None) -> int:
    """Summarise the closed days of a store and report whether they agree.

    WRITES, AND DELETES NOTHING. Each day gets a summary graph beside the runs
    it came from; the runs stay exactly as they were. That is what makes this
    safe to run against a live store and what makes the report meaningful --
    both representations are there to be compared.

    A non-zero exit means at least one day's summary disagrees with its runs,
    which is the one result that must stop anything being deleted later.
    """
    import sys

    args = sys.argv[1:] if argv is None else argv
    if not args or args[0] in ("-h", "--help"):
        print(
            "usage: daily.py <store> [--keep-days N] [--check|--drop]\n"
            "  summarise every day except the newest N (default 3), then check\n"
            "  each summary against the runs it came from.\n"
            "  --check verifies the summaries already written, writing nothing:\n"
            "          this is the gate a pruning step must pass, because it can\n"
            "          see a summary that has gone stale.\n"
            "  --drop  removes the summaries again, leaving the runs untouched.",
            file=sys.stderr,
        )
        return 2
    from pathlib import Path

    path = args[0]
    if not Path(path).is_dir():
        print(f"{path} is not an existing store directory", file=sys.stderr)
        return 2
    keep = 3
    drop = "--drop" in args
    # VERIFY WITHOUT WRITING. The default path summarises and then checks, which
    # proves the round trip but can never catch a summary that has gone stale --
    # it has just been rewritten from the runs. A pruning step needs the other
    # question: does the summary ALREADY in the store still match the day it
    # claims to describe? A run arriving after its day was summarised is exactly
    # that, and it is the shape that would delete a day whose summary is short
    # by a sweep.
    check_only = "--check" in args
    if "--keep-days" in args:
        keep = int(args[args.index("--keep-days") + 1])

    store = Store(path)
    days = days_in_store(store)
    # THE TAIL STAYS RAW. `fleet.fleet_history` reads the newest 40 sweeps per
    # sweep, which is under two days, and a day still being written is not a
    # closed day. Three is that with room to spare.
    closed = days[:-keep] if keep else days
    print(f"{len(days)} day(s) in the store, {len(closed)} closed, keeping the newest {keep} raw")

    if drop:
        for day in closed:
            store.update(f"DROP SILENT GRAPH <{graph_iri(day)}>")
        print(f"dropped {len(closed)} summary graph(s); the runs are untouched")
        return 0

    raw_total = summary_total = 0
    failures: list[str] = []
    for day in closed:
        if check_only:
            pairs, cells = len(read_summary(store, day)), None
        else:
            cells = read_day(store, day)
            pairs = summarise_day(store, day, cells=cells)
        problems = agrees_with_raw(store, day, raw=cells)
        raw = day_quads(store, day)
        kept = 0
        for row in store.query(
            "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <%s> { ?s ?p ?o } }" % graph_iri(day)
        ):
            kept = int(row["n"].value)
        raw_total += raw
        summary_total += kept
        verdict = "ok" if not problems else f"{len(problems)} DISAGREEMENT(S)"
        ratio = f"{raw / kept:.0f}x" if kept else "-"
        print(f"  {day}  {pairs:5d} pairs  {raw:8d} raw  {kept:6d} summary  {ratio:>5}  {verdict}")
        for problem in problems[:5]:
            print(f"      {problem}")
        if problems:
            failures.append(day)

    print(
        f"total: {raw_total} quads of runs summarised into {summary_total}"
        + (f" ({raw_total / summary_total:.0f}x)" if summary_total else "")
    )
    if failures:
        print(f"DISAGREED on {len(failures)} day(s): {', '.join(failures)}")
        print("nothing may be deleted until this is empty")
        return 1
    print("every closed day's summary agrees with the runs it came from")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

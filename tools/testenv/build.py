"""Build a multi-run test environment: sweep the fakes for N days, load it all.

What this produces that nothing else in the repo does: a store holding MANY run
graphs, with verdicts that CHANGE between them. Every committed fixture is one
or two runs built by hand. The history work needs a store where an endpoint went
down on day 4 and came back on day 7, and where the derived `current` graph has
been advanced ten times by the real loader rather than constructed.

No third-party endpoint is contacted. Everything is 127.0.0.1.

Usage:
    python tools/testenv/build.py            # 10 days, fresh
    python tools/testenv/build.py --days 30
    python tools/testenv/build.py --serve    # then serve the result on :8732
"""
import argparse, os, shutil, subprocess, sys, time
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import urlopen

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
RUN = HERE / "run"
PROBER = REPO / "prober" / "target" / "release" / "sparqlwatch-prober"
DORMANCY = REPO / "prober" / "target" / "release" / "dormancy"
PY = REPO / "web" / ".venv" / "bin" / "python"

PORTS = [9001, 9002, 9003, 9004, 9005, 9006, 9007, 9008]


def wait_for_fakes(timeout=20.0):
    """Ready means "answers HTTP", not "answers 200".

    Some fakes are deliberately 503 on day 1 (`newcomer`), and a readiness check
    that waits for 200 would hang forever on exactly the endpoint whose whole job
    is to be down at the start. An HTTPError means the server replied, which is
    what readiness is.
    """
    deadline = time.time() + timeout
    for port in PORTS:
        while True:
            try:
                urlopen(f"http://127.0.0.1:{port}/", timeout=1).read()
                break
            except HTTPError:
                break
            except Exception:
                if time.time() > deadline:
                    raise SystemExit(f"fake on {port} never came up")
                time.sleep(0.1)


def write_inputs():
    RUN.mkdir(exist_ok=True)
    (RUN / "endpoints.toml").write_text(
        "# The fakes from tools/testenv/fakes.py. Loopback only, on purpose.\n"
        "endpoint = [\n"
        + "".join(f'  "http://127.0.0.1:{p}/sparql",\n' for p in PORTS)
        + "]\n"
    )
    # The real exclusion list would be read from the repo, but an empty one keeps
    # this environment independent of edits to the shipped registry.
    (RUN / "exclusions.toml").write_text(
        "# Empty on purpose: nothing here is a real host anyone could ask us to\n"
        "# stop probing, and reading the shipped list would couple this\n"
        "# environment to edits made for real endpoints.\nhost = []\n"
    )


def sweep(day: int, at: str, out: Path, max_cost: str) -> float:
    (RUN / "day.txt").write_text(f"{day}\n")
    t0 = time.perf_counter()
    proc = subprocess.run(
        [str(PROBER),
         "--endpoints", str(RUN / "endpoints.toml"),
         "--metrics", str(REPO / "prober" / "metrics.toml"),
         "--exclusions", str(RUN / "exclusions.toml"),
         "--state", str(RUN / "dormancy.toml"),
         "--out", str(out),
         "--at", at,
         "--max-cost", max_cost],
        capture_output=True, text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr[-2000:])
        raise SystemExit(f"day {day} sweep failed")
    return time.perf_counter() - t0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=10)
    ap.add_argument("--keep", action="store_true", help="add to the existing store")
    ap.add_argument("--serve", action="store_true")
    args = ap.parse_args()

    if not PROBER.exists():
        raise SystemExit(f"build the prober first: cargo build --release ({PROBER} missing)")

    store = RUN / "store.db"
    runs = RUN / "runs"
    if not args.keep:
        for p in (store, runs):
            shutil.rmtree(p, ignore_errors=True)
        (RUN / "dormancy.toml").unlink(missing_ok=True)
    runs.mkdir(parents=True, exist_ok=True)
    write_inputs()

    subprocess.run([str(DORMANCY), "init", "--state", str(RUN / "dormancy.toml")],
                   capture_output=True, check=False)

    print(f"sweeping {len(PORTS)} fakes for {args.days} days")
    wait_for_fakes()
    made = []
    for day in range(1, args.days + 1):
        at = f"2026-09-{day:02d}T03:00:00Z"
        out = runs / f"run-{at.replace(':', '-')}.nq"
        # Expensive metrics on day 1 and then weekly, which is the split this
        # project's cost class exists for: availability daily, content rarely.
        cost = "expensive" if day == 1 or day % 7 == 0 else "cheap"
        secs = sweep(day, at, out, cost)
        made.append(out)
        print(f"  day {day:>2}  {at}  {cost:<9} {secs:5.1f}s  {out.stat().st_size/1024:6.1f} KB")

    print("\nloading every run, in order, through the real loader")
    sys.path.insert(0, str(REPO / "web"))
    from pyoxigraph import Store
    from load_run import load_run
    st = Store(str(store))
    for out in made:
        res = load_run(st, out.read_bytes())
        if getattr(res, "drifted", None):
            print(f"  DRIFT after {out.name}: {res.drifted}")
    print(f"  store: {len(st):,} quads, {len(list(st.named_graphs()))} named graphs")
    del st

    if args.serve:
        env = dict(os.environ, SPARQLWATCH_STORE=str(store))
        print("\nserving on http://127.0.0.1:8732 (ctrl-c to stop)")
        subprocess.run([str(PY), "-m", "uvicorn", "app:app",
                        "--host", "127.0.0.1", "--port", "8732"],
                       cwd=str(REPO / "web"), env=env)


if __name__ == "__main__":
    main()

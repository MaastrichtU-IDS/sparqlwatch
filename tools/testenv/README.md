# Test environment

A store holding MANY run graphs, with verdicts that change between them, built
from endpoints that misbehave on a schedule. No third-party host is contacted:
everything is 127.0.0.1.

## Why this exists

Every committed fixture is one or two runs, built by hand. That is enough to test
the loader and the readers, and not enough to build anything that displays
HISTORY: for that you need a store where an endpoint went down on day 4 and came
back on day 7, where `urn:sparqlwatch:current` has been advanced ten times by the
real loader rather than constructed, and where the dormancy state has actually
accumulated.

It also answers a question the repo could not otherwise answer cheaply: what does
a daily cadence do to store size, sweep duration, and the derived graph.

## Running it

```bash
cargo build --release --manifest-path prober/Cargo.toml    # once
web/.venv/bin/python tools/testenv/fakes.py &              # the eight fakes
web/.venv/bin/python tools/testenv/build.py --days 10      # sweep and load
web/.venv/bin/python tools/testenv/build.py --days 10 --serve   # and serve on :8732
```

Everything lands in `tools/testenv/run/`, which is git-ignored: the store, the
run files, the dormancy state, and the generated registry. Delete it and rebuild;
nothing in there is precious, which is the difference between this and
`~/code/sparqlwatch-runs/`, where a lost run is gone.

`--keep` adds days to an existing store instead of starting over, so a longer
history can be grown without re-sweeping what is already there.

## The eight fakes, and what each is for

| port | name | behaviour | the transition it produces |
|---|---|---|---|
| 9001 | `steady` | healthy every day | none: the control |
| 9002 | `flaky` | 503 on days 4 to 6 | verified, then indeterminate, then verified |
| 9003 | `gains-cors` | no CORS headers until day 5 | absent, then verified, on `cors` |
| 9004 | `loses-sd` | 404s its description from day 7 | the service-description level drops |
| 9005 | `html-console` | 200 with an HTML page | permanently indeterminate |
| 9006 | `garbage` | 200 with truncated JSON | permanently indeterminate, a different way |
| 9007 | `no-cors` | never any CORS headers | none: a stable negative |
| 9008 | `newcomer` | 503 until day 3 | indeterminate, then verified |

## Three decisions worth knowing before changing this

**Outages are 503, not hangs.** A hanging socket costs the prober its full 30
second request budget, and the budgets are compiled in rather than exposed as
flags, so one hanging fake would add minutes to every sweep. A 5xx is
`indeterminate` by the same rule a real gateway error is, and it is instant.

The cost of that choice: **dormancy never triggers here.** Relegation is
cost-weighted, and an endpoint earns a strike by costing at least
`DEFAULT_COST_MS` (60 s). A fast 503 costs nothing, so nothing is ever relegated.
Testing the dormancy path needs a fake that genuinely hangs, and needs somebody
to accept the wall clock that implies.

**One port per fake, and that is load-bearing.** `politeness::host_key` strips
only `:80` and `:443`, so `127.0.0.1:9001` and `127.0.0.1:9002` are different
hosts and the sweep probes them in parallel. Put two fakes on one port behind a
path and the 2 second per-host gap serialises the whole sweep.

**Queries are answered for real**, by pyoxigraph, against a small graph. So the
verdicts here are the prober's actual judgments about actual responses, not
verdicts a mock asserted. Where a fake cannot answer something, the failure is a
real failure: no GeoSPARQL is implemented, so `geo-functions` fails the way it
would against an endpoint that does not support it.

## Cost split

Day 1 and every seventh day sweep at `--max-cost expensive`; the rest are
`cheap`. That is the split the cost class exists for, and it mirrors what a real
daily cadence would want: availability every day, content rarely.

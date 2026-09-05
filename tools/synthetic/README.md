# The synthetic endpoint

A real SPARQL 1.1 endpoint over a generated dataset whose counts are known
exactly. First entry in `prober/endpoints.toml`, and the control the other two
are read against.

```bash
cd web && .venv/bin/python ../tools/synthetic/serve.py          # port 9200
cd web && .venv/bin/python ../tools/synthetic/serve.py --declare wrong
```

`GET /truth` returns the ground truth as JSON, so a test can compare a
published number against the dataset rather than against another guess.

## Why this is not one of the testenv fakes

`tools/testenv/fakes.py` returns canned bodies and misbehaves on a schedule.
That is what history tests need, and it is useless for checking a COUNT: a
fake that answers `42` to everything cannot tell a working count from a broken
one. This runs pyoxigraph over a real dataset, so every aggregate the prober
sends is really evaluated.

## What it is shaped to catch

It found its first defect before any count metric shipped. Measured
2026-09-05 against this dataset:

| query | answer | truth |
|---|---|---|
| `SELECT (COUNT(*)) WHERE { ?s ?p ?o }` | 9 | 47 |
| `SELECT (COUNT(*)) WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }` | 47 | 47 |

The naive form counts the DEFAULT GRAPH ONLY and undercounts this dataset five
fold, silently, as a confident number. Any count metric has to use the union
form, which is what `geo-data` and the class enumeration already do. A dataset
that lived entirely in one graph could not have shown this, so the generator
deliberately puts triples in the default graph AND in three named ones.

It is also shaped so a class profile has something to distinguish: five
classes with different property sets, and one property (`ex:value`) carrying
two datatypes across its subjects, because a fixture where every property has
exactly one datatype cannot tell a working `datatypes` count from a hardcoded
`1`.

## The `--declare` knob

The description served at the queryless GET states the dataset's size, and
this chooses what it claims. It is how all four arms of the declared/observed
axis are produced on demand, against an endpoint that cannot be wrong about
itself by accident:

| mode | claims | expected verdict |
|---|---|---|
| `correct` | the real counts | `verified` |
| `stale` | about 3% low, inside tolerance | `verified` |
| `wrong` | an order of magnitude out | `declared-but-wrong` |
| `none` | no counts at all | `undeclared-but-verified` |

`stale` is the one worth keeping. A VoID file is written once and the dataset
it describes keeps growing, so a monitor that calls a few percent "wrong" is
crying wolf on almost every real endpoint. That mode is the regression test
for the tolerance.

# sparqlwatch web

The Python side of sparqlwatch: it loads a prober run into an on-disk
Oxigraph store and holds the read queries against it. The Rust prober is
untouched by anything here; it keeps writing N-Quads files, and this side
starts where that file leaves off.

## The interpreter must be 3.12

Use `python3.12`, not the system `python3`.

On this machine the system `python3` is 3.9.6, which is past end of life.
pyoxigraph happens to be importable there too, which makes it tempting to
skip the venv, but this project exists partly because its predecessor
(umakadata) was rejected for running an entirely end-of-life stack. Building
a new subsystem on a dead interpreter would repeat exactly that mistake, so
the venv below is not optional scaffolding: it is the only sanctioned way to
run this code.

Before creating the venv, confirm the interpreter you are about to use is
3.12:

```bash
python3.12 --version
# Python 3.12.x, not 3.9.x
```

If `python3.12` is not on PATH, install one (for example via a Python
version manager or the official installer) before continuing. Do not
substitute `python3.11`: it has no pyoxigraph wheel verified for this
project, and `python3` (3.9) is explicitly out.

## Set up the venv

From the repository root:

```bash
python3.12 -m venv web/.venv
source web/.venv/bin/activate
pip install -r web/requirements.txt
```

`web/.venv/` is not committed (see the repository's `.gitignore`); recreate
it with the commands above whenever you check out a fresh clone.

Dependencies are pinned to exact versions in `web/requirements.txt`
(`pyoxigraph==0.5.9`, `pytest==9.1.1`), not compatible-release ranges. A
quality monitor whose own dependencies drift is not one anybody should
trust.

## Run the tests

With the venv activated:

```bash
cd web
python -m pytest
```

All tests are offline: they read committed fixture files under
`web/tests/fixtures/` and never open a network connection. See
`web/tests/test_fixture.py` for what each fixture is and where it came from.

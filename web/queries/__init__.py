"""The SPARQL this service asks, one .rq file per question.

The text lives in .rq files rather than in Python string literals so that a
query can be pasted straight into a SPARQL console (or an endpoint's own
editor) and run unchanged while it is being debugged. That only holds while
the file is executable as written, so nothing here templates or rewrites the
text: parameters reach a query through pyoxigraph's variable substitution,
never through string formatting.
"""

from __future__ import annotations

from pathlib import Path

_QUERY_DIR = Path(__file__).parent


def read_query(name: str) -> str:
    """Return the text of ``<name>.rq`` from this directory."""
    return (_QUERY_DIR / f"{name}.rq").read_text(encoding="utf-8")

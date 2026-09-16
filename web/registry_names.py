"""The names the registry files carry, and what a row shows.

A title here is NOT a measurement. It is what a catalogue called an endpoint --
for the 543 seeded from lod-data.json, what the LOD cloud called it -- and the
page attributes it rather than presenting it as something a sweep found. The
front page says this registry is measured rather than asserted, and a borrowed
title is an assertion.

Read from the registry files rather than from the store for the same reason:
web/queries/index_description.rq constructs nothing that is not already in a run
graph, and a catalogue's title is an input, not an observation. Putting it in the
graph to get it onto the page would break that guarantee for a fact nobody
measured.
"""

from __future__ import annotations

import tomllib
from pathlib import Path
from typing import NamedTuple
from urllib.parse import urlparse


class Name(NamedTuple):
    """What is known about an endpoint's identity, from the registry alone."""

    title: str | None
    domain: str | None
    datasets: int | None
    host: str


def _host(url: str) -> str:
    """The authority, for a row that has no title to show."""
    return urlparse(url).netloc or url


def load_names(paths: list[Path]) -> dict[str, Name]:
    """Every endpoint any of these files names, keyed by its URL.

    A file that does not exist, is unreadable, has invalid encoding, is
    malformed TOML, or contains wrong-shaped entries contributes nothing
    rather than raising. The site must serve before anyone has seeded a
    registry, and a corrupted file must not prevent loading the remaining
    paths. One malformed file erasing good data would be precisely the
    failure that this tolerance exists to prevent.

    Where two files name one endpoint, the entry carrying a title wins,
    whichever order they were read in. The dev registries are bare strings and
    list some of the same URLs as the seeded ones; without this rule, loading
    them second would erase a real name.
    """
    out: dict[str, Name] = {}
    for path in paths:
        try:
            raw = tomllib.loads(Path(path).read_text())
            for entry in raw.get("endpoint", []):
                if isinstance(entry, str):
                    url, title, domain, datasets = entry, None, None, None
                elif isinstance(entry, dict):
                    url = entry.get("url", "")
                    title = entry.get("title")
                    domain = entry.get("domain")
                    datasets = entry.get("datasets")
                else:
                    continue
                if not url:
                    continue
                candidate = Name(title, domain, datasets, _host(url))
                existing = out.get(url)
                if existing is None or (existing.title is None and candidate.title):
                    out[url] = candidate
        except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
            continue
    return out


def display(name: Name | None, url: str) -> str:
    """What the row leads with.

    The host, not a title, when an endpoint serves several datasets: none of
    their names is the server's name, and picking one asserts something untrue.
    The URL itself when no registry names it at all, which is what a row for an
    endpoint dropped from the registry since its last run shows.
    """
    if name is None:
        return url
    if name.title:
        return name.title
    return name.host

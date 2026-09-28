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

    Where two files name one endpoint, each of `title`, `domain` and
    `datasets` is kept from whichever file set it FIRST, independently of the
    other two fields, whichever order the files were read in. This was a
    title-only rule until 2026-09-17 -- "the entry carrying a title wins" --
    and that was a bug, not a simplification: a candidate that carried a
    `domain` or a `datasets` count but no `title` (exactly the shape of a
    multi-dataset endpoint, which by definition has no one title -- see
    `display` below) could never override an earlier bare entry, so which of
    two files' domains survived was decided by glob order rather than by
    which one actually said something. Measured against the shipped registry,
    that bug silently dropped the domain of 13 real endpoints, because
    prober/registry/calibration-sample.toml lists some of them as bare,
    title-less strings and is read (alphabetically) before lod-cloud.toml's
    richer entries for the same URLs. Merging per field is what a dev
    registry listing a URL as a bare string was always supposed to mean:
    "I don't have anything to ADD", not "erase what another file said".
    """
    out: dict[str, Name] = {}
    for path in paths:
        try:
            raw = tomllib.loads(Path(path).read_text())
            # `[[service]]` records are flattened into the same per-URL shape
            # the loop below already reads, with EVERY url a service answers to
            # -- preferred, alternative and invalid -- carrying the service's
            # name. That is what makes a row published under a demoted spelling
            # still read as the service it belongs to: the store holds those
            # URLs permanently, because `emit::subject_iri` embeds whatever was
            # swept and run graphs are never rewritten.
            entries = list(raw.get("endpoint", []))
            for svc in raw.get("service", []):
                if not isinstance(svc, dict) or not svc.get("endpoint"):
                    continue
                shared = {
                    "title": svc.get("title"),
                    "domain": svc.get("domain"),
                    "datasets": svc.get("datasets"),
                }
                entries.append({"url": svc["endpoint"], **shared})
                for role in ("alternative", "invalid"):
                    for alias in svc.get(role) or []:
                        if isinstance(alias, dict) and alias.get("url"):
                            entries.append({"url": alias["url"], **shared})
            for entry in entries:
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
                if existing is None:
                    out[url] = candidate
                elif (existing.title is None) != (candidate.title is None):
                    # ONE of the two names this endpoint; the other only counts
                    # datasets for it. The one with the name also decides
                    # `datasets`, whichever order the files were read in.
                    # `datasets` travels with the title -- the naming file's
                    # own value, often None, rather than one inherited from a
                    # file that had no title for this URL at all.
                    #
                    # Symmetric on purpose. An earlier version tested only
                    # `existing.title is None and candidate.title is not None`,
                    # which fixed the shipped order and left the reverse
                    # broken, while the test's docstring claimed both. That is
                    # a promise nothing checked; it is checked now.
                    #
                    # Fix (a) of the whole-branch review:
                    # `query.wikidata.org` gets `datasets = 2` from
                    # lod-cloud.toml's bulk import (which names no title for
                    # it) and then "Wikidata" from kg-catalog.toml. The
                    # ordinary per-field merge below kept BOTH, and
                    # `display`'s datasets-before-title rule then showed the
                    # host, hiding the one deliberate name on arguably the
                    # registry's most recognisable endpoint. A `datasets`
                    # count set on the SAME entry as a title (a genuinely
                    # ambiguous multi-dataset endpoint) still survives: that
                    # shape is decided in the `existing is None` branch above,
                    # untouched by this one.
                    named = candidate if candidate.title is not None else existing
                    out[url] = Name(
                        title=named.title,
                        domain=existing.domain if existing.domain is not None else candidate.domain,
                        datasets=named.datasets,
                        host=existing.host,
                    )
                else:
                    out[url] = Name(
                        title=existing.title if existing.title is not None else candidate.title,
                        domain=existing.domain if existing.domain is not None else candidate.domain,
                        datasets=existing.datasets if existing.datasets is not None else candidate.datasets,
                        host=existing.host,
                    )
        except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
            continue
    return out


def display(name: Name | None, url: str) -> str:
    """What the row leads with.

    The host, not a title, when an endpoint serves several datasets: none of
    their names is the server's name, and picking one asserts something untrue.
    The URL itself when no registry names it at all, which is what a row for an
    endpoint dropped from the registry since its last run shows.

    `datasets` is checked BEFORE `title`, fix-round-2, and the order is
    load-bearing now that load_names (fix-round-1) merges each field
    independently: a row can carry a `title` from one registry file and a
    `datasets` count from another, and checking `title` first would show that
    borrowed title over an endpoint this function's own docstring says must
    show its host instead. No registry shipped today produces that
    combination (every title-carrying entry is single-dataset), so this was
    dormant rather than live, and checking `datasets` first closes it rather
    than leaving it to the data staying clean.
    """
    if name is None:
        return url
    if name.datasets:
        return name.host
    if name.title:
        return name.title
    return name.host


def load_aliases(paths: list[Path]) -> dict[str, str]:
    """Every non-preferred url mapped to the endpoint it belongs to.

    THE READ SIDE OF THE SERVICE SCHEMA, and the whole reason it is read-side:
    ``emit::subject_iri`` embeds the swept url in every subject and run graphs
    are never rewritten, so a row published under a spelling since demoted
    cannot be re-identified. It is resolved instead, here.

    Same tolerance as ``load_names``: a file that does not exist, does not
    parse, or holds wrong-shaped entries contributes nothing rather than
    raising. A broken registry must not stop the site serving, and an alias map
    that came back empty costs a row its redirect, not the page.
    """
    out: dict[str, str] = {}
    for path in paths:
        try:
            raw = tomllib.loads(Path(path).read_text())
        except Exception:
            continue
        for svc in raw.get("service", []) or []:
            if not isinstance(svc, dict):
                continue
            endpoint = svc.get("endpoint")
            if not endpoint:
                continue
            for role in ("alternative", "invalid"):
                for alias in svc.get(role) or []:
                    if not isinstance(alias, dict):
                        continue
                    url = alias.get("url")
                    # An alias that is ALSO somebody's endpoint is not resolved
                    # away. `registry::load_services` refuses that file, but
                    # this module never raises, so the safe reading here is to
                    # leave the url alone: a wrong redirect would send a reader
                    # to a different service's page.
                    if url and url != endpoint:
                        out.setdefault(url, endpoint)
    return out

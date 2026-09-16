"""Ranked matching over one endpoint's vocabulary.

Keyword search over this list already worked -- a substring test against the
local name, prefix and IRI. What it could not do was find `Receptor` from
`recpetor`, or `hasDrugTarget` from `drug target`, and people type both.

The rules live in Python rather than only in the browser so that they are
testable under pytest: this repo's CI is cargo plus pytest and carries no
JavaScript harness. web/static/vocab-search.js is a transliteration of this
module, and tests/fixtures/vocab_match_cases.json is the table both are
checked against.
"""

from __future__ import annotations

import re

# camelCase boundaries, and the punctuation that separates words in an IRI.
_BOUNDARY = re.compile(r"(?<=[a-z0-9])(?=[A-Z])|[_\-./:]+")

# Below four characters, edit distance 1 matches almost everything, so the
# fuzzy tier would return the whole vocabulary for a three-letter typo.
MIN_FUZZY_LENGTH = 4


def tokenize(name: str) -> list[str]:
    """One term's name, as the words it is written from."""
    return [part.lower() for part in _BOUNDARY.split(name) if part]


def _within_one_edit(a: str, b: str) -> bool:
    """Damerau-Levenshtein distance <= 1, decided without building a matrix.

    Transposition counts as ONE edit, and that is not a refinement -- it is the
    case this tier exists for. `recpetor` for `receptor` is two adjacent letters
    swapped, which plain Levenshtein scores as 2, so a distance-1 Levenshtein
    test rejects the very example this feature was specified around.
    """
    if a == b:
        return True
    la, lb = len(a), len(b)
    if abs(la - lb) > 1:
        return False
    if la > lb:
        a, b, la, lb = b, a, lb, la
    # Find the first and last positions where they differ.
    head = 0
    while head < la and a[head] == b[head]:
        head += 1
    tail = 0
    while tail < la - head and a[la - 1 - tail] == b[lb - 1 - tail]:
        tail += 1

    if la == lb:
        middle = la - head - tail
        if middle <= 1:
            return True  # one substitution, or none
        # One adjacent transposition: exactly two differing characters, swapped.
        return middle == 2 and a[head] == b[head + 1] and a[head + 1] == b[head]
    # One insertion or deletion: the differing run is a single character.
    return la - head - tail == 0


def score_word(word: str, tokens: list[str], haystack: str) -> int:
    """How well one typed word answers to one term. 0 (not at all) to 4 (exact)."""
    if any(t == word for t in tokens):
        return 4
    if any(t.startswith(word) for t in tokens):
        return 3
    if word in haystack:
        return 2
    if len(word) >= MIN_FUZZY_LENGTH and any(_within_one_edit(word, t) for t in tokens):
        return 1
    return 0


def rank(terms: list[dict], query: str) -> list[dict]:
    """The terms that answer to a query, best first, each in its band.

    A term is a `match` when every typed word scores 2 or better -- it is
    present, even if not adjacently or in that order. It is a `close` match
    when at least half the words score at all, which is what a typo produces.
    Anything else is omitted rather than shown greyed: a list that never
    shortens does not answer the question "is this here".
    """
    words = query.strip().lower().split()
    if not words:
        return list(terms)

    ranked = []
    for term in terms:
        tokens = (term.get("tokens") or "").split()
        haystack = " ".join(
            (term.get("local", ""), term.get("prefix", ""), term.get("iri", ""))
        ).lower()
        scores = [score_word(w, tokens, haystack) for w in words]
        if all(s >= 2 for s in scores):
            band = "match"
        elif sum(1 for s in scores if s >= 1) * 2 >= len(words):
            band = "close"
        else:
            continue
        ranked.append({**term, "band": band, "score": sum(scores)})

    # Band first, then total score, then the shorter name, then alphabetically.
    # The last two exist so the order cannot shuffle between keystrokes.
    ranked.sort(
        key=lambda t: (
            0 if t["band"] == "match" else 1,
            -t["score"],
            len(t.get("local", "")),
            t.get("local", ""),
        )
    )
    return ranked

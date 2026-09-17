"""The scoring rules behind the vocabulary search.

They live in Python so they can be tested here; web/static/vocab-search.js is a
transliteration, checked against the same table by
test_the_javascript_agrees_with_python.
"""

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from vocab_match import rank, score_word, tokenize
from test_page import CONTENT_ENDPOINT as ENDPOINT_WITH_VOCABULARY

CASES = json.loads(
    (Path(__file__).parent / "fixtures" / "vocab_match_cases.json").read_text()
)


@pytest.mark.parametrize(
    "name,expected",
    [
        ("hasDrugTarget", ["has", "drug", "target"]),
        ("nuclear_receptor-family", ["nuclear", "receptor", "family"]),
        ("Drug", ["drug"]),
        ("rdfs:label", ["rdfs", "label"]),
        ("ID", ["id"]),
    ],
)
def test_tokenize_splits_the_way_names_are_written(name, expected):
    assert tokenize(name) == expected


@pytest.mark.parametrize(
    "word,tokens,haystack,expected",
    [
        ("drug", ["drug", "target"], "drugtarget drugbank", 4),
        ("dru", ["drug", "target"], "drugtarget drugbank", 3),
        ("rugt", ["drug", "target"], "drugtarget drugbank", 2),
        ("recpetor", ["receptor"], "receptor drugbank", 1),  # transposition
        ("receptr", ["receptor"], "receptor drugbank", 1),  # deletion
        ("receptorr", ["receptor"], "receptor drugbank", 1),  # insertion
        ("zzz", ["receptor"], "receptor drugbank", 0),
    ],
)
def test_the_five_scores(word, tokens, haystack, expected):
    assert score_word(word, tokens, haystack) == expected


def test_a_three_letter_typo_does_not_fuzzy_match():
    """At three characters, edit distance 1 matches almost everything."""
    assert score_word("dgu", ["drug"], "drug") == 0


def test_a_typo_finds_the_term_substring_matching_misses():
    terms = [{"local": "Receptor", "prefix": "drugbank", "iri": "", "tokens": "receptor"}]
    got = rank(terms, "recpetor")
    assert [t["local"] for t in got] == ["Receptor"]
    assert got[0]["band"] == "close"


def test_a_namespace_match_lands_in_the_close_band():
    """`Pathway` scores on "drug" only because its prefix is drugbank.

    That is a real match -- somebody searching a drugbank endpoint for "drug"
    means the namespace -- but it is not what they asked for, so it sits under
    `close matches` rather than beside the terms that matched both words.
    """
    terms = [
        {"local": "DrugTarget", "prefix": "drugbank", "iri": "", "tokens": "drug target"},
        {"local": "Pathway", "prefix": "drugbank", "iri": "", "tokens": "pathway"},
    ]
    got = rank(terms, "drug target")
    assert [t["band"] for t in got] == ["match", "close"]


def test_two_words_in_the_wrong_order_still_match():
    # Unrelated's prefix deliberately does NOT contain "drug" or "target": if it
    # did, it would land in the close band by the same namespace-match rule
    # test_a_namespace_match_lands_in_the_close_band pins down above, and this
    # test would then be asserting two different things about that rule at once.
    terms = [
        {"local": "targetOfDrug", "prefix": "drugbank", "iri": "", "tokens": "target of drug"},
        {"local": "Unrelated", "prefix": "other", "iri": "", "tokens": "unrelated"},
    ]
    got = rank(terms, "drug target")
    assert [t["local"] for t in got] == ["targetOfDrug"]
    assert got[0]["band"] == "match"


def test_exact_outranks_prefix_outranks_substring():
    terms = [
        {"local": "DrugInteraction", "prefix": "db", "iri": "", "tokens": "drug interaction"},
        {"local": "Drug", "prefix": "db", "iri": "", "tokens": "drug"},
        {"local": "antidrugAgent", "prefix": "db", "iri": "", "tokens": "antidrug agent"},
    ]
    assert [t["local"] for t in rank(terms, "drug")][0] == "Drug"


def test_an_empty_query_returns_everything_in_order():
    terms = [
        {"local": "B", "prefix": "db", "iri": "", "tokens": "b"},
        {"local": "A", "prefix": "db", "iri": "", "tokens": "a"},
    ]
    assert [t["local"] for t in rank(terms, "")] == ["B", "A"]
    assert all("band" not in t for t in rank(terms, ""))


def test_the_shared_case_table_holds():
    """The same table web/static/vocab-search.js is checked against.

    Bands, not just names. A name-only check cannot tell `close` from
    `match`, which is exactly the axis a mutation of vocab-search.js
    collapsed once (see test_the_javascript_agrees_with_python's docstring)
    while every name in this table still matched and the suite stayed green.
    """
    for case in CASES:
        ranked = rank(case["terms"], case["query"])
        got = [t["local"] for t in ranked]
        assert got == case["expected"], f"{case['query']!r}: {got} != {case['expected']}"
        got_bands = [t.get("band") for t in ranked]
        assert got_bands == case["expected_bands"], (
            f"{case['query']!r}: bands {got_bands} != {case['expected_bands']}"
        )


HIGHLIGHT_CASES = [
    # text, query, expected with matches in brackets
    ("hasDrugTarget", "drug", "has[Drug]Target"),
    ("hasDrugTarget", "DRUG", "has[Drug]Target"),
    ("hasDrugTarget", "has target", "[has]Drug[Target]"),
    # Overlaps resolve longest-first from the earliest start, so two needles
    # covering the same characters draw one mark and never a nested pair.
    ("abc", "a abc", "[abc]"),
    ("abab", "ab", "[ab][ab]"),
    # A fuzzy hit marks NOTHING. `rank` would surface this row in the "close
    # matches" band, and a mark over `Drug` for the query `recpetor` would
    # claim those characters are what was asked for.
    ("hasDrugTarget", "recpetor", "hasDrugTarget"),
    ("hasDrugTarget", "", "hasDrugTarget"),
    ("hasDrugTarget", "   ", "hasDrugTarget"),
    # Markup in the query is matched as characters, never interpreted. The DOM
    # half builds text nodes from these segments and writes no innerHTML, so
    # this is the value that reaches createTextNode.
    ("a<img src=x>b", "<img", "a[<img] src=x>b"),
    ("nothing here", "zzz", "nothing here"),
]


@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
def test_the_search_marks_what_it_matched_and_nothing_else(tmp_path):
    """Highlighting is a pure function over (text, query), tested as one.

    It returns SEGMENTS rather than a string of HTML, and that shape is the
    security property as much as a convenience: the query is whatever someone
    typed into the box, so a function assembling `<mark>` + needle + `</mark>`
    would put markup from the search field into the page. The caller builds
    text nodes. The `<img` case below is the one that would show it.

    The other claim is honesty. `rank` also accepts a token prefix and a
    one-edit typo, and this matcher accepts neither: a row surfaced for
    `recpetor` appears under "close matches" with nothing marked, which says
    it is here and not because it contains what you typed.

    Skipped where node is absent, like the transliteration test below it.
    """
    script = Path(__file__).resolve().parents[1] / "static" / "vocab-search.js"
    mjs_copy = tmp_path / "vocab-search.mjs"
    mjs_copy.write_bytes(script.read_bytes())

    harness = f"""
      import {{ highlightRanges }} from {str(mjs_copy)!r};
      const cases = {json.dumps([[t, q] for t, q, _ in HIGHLIGHT_CASES])};
      const drawn = cases.map(([text, query]) =>
        highlightRanges(text, query)
          .map((s) => (s.mark ? "[" + s.text + "]" : s.text)).join(""));
      // Every result must concatenate back to the input: a highlighter that
      // drops or duplicates a character has rewritten the term.
      const identity = cases.every(([text, query]) =>
        highlightRanges(text, query).map((s) => s.text).join("") === text);
      console.log(JSON.stringify({{drawn, identity}}));
    """
    result = subprocess.run(
        ["node", "--input-type=module", "-e", harness],
        capture_output=True, text=True, check=True,
    )
    got = json.loads(result.stdout)
    assert got["drawn"] == [expected for _, _, expected in HIGHLIGHT_CASES]
    assert got["identity"], "a highlighted term is not its own text"


@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
def test_the_javascript_agrees_with_python(tmp_path):
    """The browser and the server must score identically.

    There is no JavaScript harness in this repo's CI -- it is cargo plus pytest
    -- so this test runs only where node happens to exist, and the Python rules
    above are the ones that gate. That is the trade this arrangement makes: the
    rules are always tested, and the transliteration is checked wherever it can
    be. If the two ever drift, it will be here that it shows.

    Checks bands as well as names, which it did not always do. A JS mutation
    that labels every surviving term "match" -- collapsing "close matches"
    into "matches", i.e. presenting a typo hit as an exact one -- passed this
    suite 21/21 while the shared fixture carried only `expected` names, none
    of which that mutation changes. `expected_bands` is the fixture column
    added to close that gap; see below for the mutation demonstrated live.
    """
    script = Path(__file__).resolve().parents[1] / "static" / "vocab-search.js"
    cases = Path(__file__).parent / "fixtures" / "vocab_match_cases.json"

    # A bare .js with no controlling package.json is CommonJS as far as node
    # is concerned, and this repo deliberately has no package.json anywhere
    # (see the module docstring above) -- so `import` of vocab-search.js
    # directly fails with a CommonJS/ESM syntax error. .mjs has been
    # unambiguous ESM since node 12.17 with no flag needed, so the fix is to
    # hand node a copy under that extension rather than tell it to guess:
    # a flag good enough to guess right also has to exist on whatever node is
    # installed, and on an older node it does not, which turns the
    # `shutil.which` skip above into a hard error instead of a skip.
    mjs_copy = tmp_path / "vocab-search.mjs"
    mjs_copy.write_bytes(script.read_bytes())

    harness = f"""
      import {{ rank }} from {str(mjs_copy)!r};
      import {{ readFileSync }} from 'node:fs';
      const cases = JSON.parse(readFileSync({str(cases)!r}, 'utf8'));
      const names = cases.map(c => rank(c.terms, c.query).map(t => t.local));
      const bands = cases.map(c => rank(c.terms, c.query).map(t => t.band ?? null));
      console.log(JSON.stringify({{names, bands}}));
    """
    result = subprocess.run(
        ["node", "--input-type=module", "-e", harness],
        capture_output=True, text=True, check=True,
    )
    got = json.loads(result.stdout)
    assert got["names"] == [c["expected"] for c in CASES], (
        "vocab-search.js and vocab_match.py disagree on WHICH terms match; "
        "they are the same rules written twice and must stay that way"
    )
    assert got["bands"] == [c["expected_bands"] for c in CASES], (
        "vocab-search.js and vocab_match.py disagree on the BAND a term "
        "lands in -- a typo hit labelled the same as an exact one is exactly "
        "the defect this comparison exists to catch"
    )


def test_every_term_carries_its_tokens(store_content_profiles):
    """The browser must not re-tokenise every row on every keystroke."""
    from explore_payload import endpoint_vocabulary

    terms = endpoint_vocabulary(store_content_profiles, ENDPOINT_WITH_VOCABULARY)
    assert terms, "the fixture must have vocabulary"
    for term in terms:
        assert term["tokens"] == " ".join(tokenize(term["local"]) + tokenize(term["prefix"]))

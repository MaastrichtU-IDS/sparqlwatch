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
    """The same table web/static/vocab-search.js is checked against."""
    for case in CASES:
        got = [t["local"] for t in rank(case["terms"], case["query"])]
        assert got == case["expected"], f"{case['query']!r}: {got} != {case['expected']}"


@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
def test_the_javascript_agrees_with_python():
    """The browser and the server must score identically.

    There is no JavaScript harness in this repo's CI -- it is cargo plus pytest
    -- so this test runs only where node happens to exist, and the Python rules
    above are the ones that gate. That is the trade this arrangement makes: the
    rules are always tested, and the transliteration is checked wherever it can
    be. If the two ever drift, it will be here that it shows.
    """
    script = Path(__file__).resolve().parents[1] / "static" / "vocab-search.js"
    cases = Path(__file__).parent / "fixtures" / "vocab_match_cases.json"
    harness = f"""
      import {{ rank }} from {str(script)!r};
      import {{ readFileSync }} from 'node:fs';
      const cases = JSON.parse(readFileSync({str(cases)!r}, 'utf8'));
      const out = cases.map(c => rank(c.terms, c.query).map(t => t.local));
      console.log(JSON.stringify(out));
    """
    result = subprocess.run(
        # --experimental-detect-module: there is no package.json anywhere in
        # this repo (by design -- see the module docstring above), so nothing
        # tells node vocab-search.js is an ES module rather than CommonJS.
        # This flag makes node look at the file's own syntax instead.
        ["node", "--experimental-detect-module", "--input-type=module", "-e", harness],
        capture_output=True, text=True, check=True,
    )
    got = json.loads(result.stdout)
    assert got == [c["expected"] for c in CASES], (
        "vocab-search.js and vocab_match.py disagree; they are the same rules "
        "written twice and must stay that way"
    )


def test_every_term_carries_its_tokens(store_content_profiles):
    """The browser must not re-tokenise every row on every keystroke."""
    from explore_payload import endpoint_vocabulary

    terms = endpoint_vocabulary(store_content_profiles, ENDPOINT_WITH_VOCABULARY)
    assert terms, "the fixture must have vocabulary"
    for term in terms:
        assert term["tokens"] == " ".join(tokenize(term["local"]) + tokenize(term["prefix"]))

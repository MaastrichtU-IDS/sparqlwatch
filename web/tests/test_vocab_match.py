"""The scoring rules behind the vocabulary search.

They live in Python so they can be tested here; web/static/vocab-search.js is a
transliteration, checked against the same table by
test_the_javascript_agrees_with_python.
"""

import json
from pathlib import Path

import pytest

from vocab_match import rank, score_word, tokenize

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

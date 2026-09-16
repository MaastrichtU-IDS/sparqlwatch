"""Reading the registry files the prober is seeded from.

These files are the site's only source of a name. They are NOT measurements:
a title is what a catalogue called an endpoint, and the site says so rather
than presenting it as something a sweep found.
"""

from pathlib import Path

import pytest

from registry_names import Name, display, load_names


def test_a_bare_string_registry_yields_a_name_with_no_title(tmp_path):
    """endpoints.toml is hand-kept in the old shape and is not regenerated."""
    p = tmp_path / "bare.toml"
    p.write_text('endpoint = ["https://example.org/sparql"]\n')
    got = load_names([p])
    assert got["https://example.org/sparql"].title is None
    assert got["https://example.org/sparql"].host == "example.org"


def test_a_table_registry_yields_the_title_and_domain(tmp_path):
    p = tmp_path / "rich.toml"
    p.write_text(
        '[[endpoint]]\n'
        'url = "https://example.org/sparql"\n'
        'title = "Example Dataset"\n'
        'domain = "life_sciences"\n'
    )
    got = load_names([p])
    assert got["https://example.org/sparql"].title == "Example Dataset"
    assert got["https://example.org/sparql"].domain == "life_sciences"


def test_a_multi_dataset_endpoint_shows_its_host_not_a_title(tmp_path):
    """The rule this whole feature turns on.

    One server hosting 42 datasets has no single name, and choosing one of the
    42 would assert something untrue about the server.
    """
    p = tmp_path / "many.toml"
    p.write_text(
        '[[endpoint]]\nurl = "https://many.example/sparql"\ndatasets = 42\n'
    )
    got = load_names([p])
    name = got["https://many.example/sparql"]
    assert name.title is None
    assert name.datasets == 42
    assert display(name, "https://many.example/sparql") == "many.example"


def test_a_multi_dataset_endpoint_shows_its_host_even_with_a_borrowed_title():
    """Fix-round-2: a name carrying BOTH a title and a datasets count still
    shows the host, not the title.

    Unreachable through a single registry file today (nothing shipped pairs
    the two), but load_names (fix-round-1) merges title, domain and datasets
    independently, so a title from one file and a datasets count from
    another land on the same Name -- and display() checking `title` before
    `datasets` would show the borrowed title over an endpoint serving many
    datasets, exactly the misrepresentation this function's own docstring
    says it prevents. Built directly rather than through two registry files,
    so this pins display()'s own rule rather than load_names' merge order.
    """
    name = Name(title="One Of Forty-Two", domain=None, datasets=42, host="many.example")
    assert display(name, "https://many.example/sparql") == "many.example"


def test_an_endpoint_in_no_registry_displays_its_url(tmp_path):
    """A store can hold runs for an endpoint since dropped from the registry."""
    assert display(None, "https://gone.example/sparql") == "https://gone.example/sparql"


def test_a_later_file_does_not_silently_lose_to_an_earlier_one(tmp_path):
    """Two registries are loaded; kg-catalog's nine are hand-written.

    If both name an endpoint, the one with a title wins over one without,
    because a bare-string dev registry listing the same URL must not erase a
    real name.
    """
    a = tmp_path / "a.toml"
    a.write_text('endpoint = ["https://example.org/sparql"]\n')
    b = tmp_path / "b.toml"
    b.write_text(
        '[[endpoint]]\nurl = "https://example.org/sparql"\ntitle = "Real Name"\n'
    )
    assert load_names([a, b])["https://example.org/sparql"].title == "Real Name"
    assert load_names([b, a])["https://example.org/sparql"].title == "Real Name"


def test_a_bare_entry_does_not_erase_a_later_domain(tmp_path):
    """The bug fix-round-1 found: the merge used to key on TITLE alone, so a
    candidate that carried a domain (or a datasets count) but no title --
    exactly what a multi-dataset endpoint looks like -- could never override
    an earlier bare entry. Measured against the shipped registry, that
    silently dropped the domain of 13 real endpoints, because
    prober/registry/calibration-sample.toml lists some of them bare and is
    read before lod-cloud.toml's richer, still title-less entries for the
    same URLs. Fields now merge independently, so this must hold in BOTH
    read orders."""
    bare = tmp_path / "bare.toml"
    bare.write_text('endpoint = ["https://example.org/sparql"]\n')
    rich = tmp_path / "rich.toml"
    rich.write_text(
        '[[endpoint]]\nurl = "https://example.org/sparql"\n'
        'domain = "government"\ndatasets = 2\n'
    )
    bare_then_rich = load_names([bare, rich])["https://example.org/sparql"]
    assert bare_then_rich.domain == "government"
    assert bare_then_rich.datasets == 2

    rich_then_bare = load_names([rich, bare])["https://example.org/sparql"]
    assert rich_then_bare.domain == "government"
    assert rich_then_bare.datasets == 2


def test_a_missing_registry_file_is_not_fatal(tmp_path):
    """The site must serve before anyone has seeded a registry."""
    assert load_names([tmp_path / "nope.toml"]) == {}


def test_a_file_with_invalid_utf8_contributes_nothing(tmp_path):
    """The site must serve before anything is seeded, even with bad encoding."""
    p = tmp_path / "broken.toml"
    p.write_bytes(b'endpoint = ["https://example.org/sparql"]\n\xff\xfe')
    got = load_names([p])
    assert got == {}


def test_a_file_with_wrong_shaped_entries_contributes_nothing(tmp_path):
    """An endpoint array holding non-string, non-table values is skipped."""
    p = tmp_path / "malformed.toml"
    p.write_text('endpoint = [1, 2, 3]\n')
    got = load_names([p])
    assert got == {}


def test_a_bad_first_file_does_not_prevent_loading_the_second(tmp_path):
    """One corrupted file must not cost you the good one.

    This is the property the fix is really for: a bad file must not prevent
    remaining paths from loading.
    """
    bad = tmp_path / "bad.toml"
    bad.write_bytes(b'endpoint = ["https://example.org/sparql"]\n\xff\xfe')
    good = tmp_path / "good.toml"
    good.write_text('endpoint = ["https://good.example/sparql"]\n')
    got = load_names([bad, good])
    assert "https://good.example/sparql" in got
    assert got["https://good.example/sparql"].host == "good.example"


def test_the_real_registries_parse():
    """Against the files actually shipped, not a fixture."""
    here = Path(__file__).resolve().parents[2] / "prober" / "registry"
    if not here.is_dir():
        pytest.skip("registry sources are not in a runtime image")
    got = load_names(sorted(here.glob("*.toml")))
    assert len(got) > 500, f"expected the seeded registry, got {len(got)}"
    titled = [n for n in got.values() if n.title]
    assert titled, "no registry entry carries a title"


def test_every_seeded_endpoint_shows_something_other_than_a_bare_url():
    """Not a style rule: a row showing a URL where every other row shows a name
    reads as a failure, and there should be none by accident."""
    here = Path(__file__).resolve().parents[2] / "prober" / "registry"
    if not here.is_dir():
        pytest.skip("registry sources are not in a runtime image")
    names = load_names(sorted(here.glob("*.toml")))
    bare = [u for u, n in names.items() if display(n, u) == u]
    assert not bare, f"{len(bare)} endpoints would show a bare URL: {bare[:3]}"

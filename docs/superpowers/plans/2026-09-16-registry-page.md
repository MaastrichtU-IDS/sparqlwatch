# Registry Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the registry scannable — named endpoints and facet pills above the fold, the metrics matrix below them.

**Architecture:** The catalogue dump `seed-registry` already downloads carries a `title` and `domain` beside every endpoint URL; the seeder starts keeping them, so the generated registry files become arrays of tables. The prober's reader tolerates both shapes and its behaviour is unchanged. The web process reads the registry files directly — they are already in the image — and the same title map feeds both the page and `_matches_query`, so both representations of the index still name the same endpoints.

**Tech Stack:** Rust (`serde`, `toml`), Python 3.12 (`tomllib`, FastAPI, Jinja2), pytest, cargo.

**Spec:** `docs/superpowers/specs/2026-09-16-registry-page-design.md` — read it first; this plan argues from it.

## Global Constraints

- **No new request is made to anyone's server.** `seed-registry` downloads the dump it already downloads; the prober probes the same endpoints in the same order and sends nothing different. No file under `web/queries/` is edited.
- **The title never enters the run graph.** `queries/index_description.rq` rests on constructing nothing that is not already in a run graph; a catalogue title is an input, not an observation.
- **`?q=` must name the same endpoints in HTML and RDF.** One predicate, `_matches_query`, serves both branches. If it learns about titles, it learns for both.
- **Names are never presented as sparqlwatch's own finding.** The title sits above the URL, and the page states once where names come from.
- **An endpoint with more than one title shows its host, never a title** — choosing one of 42 asserts something untrue.
- **Both registry shapes must parse.** `endpoints.toml` and `endpoints.container.toml` stay bare strings; only generated files gain fields.
- Token names are the existing nineteen plus `--fill`. `color-scheme` is `light dark`.
- Python: run `pytest` from `web/`. Rust: `cargo test` and `cargo clippy --all-targets -- -D warnings` from `prober/`. Commit after every task.

---

## File Structure

| File | Responsibility |
|---|---|
| `prober/src/registry.rs` | **Modify.** `EndpointFile` accepts strings or tables; `load_endpoints` still returns `Vec<String>`. |
| `prober/src/seed.rs` | **Modify.** `Seeded` carries a title and domain per endpoint. |
| `prober/src/bin/seed-registry.rs` | **Modify.** `registry_toml` writes the table form. |
| `prober/registry/kg-catalog.toml` | **Modify.** Nine hand-written titles. |
| `web/registry_names.py` | **Create.** Reads the registry TOMLs; the only thing that knows their shape. |
| `web/app.py` | `_matches_query` consults names; `_index_rows` carries them; pills. |
| `web/templates/index.html` | Row layout, pills, matrix moved below. |
| `web/tests/test_registry_names.py` | **Create.** The reader's own tests. |

---

### Task 1: The registry reader accepts both shapes

**Files:**
- Modify: `prober/src/registry.rs:40-43` (`EndpointFile`) and `:60` (`load_endpoints`)
- Test: `prober/src/registry.rs` (inline `#[cfg(test)]`, following the file's existing pattern)

**Interfaces:**
- Produces: `load_endpoints(toml_text: &str, excluded: &[Exclusion]) -> anyhow::Result<Vec<String>>` — **signature unchanged**. Later tasks rely on that.
- Produces: `pub struct RegistryEntry { pub url: String, pub title: Option<String>, pub domain: Option<String>, pub datasets: Option<u32> }` and `pub fn load_registry(toml_text: &str) -> anyhow::Result<Vec<RegistryEntry>>` — Task 3 writes this shape; nothing in the prober's sweep path reads it.

This task is first because every other task depends on the file format being readable, and because getting it wrong breaks the dev registries on the next sweep.

- [ ] **Step 1: Write the failing test**

Add to `prober/src/registry.rs`'s test module:

```rust
#[test]
fn a_bare_string_list_still_loads() {
    // endpoints.toml and endpoints.container.toml are hand-written in this
    // shape and are not regenerated. A reader that only understood the new
    // shape would fail the next sweep on a file nobody touched.
    let text = r#"endpoint = ["https://example.org/sparql"]"#;
    let got = load_endpoints(text, &[]).expect("the bare form must still parse");
    assert_eq!(got, vec!["https://example.org/sparql".to_string()]);
}

#[test]
fn a_table_list_loads_and_yields_its_urls() {
    let text = r#"
[[endpoint]]
url = "https://example.org/sparql"
title = "Example"
domain = "government"
"#;
    let got = load_endpoints(text, &[]).expect("the table form must parse");
    assert_eq!(got, vec!["https://example.org/sparql".to_string()]);
}

#[test]
fn the_two_shapes_may_be_mixed_in_one_file() {
    // Not a shape we write, but a shape a half-finished hand edit produces,
    // and refusing it with a serde error names neither line.
    let text = r#"
endpoint = ["https://a.example/sparql", { url = "https://b.example/sparql", title = "B" }]
"#;
    let got = load_endpoints(text, &[]).expect("a mixed list must parse");
    assert_eq!(got.len(), 2);
}

#[test]
fn load_registry_keeps_what_load_endpoints_drops() {
    let text = r#"
[[endpoint]]
url = "https://example.org/sparql"
title = "Example"
domain = "government"

[[endpoint]]
url = "https://many.example/sparql"
datasets = 42
"#;
    let got = load_registry(text).expect("the registry form must parse");
    assert_eq!(got[0].title.as_deref(), Some("Example"));
    assert_eq!(got[0].domain.as_deref(), Some("government"));
    assert_eq!(got[1].title, None);
    assert_eq!(got[1].datasets, Some(42));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd prober && cargo test registry:: 2>&1 | tail -20`
Expected: FAIL — `load_registry` does not exist, and the table-form tests fail to deserialise into `Vec<String>`.

- [ ] **Step 3: Implement**

Replace `EndpointFile` at `registry.rs:40-43`:

```rust
/// One entry as the file may spell it.
///
/// Untagged rather than a struct with optional fields, because the four
/// registry files do not agree and are not written by the same hand:
/// `endpoints.toml` and `endpoints.container.toml` are hand-kept lists of bare
/// strings, and `seed-registry` writes tables. A struct would reject every bare
/// string and fail the next sweep on a file nobody edited.
#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Url(String),
    Described(RegistryEntry),
}

/// An entry with whatever the catalogue said about it.
///
/// `title` is ABSENT, not empty, for an endpoint that serves many datasets:
/// `datasets` carries the count instead, and the site shows the host. Picking
/// one of forty-two titles would assert something untrue about the server.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RegistryEntry {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub datasets: Option<u32>,
}

#[derive(Deserialize)]
struct EndpointFile {
    endpoint: Vec<Entry>,
}
```

In `load_endpoints`, replace `let deduped = dedupe(&file.endpoint);` with:

```rust
    // The sweep path wants URLs and nothing else, so the extra fields stop
    // here. Every rule below this line judges the string, and none of them
    // has an opinion about a title.
    let urls: Vec<String> = file
        .endpoint
        .into_iter()
        .map(|e| match e {
            Entry::Url(u) => u,
            Entry::Described(d) => d.url,
        })
        .collect();
    let deduped = dedupe(&urls);
```

And add, beside `load_endpoints`:

```rust
/// Every entry with what the catalogue said about it, unfiltered.
///
/// Deliberately not `load_endpoints`: that function applies the sweep's
/// policy -- dedupe, credentials, exclusions, reserved names -- and the site
/// needs a lookup table, not a sweep list. An endpoint the sweep refuses can
/// still appear in a stored run from before the refusal, and its row should
/// still find a name.
pub fn load_registry(toml_text: &str) -> anyhow::Result<Vec<RegistryEntry>> {
    let file: EndpointFile = toml::from_str(toml_text)?;
    Ok(file
        .endpoint
        .into_iter()
        .map(|e| match e {
            Entry::Url(url) => RegistryEntry { url, title: None, domain: None, datasets: None },
            Entry::Described(d) => d,
        })
        .collect())
}
```

- [ ] **Step 4: Run the tests**

Run: `cd prober && cargo test registry:: 2>&1 | tail -6`
Expected: all pass.

- [ ] **Step 5: Prove the prober is unaffected**

Run: `cd prober && cargo test 2>&1 | tail -6 && cargo clippy --all-targets -- -D warnings 2>&1 | tail -3`
Expected: the whole suite green, clippy silent. `load_endpoints`' signature did not change, so nothing downstream should have moved.

- [ ] **Step 6: Commit**

```bash
git add prober/src/registry.rs
git commit -m "Read a registry entry that carries more than a URL"
```

---

### Task 2: The seeder keeps the titles it already reads

**Files:**
- Modify: `prober/src/seed.rs:41-44` (`Seeded`) and `:133-140` (the extraction loop)
- Modify: `prober/src/bin/seed-registry.rs:150-162` (`registry_toml`)
- Test: `prober/src/seed.rs` inline tests

**Interfaces:**
- Consumes: `RegistryEntry` from Task 1.
- Produces: `Seeded { pub endpoints: Vec<RegistryEntry>, pub counts: Counts }` — the field keeps its name and changes element type. `registry_toml(&[RegistryEntry]) -> String`.

**The fact this task rests on, measured against the live dump on 2026-09-16:** every one of the 543 registry endpoints has a `title` (543 of 543), 40 of them have more than one, and one has 42. Do not assume a title is unique per endpoint — the whole point of this task is that it is not.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_dataset_title_travels_with_its_endpoint() {
    let dump = br#"{
      "a": {"title": "Alpha", "domain": "government",
            "sparql": [{"access_url": "https://a.example/sparql"}]}
    }"#;
    let got = candidates(dump, &[]).expect("the dump parses");
    assert_eq!(got.endpoints[0].url, "https://a.example/sparql");
    assert_eq!(got.endpoints[0].title.as_deref(), Some("Alpha"));
    assert_eq!(got.endpoints[0].domain.as_deref(), Some("government"));
    assert_eq!(got.endpoints[0].datasets, None);
}

#[test]
fn an_endpoint_serving_many_datasets_gets_a_count_and_no_title() {
    // Measured on the 2026-06-15 dump: 40 registry endpoints carry more than
    // one title and one carries 42. "Results of R&D" is not the name of a
    // server hosting forty-two things, so the site shows the host instead.
    let dump = br#"{
      "a": {"title": "Alpha", "domain": "government",
            "sparql": [{"access_url": "https://many.example/sparql"}]},
      "b": {"title": "Beta", "domain": "government",
            "sparql": [{"access_url": "https://many.example/sparql"}]}
    }"#;
    let got = candidates(dump, &[]).expect("the dump parses");
    assert_eq!(got.endpoints.len(), 1, "one endpoint, however many datasets");
    assert_eq!(got.endpoints[0].title, None, "no title may be chosen from two");
    assert_eq!(got.endpoints[0].datasets, Some(2));
}

#[test]
fn an_empty_domain_is_absent_rather_than_empty() {
    // 118 registry endpoints have "" for a domain in the dump. An empty string
    // reaching the site becomes a facet pill with no name.
    let dump = br#"{
      "a": {"title": "Alpha", "domain": "",
            "sparql": [{"access_url": "https://a.example/sparql"}]}
    }"#;
    let got = candidates(dump, &[]).expect("the dump parses");
    assert_eq!(got.endpoints[0].domain, None);
}

#[test]
fn the_registry_file_round_trips_through_the_reader() {
    let entries = vec![
        crate::registry::RegistryEntry {
            url: "https://a.example/sparql".into(),
            title: Some("Alpha".into()),
            domain: Some("government".into()),
            datasets: None,
        },
        crate::registry::RegistryEntry {
            url: "https://many.example/sparql".into(),
            title: None,
            domain: None,
            datasets: Some(42),
        },
    ];
    let text = registry_toml(&entries);
    let back = crate::registry::load_registry(&text).expect("what we write, we read");
    assert_eq!(back, entries);
}
```

The last test belongs in `seed-registry.rs`'s test module, since `registry_toml` lives there.

- [ ] **Step 2: Run to verify it fails**

Run: `cd prober && cargo test seed 2>&1 | tail -20`
Expected: FAIL — `got.endpoints[0].url` does not compile; `endpoints` is `Vec<String>`.

- [ ] **Step 3: Implement the extraction**

In `seed.rs`, change `Seeded`:

```rust
pub struct Seeded {
    /// The candidates, in dataset-key order, after every refusal, each with
    /// whatever the dump said about it.
    pub endpoints: Vec<registry::RegistryEntry>,
    pub counts: Counts,
}
```

In `candidates`, collect titles per URL before building the list. Replace the `found` accumulation with a map that counts datasets per URL and keeps the first title and domain seen:

```rust
    // A URL may appear under several datasets: 40 do in the 2026-06-15 dump,
    // one under 42. So this accumulates per URL rather than per dataset, and
    // the count is what decides whether a title is usable at all.
    //
    // A HashMap and not an ordered map: `found` already carries first-seen
    // order and the refusal pipeline preserves it, so this only has to
    // answer "what did the dump say about this URL" and never decides
    // sequence.
    let mut seen: std::collections::HashMap<String, (Vec<String>, Option<String>)> =
        std::collections::HashMap::new();
```

and, where a URL is found, push the dataset's title and record its domain:

```rust
                if !url.is_empty() {
                    let title = dataset.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    // An empty string is a missing value spelled differently:
                    // 118 registry endpoints carry "" for a domain, and an
                    // empty domain reaching the site is a pill with no name.
                    let domain = dataset
                        .get("domain")
                        .and_then(|d| d.as_str())
                        .filter(|d| !d.is_empty())
                        .map(str::to_string);
                    let slot = seen.entry(url.to_string()).or_insert((Vec::new(), None));
                    if !title.is_empty() {
                        slot.0.push(title.to_string());
                    }
                    if slot.1.is_none() {
                        slot.1 = domain;
                    }
                    found.push(url.to_string());
                }
```

Then build the entries after the refusal pipeline has produced the final URL list, so refused URLs do not appear:

```rust
    let endpoints = final_urls
        .into_iter()
        .map(|url| {
            let (titles, domain) = seen.remove(&url).unwrap_or_default();
            // One title names the endpoint. Two or more mean the URL is a
            // server hosting several datasets, and none of their names is its
            // name, so the count goes instead and the site shows the host.
            let (title, datasets) = match titles.len() {
                0 => (None, None),
                1 => (Some(titles.into_iter().next().unwrap()), None),
                n => (None, Some(n as u32)),
            };
            registry::RegistryEntry { url, title, domain, datasets }
        })
        .collect();
```

Read the existing function before editing: `final_urls` above is a placeholder for whatever the last refusal stage binds. Use the real name; do not introduce a new binding.

**No new crate.** `prober/Cargo.toml` has no `indexmap` and does not need one: the order that makes two seeds diff readably comes from `found`, which the refusal pipeline preserves, so the map above is only a lookup. Do not add a dependency for this.

- [ ] **Step 4: Implement the writer**

In `seed-registry.rs`, change `registry_toml`:

```rust
fn registry_toml(endpoints: &[registry::RegistryEntry]) -> String {
    #[derive(Serialize)]
    struct EndpointFile<'a> {
        endpoint: &'a [registry::RegistryEntry],
    }
    let body = toml::to_string_pretty(&EndpointFile { endpoint: endpoints })
        .expect("an array of tables of strings is representable in TOML");
    format!("{GENERATED}{REGISTRY_HEADER}{body}")
}
```

`RegistryEntry` needs `Serialize` added to its derive in `registry.rs`, and `#[serde(skip_serializing_if = "Option::is_none")]` on the three optional fields so an endpoint with no domain does not write `domain = ` — check the rendered output by eye.

- [ ] **Step 5: Run the tests**

Run: `cd prober && cargo test 2>&1 | tail -6`
Expected: pass. The fixed-point test in `seed-registry.rs` may fail because the generated file's shape changed — that is expected and correct; regenerate in the next step.

- [ ] **Step 6: Regenerate the registry and read the diff**

Run: `cd prober && cargo run --bin seed-registry 2>&1 | tail -5`

Then look at the result: `git diff --stat prober/registry/` and `head -30 prober/registry/lod-cloud.toml`.

Check by eye, and report in your report: that 543 entries are still present, that a single-dataset endpoint carries a title, and that at least one carries `datasets` with no title. If the count moved from 543, stop and say so — a re-seed that changes the registry's membership is a different decision from this plan's.

- [ ] **Step 7: Commit**

```bash
git add prober/src/seed.rs prober/src/bin/seed-registry.rs prober/src/registry.rs prober/registry/
git commit -m "Keep the names the catalogue already gave us"
```

---

### Task 3: The site reads the registry

**Files:**
- Create: `web/registry_names.py`
- Create: `web/tests/test_registry_names.py`

**Interfaces:**
- Produces:
  - `load_names(paths: list[Path]) -> dict[str, Name]` — keyed by endpoint URL.
  - `class Name(NamedTuple): title: str | None; domain: str | None; datasets: int | None; host: str`
  - `def display(name: Name | None, url: str) -> str` — what the row shows.
  - Task 4 imports all three.

Nothing in this task touches `app.py`; it is a reader with its own tests.

- [ ] **Step 1: Write the failing test**

Create `web/tests/test_registry_names.py`:

```python
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


def test_a_missing_registry_file_is_not_fatal(tmp_path):
    """The site must serve before anyone has seeded a registry."""
    assert load_names([tmp_path / "nope.toml"]) == {}


def test_the_real_registries_parse():
    """Against the files actually shipped, not a fixture."""
    here = Path(__file__).resolve().parents[2] / "prober" / "registry"
    if not here.is_dir():
        pytest.skip("registry sources are not in a runtime image")
    got = load_names(sorted(here.glob("*.toml")))
    assert len(got) > 500, f"expected the seeded registry, got {len(got)}"
    titled = [n for n in got.values() if n.title]
    assert titled, "no registry entry carries a title"
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && .venv/bin/python -m pytest tests/test_registry_names.py -q`
Expected: FAIL — `ModuleNotFoundError: No module named 'registry_names'`.

- [ ] **Step 3: Implement**

Create `web/registry_names.py`:

```python
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

    A file that does not exist contributes nothing rather than raising: the
    site must serve before anyone has seeded a registry, and it already
    tolerates a store with no runs in it.

    Where two files name one endpoint, the entry carrying a title wins,
    whichever order they were read in. The dev registries are bare strings and
    list some of the same URLs as the seeded ones; without this rule, loading
    them second would erase a real name.
    """
    out: dict[str, Name] = {}
    for path in paths:
        try:
            raw = tomllib.loads(Path(path).read_text())
        except (OSError, tomllib.TOMLDecodeError):
            continue
        for entry in raw.get("endpoint", []):
            if isinstance(entry, str):
                url, title, domain, datasets = entry, None, None, None
            else:
                url = entry.get("url", "")
                title = entry.get("title")
                domain = entry.get("domain")
                datasets = entry.get("datasets")
            if not url:
                continue
            candidate = Name(title, domain, datasets, _host(url))
            existing = out.get(url)
            if existing is None or (existing.title is None and candidate.title):
                out[url] = candidate
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
```

- [ ] **Step 4: Run the tests**

Run: `cd web && .venv/bin/python -m pytest tests/test_registry_names.py -v`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add web/registry_names.py web/tests/test_registry_names.py
git commit -m "Read the names the registry carries"
```

---

### Task 4: Search matches names, in both representations

**Files:**
- Modify: `web/app.py` — `_matches_query` (grep for `def _matches_query`), and a module-level name map beside `STYLESHEET_PATH`
- Modify: `web/tests/test_negotiation.py`, `web/tests/test_index.py`

**Interfaces:**
- Consumes: `load_names`, `Name`, `display` from Task 3.
- Produces: `_matches_query(endpoint: str, needle: str | None) -> bool` — **signature unchanged**, behaviour widened. `_NAMES: dict[str, Name]`, read once at import.

**Why this task is separate from the page.** `?q=` filtering both representations through one predicate is the invariant that the previous redesign was corrected for. Widening what the predicate matches is exactly where that invariant breaks, so it lands and is tested before any markup changes.

- [ ] **Step 1: Write the failing test**

Append to `web/tests/test_negotiation.py`:

```python
def test_a_query_matching_a_title_narrows_both_representations(
    client_for, store_registry_sample
):
    """The invariant, under the case this feature introduces.

    Before names, ?q= could only match a URL, and both representations
    filtered on the same string. A title lives in the registry files and not in
    the run graph, so the RDF branch cannot see it unless the predicate they
    share does -- and if only the HTML saw it, the two would describe different
    endpoint sets under the same URL.
    """
    import app as app_module

    titled = [u for u, n in app_module._NAMES.items() if n.title]
    if not titled:
        pytest.skip("no registry entry carries a title in this checkout")
    url = titled[0]
    needle = app_module._NAMES[url].title.split()[0].lower()

    client = client_for(store_registry_sample)
    listed = set(endpoints_shown(client.get(f"/?q={needle}", headers={"accept": "text/html"}).text))
    graph = parse_graph(client.get(f"/?q={needle}", headers={"accept": "text/turtle"}).text)
    described = {
        str(t.object.value)
        for t in graph
        if str(t.predicate.value) in (
            "http://www.w3.org/ns/dqv#computedOn",
            "urn:sparqlwatch:notMeasuredOn",
        )
    }
    assert described == listed
```

Define `endpoints_shown` and `parse_graph` locally in that file if they are not already there — match the file's existing helpers rather than importing across test modules.

Append to `web/tests/test_index.py`:

```python
def test_a_query_matches_a_host(client_for, store_registry_sample):
    body = client_for(store_registry_sample).get("/?q=uniprot", headers={"accept": "text/html"}).text
    assert endpoints_shown(body), "a host match must still work"


def test_no_title_is_published_as_rdf(client_for, store_registry_sample):
    """A catalogue's title is not a measurement and does not enter the graph."""
    rdf = client_for(store_registry_sample).get("/", headers={"accept": "text/turtle"}).text
    assert "dcterms:title" not in rdf
    assert "http://purl.org/dc/terms/title" not in rdf
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && .venv/bin/python -m pytest tests/test_negotiation.py -k title -v`
Expected: FAIL — `app` has no attribute `_NAMES`.

- [ ] **Step 3: Implement**

In `web/app.py`, beside the other import-time constants (near `STYLESHEET_PATH`):

```python
from registry_names import Name, display, load_names

# The registry files, read once. The Dockerfile copies prober/registry/ into
# the image, so these are present at runtime; in a checkout they are the same
# files seed-registry writes.
#
# A missing directory yields an empty map rather than an error: the site serves
# before anything is seeded, and every row falls back to its URL.
_REGISTRY_DIR = Path(__file__).resolve().parents[1] / "prober" / "registry"
_NAMES: dict[str, Name] = load_names(sorted(_REGISTRY_DIR.glob("*.toml")))
```

Widen `_matches_query`:

```python
def _matches_query(endpoint: str, needle: str | None) -> bool:
    """Whether one endpoint answers to a search.

    Case-insensitive substring over the endpoint URL, its host, and the title
    the registry carries for it.

    This is ONE function on purpose, and it now reads _NAMES for the same
    reason: both representations of the index filter through it, so the page
    and the data agree by construction. A title that only the HTML could match
    would make ?q= mean two different things at one URL.
    """
    if not needle:
        return True
    needle = needle.strip().lower()
    if needle in endpoint.lower():
        return True
    name = _NAMES.get(endpoint)
    if name is None:
        return False
    return bool(
        (name.title and needle in name.title.lower())
        or needle in name.host.lower()
    )
```

- [ ] **Step 4: Run the tests**

Run: `cd web && .venv/bin/python -m pytest tests/test_negotiation.py tests/test_index.py -q`
Expected: pass.

- [ ] **Step 5: Run the whole suite**

Run: `cd web && .venv/bin/python -m pytest -q`
Expected: no new failures.

- [ ] **Step 6: Commit**

```bash
git add web/app.py web/tests/test_negotiation.py web/tests/test_index.py
git commit -m "Let a search find an endpoint by the name it is known by"
```

---

### Task 5: The row leads with a name

**Files:**
- Modify: `web/app.py` — `_index_rows` (grep for `def _index_rows`)
- Modify: `web/templates/index.html` — the row markup and its `head_extra` rules
- Modify: `web/tests/test_index.py`

**Interfaces:**
- Consumes: `_NAMES` and `display` from Task 4.
- Produces: each row dict gains `name: str`, `host: str`, `datasets: int | None`, `domain: str | None`. Task 6 reads `domain` for the pills.

- [ ] **Step 1: Write the failing test**

```python
def test_a_row_leads_with_the_name_and_keeps_the_url(client_for, store_registry_sample):
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    names = texts_with(body, "data-row-name")
    urls = texts_with(body, "data-row-url")
    assert names and len(names) == len(urls), "every row names itself and shows its URL"


def test_a_row_for_a_multi_dataset_endpoint_shows_a_count(client_for, store_many_datasets):
    """The rule: the host, and how many things it serves, never one of their names."""
    body = client_for(store_many_datasets).get("/", headers={"accept": "text/html"}).text
    assert "42 datasets" in body


def test_the_page_says_where_names_come_from(client_for, store_registry_sample):
    """A borrowed title is attributed, or the registry is asserting it."""
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    assert "data-name-provenance" in body
```

`store_many_datasets` does not exist. Add it to `web/tests/conftest.py` beside the others, loading a fixture whose endpoint is one the registry marks with `datasets`. If no such registry entry exists in the checkout, build the fixture registry file in `tmp_path` and monkeypatch `app._NAMES` — say in your report which you did.

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && .venv/bin/python -m pytest tests/test_index.py -k "leads_with_the_name or multi_dataset or provenance" -v`
Expected: 3 FAIL.

- [ ] **Step 3: Implement the context**

In `_index_rows`, for each row, add:

```python
        name = _NAMES.get(entry.endpoint)
        row["name"] = display(name, entry.endpoint)
        row["host"] = name.host if name else entry.endpoint
        row["datasets"] = name.datasets if name else None
        row["domain"] = name.domain if name else None
```

Read the existing function first and add these to whatever dict it already builds; do not restructure it.

- [ ] **Step 4: Implement the markup**

In `templates/index.html`, the row's leading cell becomes:

```html
<span class="row-id">
  <a data-row-name href="{{ row.href }}">{{ row.name }}</a>
  {%- if row.datasets %} <span class="datasets">{{ row.datasets }} datasets</span>{% endif %}
  <span class="url" data-row-url>{{ row.endpoint }}</span>
</span>
```

Keep `data-endpoint="{{ row.endpoint }}"` on the `<li>` — `?q=`'s tests and the client-side filter both read it.

Add to `head_extra`:

```css
  .row-id .url { display: block; font-family: var(--mono); font-size: 11.5px;
                 color: var(--text-dim); margin-top: 2px;
                 overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row-id .datasets { font-size: 12px; color: var(--text-dim); }
```

And once, above the rows:

```html
<p class="note" data-name-provenance>
  Names come from the catalogues these endpoints were registered in, not from
  anything this service measured. The URL beneath each one is what was probed.
</p>
```

- [ ] **Step 5: Run the tests**

Run: `cd web && .venv/bin/python -m pytest tests/test_index.py -q`
Expected: pass.

- [ ] **Step 6: Look at it**

Build a store and serve it:

```bash
cd web && .venv/bin/python load_run.py /tmp/s tests/fixtures/run-registry-sample.nq
SPARQLWATCH_STORE=/tmp/s .venv/bin/python -m uvicorn app:app --port 8099
```

Render `/` at 1200px and 390px with playwright. Report: horizontal overflow at each width, and whether a long title (the dump has one at 58 characters) wraps or truncates sensibly beside its URL.

- [ ] **Step 7: Commit**

```bash
git add web/app.py web/templates/index.html web/tests/test_index.py web/tests/conftest.py
git commit -m "Lead a registry row with the name the endpoint is known by"
```

---

### Task 6: Facet pills, and the matrix moves down

**Files:**
- Modify: `web/app.py` — `_index_context`
- Modify: `web/templates/index.html`
- Modify: `web/tests/test_index.py`

**Interfaces:**
- Consumes: `row["domain"]` from Task 5.
- Produces: `pills: list[dict]` in the index context — `{"label": str, "param": str, "value": str, "count": int, "on": bool}`.

- [ ] **Step 1: Write the failing test**

```python
def test_the_pills_carry_their_counts(client_for, store_registry_sample):
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    pills = with_attribute(body, "data-pill")
    assert pills, "the registry offers no facets"
    for p in pills:
        assert p["data-pill-count"].isdigit(), f"{p['data-pill']} has no count"


def test_a_pill_is_a_link_that_works_without_javascript(client_for, store_registry_sample):
    """A faceted view must be shareable, like ?q=."""
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    hrefs = [a["href"] for a in with_attribute(body, "data-pill")]
    assert all(h.startswith("/?") for h in hrefs), f"pills are not links: {hrefs}"


def test_a_domain_pill_narrows_the_rows(client_for, store_registry_sample):
    client = client_for(store_registry_sample)
    everything = endpoints_shown(client.get("/", headers={"accept": "text/html"}).text)
    narrowed = endpoints_shown(
        client.get("/?domain=government", headers={"accept": "text/html"}).text
    )
    assert 0 < len(narrowed) < len(everything)


def test_the_matrix_sits_below_the_rows(client_for, store_registry_sample):
    """Demoted, not removed: it is the only way to ask a precise question."""
    body = client_for(store_registry_sample).get("/", headers={"accept": "text/html"}).text
    assert body.index('data-facet-group="matrix"') > body.index("data-endpoint=")
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && .venv/bin/python -m pytest tests/test_index.py -k "pill or matrix_sits" -v`
Expected: 4 FAIL.

- [ ] **Step 3: Implement**

`index_resource` takes `domain: str | None = Query(None)`. In `_index_context`, filter by it beside `q`:

```python
    entries = [e for e in entries if _matches_query(e.endpoint, q)]
    if domain:
        entries = [e for e in entries if (_NAMES.get(e.endpoint) or _NO_NAME).domain == domain]
```

where `_NO_NAME = Name(None, None, None, "")` is a module constant, so the expression does not branch twice.

**`domain` must filter the RDF branch too.** It is the same class of change as `?q=`: pass it to `_only_matching_endpoints` or apply the same predicate, and add a negotiation test mirroring Task 4's. If you find that awkward, stop and report rather than shipping a filter that only one representation honours.

Build the pills from the filtered-before-facet entry set, so a count describes what clicking would yield:

```python
    # Counts describe what the pill would show, which is why they are computed
    # from the query-filtered set and not from the whole registry: a pill
    # reading 134 beside a search that has already cut the list to twelve is
    # describing a page the reader is not looking at.
    domains = Counter(
        d for e in entries if (d := (_NAMES.get(e.endpoint) or _NO_NAME).domain)
    )
```

Take the three commonest domains plus the measured facets (`answering`, `declares VoID`, `federates`) — derive the measured ones from the verdicts already on each entry rather than by re-querying.

- [ ] **Step 4: Implement the markup and move the matrix**

Pills, above the rows:

```html
<div class="pills">
  {% for p in pills %}
  <a class="pill{% if p.on %} on{% endif %}" data-pill="{{ p.value }}"
     data-pill-count="{{ p.count }}" href="/?{{ p.param }}={{ p.value | urlencode }}">
    {{ p.label }} <i>{{ p.count }}</i></a>
  {% endfor %}
</div>
```

Then **move** the `<section class="panel" data-facet-group="matrix">` block so it sits after the rows list. Move it whole — do not retype it, and do not change a single assertion inside it. Its facet tests must keep passing unchanged.

- [ ] **Step 5: Run the tests**

Run: `cd web && .venv/bin/python -m pytest tests/test_index.py tests/test_negotiation.py -q`
Expected: pass, including the pre-existing matrix facet tests.

- [ ] **Step 6: Look at it, then commit**

Render `/` and `/?domain=government` at 1200px and 390px. Report overflow at each and whether the pills wrap rather than scroll.

```bash
git add web/app.py web/templates/index.html web/tests/test_index.py tests/test_negotiation.py
git commit -m "Offer the registry's common questions as links, and demote the grid"
```

---

### Task 7: Close the loop

**Files:**
- Modify: `prober/registry/kg-catalog.toml`
- Modify: `web/tests/mobile/probe_mobile.py` if a page overflows
- Modify: `docs/kg-catalog-endpoints.md` only if it disagrees with the registry

- [ ] **Step 1: Give the nine catalogue endpoints their names**

`prober/registry/kg-catalog.toml` is hand-maintained and says so. Convert its nine bare strings to the table form, taking each title from `docs/kg-catalog-endpoints.md`, which already names them. Keep the file's header comments.

Verify: `cd prober && cargo test registry:: && cargo run --bin prober -- --help >/dev/null` — the file must still load.

- [ ] **Step 2: Assert every registry endpoint resolves to something showable**

Add to `web/tests/test_registry_names.py`:

```python
def test_every_seeded_endpoint_shows_something_other_than_a_bare_url():
    """Not a style rule: a row showing a URL where every other row shows a name
    reads as a failure, and there should be none by accident."""
    here = Path(__file__).resolve().parents[2] / "prober" / "registry"
    if not here.is_dir():
        pytest.skip("registry sources are not in a runtime image")
    names = load_names(sorted(here.glob("*.toml")))
    bare = [u for u, n in names.items() if display(n, u) == u]
    assert not bare, f"{len(bare)} endpoints would show a bare URL: {bare[:3]}"
```

If this fails, it has found registry entries with neither a title nor a host — report the list rather than weakening the test.

- [ ] **Step 3: Run the mobile probe**

```bash
cd web && .venv/bin/python load_run.py /tmp/s tests/fixtures/run-registry-sample.nq
SPARQLWATCH_STORE=/tmp/s .venv/bin/python -m uvicorn app:app --port 8099 &
.venv/bin/python tests/mobile/probe_mobile.py --base http://localhost:8099
```

Expected: 8 routes × 3 viewports, 0px overflow. **A long title is the new risk** — the dump has one at 58 characters, and it sits beside a URL in a narrow column. If `/` overflows, fix it in the stylesheet with wrapping or truncation, never `overflow-x: hidden`.

- [ ] **Step 4: Run everything**

```bash
cd web && .venv/bin/python -m pytest -q
cd ../prober && cargo test --quiet && cargo clippy --all-targets -- -D warnings
```

- [ ] **Step 5: Report the shape of the result**

Record in the commit message: how many of the 552 endpoints show a title, how many show a host with a dataset count, and how many show a bare URL.

- [ ] **Step 6: Commit**

```bash
git add prober/registry/kg-catalog.toml web/tests/ web/static/
git commit -m "Name the nine, and prove every row has something to show"
```

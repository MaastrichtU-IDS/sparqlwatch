# Stage 1c-a: Metrics Mean What They Say

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make four published metrics mean exactly what their labels claim, before a 548-endpoint registry sweep starts publishing them at scale.

**Architecture:** No new subsystems. One prerequisite that makes a whole class of bug impossible (an exhaustive probe dispatch), then four independent corrections: scope capability declarations to the service actually probed while grading the document as published (`declare.rs`), publish whether declarations were readable at all (`emit.rs`), add an `OPTIONS` preflight probe kind alongside the existing simple-GET CORS probe (`client.rs`, `resolve.rs`), and broaden the two graph-scoped metrics to named graphs (`metrics.toml`).

**Tech Stack:** Rust 1.96, edition 2021. tokio, reqwest 0.13, oxrdf 0.3 + oxrdfio 0.2, wiremock 0.6.5. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## Why this plan exists, and why it is half of stage 1c

Stage 1c was scoped as "registry seeding prerequisites" and accumulated eight
items. Four make what we already publish more truthful; four (cost classes,
per-host politeness, concurrency, incremental writes) make it safe and
affordable to probe 548 endpoints. Those groups share no code and neither
depends on the other, so they are two plans. This is the first: the output has to
mean what it says before volume makes it authoritative. The throughput half
follows as **1c-b**, and stage 1d (seeding) needs both.

## This is revision 2. Read this section.

Revision 1 was reviewed before execution and had **6 Critical and 9 Important
defects in the plan itself**. Three of them would have published a confident
wrong answer while every test the plan specified stayed green. They are fixed
below, and the fixes are called out where they land, because a future reader
deserves to know which parts of this plan exist because the obvious version was
wrong:

- The `match def.kind` in `lib.rs` ends in `_ => client.ask(...)`, a catch-all on
  a closed enum. Adding a probe kind without a dispatch arm therefore compiles,
  sends a GET with `?query=`, and publishes `absent`. Task 0 removes the
  catch-all so that mistake becomes a compile error.
- `tests/fixtures/virtuoso-stub.ttl:6` and `substantial.ttl:7` **do** state
  `sd:endpoint`. Revision 1 asserted the opposite and told the implementer to
  treat a resulting failure as a fallback bug, which would have reintroduced the
  exact leak Task 1 removes.
- A `sd:endpoint` that is stated but does not match ours (scheme mismatch,
  redirect, default port, host case) emptied the scope set. Because the grade
  read whole-document `triples` while every other grade input was scoped, that
  published "informativeness = stub" with verdict `verified` for a level-4
  description. Task 1 separates the two reductions so this cannot happen.
- Revision 1's Task 2 argued a tri-state `Declared` was unnecessary because the
  `service-description` row already tells a consumer whether a declaration was
  readable. That argument is **empirically false**: a description that declares
  `sfWithin` and then breaks mid-parse publishes `service-description =
  indeterminate` alongside `geo-functions = verified`. Task 2 now publishes the
  fact directly instead of inferring it.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.**
- `resolve()` in `src/resolve.rs` is the single home of judgement and stays a
  pure function. `src/client.rs` returns evidence and holds no opinion.
  `src/emit.rs` is a pure function of its inputs, with no clock read and no
  randomness.
- `--at` is never read from the clock.
- No composite score and no ranking, anywhere.
- **`absent` and `verified` are assertive verdicts.** Either may be published
  only when the evidence establishes it: the endpoint itself answered, with a
  status that speaks to the question, in a form we could parse. Everything else
  is `indeterminate`. When unsure which of the two a case is, it is
  `indeterminate`.
- Every test runs offline against a local `wiremock`. Only `live_smoke` touches
  real endpoints and stays `#[ignore]`d.
- Any new field on `MetricDef` must be added to the canonical string in
  `definitions_revision` **and** covered by
  `the_revision_is_a_pure_function_of_the_definitions`. This plan adds no
  `MetricDef` field, but it does add a metric to `metrics.toml`, which changes
  the published revision. That is correct: the definition list really changed.
- No em-dashes in prose, code comments or Markdown. Use commas, colons,
  parentheses, or separate sentences.
- After any experiment that temporarily edits source, restore it and run
  `touch src/*.rs tests/*.rs` before the final test run. A build artifact newer
  than the restored sources makes `cargo test` report the reverted code, which
  has already cost this project a full debugging cycle.

## Starting state

`main` at commit `774094d`. 112 tests pass, 1 ignored, clippy clean with
`--all-targets -- -D warnings`.

Relevant existing shapes, read from the source rather than remembered:

```rust
// src/metrics.rs
pub enum ProbeKind { Liveness, Cors, AskFilter, AskData, SelectIris, FetchWellKnown }

pub struct MetricDef {
    pub id: String, pub label: String, pub dimension: String, pub kind: ProbeKind,
    pub query: Option<String>, pub expect: Option<bool>, pub var: Option<String>,
    pub declared_by: Option<String>, pub graded: bool,
}

// src/declare.rs  (sets, NOT Vec)
pub struct Declarations {
    pub features: BTreeSet<String>,
    pub extension_functions: BTreeSet<String>,
    pub languages: BTreeSet<String>,
    pub triples: usize,
    pub names_dataset: bool,
    pub has_void_partitions: bool,
    pub has_entailment: bool,
    pub has_example_resources: bool,
}
pub fn parse_declarations(body: &str, content_type: Option<&str>) -> Declarations;
// Single STREAMING pass: iterates the parser's quads and extracts each inline.
// On a parse error it `break`s and keeps what it already collected.

// src/resolve.rs
pub struct Declared { pub claimed: bool }
impl Declared { pub fn from(defs: &Declarations, def: &MetricDef) -> Declared }
```

For `Observation`'s real field list, read `src/observe.rs`. Do not trust any
summary of it, including this plan's: revision 1's "verbatim" block was wrong
about it.

---

## Task 0: Make an undispatched probe kind a compile error

`prober/src/lib.rs` dispatches probes with:

```rust
match def.kind {
    ProbeKind::AskData => client.ask_literal(...).await,
    ProbeKind::SelectIris => client.select_iris(...).await,
    ProbeKind::Cors => client.cors(ep, &q).await,
    _ => client.ask(ep, &q).await,
}
```

The `_` arm defeats the exhaustiveness checking that makes a closed enum worth
having. Add `ProbeKind::CorsPreflight` in Task 3 and forget its dispatch arm, and
this compiles, sends `GET <endpoint>?query=`, and publishes `absent` for an
endpoint whose preflight is perfect. Every test in Task 3 that calls
`Client::preflight` directly stays green while it happens.

Fix the structure first, so Task 3 cannot make that mistake.

**Files:**
- Modify: `prober/src/lib.rs`
- Test: none new. The compiler is the test, which is the point.

**Interfaces:** none change.

- [ ] **Step 1: Replace the catch-all with explicit arms**

Name every current kind. `Liveness`, `AskFilter` and `FetchWellKnown` are the
ones the catch-all was serving; check what each actually needs:
`Liveness` and `AskFilter` both want `client.ask(ep, &q)`. `FetchWellKnown`
never reaches this match (its fetch is issued once per endpoint in
`probe_endpoint`, ahead of the per-metric dispatch), so give it an arm that says
so and cannot silently do a wrong thing. Prefer `unreachable!` with a message
naming why, over a plausible-looking fallback that hides a routing bug.

Add a comment stating the rule: **no `_` arm in this match, ever.** A new probe
kind must fail to compile until it is dispatched.

- [ ] **Step 2: Prove the compiler now catches it**

Temporarily add a seventh variant to `ProbeKind` (for example
`ProbeKind::Scratch`) without touching the match. Expected: `cargo build` fails
with a non-exhaustive-match error naming it. Remove the variant, then
`touch src/*.rs tests/*.rs`.

Record in your report the exact error message, so the next reader knows the
guard works.

- [ ] **Step 3: Run the suite and commit**

Run: `cargo test --manifest-path prober/Cargo.toml`
Expected: 112 pass, 1 ignored. This task changes no behaviour.

```bash
git add prober/src/lib.rs
git commit -m "refactor(prober): no catch-all in the probe dispatch, so a new kind cannot be silently mis-routed"
```

---

## Task 1: Scope capability declarations to the service probed, grade the document as published

A fetched description is reduced to `Declarations` over **every** triple in the
graph, so a document describing two services credits the endpoint we probed with
the other service's declarations. The concrete harm: a Fuseki host serving two
datasets, one declaring `sd:extensionFunction geof:sfWithin`, makes the other
dataset report `verified` for GeoSPARQL functions it does not have.

Two things must be true at once, and conflating them is what made revision 1
dangerous:

- **A capability claim is about one service.** `declared_by` matching must read
  only the service we probed. Scoped.
- **The grade is about the document as published.** `service-description`
  measures how informative the description is; an operator who published one rich
  document covering two services published a rich document. Unscoped.

Revision 1 scoped the capability sets while counting `triples` whole-document.
On any scope mismatch that combination published `Level(1)`, whose stated
meaning is "a stub", for a level-4 description, with verdict `verified`. A false
assertive claim. Keeping the two reductions explicitly separate removes the
whole failure mode rather than narrowing it.

**Files:**
- Modify: `prober/src/declare.rs`
- Modify: `prober/src/client.rs` (record the post-redirect URL)
- Modify: `prober/src/observe.rs` (a field to hold it)
- Modify: `prober/src/lib.rs` (thread the endpoint URL through)
- Modify: `prober/tests/declare.rs` (existing fixture tests, see Step 5)
- Test: `prober/tests/declare.rs`

**Interfaces:**
- Produces: `parse_declarations(body: &str, content_type: Option<&str>, endpoint: &str) -> Declarations`,
  where the capability sets (`features`, `extension_functions`, `languages`) are
  scoped to the service matching `endpoint`, and the grade inputs (`triples`,
  `names_dataset`, `has_void_partitions`, `has_entailment`,
  `has_example_resources`) describe the whole document.
- Produces: `Observation.final_url: Option<String>`, the URL a fetch ended on
  after redirects. `None` for every probe that is not a fetch.

- [ ] **Step 1: Write the failing tests**

All of these go in `prober/tests/declare.rs`. Note the third and fourth: they
pin the grade/claim separation, which is the part revision 1 got wrong.

```rust
#[test]
fn a_two_service_description_does_not_credit_the_wrong_endpoint() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/geo> a sd:Service ;
    sd:endpoint <http://example.org/geo/sparql> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/plain> a sd:Service ;
    sd:endpoint <http://example.org/plain/sparql> .
"#;
    const SFWITHIN: &str = "http://www.opengis.net/def/function/geosparql/sfWithin";
    assert!(parse_declarations(DOC, Some("text/turtle"), "http://example.org/geo/sparql")
        .declares(SFWITHIN), "the geo service really does declare sfWithin");
    assert!(!parse_declarations(DOC, Some("text/turtle"), "http://example.org/plain/sparql")
        .declares(SFWITHIN), "the plain service must not inherit its neighbour's function");
}

#[test]
fn a_description_naming_no_endpoint_is_still_read_whole() {
    // Most real descriptions, including the 21 byte-identical Virtuoso stubs in
    // the survey, state no sd:endpoint. Scoping those to nothing would turn
    // every one of them into a false `undeclared`.
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ; sd:extensionFunction geof:sfWithin .
"#;
    assert!(parse_declarations(DOC, Some("text/turtle"), "http://example.org/anything")
        .declares("http://www.opengis.net/def/function/geosparql/sfWithin"));
}

/// The grade describes the document as published, so it must NOT move when the
/// probed endpoint changes. Revision 1 of this plan scoped the grade inputs
/// except `triples`, which published "stub" for a rich description.
#[test]
fn the_grade_inputs_describe_the_whole_document_not_the_scoped_service() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix void: <http://rdfs.org/ns/void#> .
<http://example.org/rich> a sd:Service ;
    sd:endpoint <http://example.org/rich/sparql> ;
    sd:defaultDataset <http://example.org/ds> ;
    sd:defaultEntailmentRegime <http://www.w3.org/ns/entailment/RDFS> .
<http://example.org/ds> void:classPartition [ void:class <http://example.org/C> ] .
<http://example.org/bare> a sd:Service ; sd:endpoint <http://example.org/bare/sparql> .
"#;
    let rich = parse_declarations(DOC, Some("text/turtle"), "http://example.org/rich/sparql");
    let bare = parse_declarations(DOC, Some("text/turtle"), "http://example.org/bare/sparql");
    assert_eq!(rich.triples, bare.triples, "triples counts the document, not the service");
    assert_eq!(rich.has_entailment, bare.has_entailment, "so does the entailment flag");
    assert_eq!(rich.has_void_partitions, bare.has_void_partitions);
    assert_eq!(rich.names_dataset, bare.names_dataset);
    assert!(rich.has_entailment, "the fixture must be rich or this proves nothing");
}

/// A stated `sd:endpoint` that matches nothing must not silently strip the
/// document. Scheme mismatch is extremely common: the registry holds http://,
/// the server publishes https:// and redirects us there.
#[test]
fn a_scheme_or_slash_or_port_difference_is_the_same_endpoint() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <https://Example.ORG:443/sparql/> ;
    sd:extensionFunction geof:sfWithin .
"#;
    const SFWITHIN: &str = "http://www.opengis.net/def/function/geosparql/sfWithin";
    for probed in [
        "http://example.org/sparql",
        "https://example.org/sparql",
        "https://example.org/sparql/",
        "http://www.example.org/sparql",
    ] {
        assert!(
            parse_declarations(DOC, Some("text/turtle"), probed).declares(SFWITHIN),
            "{probed} is the same service as the published sd:endpoint"
        );
    }
}

/// When endpoints are stated and none matches even after normalising, a
/// single-service document is still about the endpoint we fetched it from.
#[test]
fn a_single_service_document_that_matches_nothing_is_still_read_whole() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://internal.lan/sparql> ;
    sd:extensionFunction geof:sfWithin .
"#;
    assert!(
        parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql")
            .declares("http://www.opengis.net/def/function/geosparql/sfWithin"),
        "one service, fetched from the endpoint being probed: the mismatch is theirs, not ours"
    );
}

/// But a MULTI-service document that matches nothing must declare nothing,
/// because crediting one of several services at random is exactly the leak.
#[test]
fn a_multi_service_document_that_matches_nothing_declares_nothing() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/a> a sd:Service ;
    sd:endpoint <http://internal.lan/a> ; sd:extensionFunction geof:sfWithin .
<http://example.org/b> a sd:Service ; sd:endpoint <http://internal.lan/b> .
"#;
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql");
    assert!(!d.declares("http://www.opengis.net/def/function/geosparql/sfWithin"));
    assert!(d.triples > 0, "the document is still graded, only the claim is withheld");
}

/// Scoping must reach the dataset and graph nodes a service points at, or every
/// scoped description loses its VoID partitions. `sd:defaultGraph` is included
/// because real descriptions hang partitions off it, not only `defaultDataset`.
#[test]
fn a_scoped_services_dataset_and_default_graph_stay_in_scope() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix void: <http://rdfs.org/ns/void#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://example.org/sparql> ;
    sd:defaultDataset <http://example.org/ds> .
<http://example.org/ds> sd:defaultGraph <http://example.org/g> .
<http://example.org/g> void:propertyPartition [ void:property geof:sfWithin ] .
"#;
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql");
    assert!(d.has_void_partitions, "partitions two links deep are still this service's");
}
```

Additionally, extend the existing scoping coverage to more than one field. A
test suite that pins only `extension_functions` leaves `features` and
`languages` free to leak. Add one test asserting that on the two-service fixture,
the probed service's `features` and `languages` sets are also free of the
neighbour's values.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml --test declare`
Expected: compile error first (`parse_declarations` takes 2 arguments). After
widening the signature, expect the two-service, normalisation, and
single-service-fallback tests to fail.

- [ ] **Step 3: Implement it**

Restructure `parse_declarations`:

1. **Collect the parsed quads into a `Vec` first.** The current implementation is
   a single streaming pass, which cannot work here: the `sd:endpoint` triple that
   decides the scope may arrive after the declarations it governs. Memory is
   bounded by the 256 KiB body cap; say so in a comment or the next reader will
   assume it is unbounded.
2. **Preserve the error semantics exactly.** On a parse error, stop and keep
   everything collected so far. `prober/tests/declare.rs` already pins this with
   a mid-parse-error fixture and that test must keep passing.
3. Count `triples` and set every grade flag during this pass, **unscoped**.
4. Compute the scope set.
5. Extract the capability sets in a second pass, **scoped**.

The parser yields `Quad`, not `Triple`. Import `std::collections::HashSet`
alongside the existing `BTreeSet`.

```rust
const SD_ENDPOINT: &str = "http://www.w3.org/ns/sparql-service-description#endpoint";
// Linking predicates: a service's subtree includes what it points at.
// `defaultGraph` and `namedGraph` are here because real descriptions hang VoID
// partitions off them, not only off `defaultDataset`.
const LINKING: [&str; 6] = [
    "http://www.w3.org/ns/sparql-service-description#defaultDataset",
    "http://www.w3.org/ns/sparql-service-description#availableGraphs",
    "http://www.w3.org/ns/sparql-service-description#namedGraph",
    "http://www.w3.org/ns/sparql-service-description#defaultGraph",
    "http://www.w3.org/ns/sparql-service-description#graph",
    "http://www.w3.org/ns/sparql-service-description#graphCollection",
];

/// Compare two endpoint URLs the way an operator means them, not byte for byte.
/// Ignores scheme, a trailing slash, a default port, host case, and a leading
/// `www.`. Every one of those disagreements is common between a registry URL and
/// a published `sd:endpoint`, and treating them as different services strips a
/// real description down to nothing.
fn same_endpoint(a: &str, b: &str) -> bool {
    fn norm(u: &str) -> String {
        let s = u.trim();
        let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
        let (host, path) = match s.find('/') {
            Some(i) => (&s[..i], s[i..].trim_end_matches('/')),
            None => (s, ""),
        };
        let host = host.to_ascii_lowercase();
        let host = host.strip_suffix(":443").or_else(|| host.strip_suffix(":80")).unwrap_or(&host);
        let host = host.strip_prefix("www.").unwrap_or(host);
        format!("{host}{path}")
    }
    norm(a) == norm(b)
}
```

Scope selection, stated as rules rather than code so the edge cases are explicit:

- No `sd:endpoint` triple anywhere: **no scope**, read the whole document.
- Some `sd:endpoint` matches (by `same_endpoint`, against the probed URL **or**
  the post-redirect URL from Step 4): scope to those subjects, expanded
  transitively through `LINKING`. Bound the expansion by the quad count so a
  cyclic document cannot spin.
- `sd:endpoint` triples exist, none matches, and the document describes exactly
  **one** service (one distinct `sd:endpoint` subject): **no scope**. We fetched
  this document from the endpoint we are probing and it describes one service;
  the URL disagreement is theirs.
- `sd:endpoint` triples exist, none matches, and the document describes more
  than one service: **empty scope**. Capability sets come out empty; the grade
  is unaffected because it is unscoped.

Blank-node subjects reached through a linking predicate are in scope by the same
rule, which is what makes `void:propertyPartition [ void:property ... ]` work.

- [ ] **Step 4: Record the post-redirect URL**

`Client::fetch_rdf` follows redirects (reqwest's default, 10 hops) and keeps no
record of where it landed, so scoping would compare the pre-redirect string. Add
`final_url: Option<String>` to `Observation`, set it from `resp.url()` in
`fetch_rdf` before the body is consumed, and `None` at every other construction
site including `Observation::failed`. Thread it into the scope match in
`lib.rs`.

Add a test in `prober/tests/fetch.rs`: a 301 from `/a` to `/b` serving a
description whose `sd:endpoint` is `/b`, probed as `/a`, must still find the
declaration. Mount both paths on the mock.

- [ ] **Step 5: Update the existing fixture tests, and do not weaken the fallback**

`tests/fixtures/virtuoso-stub.ttl:6` and `tests/fixtures/substantial.ttl:7`
**both state** `sd:endpoint <http://example.org/sparql>`. Any existing test
using them must now pass `"http://example.org/sparql"` as the endpoint, or the
single-service fallback will carry them and the test will pass for the wrong
reason.

Check each existing test that calls `parse_declarations` and pass the matching
URL explicitly. **If a test fails here, the fix is the test's endpoint argument,
never a widening of the scope rules.** Widening them to make a fixture pass
reintroduces the cross-service leak this task exists to remove.

- [ ] **Step 6: Run everything**

Run: `cargo test --manifest-path prober/Cargo.toml`
Expected: all new tests pass and the pre-existing 112 still pass.

- [ ] **Step 7: Prove the scoping is load-bearing**

Make the scope computation always return "no scope". Expected:
`a_two_service_description_does_not_credit_the_wrong_endpoint` and
`a_multi_service_document_that_matches_nothing_declares_nothing` both fail.
Then make it always return an empty scope. Expected: the fallback tests fail.
Restore, `touch src/*.rs tests/*.rs`, re-run.

- [ ] **Step 8: Commit**

```bash
git add prober/src prober/tests
git commit -m "fix(prober): claim from the service we probed, grade the document as published"
```

---

## Task 2: Publish whether declarations were readable, instead of inferring it

Revision 1 of this plan argued that a tri-state `Declared` was unnecessary
because the `service-description` row already tells a consumer whether a
declaration was readable. **That argument is false**, and the counterexample is
cheap: a description that declares `sfWithin` and then hits a syntax error
mid-parse keeps the declarations collected before the error (deliberately, see
`declare.rs`) while the body fails to classify as RDF, so the run publishes
`service-description = indeterminate` next to `geo-functions = verified`. A
consumer joining those two rows concludes we could not read the description, and
we could. Task 1 breaks the inference in the other direction too: a
multi-service document that matches nothing yields no declarations while the
`service-description` row says `verified`.

So publish the fact rather than inferring it. One boolean per endpoint per run,
no change to the six-verdict vocabulary and no new machinery in `resolve()`.

**Files:**
- Modify: `prober/src/emit.rs` (emit the fact, and carry it on the run's data)
- Modify: `prober/src/lib.rs` (compute it where the fetch outcome is known)
- Modify: `prober/src/resolve.rs` (doc comment on `Declared`)
- Modify: `prober/README.md` (verdict table wording, and the limitations list)
- Modify: `tools/render-run.mjs` (it hardcodes the metric list; see Step 5)
- Test: `prober/tests/end_to_end.rs`, `prober/src/emit.rs` unit tests

**Interfaces:**
- Produces: a per-endpoint quad `<endpoint> urn:sparqlwatch:declarationsRead
  "true"^^xsd:boolean` in the run graph, true when the description fetch produced
  a parseable graph of at least one triple, false otherwise. Exactly one such
  quad per endpoint per run, including endpoints whose fetch failed.

- [ ] **Step 1: Write the failing tests**

```rust
/// The fact a consumer cannot currently infer. A description that parses
/// partially declares something AND fails to classify, so neither the
/// service-description verdict nor the presence of a declaration tells you
/// whether we read their declarations. Publish it directly.
#[tokio::test]
async fn a_partially_parsed_description_still_reports_its_declarations_as_read() {
    // Declares sfWithin, then breaks. `declare.rs` keeps what parsed; the body
    // does not classify as RDF, so service-description is indeterminate.
    const BROKEN: &str = concat!(
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .\n",
        "@prefix geof: <http://www.opengis.net/def/function/geosparql/> .\n",
        "<http://example.org/s> sd:extensionFunction geof:sfWithin .\n",
        "<http://example.org/s> sd:endpoint <<<< not turtle at all\n",
    );
    let run = sweep_with_description(BROKEN, "text/turtle").await;
    assert_eq!(
        declarations_read(&run),
        Some(true),
        "we did read declarations out of this body, whatever the grade says"
    );
    assert_eq!(
        verdict_of(&run, "service-description"),
        Verdict::Indeterminate,
        "the fixture must reproduce the mismatch or it proves nothing"
    );
}

#[tokio::test]
async fn an_unreadable_description_reports_declarations_as_not_read() {
    let run = sweep_with_status(500).await;
    assert_eq!(declarations_read(&run), Some(false));
}

#[tokio::test]
async fn every_endpoint_gets_exactly_one_declarations_read_fact() {
    // Including the ones whose fetch failed: a missing fact is indistinguishable
    // from a false one to a consumer writing a SPARQL query.
    let run = sweep_two_endpoints_one_broken().await;
    assert_eq!(count_declarations_read_quads(&run), 2);
}

/// The one thing an unreadable description must never do is manufacture a
/// declaration. Assert over the declaration-backed metrics specifically: for
/// those, an unread description must land on `undeclared-but-verified` when the
/// probe confirms the capability. Revision 1 asserted only that the verdict was
/// not `declared-only` or `declared-but-wrong`, which a `verified` slips past.
#[tokio::test]
async fn an_unreadable_description_never_manufactures_a_declaration() {
    let run = sweep_with_status_and_working_queries(500).await;
    let defs = load_shipped_metrics();
    let backed: Vec<&MetricDef> = defs.iter().filter(|d| d.declared_by.is_some()).collect();
    assert!(!backed.is_empty(), "there must be a declaration-backed metric or this proves nothing");
    for d in backed {
        assert_eq!(
            verdict_of(&run, &d.id),
            Verdict::UndeclaredButVerified,
            "metric {} claimed a declaration from a description we could not read",
            d.id
        );
    }
}
```

Write the small helpers (`sweep_with_description`, `declarations_read`,
`verdict_of`, ...) on top of whatever mock-and-sweep helper
`prober/tests/end_to_end.rs` already uses. Do not build a second harness.
`query_param_is_missing` is the wiremock matcher that separates the queryless
fetch from the query requests; it exists in wiremock 0.6.5.

Also add a unit test in `emit.rs` asserting the quad's shape: subject is the
endpoint IRI, predicate is `urn:sparqlwatch:declarationsRead`, object is an
`xsd:boolean` literal, graph is the run graph.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml`
Expected: the new tests fail; nothing emits the fact yet. The fourth test may
fail on `Verified` rather than on a missing fact, which is the C3 hole in
revision 1's test and is exactly what we want it to catch.

- [ ] **Step 3: Implement**

In `lib.rs`, where the fetch outcome and parsed `Declarations` are both in scope,
compute `declarations_read = declarations.triples > 0`. That is the honest
definition: we got a graph and read at least one triple out of it. Carry it to
the emitter alongside the rows. `emit.rs` stays a pure function of its inputs.

Emit one quad per endpoint. Do not attach it to a measurement row: it is a fact
about the fetch, not a measurement of a metric, and putting it on a row would
imply it was measured against a metric definition.

- [ ] **Step 4: Correct the wording in both places**

`prober/src/resolve.rs`, on `Declared`: state that `claimed: false` means "no
declaration was seen", covering both "declares nothing" and "we could not read
it", and that the two are distinguished by the endpoint's `declarationsRead`
fact. Do **not** repeat revision 1's claim that the `service-description` row
carries that information; it does not, and a comment saying so would mislead the
next reader.

`prober/README.md`, the verdict table row for `undeclared-but-verified`:
currently "Works, but the endpoint advertises nothing", which overstates when we
never read the description. Reword to "Works; no declaration was seen (see the
endpoint's `declarationsRead` fact for whether we could read its description)".

Also fix `README.md`'s "Known limitations": it currently lists the boolean
`declared` flag conflating a failed fetch with a declares-nothing description as
deferred to 1c. That is what this task addresses, so the entry must be removed
rather than left asserting a limitation that no longer holds.

- [ ] **Step 5: Update the run viewer**

`tools/render-run.mjs` hardcodes the metric list in `ABBR` and in the `metrics`
array, so a run containing a metric it does not know renders without it and the
page silently under-reports. Add `cors-preflight` (Task 3 adds the metric) with a
distinct abbreviation, and make an unknown metric render with a fallback label
rather than vanish. A viewer that hides measurements is worse than one that
looks untidy.

- [ ] **Step 6: Prove the tests are load-bearing**

Make `declarations_read` a constant `false`. Expected: the partial-parse test
fails. Make it a constant `true`. Expected: the unreadable-description test
fails. Make `Declared::from` return `claimed: true` unconditionally. Expected:
the fourth test fails on a `verified` verdict. Restore after each, and
`touch src/*.rs tests/*.rs` before the final run.

- [ ] **Step 7: Commit**

```bash
git add prober/src prober/tests prober/README.md tools/render-run.mjs
git commit -m "feat(prober): publish whether we could read an endpoint's declarations"
```

---

## Task 3: An `OPTIONS` preflight probe, alongside the simple-GET one

`metrics.toml` states the problem itself: the `cors` metric observes an
`access-control-allow-origin` header on a **simple GET**, which is weaker than
what a browser does. A real cross-origin SPARQL query is preflighted, so an
endpoint that sets the header on GET but refuses `OPTIONS` passes this metric and
still fails in the embedded editor stage 3b ships.

Add a second probe kind rather than replacing the first. Both facts are worth
publishing: the simple-GET header is what a `curl` user sees, the preflight is
what a browser sees, and an endpoint can genuinely have one and not the other.

Task 0 has already removed the dispatch catch-all, so forgetting Step 6 here is
now a compile error rather than a silent `absent`.

**Files:**
- Modify: `prober/src/metrics.rs` (`ProbeKind::CorsPreflight`)
- Modify: `prober/src/client.rs` (an `OPTIONS` request, a no-redirect client, and the `ORIGIN` fix)
- Modify: `prober/src/observe.rs` (three preflight header fields)
- Modify: `prober/src/resolve.rs` (resolve the new kind)
- Modify: `prober/src/lib.rs` (dispatch it)
- Modify: `prober/metrics.toml` (the metric, and the stale deferral comment)
- Modify: `prober/README.md` (the absent-requires-2xx contract, see Step 7)
- Modify: `prober/tests/client.rs` (an existing test becomes false, see Step 7)
- Test: `prober/tests/cors_preflight.rs` (new)

**Interfaces:**
- Produces: `ProbeKind::CorsPreflight`; `Client::preflight(&self, url: &str) -> Observation`;
  `Observation.allow_origin/allow_methods/allow_headers: Option<String>`, all
  `None` for every non-preflight probe and at `Observation::failed`.
  `allow_origin` holds the header's **value**, because the existing
  `Observation.cors` bool records only presence and presence is not a grant.

- [ ] **Step 1: The verdict rules**

Resolve in this order. **The status gate comes first**: revision 1 stated the
rules as a table and the natural implementation checked headers first, which
returned `Verified` for `405 + ACAO: *`. Order is part of the specification.

1. Expired budget, or a transport error: `Indeterminate`.
2. Status `405` or `501`: `Absent`, **regardless of any CORS headers present**.
   Fetch requires a preflight to answer with an ok status, so a 405 fails the
   preflight whatever headers ride along.
3. Any 3xx: `Absent`. A browser fails a redirected preflight. (Step 4 also stops
   the client from following it.)
4. Any other non-2xx: `Indeterminate`. It describes our request or the server's
   state, not its CORS policy.
5. 2xx and `access-control-allow-origin` grants our origin (`*`, or our exact
   origin) and `access-control-allow-methods` permits GET (absent header counts
   as permitting, since it is optional for simple methods): `Verified`.
6. 2xx otherwise: `Absent`. The endpoint answered the preflight and did not
   grant us the request we would make.

- [ ] **Step 2: Write the failing tests**

Create `prober/tests/cors_preflight.rs`. No `mod support;` line: the other
integration tests do not share such a module. Write a local
`preflight_and_resolve(url) -> (Verdict, Observation)` helper.

Cover, one test each: a grant with `*` and `GET, POST, OPTIONS`; a grant with no
methods header; `405` with no headers; **`405` with `access-control-allow-origin:
*`** (the case revision 1 missed, and the one a header-first implementation gets
wrong); `501`; a `303` redirect to a path that would grant (must be `Absent`);
`429`, `500` and `503` (each `Indeterminate`); 2xx with no ACAO; 2xx with
`access-control-allow-origin: https://example.com` (a grant to somebody else,
must be `Absent`); 2xx granting only `POST`; and a test that the request really
is a preflight, using `header_exists("origin")` and
`header("access-control-request-method", "GET")` as mock matchers so a
non-preflight request fails to match.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml --test cors_preflight`
Expected: compile failure, no `ProbeKind::CorsPreflight`, no `Client::preflight`.

- [ ] **Step 4: The client**

Add the three `Option<String>` fields to `Observation` and set them `None`
everywhere else, `Observation::failed` included.

`Client::preflight` sends `OPTIONS` with `Origin`,
`Access-Control-Request-Method: GET`, and
`Access-Control-Request-Headers: content-type`. Without the request-method
header this is not a preflight and a correct server may ignore it, which would
make every verdict above meaningless.

**It must not follow redirects.** reqwest's redirect policy is per-Client, so
hold a second `reqwest::Client` built with `redirect(Policy::none())` and use it
for this probe only. A 303 otherwise rewrites the `OPTIONS` to a `GET`, and an
endpoint that refuses `OPTIONS` but sets ACAO on a simple GET publishes
`Verified`: the precise endpoint this metric exists to catch. Comment it.

Record `status`, `cors`, `allow_origin`, `allow_methods`, `allow_headers`,
`elapsed_ms`. Do not read or classify the body; set `body_kind` to
`BodyKind::None`.

**Fix `ORIGIN` while you are here.** `client.rs:8` is
`const ORIGIN: &str = "https://sparqlwatch.example";`, a placeholder that does
not resolve. We announce it to every endpoint we probe, and an operator running
an origin allowlist cannot allowlist a domain that does not exist. Change it to
`"https://sparqlwatch.dev.k8s.semanticscience.org"`, matching the `User-Agent`
a few lines below. Same defect as the placeholder `User-Agent` corrected in
`8c22c1e`, in the other header we send strangers.

- [ ] **Step 5: Resolve it**

Implement Step 1's ordered rules in a `ProbeKind::CorsPreflight` arm. Put both
header predicates in named helpers with their own unit tests:

```rust
/// Whether `access-control-allow-methods` permits the GET we would send. An
/// absent header is a grant: the header is optional and a preflight that
/// answered without it refused nothing. An empty header is NOT a grant, because
/// the server stated a list and GET is not in it.
fn allows_get(allow_methods: Option<&str>) -> bool

/// Whether `access-control-allow-origin` grants OUR origin. A wildcard grants
/// everyone; an exact echo of our origin grants us. Anything else is a grant to
/// somebody else, and reporting it as ours would publish `verified` for an
/// endpoint that would refuse us in a browser.
fn grants_our_origin(allow_origin: Option<&str>) -> bool
```

`allows_get` unit tests: `None`, `"*"`, `"GET"`, `"get"`, `"GET, POST"`,
`"POST"`, `""`, and `"POSTGET"` (must not match, the case a naive `contains`
gets wrong). `grants_our_origin` unit tests: `None`, `"*"`, our exact origin,
our origin in different case, `"https://example.com"` (false), `""` (false).
Share the `ORIGIN` constant rather than retyping the literal, so the two cannot
drift.

- [ ] **Step 6: Dispatch and define**

`lib.rs`: route `ProbeKind::CorsPreflight` to `client.preflight(ep)`. It takes no
`query`. Task 0's exhaustive match means omitting this does not compile.

`prober/metrics.toml`: add the metric and rewrite the stale comment above `cors`
so it no longer says the preflight probe is deferred:

```toml
# Two CORS facts, deliberately separate. `cors` is what a curl user sees: an
# access-control-allow-origin header on a simple GET. `cors-preflight` is what a
# browser sees, and it decides whether the embedded editor can talk to this
# endpoint at all. An endpoint can genuinely have one and not the other, so
# neither subsumes the other.
[[metric]]
id = "cors-preflight"
label = "Answers a CORS preflight for a cross-origin GET"
dimension = "interoperability"
kind = "CorsPreflight"
```

- [ ] **Step 7: Update what this task makes false**

Three places state something that stops being true:

- `prober/tests/client.rs::only_the_cors_probe_sends_an_origin_header` becomes
  false: the preflight also sends one. Rename it to say what is now true (two
  probes announce an origin, the others do not) and extend it to assert the
  preflight does so. Do not delete it.
- `prober/README.md`'s contract paragraph and `resolve.rs`'s doc both state when
  `absent` may be claimed. This task adds a second non-2xx absence (405, 501,
  3xx on a preflight) alongside the existing 404/410 one. Update both, with the
  reason: those statuses are the endpoint answering the question we asked.
- Any existing test asserting a measurement count or row total changes with the
  extra metric. Update the expected numbers; do not delete the assertions.
- `prober/src/metrics.rs:166`'s `every_probe_kind_has_a_probe` must cover the new
  kind rather than being narrowed to skip it.

- [ ] **Step 8: Run everything and commit**

Run: `cargo test --manifest-path prober/Cargo.toml`
Run: `cargo clippy --manifest-path prober/Cargo.toml --all-targets -- -D warnings`

```bash
git add prober/src prober/metrics.toml prober/tests prober/README.md
git commit -m "feat(prober): probe the CORS preflight a browser actually sends"
```

---

## Task 4: Look in named graphs, not only the default graph

`geo-data` and `classes` query only the default graph. On an engine where the
default graph is not the union of the named graphs, an endpoint holding
everything in named graphs answers empty and we publish `absent`. That is a
confident wrong answer about an endpoint full of exactly the content we said it
lacked, and it is the most likely false `absent` in the metric set once seeding
starts.

There is no published history to preserve: no production sweep has run, so
changing what these two metrics mean costs nothing today and much more after 1d.

**Files:**
- Modify: `prober/metrics.toml`
- Modify: `prober/README.md` (drop the default-graph caveat)
- Test: `prober/tests/end_to_end.rs`

**Interfaces:** none. This task changes data, not code, which is the point of
metrics being data.

- [ ] **Step 1: Write the failing test**

Revision 1's test grepped for `GRAPH` and `UNION`, which
`GRAPH ?g { ?s geo:asWKT ?g }` passes: the graph variable collides with the
result variable, the pattern can never match, and the metric publishes a silent
false `absent`. The test must catch the very bug the task exists to remove, so
assert on the **relationship** between the graph variable and the metric's
declared `var`:

```rust
/// Pin both halves of the fix, reading the shipped definitions rather than a
/// fixture so editing the file cannot silently narrow them again.
#[test]
fn the_content_metrics_reach_named_graphs_without_colliding_variables() {
    let defs = load_shipped_metrics();
    for id in ["geo-data", "classes"] {
        let d = defs.iter().find(|d| d.id == id).expect("metric must exist");
        let q = d.query.as_deref().unwrap_or("");
        let var = d.var.as_deref().expect("both metrics read a bound variable");

        assert!(q.contains("GRAPH ?"), "{id} must look in named graphs too");
        assert!(q.to_uppercase().contains("UNION"), "{id} must still look in the default graph");

        // Every GRAPH variable in the query must differ from the result
        // variable. `GRAPH ?g { ?s geo:asWKT ?g }` parses, runs, and can never
        // match: the graph name is joined against the geometry literal. The
        // result is an empty binding set, which resolves to `absent`, which is
        // the exact false negative this metric change exists to remove.
        for (i, _) in q.match_indices("GRAPH ?") {
            let rest = &q[i + "GRAPH ?".len()..];
            let g: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            assert!(!g.is_empty(), "{id} has a malformed GRAPH variable");
            assert_ne!(
                g, var,
                "{id} binds ?{var} as its result AND as its graph name, so it can never match"
            );
        }

        assert!(
            !d.label.to_lowercase().contains("default graph"),
            "{id}'s label still claims default-graph-only scope"
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path prober/Cargo.toml --test end_to_end the_content_metrics_reach_named_graphs_without_colliding_variables`
Expected: FAIL, the shipped queries contain no `GRAPH`.

- [ ] **Step 3: Broaden the two queries**

```toml
# Both queries look in the default graph AND in named graphs. Default-graph-only
# versions published `absent` for an endpoint holding everything in named
# graphs, a confident wrong answer about an endpoint full of exactly the content
# we said it lacked. The UNION costs more on a large endpoint, which is what the
# cost class in stage 1c-b is for.
#
# The graph variable is ?anyg, never the result variable. `GRAPH ?g { ?s
# geo:asWKT ?g }` would join the graph name against the geometry literal, match
# nothing, and publish a silent false `absent`.
[[metric]]
id = "geo-data"
label = "Holds WKT geometry"
dimension = "content"
kind = "AskData"
var = "g"
query = """
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
SELECT ?g WHERE { { ?s geo:asWKT ?g } UNION { GRAPH ?anyg { ?s geo:asWKT ?g } } } LIMIT 1
"""

[[metric]]
id = "classes"
label = "Distinct classes"
dimension = "content"
kind = "SelectIris"
var = "c"
query = """
SELECT DISTINCT ?c WHERE { { ?s a ?c } UNION { GRAPH ?anyg { ?s a ?c } } } LIMIT 200
"""
```

- [ ] **Step 4: Prove each metric still works, in both graph positions**

A structural test cannot show the queries return data. Add two wiremock tests
per metric is not possible (wiremock is not a SPARQL engine), so verify against
a real engine instead: the repo's `live_smoke` test is `#[ignore]`d for exactly
this. Add an ignored test, or extend `live_smoke`, that runs both queries
against a real endpoint and asserts a non-empty binding set, and record its
output in your report by running it explicitly with
`cargo test --manifest-path prober/Cargo.toml --test live_smoke -- --ignored`.

If a UNION times out where the simple query succeeded, that is a real finding:
report the endpoint and the elapsed time. Do not quietly narrow the query to
make the number look better.

- [ ] **Step 5: Baseline against a real sweep**

Before this task's edit, capture a baseline:

```bash
git stash && cargo run --manifest-path prober/Cargo.toml -- --at 2026-08-21T12:00:00Z --out /tmp/run-before.nq && git stash pop
cargo run --manifest-path prober/Cargo.toml -- --at 2026-08-21T12:00:00Z --out /tmp/run-after.nq
```

Compare the `geo-data` and `classes` verdicts per endpoint. Expected: no row
moves from a positive verdict to `absent` or `indeterminate`. Report any that
does, with the endpoint and both verdicts. Revision 1 asked for the after-run
only, which cannot show a regression.

- [ ] **Step 6: Update the README and commit**

Remove the "Known limitations" entry about `geo-data` and `classes` querying
only the default graph, and the `metrics.toml` comment saying the named-graph
probe is deferred. Both are now done.

```bash
git add prober/metrics.toml prober/tests prober/README.md
git commit -m "fix(prober): look in named graphs, so a partitioned endpoint is not reported empty"
```

---

## Done criteria

- All five tasks committed, suite green, clippy clean with `--all-targets -- -D warnings`.
- A real sweep runs and no endpoint's verdict moved from positive to negative
  except where the report explains why.
- `prober/README.md`'s "Known limitations" no longer lists declaration scoping,
  the boolean `declared` flag, the simple-GET-only CORS caveat, or the
  default-graph-only caveat: all four are fixed here. The 256 KiB truncation
  entry stays, and so does anything 1c-b owns.
- `prober/metrics.toml` contains no comment claiming a probe is deferred when it
  now exists.
- `prober/src/lib.rs`'s probe dispatch has no `_` arm.

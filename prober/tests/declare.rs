use sparqlwatch_prober::declare::{parse_declarations, Declarations};

const SD: &str = "http://www.w3.org/ns/sparql-service-description#";

/// The `sd:endpoint` that `fixtures/virtuoso-stub.ttl` and
/// `fixtures/substantial.ttl` both actually state. Passing it explicitly is
/// what keeps those tests honest: if they were probed as some other URL they
/// would be carried by the single-service fallback and would pass without
/// exercising the scope match at all.
const STUB_ENDPOINT: &str = "http://example.org/sparql";

#[test]
fn a_virtuoso_stub_declares_only_the_two_stock_features() {
    let d = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), Some("text/turtle"), STUB_ENDPOINT);
    assert!(d.declares(&format!("{SD}UnionDefaultGraph")));
    assert!(d.declares(&format!("{SD}DereferencesURIs")));
    // The finding that matters: 21 of 28 real descriptions look exactly like
    // this, and not one of them declares anything geospatial.
    assert!(!d.declares("http://www.opengis.net/def/function/geosparql/sfWithin"));
    assert!(d.extension_functions.is_empty(), "no real stub declares extension functions");
    assert!(!d.has_void_partitions);
    assert!(!d.has_entailment);
}

#[test]
fn a_substantial_description_reports_its_richer_signals() {
    let d = parse_declarations(include_str!("fixtures/substantial.ttl"), Some("text/turtle"), STUB_ENDPOINT);
    assert!(d.names_dataset);
    assert!(d.has_void_partitions);
    assert!(d.has_entailment);
    assert!(d.triples > 14);
}

#[test]
fn a_syntax_error_partway_through_keeps_what_parsed_before_it() {
    let d = parse_declarations(include_str!("fixtures/partial.ttl"), Some("text/turtle"), STUB_ENDPOINT);
    // One complete triple precedes the unterminated IRI that ends the parse.
    // Both assertions matter: the count alone would also be satisfied by a
    // naive implementation that discards everything on any error and
    // happens to have zero valid triples to lose (as malformed.ttl does), so
    // this fixture puts one triple *before* the error and checks that its
    // content survived, not just that some number came out.
    assert_eq!(d.triples, 1);
    assert!(d.declares(&format!("{SD}UnionDefaultGraph")));
}

#[test]
fn a_malformed_body_yields_empty_declarations_rather_than_panicking() {
    let d = parse_declarations(include_str!("fixtures/malformed.ttl"), Some("text/turtle"), STUB_ENDPOINT);
    assert_eq!(d.triples, 0);
    assert!(d.features.is_empty());
}

#[test]
fn an_unknown_content_type_still_parses_if_the_payload_is_turtle() {
    let d = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), None, STUB_ENDPOINT);
    assert!(d.declares(&format!("{SD}UnionDefaultGraph")), "should fall back to Turtle");
}

#[test]
fn empty_declarations_declare_nothing() {
    let d = Declarations::empty();
    assert!(!d.declares(&format!("{SD}UnionDefaultGraph")));
    assert_eq!(d.triples, 0);
}

#[test]
fn a_void_example_resource_is_reported() {
    // The spec's level 4 is "an entailment regime, example resources, or
    // extension functions", so the flag the grader reads has to be populated
    // from the graph, not just declared on the struct.
    let d = parse_declarations(
        r#"@prefix void: <http://rdfs.org/ns/void#> .
<http://example.org/sparql> void:exampleResource <http://example.org/thing> ."#,
        Some("text/turtle"),
        STUB_ENDPOINT,
    );
    assert!(d.has_example_resources);
    assert_eq!(d.triples, 1);

    let stub = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), Some("text/turtle"), STUB_ENDPOINT);
    assert!(!stub.has_example_resources, "the stock Virtuoso stub names no example resource");
}

// ---------------------------------------------------------------------------
// Scoping: a capability claim is about ONE service, the grade is about the
// whole document.
//
// A description may cover several services (a Fuseki host serving two
// datasets is the common case). Reducing every triple in such a document to
// one set of declarations credits the endpoint we probed with its
// neighbour's capabilities, which publishes `verified` for something the
// endpoint does not have. The grade is the opposite: an operator who
// published one rich document covering two services published a rich
// document, so it is read whole.

const SFWITHIN: &str = "http://www.opengis.net/def/function/geosparql/sfWithin";

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
    assert!(parse_declarations(DOC, Some("text/turtle"), "http://example.org/geo/sparql")
        .declares(SFWITHIN), "the geo service really does declare sfWithin");
    assert!(!parse_declarations(DOC, Some("text/turtle"), "http://example.org/plain/sparql")
        .declares(SFWITHIN), "the plain service must not inherit its neighbour's function");
}

/// A suite that pins only `extension_functions` leaves `features` and
/// `languages` free to leak, and all three are read by `declared_by`.
#[test]
fn no_capability_set_leaks_across_services() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/rich> a sd:Service ;
    sd:endpoint <http://example.org/rich/sparql> ;
    sd:feature sd:UnionDefaultGraph ;
    sd:supportedLanguage sd:SPARQL11Query ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/plain> a sd:Service ;
    sd:endpoint <http://example.org/plain/sparql> ;
    sd:feature sd:DereferencesURIs ;
    sd:supportedLanguage sd:SPARQL10Query .
"#;
    let plain = parse_declarations(DOC, Some("text/turtle"), "http://example.org/plain/sparql");
    assert_eq!(
        plain.features,
        std::iter::once(format!("{SD}DereferencesURIs")).collect(),
        "features must be the probed service's own"
    );
    assert_eq!(
        plain.languages,
        std::iter::once(format!("{SD}SPARQL10Query")).collect(),
        "languages must be the probed service's own"
    );
    assert!(plain.extension_functions.is_empty(), "and it declares no extension function at all");

    let rich = parse_declarations(DOC, Some("text/turtle"), "http://example.org/rich/sparql");
    assert_eq!(rich.features, std::iter::once(format!("{SD}UnionDefaultGraph")).collect());
    assert_eq!(rich.languages, std::iter::once(format!("{SD}SPARQL11Query")).collect());
    assert!(rich.declares(SFWITHIN));
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
        .declares(SFWITHIN));
}

/// The grade describes the document as published, so it must NOT move when the
/// probed endpoint changes. An earlier revision of this change scoped the grade
/// inputs except `triples`, which published "stub" for a rich description.
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
<http://example.org/other> a sd:Service ;
    sd:endpoint <http://elsewhere.example/sparql> .
"#;
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
    // The same document read as the OTHER service, to prove the four cases
    // above pass by matching rather than by falling back to the whole
    // document. Without the second service they would.
    assert!(
        !parse_declarations(DOC, Some("text/turtle"), "http://elsewhere.example/sparql").declares(SFWITHIN),
        "normalisation must match one service, not dissolve the scope"
    );
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
        parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql").declares(SFWITHIN),
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
    assert!(!d.declares(SFWITHIN));
    assert!(d.triples > 0, "the document is still graded, only the claim is withheld");
}

/// The grade describes the document as published, so it must not move with the
/// scope. Two services and a probed URL matching neither, so the capability
/// scope is genuinely EMPTY (a single-service document would fall back to
/// whole-document and prove nothing). If a grade input were ever scoped, an
/// empty scope would zero it and this fails.
#[test]
fn a_grade_input_survives_an_empty_capability_scope() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix void: <http://rdfs.org/ns/void#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://internal.lan/one> ;
    sd:defaultDataset <http://example.org/ds> .
<http://example.org/ds> sd:defaultGraph <http://example.org/g> .
<http://example.org/g> void:propertyPartition [ void:property geof:sfWithin ] ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/other> a sd:Service ; sd:endpoint <http://internal.lan/two> .
"#;
    // Probed as a URL that matches nothing in the document, deliberately: the
    // grade describes what the operator published, so it must not move with the
    // scope. Asserting this from a NON-matching endpoint is what makes the test
    // say something; asserting it from the matching one held either way.
    let d = parse_declarations(DOC, Some("text/turtle"), "http://elsewhere.example/sparql");
    // A real guard: the fixture now carries `sd:extensionFunction geof:sfWithin`,
    // which an unscoped capability read WOULD pick up. So this failing means the
    // scope is not empty, and the grade assertion below would prove nothing.
    // (The earlier version asserted on a `void:property`, which `declares()`
    // never looks at, so it held whatever the scope did.)
    assert!(
        !d.declares("http://www.opengis.net/def/function/geosparql/sfWithin"),
        "the fixture must produce an EMPTY capability scope or the grade assertion proves nothing"
    );
    assert!(
        d.has_void_partitions,
        "a grade input two links deep is part of the published document, whatever we probed"
    );
    assert!(d.triples > 0, "and so is the triple count");
}

/// The load-bearing counterpart to the grade test above. `has_void_partitions`
/// is a grade
/// input, so it is unscoped and would be true however scoping behaved; only a
/// CAPABILITY reached through the linking predicates can show that the
/// transitive expansion runs, and that it stops at the right service. Two
/// services, each pointing at its own dataset and default graph two hops
/// deep, so the assertion fails if the expansion is missing (sfWithin lost)
/// and fails the other way if it is unbounded (sfContains leaks).
#[test]
fn a_capability_two_links_deep_belongs_to_the_service_that_points_at_it() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/mine> a sd:Service ;
    sd:endpoint <http://example.org/mine/sparql> ;
    sd:defaultDataset <http://example.org/mine/ds> .
<http://example.org/mine/ds> sd:defaultGraph <http://example.org/mine/g> .
<http://example.org/mine/g> sd:extensionFunction geof:sfWithin .

<http://example.org/theirs> a sd:Service ;
    sd:endpoint <http://example.org/theirs/sparql> ;
    sd:defaultDataset <http://example.org/theirs/ds> .
<http://example.org/theirs/ds> sd:defaultGraph <http://example.org/theirs/g> .
<http://example.org/theirs/g> sd:extensionFunction geof:sfContains .
"#;
    const SFCONTAINS: &str = "http://www.opengis.net/def/function/geosparql/sfContains";
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/mine/sparql");
    assert!(d.declares(SFWITHIN), "a capability two links down the service's own subtree is its own");
    assert!(!d.declares(SFCONTAINS), "the neighbour's subtree is not");
}

/// A document whose linking predicates form a cycle must terminate. The
/// expansion is bounded by the quad count for exactly this reason.
#[test]
fn a_cyclic_linking_chain_terminates() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://example.org/sparql> ;
    sd:defaultDataset <http://example.org/a> .
<http://example.org/a> sd:defaultGraph <http://example.org/b> .
<http://example.org/b> sd:graph <http://example.org/a> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/elsewhere> a sd:Service ; sd:endpoint <http://other.example/sparql> .
"#;
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql");
    assert!(d.declares(SFWITHIN));
}

/// The grade must not move with the probed endpoint. A two-service document
/// where only the NEIGHBOUR declares an extension function is still a document
/// that declares one, so both endpoints see the same document-wide flag while
/// only the neighbour gets the capability claim. Feeding the scoped set into the
/// grade published Level(1), meaning "a stub", for one endpoint and Level(4) for
/// the other, from identical bytes.
#[test]
fn the_document_wide_extension_function_flag_does_not_move_with_the_probed_endpoint() {
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

    let geo = parse_declarations(DOC, Some("text/turtle"), "http://example.org/geo/sparql");
    let plain = parse_declarations(DOC, Some("text/turtle"), "http://example.org/plain/sparql");

    assert!(
        geo.doc_declares_extension_functions && plain.doc_declares_extension_functions,
        "the document declares an extension function whichever service we probed"
    );
    // And the claim still belongs to exactly one of them, which is the whole
    // point of keeping the two separate.
    assert!(geo.declares(SFWITHIN));
    assert!(!plain.declares(SFWITHIN));
}

/// A `sd:Service` block that states no `sd:endpoint` is still a service the
/// document describes. Counting only `sd:endpoint` subjects made it invisible,
/// so this two-service document was read as one, took the single-service
/// fallback, and credited the probed endpoint with BOTH functions.
#[test]
fn a_service_block_that_states_no_endpoint_still_counts_as_a_service() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/a> a sd:Service ;
    sd:endpoint <http://internal.lan/a> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/b> a sd:Service ;
    sd:extensionFunction geof:sfContains .
"#;
    const SFCONTAINS: &str = "http://www.opengis.net/def/function/geosparql/sfContains";
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql");
    assert!(!d.declares(SFWITHIN), "the stated endpoint does not match us, so its function is not ours");
    assert!(
        !d.declares(SFCONTAINS),
        "and the endpoint-less service block is a second service, not a document to read whole"
    );
    // Still graded, and the control: probing the URL that DOES match reads that
    // service and only that one.
    assert!(d.triples > 0);
    let a = parse_declarations(DOC, Some("text/turtle"), "http://internal.lan/a");
    assert!(a.declares(SFWITHIN) && !a.declares(SFCONTAINS));
}

/// The one service block of a Virtuoso stub is a service by both halves of the
/// rule (typed, and stating an endpoint), so widening the count must not turn
/// the stubs into a multi-service document that declares nothing.
#[test]
fn widening_the_service_count_does_not_strip_the_single_service_stub() {
    let d = parse_declarations(
        include_str!("fixtures/virtuoso-stub.ttl"),
        Some("text/turtle"),
        "http://elsewhere.example/sparql",
    );
    assert!(
        d.declares(&format!("{SD}UnionDefaultGraph")),
        "one service, probed under another URL: the mismatch is theirs, so it is read whole"
    );
}

/// The expansion must stop at a node that is itself another of the document's
/// services. A linking predicate pointing at a neighbour's service node
/// otherwise carries that neighbour's capability across the boundary.
#[test]
fn the_expansion_stops_at_another_described_service() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/mine> a sd:Service ;
    sd:endpoint <http://example.org/mine/sparql> ;
    sd:graph <http://example.org/theirs> .
<http://example.org/theirs> a sd:Service ;
    sd:endpoint <http://example.org/theirs/sparql> ;
    sd:extensionFunction geof:sfContains .
"#;
    const SFCONTAINS: &str = "http://www.opengis.net/def/function/geosparql/sfContains";
    let mine = parse_declarations(DOC, Some("text/turtle"), "http://example.org/mine/sparql");
    assert!(
        !mine.declares(SFCONTAINS),
        "a different service with a different endpoint keeps its own claim"
    );
    // The control: the claim is real, and belongs to the service that made it.
    let theirs = parse_declarations(DOC, Some("text/turtle"), "http://example.org/theirs/sparql");
    assert!(theirs.declares(SFCONTAINS));
}

/// The legitimate case the boundary must not break: two services pointing at
/// the SAME dataset node genuinely share that dataset, so its declarations are
/// both services'. A dataset is not a service, so it does not stop the walk.
#[test]
fn a_dataset_shared_by_two_services_is_read_by_both() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/a> a sd:Service ;
    sd:endpoint <http://example.org/a/sparql> ;
    sd:defaultDataset <http://example.org/shared> .
<http://example.org/b> a sd:Service ;
    sd:endpoint <http://example.org/b/sparql> ;
    sd:defaultDataset <http://example.org/shared> .
<http://example.org/shared> sd:extensionFunction geof:sfWithin .
"#;
    for probed in ["http://example.org/a/sparql", "http://example.org/b/sparql"] {
        assert!(
            parse_declarations(DOC, Some("text/turtle"), probed).declares(SFWITHIN),
            "{probed} points at the shared dataset, so the dataset's function is its own"
        );
    }
}

/// A hostile document can make a fixed-point expansion quadratic by writing its
/// linking chain in reverse document order, so each rescan of every quad adds
/// one subject. The old loop took 20.9 s in release and 171 s in debug on this
/// body; the worklist walk takes 41 ms and 202 ms. This is synchronous work no
/// `tokio::time::timeout` can drop, so the sweep cannot recover from it.
///
/// Asserted as "the parse returns and the capability at the far end of the
/// chain is found", not as a wall-clock bound: a timing assertion would be
/// flaky on a loaded CI box, while a quadratic expansion turns this test into a
/// three-minute stall that is impossible to miss, and a truncated one loses
/// `sfWithin` and fails outright.
#[test]
fn a_long_reversed_linking_chain_is_walked_once_not_rescanned_per_link() {
    const LINKS: usize = 10_440;
    let mut doc = String::from(
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .\n\
         @prefix geof: <http://www.opengis.net/def/function/geosparql/> .\n\
         @prefix n: <http://example.org/n> .\n",
    );
    // The capability sits at the far end, and the chain is written from the far
    // end back towards the service, whose own link comes last.
    doc.push_str(&format!("n:{LINKS} sd:extensionFunction geof:sfWithin .\n"));
    for i in (0..LINKS).rev() {
        doc.push_str(&format!("n:{i} sd:graph n:{} .\n", i + 1));
    }
    doc.push_str(
        "<http://example.org/svc> a sd:Service ;\n    sd:endpoint <http://example.org/sparql> ;\n    sd:graph n:0 .\n",
    );
    // A body a real server could send us: inside `MAX_BODY` (256 KiB), which is
    // what makes the blow-up reachable rather than theoretical.
    assert!(doc.len() < 256 * 1024, "the adversarial body must fit the fetch cap, got {}", doc.len());

    let d = parse_declarations(&doc, Some("text/turtle"), "http://example.org/sparql");
    assert!(
        d.declares(SFWITHIN),
        "the whole chain is one service's subtree, so the capability at its end is found"
    );
}

/// A `#fragment` is never sent to the server and userinfo is credentials, not
/// identity, so neither can make a genuinely identical endpoint a different
/// service. Both directions, since either side may carry it.
#[test]
fn a_fragment_or_userinfo_is_the_same_endpoint() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://example.org/sparql> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/other> a sd:Service ;
    sd:endpoint <http://elsewhere.example/sparql> .
"#;
    for probed in [
        "http://example.org/sparql#frag",
        "http://user@example.org/sparql",
        "http://user:pw@example.org/sparql#frag",
    ] {
        assert!(
            parse_declarations(DOC, Some("text/turtle"), probed).declares(SFWITHIN),
            "{probed} is the same service as the published sd:endpoint"
        );
    }
    // And the other way round: the document carries them, we probe the bare URL.
    const STATED: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://admin@example.org/sparql#service> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/other> a sd:Service ;
    sd:endpoint <http://elsewhere.example/sparql> .
"#;
    assert!(parse_declarations(STATED, Some("text/turtle"), "http://example.org/sparql").declares(SFWITHIN));
    // The guard: two services, so a match is a match rather than a fallback.
    assert!(!parse_declarations(DOC, Some("text/turtle"), "http://elsewhere.example/sparql").declares(SFWITHIN));
}

/// The query string is deliberately NOT normalised away. `?db=a` and `?db=b`
/// can be two genuinely different services on one path, and crediting one with
/// the other's capability is worse than losing a declaration.
#[test]
fn a_query_string_difference_is_a_different_endpoint() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/a> a sd:Service ;
    sd:endpoint <http://example.org/sparql?db=a> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/b> a sd:Service ;
    sd:endpoint <http://example.org/sparql?db=b> .
"#;
    assert!(
        !parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql?db=b").declares(SFWITHIN),
        "a different query string is a different service, so its function is not ours"
    );
    assert!(
        parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql?db=a").declares(SFWITHIN),
        "and the matching one still matches: the query is kept, not made to fail every comparison"
    );
}

/// All six `LINKING` predicates, not only `defaultDataset` and `defaultGraph`,
/// must carry a service's subtree to what it points at. One service, one
/// probed URL that matches it, so `matched` is non-empty and pass two is
/// already scoped to the expansion below: renaming any single predicate in
/// `LINKING` to a nonsense IRI drops that predicate's row without touching
/// the others, which is what a table-driven test over all six is for. (The
/// existing cyclic-chain test also uses `sd:graph`, but reaches its capability
/// via `defaultDataset` then `defaultGraph`, so `sd:graph` there is
/// decorative, not coverage.)
#[test]
fn each_linking_predicate_carries_a_capability_behind_it() {
    for predicate in ["defaultDataset", "availableGraphs", "namedGraph", "defaultGraph", "graph", "graphCollection"] {
        let doc = format!(
            r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://example.org/sparql> ;
    sd:{predicate} <http://example.org/via> .
<http://example.org/via> sd:extensionFunction geof:sfWithin .
"#
        );
        let d = parse_declarations(&doc, Some("text/turtle"), "http://example.org/sparql");
        assert!(
            d.declares(SFWITHIN),
            "sd:{predicate} must carry the service's subtree to what it points at"
        );
    }
}

/// The blank-node arm of the expansion (`Term::BlankNode(b) => ...`) is what
/// makes a CAPABILITY hung off a blank node the probed service's own. VoID
/// partitions do not exercise this arm at all: they feed a grade input, read
/// unscoped in pass one, so a blank node under `void:propertyPartition` never
/// reaches pass two's expansion either way. Two services, each with its
/// capability two blank nodes deep under its own `sd:defaultDataset`, so the
/// assertion fails if the arm is missing (`sfWithin` lost) and fails the other
/// way if blank nodes were not scoped at all (`sfContains` leaks).
#[test]
fn a_capability_behind_a_blank_node_belongs_to_the_service_that_points_at_it() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/mine> a sd:Service ;
    sd:endpoint <http://example.org/mine/sparql> ;
    sd:defaultDataset [ sd:defaultGraph [ sd:extensionFunction geof:sfWithin ] ] .
<http://example.org/theirs> a sd:Service ;
    sd:endpoint <http://example.org/theirs/sparql> ;
    sd:defaultDataset [ sd:defaultGraph [ sd:extensionFunction geof:sfContains ] ] .
"#;
    const SFCONTAINS: &str = "http://www.opengis.net/def/function/geosparql/sfContains";
    let mine = parse_declarations(DOC, Some("text/turtle"), "http://example.org/mine/sparql");
    assert!(mine.declares(SFWITHIN), "a capability behind its own blank-node subtree is its own");
    assert!(!mine.declares(SFCONTAINS), "and must not leak the neighbour's blank-node subtree");
}

/// The guard against a matcher that returns true on empty input: two URLs
/// that both normalise to nothing must not be "the same endpoint". Without
/// the guard, a document that (mis)states `sd:endpoint <http://>` would be
/// credited to a probe of `""` or `"http://"`, because both normalise to the
/// empty string and plain `norm(a) == norm(b)` would then hold. A multi-service
/// document, so a false match here is visible as a real capability credit
/// rather than the single-service fallback masking it.
#[test]
fn two_urls_that_normalise_to_nothing_are_not_the_same_endpoint() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/degenerate> a sd:Service ;
    sd:endpoint <http://> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/real> a sd:Service ;
    sd:endpoint <http://example.org/real/sparql> ;
    sd:extensionFunction geof:sfContains .
"#;
    const SFCONTAINS: &str = "http://www.opengis.net/def/function/geosparql/sfContains";
    for probed in ["", "http://"] {
        let d = parse_declarations(DOC, Some("text/turtle"), probed);
        assert!(
            !d.declares(SFWITHIN),
            "a degenerate probed URL {probed:?} must not match a degenerate stated sd:endpoint"
        );
        assert!(!d.declares(SFCONTAINS), "and must not fall back to reading the document whole either");
    }
}

/// A subject whose `sd:endpoint` object is not an IRI (a literal here) still
/// counts towards the service count, even though a non-IRI object can never
/// match a probed URL. That is the safe choice: moving the `services.insert`
/// inside the `if let Term::NamedNode` guard would make this look like ONE
/// service to `scope_of` (only "real" would count), which for a probed URL
/// that matches neither falls back to reading the document whole and leaks
/// the malformed service's function to a probe that matches nothing.
#[test]
fn a_non_iri_endpoint_still_counts_as_a_service_so_the_document_is_not_read_whole() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/malformed> sd:endpoint "not-a-url" ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/real> a sd:Service ;
    sd:endpoint <http://example.org/real/sparql> ;
    sd:extensionFunction geof:sfContains .
"#;
    const SFCONTAINS: &str = "http://www.opengis.net/def/function/geosparql/sfContains";
    let d = parse_declarations(DOC, Some("text/turtle"), "http://elsewhere.example/sparql");
    assert!(!d.declares(SFWITHIN), "the malformed service's function must not leak");
    assert!(!d.declares(SFCONTAINS), "nor must the real service's: nothing matched, so the scope is empty");
    assert!(d.triples > 0, "the document is still graded, only the claim is withheld");
}

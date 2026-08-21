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

/// The load-bearing half of the test above. `has_void_partitions` is a grade
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

use sparqlwatch_prober::declare::{parse_declarations, Declarations};

const SD: &str = "http://www.w3.org/ns/sparql-service-description#";

#[test]
fn a_virtuoso_stub_declares_only_the_two_stock_features() {
    let d = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), Some("text/turtle"));
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
    let d = parse_declarations(include_str!("fixtures/substantial.ttl"), Some("text/turtle"));
    assert!(d.names_dataset);
    assert!(d.has_void_partitions);
    assert!(d.has_entailment);
    assert!(d.triples > 14);
}

#[test]
fn a_syntax_error_partway_through_keeps_what_parsed_before_it() {
    let d = parse_declarations(include_str!("fixtures/partial.ttl"), Some("text/turtle"));
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
    let d = parse_declarations(include_str!("fixtures/malformed.ttl"), Some("text/turtle"));
    assert_eq!(d.triples, 0);
    assert!(d.features.is_empty());
}

#[test]
fn an_unknown_content_type_still_parses_if_the_payload_is_turtle() {
    let d = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), None);
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
    );
    assert!(d.has_example_resources);
    assert_eq!(d.triples, 1);

    let stub = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), Some("text/turtle"));
    assert!(!stub.has_example_resources, "the stock Virtuoso stub names no example resource");
}

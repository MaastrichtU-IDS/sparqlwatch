//! The class profile fan-out: one query per class, under the ENDPOINT budget.
//!
//! Separated from client.rs's tests because what is under test here is
//! orchestration rather than parsing: how many queries go out, what happens when
//! the budget expires partway, and what is published about the classes never
//! reached.

use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::politeness::Politeness;
use sparqlwatch_prober::profile::{ladder_from, profile_classes, ProfileOutcome, Sampling};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ROWS: &str = r#"{
  "head": {"vars": ["p", "subjects", "datatypes", "anyDatatype"]},
  "results": {"bindings": [
    {"p": {"type": "uri", "value": "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"},
     "subjects": {"type": "literal", "value": "40"}},
    {"p": {"type": "uri", "value": "http://xmlns.com/foaf/0.1/name"},
     "subjects": {"type": "literal", "value": "38"}}
  ]}
}"#;

fn classes(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("http://example.org/Class{i}")).collect()
}

#[tokio::test]
async fn one_query_per_class_and_a_profile_for_each() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ROWS))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let mut out = ProfileOutcome::default();
    profile_classes(&url, &classes(3), Sampling::Exact, &c, Budget::default(), &mut out).await;
    out.name_unreached(&classes(3));

    assert_eq!(out.profiles.len(), 3, "one profile per class");
    assert!(out.unreached.is_empty(), "nothing was left unreached");
    // The class each profile is about must be carried, or two profiles of one
    // endpoint are indistinguishable.
    let mut seen: Vec<&str> = out.profiles.iter().map(|p| p.class.as_str()).collect();
    seen.sort();
    assert_eq!(seen, vec![
        "http://example.org/Class0",
        "http://example.org/Class1",
        "http://example.org/Class2",
    ]);
    assert_eq!(out.profiles[0].rows.len(), 2);
    assert_eq!(out.profiles[0].sampling, Sampling::Exact);
}

#[tokio::test]
async fn an_expiring_budget_keeps_what_finished_and_names_what_it_did_not_reach() {
    // The property the whole design turns on. A fan-out that lost its finished
    // work on expiry would make a long class list unprofilable, and one that
    // said nothing about the classes it skipped would leave a reader unable to
    // tell "this class has no properties" from "we never asked".
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(ROWS)
                .set_delay(Duration::from_millis(120)),
        )
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    // An endpoint budget that admits a couple of classes and not twenty.
    let budget = Budget {
        request: Duration::from_secs(30),
        metric: Duration::from_secs(60),
        endpoint: Duration::from_millis(300),
    };
    // Wrapped in the ENDPOINT budget, which is how production calls it: the
    // pass does not enforce that bound itself, the caller's timeout does.
    let all = classes(20);
    let mut out = ProfileOutcome::default();
    let outcome = budget
        .with_endpoint_budget(profile_classes(
            &url, &all, Sampling::Exact, &c, budget, &mut out,
        ))
        .await;
    assert!(outcome.is_err(), "the endpoint budget must have expired");
    // The caller names the tail, because a cancelled loop cannot name its own.
    out.name_unreached(&all);

    assert!(!out.profiles.is_empty(), "the finished profiles must survive");
    assert!(out.profiles.len() < 20, "the budget must have cut it short");
    assert_eq!(
        out.profiles.len() + out.unreached.len(),
        20,
        "every class is either profiled or named as unreached, never neither"
    );
    // And the two sets must not overlap: a class cannot be both.
    for p in &out.profiles {
        assert!(!out.unreached.contains(&p.class), "{} is in both sets", p.class);
    }
}

#[tokio::test]
async fn a_class_the_endpoint_refuses_is_unreached_not_empty() {
    // A refusal is not a profile of zero properties. Same rule as the probe's:
    // an empty profile would say the class carries none.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(400).set_body_string("refused"))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let mut out = ProfileOutcome::default();
    profile_classes(&url, &classes(2), Sampling::Exact, &c, Budget::default(), &mut out).await;
    out.name_unreached(&classes(2));

    assert!(out.profiles.is_empty(), "a refusal yields no profile");
    assert_eq!(out.unreached.len(), 2, "both classes are named as unreached");
}

#[tokio::test]
async fn the_sampling_choice_reaches_the_query_and_the_fact() {
    // The prefix is evidence: a profile drawn from 1/256 of the instances must
    // say so, or a reader takes an approximation for an exact count.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ROWS))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let mut out = ProfileOutcome::default();
    profile_classes(
        &url, &classes(1), Sampling::HashPrefix("00".into()), &c,
        Budget::default(), &mut out,
    ).await;

    assert_eq!(out.profiles.len(), 1);
    assert_eq!(
        out.profiles[0].sampling,
        Sampling::HashPrefix("00".into()),
        "the fact records which sample it came from"
    );
    // And the query actually carried the filter, rather than the prefix being
    // recorded while an exact query went out.
    let sent = server.received_requests().await.unwrap();
    let q = sent[0].url.query_pairs()
        .find(|(k, _)| k == "query").map(|(_, v)| v.to_string()).unwrap();
    assert!(q.contains("SHA256"), "the prefix must reach the query: {q}");
    assert!(q.contains("\"00\""), "with the prefix it claims: {q}");
}


// ---------------------------------------------------------------------------
// The fallback ladder: a class too big to profile exactly, sampled instead
// ---------------------------------------------------------------------------

/// A server that refuses the exact scan the way ontoexplorer's gateway does
/// and answers the sampled query, chosen by whether the query carries a hash
/// filter. That is the real distinction: the URL and method are identical on
/// every rung, so only the query text can tell them apart.
async fn refuses_the_exact_scan(status: u16) -> MockServer {
    let server = MockServer::start().await;
    // Ordered: wiremock matches mounts in order, so the sampled case is
    // mounted first and the catch-all below takes everything else.
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "SHA256"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ROWS))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server)
        .await;
    server
}

/// The measurement this ladder exists for, as a test.
///
/// `owl:Restriction` on ontoexplorer's content store (1,352,666 instances)
/// answered 504 at its gateway's 30 second limit on the exact scan and 1.8s
/// with a one character prefix. Before the ladder that class was simply
/// unprofiled, and it is the biggest class on the endpoint.
#[tokio::test]
async fn a_class_too_big_to_scan_exactly_is_profiled_from_a_sample() {
    let server = refuses_the_exact_scan(504).await;
    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let mut out = ProfileOutcome::default();

    profile_classes(&url, &classes(1), Sampling::Exact, &c, Budget::default(), &mut out).await;

    assert!(out.unreached.is_empty(), "the sample succeeded: {:?}", out.unreached);
    assert_eq!(out.profiles.len(), 1, "one class, one profile");
    // And the fact says it is a SAMPLE. A reader who could not tell would take
    // a sixteenth of the instances for an exact count.
    assert_eq!(
        out.profiles[0].sampling,
        Sampling::HashPrefix("0".into()),
        "the published sampling must name the rung that actually answered"
    );
}

/// The cost guard. A 4xx says the endpoint understood the query and refused
/// it, so every rung below is refused the same way: escalating there would
/// triple the request count against every class on the endpoint for nothing.
#[tokio::test]
async fn a_refused_query_is_not_retried_on_a_smaller_sample() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let mut out = ProfileOutcome::default();

    profile_classes(&url, &classes(1), Sampling::Exact, &c, Budget::default(), &mut out).await;

    assert_eq!(out.unreached, classes(1), "named once, not profiled");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "one request and no ladder: a malformed query stays malformed"
    );
}

/// A 5xx on every rung ends the ladder with the class named ONCE, not once per
/// rung. `unreached` is published as a fact per class, so a duplicate would
/// publish the same class twice.
#[tokio::test]
async fn a_class_that_fails_every_rung_is_named_exactly_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let mut out = ProfileOutcome::default();

    profile_classes(&url, &classes(1), Sampling::Exact, &c, Budget::default(), &mut out).await;

    assert_eq!(out.unreached, classes(1));
    assert!(out.profiles.is_empty());
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        ladder_from(&Sampling::Exact).len(),
        "every rung was tried, because a 5xx is what a smaller sample could fix"
    );
    // Read from the ladder rather than written as a number. It was 3 until
    // 2026-09-15, when the bounded last rung was added for stores that refuse
    // aggregates outright; what this test is about -- that a 5xx walks the
    // whole ladder and names the class once at the end -- does not depend on
    // how many rungs there are.
    assert_eq!(ladder_from(&Sampling::Exact).len(), 4, "the ladder's length changed");
}

/// A class that answers the exact scan costs ONE request. The ladder must be
/// free when it is not needed, which is the common case.
#[tokio::test]
async fn a_class_that_answers_exactly_costs_one_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ROWS))
        .mount(&server)
        .await;
    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let mut out = ProfileOutcome::default();

    profile_classes(&url, &classes(3), Sampling::Exact, &c, Budget::default(), &mut out).await;

    assert_eq!(server.received_requests().await.unwrap().len(), 3, "one per class");
    assert!(out.profiles.iter().all(|p| p.sampling == Sampling::Exact));
}

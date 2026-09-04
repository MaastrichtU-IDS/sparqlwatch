//! The class profile fan-out: one query per class, under the ENDPOINT budget.
//!
//! Separated from client.rs's tests because what is under test here is
//! orchestration rather than parsing: how many queries go out, what happens when
//! the budget expires partway, and what is published about the classes never
//! reached.

use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::politeness::Politeness;
use sparqlwatch_prober::profile::{profile_classes, ProfileOutcome, Sampling};
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

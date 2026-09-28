//! A one-off reconnaissance pass: which candidates are alive, asked once.
//!
//! WHAT THIS IS FOR. `registry/lod-cloud.toml` holds 543 endpoints and the
//! sweep asks 6 of them. The other 537 are seeded but unswept, because
//! sweeping a stranger's server is an explicit act and nobody had made it.
//! Deciding it blind is the problem: the dump's own `status` field says 87 of
//! 725 entries answer, which is a third party's stale judgement, and admitting
//! 537 endpoints to an hourly sweep on the strength of it would point a
//! recurring load at hosts that mostly died years ago.
//!
//! So this asks each candidate ONE question, once, and prints who answered.
//! The output is a registry fragment: the alive ones, ready to be read and
//! pasted by a person who has decided to sweep them. It is reconnaissance for
//! a decision, not the decision.
//!
//! WHAT IT WILL NOT DO, and each of these is load-bearing:
//!
//! - It writes no run graph, no state file and no store. Nothing it learns is
//!   published as a measurement about anybody, because a measurement carries
//!   this service's name and one request is not a finding. The report is a
//!   file for a person to read.
//! - It reads `registry/exclusions.toml` and STOPS if it cannot. That is the
//!   same fail-closed rule `load_endpoints` applies, for the same reason: the
//!   alternative to reading the list is probing a host that asked not to be.
//! - It goes through `Politeness`, so the per-host gate that spaces a sweep's
//!   requests spaces these too. A recon pass that ignored the gate would hit
//!   an endpoint family like rkbexplorer.com -- 40 hosts, one operator -- with
//!   40 simultaneous requests.
//! - It asks the AVAILABILITY metric and nothing else, reading its verdict
//!   through `resolve::resolve`. Reusing the sweep's own reading is the point:
//!   "would a sweep find this alive" has to be answered by the sweep's rule,
//!   not by a second opinion in here that could drift from it.
//! - It subtracts the endpoints already swept, so a candidate this service
//!   already asks hourly is not asked an extra time for a report.

use clap::Parser;
use sparqlwatch_prober::{
    budget::Budget,
    client::Client,
    metrics::load_metrics,
    politeness::{Politeness, DEFAULT_MIN_GAP},
    registry::{load_endpoints, read_exclusions},
    resolve::{resolve, Declared},
    verdict::Verdict,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::task::JoinSet;
use std::path::Path;
use std::time::Duration;

/// The metric this pass asks. One cheap query, the same one every sweep opens
/// with, so a candidate that answers here is a candidate a sweep can measure.
const METRIC: &str = "availability";

#[derive(Parser)]
#[command(
    name = "recon",
    about = "Ask each candidate endpoint one question. Writes no run graph and no state."
)]
struct Args {
    /// The candidates: a registry file, usually registry/lod-cloud.toml.
    #[arg(long, default_value = "registry/lod-cloud.toml")]
    registry: String,
    /// Endpoints to leave out because a sweep already asks them. Repeatable.
    #[arg(long = "already-swept", default_values_t = [String::from("endpoints.toml")])]
    already_swept: Vec<String>,
    /// The hosts somebody asked this project not to probe. Unreadable is fatal.
    #[arg(long, default_value = sparqlwatch_prober::registry::DEFAULT_EXCLUSIONS)]
    exclusions: String,
    #[arg(long, default_value = "metrics.toml")]
    metrics: String,
    /// Where the report goes. A registry fragment plus a census, as TOML.
    ///
    /// The report is ALSO printed to stdout between markers, and that is the
    /// copy a Job leaves behind: a pod's filesystem goes with the pod, and its
    /// log does not.
    #[arg(long, default_value = "recon.toml")]
    out: String,
    /// Endpoints asked at once. Each still waits on its own host's gate.
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    /// The per-host gap, in milliseconds.
    #[arg(long, default_value_t = DEFAULT_MIN_GAP.as_millis() as u64)]
    min_gap_ms: u64,
    /// Ask at most this many. For a rehearsal on a handful before the whole list.
    #[arg(long)]
    limit: Option<usize>,
}

/// What one candidate did, in the three words this pass can honestly use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Answered our query with SPARQL results and a success status.
    Answered,
    /// Answered, but not with SPARQL results: an HTML console, a 404 page.
    /// It is reachable and it is not an endpoint we can measure.
    NotAnEndpoint,
    /// Nothing came back inside the budget, or the transport failed.
    ///
    /// DELIBERATELY NOT "down". One request proves nothing about a server,
    /// and `resolve.rs` grades this case "we never got to ask" on purpose.
    NoAnswer,
}

impl Outcome {
    fn slug(self) -> &'static str {
        match self {
            Outcome::Answered => "answered",
            Outcome::NotAnEndpoint => "not-an-endpoint",
            Outcome::NoAnswer => "no-answer",
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt().with_target(false).init();

    // FIRST, and fatal if it fails: see `registry::read_exclusions`. A pass
    // that could not read the list must not send a request.
    let excluded = read_exclusions(Path::new(&args.exclusions))?;
    let candidates = load_endpoints(&std::fs::read_to_string(&args.registry)?, &excluded)?;

    // Everything a sweep already asks, across however many lists were named.
    let mut swept: BTreeSet<String> = BTreeSet::new();
    for path in &args.already_swept {
        swept.extend(load_endpoints(&std::fs::read_to_string(path)?, &excluded)?);
    }
    let mut targets: Vec<String> =
        candidates.iter().filter(|u| !swept.contains(*u)).cloned().collect();
    let considered = candidates.len();
    let skipped_swept = considered - targets.len();
    if let Some(n) = args.limit {
        targets.truncate(n);
    }

    let defs = load_metrics(&std::fs::read_to_string(&args.metrics)?)?;
    let def = defs
        .iter()
        .find(|d| d.id == METRIC)
        .ok_or_else(|| anyhow::anyhow!("{} names no `{METRIC}` metric", args.metrics))?;
    let query = def
        .query
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the `{METRIC}` metric carries no query"))?;

    tracing::info!(
        considered,
        skipped_swept,
        asking = targets.len(),
        "reconnaissance: one request each, nothing published"
    );

    let client = Client::new(
        Budget::default(),
        Politeness::new(Duration::from_millis(args.min_gap_ms)),
    )?;

    // A bounded fan-out with `JoinSet`, the way `run_sweep` does it and
    // without a stream combinator: at most `concurrency` requests are in the
    // air, and each still waits on its own host's gate inside the client.
    let client = Arc::new(client);
    let shared_def = Arc::new(def.clone());
    let mut tasks: JoinSet<(String, Outcome, Option<u16>, u64)> = JoinSet::new();
    let mut results: Vec<(String, Outcome, Option<u16>, u64)> = Vec::new();
    let mut queue = targets.into_iter();
    let mut done = 0usize;

    loop {
        while tasks.len() < args.concurrency.max(1) {
            let Some(url) = queue.next() else { break };
            let client = Arc::clone(&client);
            let def = Arc::clone(&shared_def);
            let query = query.clone();
            tasks.spawn(async move {
                let obs = client.ask(&url, &query).await;
                // The SWEEP's reading, not a second one. `Declared::claimed`
                // is false because this pass fetches no description: it asks
                // one question and reads the answer, and `availability` is
                // confirmed by the answer alone.
                let verdict =
                    resolve(&def, Declared { claimed: false, value: None }, Ok(&obs));
                let outcome = match verdict {
                    Verdict::Verified | Verdict::UndeclaredButVerified => Outcome::Answered,
                    Verdict::Absent => Outcome::NotAnEndpoint,
                    _ => Outcome::NoAnswer,
                };
                (url, outcome, obs.status, obs.elapsed_ms)
            });
        }
        let Some(joined) = tasks.join_next().await else { break };
        match joined {
            Ok(r) => results.push(r),
            // A panicked probe costs its own row and nothing else. The pass is
            // reconnaissance: losing one candidate's answer is worth far less
            // than losing the other 536.
            Err(e) => tracing::warn!(error = %e, "a probe task failed; its row is missing"),
        }
        done += 1;
        if done.is_multiple_of(50) {
            tracing::info!(done, "asked");
        }
    }

    let mut census = std::collections::BTreeMap::<&str, usize>::new();
    for (_, o, _, _) in &results {
        *census.entry(o.slug()).or_default() += 1;
    }

    let mut alive: Vec<_> =
        results.iter().filter(|(_, o, _, _)| *o == Outcome::Answered).collect();
    alive.sort_by(|a, b| a.0.cmp(&b.0));
    let mut rest: Vec<_> =
        results.iter().filter(|(_, o, _, _)| *o != Outcome::Answered).collect();
    rest.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::new();
    out.push_str(&format!(
        "# Written by `recon`. NOT a measurement and not a registry: one request\n\
         # per endpoint, asked once, published nowhere. `answered` below means the\n\
         # endpoint replied to `{METRIC}` with SPARQL results and a success status\n\
         # at the instant it was asked -- which is a reason to consider sweeping it,\n\
         # not a finding about the service.\n\
         #\n\
         # `no-answer` is NOT \"down\": one request proves nothing about a server,\n\
         # and an egress proxy in the path answers for some of them. Check a host\n\
         # from inside the cluster before concluding anything about it.\n\
         #\n\
         # To admit the answering ones, read them and paste them into\n\
         # endpoints.toml. Nothing does that automatically: sweeping a stranger's\n\
         # server is a decision a person makes.\n\n"
    ));
    out.push_str("[census]\n");
    out.push_str(&format!("considered = {considered}\n"));
    out.push_str(&format!("already_swept = {skipped_swept}\n"));
    out.push_str(&format!("asked = {}\n", results.len()));
    for (slug, n) in &census {
        out.push_str(&format!("{} = {n}\n", slug.replace('-', "_")));
    }
    out.push_str("\n# The endpoints that answered.\n");
    for (url, _, status, ms) in &alive {
        out.push_str("\n[[endpoint]]\n");
        out.push_str(&format!("url = {}\n", toml_string(url)));
        out.push_str(&format!(
            "# {} in {} ms\n",
            status.map(|s| s.to_string()).unwrap_or_else(|| "no status".into()),
            ms
        ));
    }
    out.push_str("\n# Everything else, with what happened. Kept in the report because a\n");
    out.push_str("# pass that listed only its successes would hide its own coverage.\n");
    for (url, o, status, _) in &rest {
        out.push_str("\n[[silent]]\n");
        out.push_str(&format!("url = {}\n", toml_string(url)));
        out.push_str(&format!("outcome = \"{}\"\n", o.slug()));
        if let Some(s) = status {
            out.push_str(&format!("status = {s}\n"));
        }
    }
    std::fs::write(&args.out, &out)?;

    // AND TO STDOUT, which is the copy that survives. The first real run of
    // this pass wrote its report into a Job's emptyDir, the pod exited, and
    // `kubectl cp` refuses a completed pod: 537 endpoints were asked and the
    // answer was unreachable. A log is retained after the pod that produced it
    // has finished, so the report goes where it can still be read.
    //
    // Fenced by markers so a reader can cut it out of a log that also carries
    // tracing lines, and so `sed -n '/BEGIN/,/END/p'` is enough to recover a
    // file that parses.
    println!("----- BEGIN RECON REPORT -----");
    print!("{out}");
    println!("----- END RECON REPORT -----");

    tracing::info!(
        answered = census.get("answered").copied().unwrap_or(0),
        not_an_endpoint = census.get("not-an-endpoint").copied().unwrap_or(0),
        no_answer = census.get("no-answer").copied().unwrap_or(0),
        out = %args.out,
        "reconnaissance complete; nothing was published"
    );
    Ok(())
}

/// A TOML basic string. The URLs come from a third party's dump, so they are
/// quoted rather than interpolated: one carrying a quote or a backslash would
/// otherwise write a report that does not parse.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_with_a_quote_is_escaped_rather_than_pasted() {
        // The candidates come from a stranger's JSON dump, so this is reachable
        // input and not a hypothetical. An unescaped one writes a report that
        // does not parse, which loses the whole pass.
        assert_eq!(toml_string(r#"http://a/"b"#), r#""http://a/\"b""#);
        assert_eq!(toml_string(r"http://a/\b"), r#""http://a/\\b""#);
        assert_eq!(toml_string("http://a/\nb"), r#""http://a/\nb""#);
        assert_eq!(toml_string("http://a/\u{1}b"), r#""http://a/\u0001b""#);
    }

    #[test]
    fn the_report_the_escaper_writes_parses_back() {
        let url = r#"http://host/a"b\c"#;
        let text = format!("[[endpoint]]\nurl = {}\n", toml_string(url));
        let parsed: toml::Value = toml::from_str(&text).expect("the report must parse");
        assert_eq!(parsed["endpoint"][0]["url"].as_str(), Some(url));
    }

    #[test]
    fn only_a_positive_verdict_counts_as_answered() {
        // The three outcomes map from the SWEEP's verdict, and the mapping is
        // the one place this pass could quietly disagree with a sweep about
        // what "alive" means. `Absent` is the endpoint answering that it does
        // not speak SPARQL, which is a real answer; everything else is
        // "we never got to ask" and must not read as a finding.
        for (v, expected) in [
            (Verdict::Verified, Outcome::Answered),
            (Verdict::UndeclaredButVerified, Outcome::Answered),
            (Verdict::Absent, Outcome::NotAnEndpoint),
            (Verdict::Indeterminate, Outcome::NoAnswer),
            (Verdict::DeclaredOnly, Outcome::NoAnswer),
            (Verdict::DeclaredButWrong, Outcome::NoAnswer),
        ] {
            let got = match v {
                Verdict::Verified | Verdict::UndeclaredButVerified => Outcome::Answered,
                Verdict::Absent => Outcome::NotAnEndpoint,
                _ => Outcome::NoAnswer,
            };
            assert_eq!(got, expected, "{v:?} mapped to the wrong outcome");
        }
    }
}

#[cfg(test)]
mod report_tests {
    /// The markers the Job's log is cut on. Pinned because `ops/recon.yaml`
    /// documents the `sed` that uses them, and a rename here would leave that
    /// instruction naming a string no log carries -- which is how the first
    /// run's report was lost in the first place.
    #[test]
    fn the_fence_markers_are_the_ones_the_runbook_names() {
        let ops = std::fs::read_to_string("../ops/recon.yaml")
            .or_else(|_| std::fs::read_to_string("ops/recon.yaml"))
            .expect("ops/recon.yaml must be readable from the crate");
        for marker in ["----- BEGIN RECON REPORT -----", "----- END RECON REPORT -----"] {
            assert!(
                ops.contains(marker),
                "ops/recon.yaml does not name {marker:?}, so its recovery command cannot work"
            );
            let src = std::fs::read_to_string("src/bin/recon.rs").unwrap();
            assert!(src.contains(marker), "recon no longer prints {marker:?}");
        }
    }
}

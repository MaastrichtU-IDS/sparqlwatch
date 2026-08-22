use crate::budget::Budget;
use crate::media;
use crate::observe::{BodyKind, Observation};
use crate::politeness::{honour, parse_retry_after, Honour, Politeness, RetryAfter};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::HashSet;
use std::future::Future;
use std::time::{Duration, Instant};

/// Announced only by the two CORS probes, so the other probes cannot be
/// perturbed by a server that filters on it.
///
/// Public because `resolve` compares an endpoint's
/// `access-control-allow-origin` against it: an exact echo of our origin is a
/// grant to us, anything else is a grant to somebody else. Sharing the one
/// constant is what stops the announced origin and the compared origin from
/// drifting apart.
///
/// It has to be a domain that actually resolves. We announce it to every
/// endpoint we probe, and an operator running an origin allowlist cannot
/// allowlist a name that does not exist. Same defect as the placeholder
/// `User-Agent` corrected in 8c22c1e, in the other header we send strangers.
pub const ORIGIN: &str = "https://sparqlwatch.dev.k8s.semanticscience.org";

/// The `Accept` header for a queryless RDF fetch (e.g. a service description).
/// Distinct from the SPARQL-results `Accept` used by `get_with_body`: this
/// request is not a query at all.
const RDF_ACCEPT: &str = "text/turtle, application/rdf+xml;q=0.9, application/ld+json;q=0.8";

/// The media types that positively identify an RDF payload. Deliberately
/// narrower than `RdfFormat::from_media_type` accepts: that function maps the
/// generic `text/plain` onto N-Triples, `application/json` onto JSON-LD and
/// `application/xml` / `text/xml` onto RDF/XML, under which an empty throttle
/// body, a `{"error":"boom"}` page and a SPARQL-results XML document all parse
/// without error (measured against oxrdfio directly). Those are exactly the
/// ambiguous types, so they are rejected here. `RDF_ACCEPT` names three
/// RDF-specific types, so a cooperating server answers with one of these; a
/// genuine RDF/XML document served as bare `application/xml` degrades to
/// `indeterminate`, which is honest, rather than to a confident verdict.
const RDF_MEDIA_TYPES: [&str; 6] = [
    "text/turtle",
    "application/rdf+xml",
    "application/ld+json",
    "application/n-triples",
    "application/trig",
    "application/n-quads",
];

/// How many redirects any probe's chain resolves before we give up on it.
///
/// No probe follows a redirect implicitly (see `Client::http`): each hop is a
/// deliberate re-issue of the same request at the `Location` target, so a
/// preflight's `OPTIONS` is never rewritten into a `GET` and no hop is
/// invisible to the per-host gate. The bound is what stops a redirect cycle
/// from costing an unbounded number of requests against somebody else's
/// server; a chain that needs more than this many hops is one we did not
/// reach the end of, which is `indeterminate` rather than an answer.
///
/// ONE bound for every probe, deliberately: `gated_chain` is the only chain
/// resolver, so the preflight's bound and every other probe's bound are the
/// same number and cannot drift apart. Two constants here would be two
/// promises to keep in step.
const MAX_REDIRECT_HOPS: usize = 5;

/// Cap on the *retained* response body, applied after `resp.text().await` has
/// already buffered the whole response. This bounds what we keep in
/// `Observation.body` and what later gets hashed into declarations, not peak
/// memory: a misbehaving endpoint that serves an enormous body still costs
/// one full in-memory buffer before this cap ever runs. The 30 s request
/// timeout is the only real bound on that. Kept generous enough for a
/// service description or small dataset dump.
const MAX_BODY: usize = 256 * 1024;

/// Truncate `s` to at most `max` bytes, cutting back to the nearest char
/// boundary so the result is always valid UTF-8.
fn truncate_body(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut s = s;
    s.truncate(end);
    s
}

/// One request's outcome: the `Observation` a verdict is drawn from, plus the
/// evidence this module itself needs and no verdict rule reads.
///
/// `Retry-After` stays out of `Observation` for the same reason `Location`
/// does (see `preflight_once`): nothing in `resolve()` consults it, and
/// `Observation` is the shape a verdict is computed from. It is the client's
/// business alone whether we wait.
struct Attempt<X> {
    observation: Observation,
    /// The raw `Retry-After` header value, unparsed. Read by `retry_delay`.
    retry_after: Option<String>,
    /// The raw `Location` header value, unparsed. Read by `gated_chain`, which
    /// is the only thing that follows a redirect in this crate. Carried on
    /// every request kind because every request kind can be redirected, and a
    /// hop nobody reads is a hop reqwest would have to take for us, invisibly
    /// and ungated.
    location: Option<String>,
    /// Whatever else this request kind carries out for its caller: the
    /// response body for a query, `()` for a fetch or a preflight.
    extra: X,
}

impl<X> Attempt<X> {
    /// A request that never produced a response. No status and no headers, so
    /// there is nothing to honour, nowhere to follow, and `retry_delay` will
    /// decline on the missing status alone.
    fn failed(error: String, elapsed_ms: u64, extra: X) -> Attempt<X> {
        Attempt {
            observation: Observation::failed(error, elapsed_ms),
            retry_after: None,
            location: None,
            extra,
        }
    }
}

pub struct Client {
    /// One client for every probe, and it never follows a redirect itself.
    ///
    /// It used to be two, because only `preflight` had to see its redirects
    /// (a `303` rewrites an `OPTIONS` into a `GET`, so following one silently
    /// answers a different question). The policy is now unconditional for a
    /// second reason that applies to every probe: reqwest follows up to ten
    /// redirects INSIDE one `send()`, so those hops would ride under a single
    /// gate acquisition, ungated, unspaced and possibly on other people's
    /// hosts. The README's promise is unconditional, so the redirect policy
    /// that makes it true has to be unconditional too. `gated_chain` resolves
    /// every chain one gated hop at a time instead.
    http: reqwest::Client,
    /// The per-host gate every public probe below passes through, and the
    /// `Retry-After` cap `retry_delay` reads. Stated by the caller rather than
    /// defaulted: a `Client::new` that quietly meant "no politeness" would be
    /// exactly the silent default this crate refuses for an unknown probe
    /// kind, an unknown cost and a missing `var`. Tests pass
    /// `Politeness::unlimited()`; a sweep passes what its flags say.
    politeness: Politeness,
}

impl Client {
    pub fn new(budget: Budget, politeness: Politeness) -> anyhow::Result<Self> {
        // HTTP_PROXY / HTTPS_PROXY are read automatically thanks to the
        // `system-proxy` feature. ids3 pods have no direct internet, so this
        // is load-bearing rather than a convenience.
        let http = reqwest::Client::builder()
            .timeout(budget.request)
            // Never follow a redirect implicitly: a hop reqwest takes for us
            // is a request our per-host gate never saw. `gated_chain` follows
            // them instead, one gated hop at a time. See the field's comment.
            .redirect(reqwest::redirect::Policy::none())
            // Every request we make to a stranger's endpoint carries this, so
            // the URL has to resolve to a page explaining who we are and how to
            // ask us to stop. Stage 3 owes /about for exactly that reason.
            .user_agent(concat!(
                "sparqlwatch/", env!("CARGO_PKG_VERSION"),
                " (+https://sparqlwatch.dev.k8s.semanticscience.org/about)"
            ))
            .build()?;
        Ok(Self { http, politeness })
    }

    /// One header's value as a `String`, or `None` when it is absent or not
    /// valid UTF-8. Shared so `Retry-After` is read the same way on every
    /// request path.
    fn header(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
        headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    }

    /// One attempt, plus at most ONE retry when the endpoint answered with a
    /// throttle carrying a `Retry-After` we are willing to wait out.
    ///
    /// One retry and not a loop: a server that throttles the retry as well is
    /// telling us to come back after this sweep, not to keep knocking.
    ///
    /// The wait itself is NOT taken here, and that is deliberate. `retry_delay`
    /// records the delay on the host (`Politeness::stand_down`), and the
    /// retried walk's first gated hop waits it out while holding that host, so
    /// the pause binds every request to the server rather than only this one.
    /// A sleep here as well would be a second mechanism enforcing the same
    /// pause, and each would hide the other: with both in place, removing
    /// either one left the whole suite green.
    ///
    /// The consequence worth stating is that the retried walk reacquires the
    /// gate hop by hop, so the retried request is separately excluded and
    /// separately spaced, exactly like a first one, and the wait happens
    /// inside the metric budget where `tokio` can cancel it.
    ///
    /// The retried attempt is returned WHOLE, so the reported `elapsed_ms` is
    /// the second walk's own request time and excludes the wait. Our politeness
    /// delay is not the endpoint's response time and must not be published as
    /// one.
    ///
    /// **NEVER acquires the gate**, and must never be called from inside a
    /// held guard: `attempt` acquires, and the per-host lock is not reentrant.
    async fn honouring_retry_after<X, F, Fut>(&self, url: &str, attempt: F) -> Attempt<X>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Attempt<X>>,
    {
        let first = attempt().await;
        let Some(delay) = self.retry_delay(url, &first) else {
            return first;
        };
        tracing::info!(url, delay_ms = delay.as_millis() as u64,
                       "endpoint asked us to come back; the host is held until then, and we retry once");
        // No sleep here: the delay is already recorded against the host, and
        // the acquire inside this walk's first hop is what waits it out.
        let retried = attempt().await;
        // One retry and not a loop, but the retried response can carry an
        // instruction of its own, and an instruction is about the host rather
        // than about this request: a server that throttles the retry as well is
        // telling the whole sweep to go away, not just this metric. There is no
        // second retry to decide, so only the recording is wanted here.
        self.note_throttle(url, &retried);
        retried
    }

    /// Note what a throttle asked for: record the host-level stand-down and
    /// return the delta-seconds it named, or `None` when there was nothing
    /// readable to note.
    ///
    /// Called for EVERY response that could carry an instruction, the retried
    /// one included, and deliberately separate from the cap decision below. The
    /// cap is about how long WE are willing to wait before re-asking; this is
    /// the server asking to be left alone, which binds us whatever we decide
    /// about our own retry.
    ///
    /// - `429` or `503` are the two statuses that mean "not now, try later";
    ///   every other status is an answer about the request, not an instruction.
    /// - An HTTP-date value is recognised and still not acted on: this crate
    ///   does not parse the date form, and guessing a delay from it, wrong in
    ///   the short direction, is exactly the impoliteness the gate exists to
    ///   prevent. Logged at `warn` with the value so we learn whether real
    ///   endpoints use it.
    /// - Junk, or no header at all: nothing to note, and no delay invented.
    fn note_throttle<X>(&self, url: &str, attempt: &Attempt<X>) -> Option<Duration> {
        if !matches!(attempt.observation.status, Some(429) | Some(503)) {
            return None;
        }
        let value = attempt.retry_after.as_deref()?;
        match parse_retry_after(value) {
            RetryAfter::Seconds(d) => {
                self.politeness.stand_down(url, d);
                Some(d)
            }
            RetryAfter::HttpDate => {
                tracing::warn!(url, value, "Retry-After in HTTP-date form; not parsed, not waited out");
                None
            }
            RetryAfter::Unparseable => {
                tracing::warn!(url, value, "Retry-After is unparseable; no delay invented");
                None
            }
        }
    }

    /// How long to wait before one retry, or `None` for "report what we saw".
    ///
    /// Nothing here invents a verdict. In every non-retry case the observation
    /// is returned carrying the real status, and `resolve()` decides what a
    /// throttle means for each probe kind.
    ///
    /// The stand-down is recorded by `note_throttle` before this decides
    /// anything, so a delay too long for us to wait out still defers the host:
    /// declining to wait for our own retry is not permission to ask the same
    /// server something else. Within the cap is decided by `politeness::honour`,
    /// so the cap lives in one place and is testable without a server.
    ///
    /// A beyond-cap delay therefore means "report the throttle and stop
    /// asking", never "ignore the instruction". The later requests that then
    /// wait on the stand-down are cancelled by the metric and endpoint budgets
    /// and reported as `indeterminate`, which is what never getting to ask
    /// actually looks like.
    fn retry_delay<X>(&self, url: &str, attempt: &Attempt<X>) -> Option<Duration> {
        let requested = self.note_throttle(url, attempt)?;
        match honour(requested, self.politeness.retry_after_cap()) {
            Honour::Wait(d) => Some(d),
            Honour::TooLong => {
                tracing::info!(url, requested_s = requested.as_secs(),
                               cap_s = self.politeness.retry_after_cap().as_secs(),
                               "Retry-After beyond the cap; reporting the throttle instead of waiting");
                None
            }
        }
    }

    /// Walk a redirect chain to its end, taking the per-host gate for EVERY
    /// hop. This is the only place in the crate that follows a redirect, and
    /// the only place that acquires the gate.
    ///
    /// `hop` issues exactly ONE request at the URL it is handed and returns
    /// what came back, `Location` included. This function acquires the gate
    /// for that hop's host (which is not necessarily the host the probe was
    /// asked about: a `Location` can point anywhere), awaits the one request,
    /// and RELEASES the gate before it even looks at where to go next.
    ///
    /// Releasing between hops is the point, not a compromise. Each request to
    /// a host is then separately excluded and separately spaced, which is
    /// exactly what the README promises without qualification; a three-hop
    /// chain therefore costs two gaps, and that is the honest price of the
    /// promise. The alternative, one acquisition covering a whole chain, is
    /// what reqwest's own redirect following used to give us: up to ten
    /// requests, to any number of hosts, that the gate never saw.
    ///
    /// The gate is acquired HERE and nowhere below. `hop` runs inside the held
    /// guard, so neither it nor anything it calls may acquire: the per-host
    /// lock is not reentrant, so a second acquire is a task waiting for a lock
    /// it holds itself, which HANGS rather than failing an assertion.
    ///
    /// A chain longer than `MAX_REDIRECT_HOPS`, a `Location` we cannot
    /// resolve, or a cycle ends the walk with the `3xx` we last saw, returned
    /// as itself. No verdict is invented here: `resolve()` reads an unresolved
    /// `3xx` as `Indeterminate`, because we never reached an answer.
    async fn gated_chain<X, F, Fut>(&self, url: &str, hop: F) -> Attempt<X>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Attempt<X>>,
    {
        let mut target = url.to_string();
        // Canonicalised, because every hop after the first is a parsed and
        // rejoined URL, and a cycle check has to compare like with like.
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(Self::canonical(&target));

        let mut a = self.gated_hop(&target, &hop).await;
        // The sum of the hops' own durations, deliberately NOT the wall clock
        // of the walk: the gate's pause before each hop is our politeness, not
        // the endpoint's response time, and must never be published as one.
        // One hop, the ordinary case, therefore reports exactly what it always
        // did.
        let mut requests_ms = a.observation.elapsed_ms;
        let mut hops = 0usize;
        while a.observation.status.is_some_and(|s| (300..=399).contains(&s)) {
            if hops == MAX_REDIRECT_HOPS {
                tracing::warn!(url, hops, "redirect chain longer than the hop bound; not resolved");
                break;
            }
            let Some(next) = a.location.as_deref().and_then(|l| Self::resolve_location(&target, l)) else {
                tracing::warn!(url, status = ?a.observation.status, "redirect carried no usable Location; not resolved");
                break;
            };
            if !seen.insert(next.clone()) {
                tracing::warn!(url, next = %next, "redirect chain loops; not resolved");
                break;
            }
            tracing::debug!(from = %target, to = %next, "re-issuing the request at a redirect target");
            target = next;
            hops += 1;
            a = self.gated_hop(&target, &hop).await;
            requests_ms += a.observation.elapsed_ms;
        }
        a.observation.elapsed_ms = requests_ms;
        a
    }

    /// One hop of a chain: take this hop's host, issue the one request, give
    /// the host back. The guard is dropped when this function returns, which
    /// stamps the release time the next request to that host is spaced from.
    ///
    /// This is the ONLY `acquire` in this module. One acquisition, one
    /// outbound request, no exceptions: that pairing is the whole guarantee,
    /// and it is what makes the reentrancy rule easy to check by reading.
    async fn gated_hop<X, F, Fut>(&self, target: &str, hop: &F) -> Attempt<X>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Attempt<X>>,
    {
        let _host = self.politeness.acquire(target).await;
        hop(target.to_string()).await
    }

    /// `send_origin` is true only for the simple-GET CORS probe (the preflight
    /// does not go through this path). One request shape served
    /// every metric before, so every probe announced an `Origin`; a server that
    /// rejects unknown origins could then perturb the evidence for the metrics
    /// that are not about CORS at all.
    ///
    /// **NEVER acquires the politeness gate**, and neither may anything else
    /// this function is called from. `ask`, `cors`, `select_iris` and
    /// `ask_literal` all funnel through here, every one of them via
    /// `gated_chain`, so this runs INSIDE a guard `gated_hop` already holds.
    /// The per-host lock is not reentrant, so an acquire here would be a task
    /// waiting for a lock it holds itself: a DEADLOCK, not a slow probe and
    /// not a failing assertion. The gate is taken in `gated_hop` and nowhere
    /// else, once per outbound request.
    ///
    /// One request, and one only: a redirect is returned as the `3xx` it is,
    /// with its `Location`, for `gated_chain` to follow on a fresh acquisition.
    /// The query travels to the redirect target with us, because the question
    /// we are asking there is the same question and a `3xx` is not an answer
    /// to it.
    async fn get_with_body(&self, url: &str, query: &str, send_origin: bool) -> Attempt<String> {
        let start = Instant::now();
        let mut req = self
            .http
            .get(url)
            .query(&[("query", query)])
            .header("Accept", "application/sparql-results+json");
        if send_origin {
            req = req.header("Origin", ORIGIN);
        }
        let resp = req.send().await;
        let elapsed = start.elapsed().as_millis() as u64;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Attempt::failed(e.to_string(), elapsed, String::new()),
        };
        let status = resp.status().as_u16();
        let cors = resp.headers().contains_key("access-control-allow-origin");
        let retry_after = Self::header(resp.headers(), "retry-after");
        let location = Self::header(resp.headers(), "location");
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return Attempt::failed(e.to_string(), elapsed, String::new()),
        };

        let looks_html = ctype.contains("text/html")
            || body.trim_start().to_ascii_lowercase().starts_with("<!doctype html")
            || body.trim_start().to_ascii_lowercase().starts_with("<html");
        let json = serde_json::from_str::<serde_json::Value>(&body).ok();
        let body_kind = if looks_html {
            BodyKind::Html
        } else if json.as_ref().map(|v| v.get("boolean").is_some() || v.get("results").is_some()).unwrap_or(false) {
            BodyKind::SparqlJson
        } else {
            BodyKind::Other
        };

        let observation = Observation {
            status: Some(status),
            cors,
            boolean: json.as_ref().and_then(|v| v.get("boolean")).and_then(|b| b.as_bool()),
            bindings: Vec::new(),
            body_kind,
            body: None,
            // Not a fetch: `final_url` is the description fetch's evidence
            // alone, and nothing downstream scopes a query result by URL.
            final_url: None,
            content_type: if ctype.is_empty() { None } else { Some(ctype.clone()) },
            // CORS preflight evidence belongs to `preflight` alone.
            allow_origin: None,
            allow_methods: None,
            allow_headers: None,
            elapsed_ms: elapsed,
            error: None,
        };
        Attempt { observation, retry_after, location, extra: body }
    }

    /// A queryless GET on the endpoint itself, asking for RDF rather than
    /// SPARQL results. This is how a SPARQL service description is obtained
    /// in the wild -- no `?query=` at all, because none is being asked.
    /// Does not send `Origin`: that header belongs to the CORS probe alone.
    ///
    /// One of the six public probes. It does not acquire the gate itself: it
    /// goes through `gated_chain`, which takes the gate once per hop, so a
    /// redirected description costs one acquisition and one gap per request
    /// rather than one for the whole chain.
    pub async fn fetch_rdf(&self, url: &str) -> Observation {
        self.honouring_retry_after(url, || {
            self.gated_chain(url, |target| async move { self.fetch_rdf_once(&target).await })
        })
        .await
        .observation
    }

    /// One queryless RDF fetch, and one request only: a redirect is returned
    /// as the `3xx` it is for `gated_chain` to follow. **NEVER acquires the
    /// gate**: this runs inside the guard `gated_hop` holds, and the per-host
    /// lock is not reentrant, so acquiring here would deadlock on itself
    /// rather than fail an assertion.
    async fn fetch_rdf_once(&self, url: &str) -> Attempt<()> {
        let start = Instant::now();
        let resp = self.http.get(url).header("Accept", RDF_ACCEPT).send().await;
        let elapsed = start.elapsed().as_millis() as u64;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Attempt::failed(e.to_string(), elapsed, ()),
        };
        let status = resp.status().as_u16();
        let cors = resp.headers().contains_key("access-control-allow-origin");
        let retry_after = Self::header(resp.headers(), "retry-after");
        let location = Self::header(resp.headers(), "location");
        // Read before `resp.text()` consumes the response. This is the URL
        // this hop asked, and `gated_chain` only returns the LAST hop's
        // attempt, so for a redirected fetch it is where we actually landed:
        // the URL a redirected description is most likely to name as its
        // `sd:endpoint`.
        let final_url = resp.url().to_string();
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return Attempt::failed(e.to_string(), elapsed, ()),
        };
        // Truncate BEFORE classifying, so classification and the declaration
        // parse downstream read the same bytes. Classifying the full body while
        // `Declarations` is parsed from the truncated one published a
        // self-contradictory row: a description whose only triples sat past the
        // cut classified as `Rdf` (so `verified`) and then graded `Level(0)`,
        // which means "none served", indistinguishable by level from an absent
        // row. Reading one body throughout makes an over-large description
        // `indeterminate`, which is the honest answer: we never read it.
        let body = truncate_body(body, MAX_BODY);

        let looks_html = ctype.to_ascii_lowercase().contains("text/html")
            || body.trim_start().to_ascii_lowercase().starts_with("<!doctype html")
            || body.trim_start().to_ascii_lowercase().starts_with("<html");
        let body_kind = if looks_html {
            BodyKind::Html
        } else if Self::rdf_format_of(&ctype).is_some_and(|fmt| Self::parses_as_rdf(fmt, &body)) {
            BodyKind::Rdf
        } else {
            BodyKind::Other
        };

        let observation = Observation {
            status: Some(status),
            cors,
            boolean: None,
            bindings: Vec::new(),
            body_kind,
            body: Some(body),
            final_url: Some(final_url),
            content_type: if ctype.is_empty() { None } else { Some(ctype.to_ascii_lowercase()) },
            allow_origin: None,
            allow_methods: None,
            allow_headers: None,
            elapsed_ms: elapsed,
            error: None,
        };
        Attempt { observation, retry_after, location, extra: () }
    }

    /// The RDF format `ctype` announces, but only for a media type that
    /// identifies RDF specifically (see `RDF_MEDIA_TYPES`). Parameters such as
    /// `; charset=utf-8` are stripped by `media::essence`, which is shared
    /// with `declare.rs` so the classification and the declaration parse can
    /// never again disagree about what a body is.
    ///
    /// The allowlist stays here and is not pushed down into `media`: this
    /// function's answer decides whether a body is CLASSIFIED as RDF, which is
    /// an assertive claim, while `declare.rs` only needs a parser for a body it
    /// is going to read either way.
    fn rdf_format_of(ctype: &str) -> Option<RdfFormat> {
        let essence = media::essence(ctype);
        if !RDF_MEDIA_TYPES.contains(&essence.as_str()) {
            return None;
        }
        media::rdf_format_of(&essence)
    }

    /// Whether `body` parses under `fmt` AND yields at least one triple.
    /// Sniffing the `Content-Type` alone is not enough evidence: an endpoint
    /// can send `text/turtle` and an HTML error page underneath it, and that
    /// must not be classified as `Rdf`. Neither is a clean zero-triple parse:
    /// an empty `text/turtle` body parses fine and is equally consistent with
    /// "no description here", so it is not a positive identification of RDF
    /// content and must not license a graded `verified`.
    fn parses_as_rdf(fmt: RdfFormat, body: &str) -> bool {
        RdfParser::from_format(fmt)
            .for_reader(body.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .is_ok_and(|quads| !quads.is_empty())
    }

    /// One of the six public probes. The gate is taken per hop inside
    /// `gated_chain`, not here and not in `get_with_body`.
    pub async fn ask(&self, url: &str, query: &str) -> Observation {
        self.honouring_retry_after(url, || self.query_chain(url, query, false)).await.observation
    }

    /// The four query probes' shared chain walk: one gated request per hop,
    /// the same query asked at each. Exists so the four of them cannot drift
    /// apart in how they follow a redirect.
    ///
    /// **NEVER acquires the gate itself**; `gated_chain` does, once per hop.
    async fn query_chain(&self, url: &str, query: &str, send_origin: bool) -> Attempt<String> {
        self.gated_chain(url, move |target| async move {
            self.get_with_body(&target, query, send_origin).await
        })
        .await
    }

    /// The simple-GET CORS probe: a query request that announces an `Origin`,
    /// because the question it asks is what the endpoint does with one. Note
    /// what this measures -- an `access-control-allow-origin` header on a
    /// simple GET -- which is what a `curl` user sees and is weaker than the
    /// preflighted request a real browser editor makes. `preflight` below is
    /// the browser's question. Both facts are published; neither subsumes the
    /// other, because an endpoint can genuinely have one and not the other.
    pub async fn cors(&self, url: &str, query: &str) -> Observation {
        self.honouring_retry_after(url, || self.query_chain(url, query, true)).await.observation
    }

    /// The preflight a browser sends before a cross-origin SPARQL query: an
    /// `OPTIONS` request carrying `Origin`, `Access-Control-Request-Method`
    /// and `Access-Control-Request-Headers`. Without the request-method header
    /// this is not a preflight at all and a correct server may ignore it,
    /// which would make every verdict the resolver draws from it meaningless.
    ///
    /// No redirect is ever followed implicitly, and for this probe that was
    /// the original reason the policy exists: a `303` rewrites the `OPTIONS`
    /// into a `GET`, and an endpoint that refuses `OPTIONS` but sets
    /// `access-control-allow-origin` on a simple GET would then publish a
    /// grant, which is precisely the endpoint this metric exists to catch.
    ///
    /// A 3xx is nevertheless not an answer about the endpoint. Every other
    /// probe reaches the endpoint through its redirect, so treating the
    /// redirect itself as the preflight's answer published `absent` for a
    /// service that answers a preflight perfectly one hop away, in the same run
    /// whose `cors` row followed that same hop. So the chain is resolved
    /// DELIBERATELY, by `gated_chain`: read `Location`, resolve it against the
    /// URL we asked, and re-issue the same `OPTIONS` there, up to
    /// `MAX_REDIRECT_HOPS`, taking the per-host gate for each hop. The method
    /// is never rewritten, and the hops are ours to see, to space and to log
    /// rather than reqwest's to take invisibly.
    ///
    /// What comes back is the response at the end of the chain. A 3xx that
    /// still stands at that point (no usable `Location`, a cycle, or more hops
    /// than the bound) is returned as itself, and the resolver reads it as
    /// `indeterminate`: we never reached a preflight answer.
    ///
    /// Records the header VALUES, not just presence, because presence is not a
    /// grant. No body is read or classified: a preflight response has no
    /// meaningful body, and a `204` has none at all.
    pub async fn preflight(&self, url: &str) -> Observation {
        // One retry for the whole probe, not one per hop. A cap-sized wait on
        // each of six hops would be six times the cap inside one metric
        // budget, and the cap's arithmetic (see `DEFAULT_RETRY_AFTER_CAP`)
        // assumes a single wait. A throttled chain is therefore re-walked from
        // the start, which costs the hops again; chains in the wild are one hop
        // long, and a uniform retry rule is worth more than saving a request
        // in a case that combines a redirect with a throttle.
        //
        // The chain itself is `gated_chain`'s, shared with every other probe,
        // so the hop bound, the cycle check and the per-hop gating are one
        // implementation rather than the preflight's own.
        self.honouring_retry_after(url, || {
            self.gated_chain(url, |target| async move { self.preflight_once(&target).await })
        })
        .await
        .observation
    }

    /// One `OPTIONS` preflight, no redirect resolution: the response's
    /// `Location` is carried out on the `Attempt`, which is evidence
    /// `gated_chain` needs and no verdict rule reads, so it stays out of
    /// `Observation`.
    ///
    /// **NEVER acquires the politeness gate.** This runs once per redirect hop
    /// inside the guard `gated_hop` holds for that hop, and the per-host lock
    /// is not reentrant: acquiring here would make the task wait for a lock it
    /// holds itself, which is a DEADLOCK. It hangs; it does not fail an
    /// assertion. The gate is taken in `gated_hop` and nowhere else.
    async fn preflight_once(&self, url: &str) -> Attempt<()> {
        let start = Instant::now();
        let resp = self
            .http
            .request(reqwest::Method::OPTIONS, url)
            .header("Origin", ORIGIN)
            .header("Access-Control-Request-Method", "GET")
            .header("Access-Control-Request-Headers", "content-type")
            .send()
            .await;
        let elapsed = start.elapsed().as_millis() as u64;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Attempt::failed(e.to_string(), elapsed, ()),
        };
        let headers = resp.headers();
        let value = |name: &str| Self::header(headers, name);

        let observation = Observation {
            status: Some(resp.status().as_u16()),
            cors: headers.contains_key("access-control-allow-origin"),
            boolean: None,
            bindings: Vec::new(),
            body_kind: BodyKind::None,
            body: None,
            final_url: None,
            content_type: None,
            allow_origin: value("access-control-allow-origin"),
            allow_methods: value("access-control-allow-methods"),
            allow_headers: value("access-control-allow-headers"),
            elapsed_ms: elapsed,
            error: None,
        };
        let retry_after = value("retry-after");
        let location = value("location");
        Attempt { observation, retry_after, location, extra: () }
    }

    /// `location` resolved against `base`, absolute or relative, or `None` if
    /// there is nothing usable to resolve: an empty header, or a value no URL
    /// parser will take. `None` is what makes such a redirect
    /// `indeterminate` rather than an answer.
    fn resolve_location(base: &str, location: &str) -> Option<String> {
        let location = location.trim();
        if location.is_empty() {
            return None;
        }
        reqwest::Url::parse(base).ok()?.join(location).ok().map(|u| u.to_string())
    }

    /// `url` as its parser writes it back, so two spellings of one URL compare
    /// equal in the cycle check. An unparseable string is returned unchanged:
    /// the request will fail on it anyway, and the failure is the evidence.
    fn canonical(url: &str) -> String {
        reqwest::Url::parse(url).map(|u| u.to_string()).unwrap_or_else(|_| url.to_string())
    }

    /// Pull binding rows out of a SPARQL JSON body, keeping only the requested
    /// variable and only the requested term type.
    fn extract(body: &str, var: &str, want_literal: bool) -> Vec<String> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
            return Vec::new();
        };
        let Some(rows) = v.get("results").and_then(|r| r.get("bindings")).and_then(|b| b.as_array()) else {
            return Vec::new();
        };
        rows.iter()
            .filter_map(|row| row.get(var))
            .filter(|cell| {
                let t = cell.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if want_literal { t == "literal" || t == "typed-literal" } else { t == "uri" }
            })
            .filter_map(|cell| cell.get("value").and_then(|s| s.as_str()).map(str::to_string))
            .collect()
    }

    /// Collect IRI values of one variable. Literal values are ignored, so a
    /// caller asking for classes cannot be fooled by literals.
    pub async fn select_iris(&self, url: &str, query: &str, var: &str) -> Observation {
        let a = self.honouring_retry_after(url, || self.query_chain(url, query, false)).await;
        let mut o = a.observation;
        if o.body_kind == BodyKind::SparqlJson {
            o.bindings = Self::extract(&a.extra, var, false);
        }
        o
    }

    /// True only when the given variable is bound to a LITERAL at least once.
    /// This is the guard against the false positive measured in the wild:
    /// publications.europa.eu passes a naive `ASK { ?s geo:asWKT ?g }` while
    /// every `?g` is the IRI `rdf:nil`, so it holds zero geometry despite the
    /// naive probe reporting success.
    ///
    /// `var` must be the actual variable name the query binds. A caller that
    /// passes the wrong name gets a confident, silent false negative: no
    /// bindings are found under that name, so this reports `Some(false)`
    /// exactly as if the data were genuinely absent.
    pub async fn ask_literal(&self, url: &str, query: &str, var: &str) -> Observation {
        let a = self.honouring_retry_after(url, || self.query_chain(url, query, false)).await;
        let mut o = a.observation;
        if o.body_kind == BodyKind::SparqlJson {
            let lits = Self::extract(&a.extra, var, true);
            o.boolean = Some(!lits.is_empty());
            o.bindings = lits;
        }
        o
    }
}

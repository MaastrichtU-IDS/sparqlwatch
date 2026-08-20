use crate::budget::Budget;
use crate::observe::{BodyKind, Observation};
use oxrdfio::{RdfFormat, RdfParser};
use std::time::Instant;

/// Announced only by the CORS probe, so the other probes cannot be perturbed
/// by a server that filters on it.
const ORIGIN: &str = "https://sparqlwatch.example";

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

pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new(budget: Budget) -> anyhow::Result<Self> {
        // HTTP_PROXY / HTTPS_PROXY are read automatically thanks to the
        // `system-proxy` feature. ids3 pods have no direct internet, so this
        // is load-bearing rather than a convenience.
        let http = reqwest::Client::builder()
            .timeout(budget.request)
            .user_agent(concat!(
                "sparqlwatch/", env!("CARGO_PKG_VERSION"),
                " (+https://sparqlwatch.example/about)"
            ))
            .build()?;
        Ok(Self { http })
    }

    /// `send_origin` is true only for the CORS probe. One request shape served
    /// every metric before, so every probe announced an `Origin`; a server that
    /// rejects unknown origins could then perturb the evidence for the metrics
    /// that are not about CORS at all.
    async fn get_with_body(&self, url: &str, query: &str, send_origin: bool) -> (Observation, String) {
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
            Err(e) => return (Observation::failed(e.to_string(), elapsed), String::new()),
        };
        let status = resp.status().as_u16();
        let cors = resp.headers().contains_key("access-control-allow-origin");
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return (Observation::failed(e.to_string(), elapsed), String::new()),
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
            content_type: if ctype.is_empty() { None } else { Some(ctype.clone()) },
            elapsed_ms: elapsed,
            error: None,
        };
        (observation, body)
    }

    /// A queryless GET on the endpoint itself, asking for RDF rather than
    /// SPARQL results. This is how a SPARQL service description is obtained
    /// in the wild -- no `?query=` at all, because none is being asked.
    /// Does not send `Origin`: that header belongs to the CORS probe alone.
    pub async fn fetch_rdf(&self, url: &str) -> Observation {
        let start = Instant::now();
        let resp = self.http.get(url).header("Accept", RDF_ACCEPT).send().await;
        let elapsed = start.elapsed().as_millis() as u64;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Observation::failed(e.to_string(), elapsed),
        };
        let status = resp.status().as_u16();
        let cors = resp.headers().contains_key("access-control-allow-origin");
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return Observation::failed(e.to_string(), elapsed),
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

        Observation {
            status: Some(status),
            cors,
            boolean: None,
            bindings: Vec::new(),
            body_kind,
            body: Some(body),
            content_type: if ctype.is_empty() { None } else { Some(ctype.to_ascii_lowercase()) },
            elapsed_ms: elapsed,
            error: None,
        }
    }

    /// The RDF format `ctype` announces, but only for a media type that
    /// identifies RDF specifically (see `RDF_MEDIA_TYPES`). Parameters such as
    /// `; charset=utf-8` are stripped before the comparison.
    fn rdf_format_of(ctype: &str) -> Option<RdfFormat> {
        let essence = ctype.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if !RDF_MEDIA_TYPES.contains(&essence.as_str()) {
            return None;
        }
        RdfFormat::from_media_type(&essence)
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

    pub async fn ask(&self, url: &str, query: &str) -> Observation {
        self.get_with_body(url, query, false).await.0
    }

    /// The CORS probe: the one request that announces an `Origin`, because the
    /// question it asks is what the endpoint does with one. Note what this
    /// measures -- an `access-control-allow-origin` header on a simple GET --
    /// which is weaker than the preflighted request a real browser editor
    /// makes. An `OPTIONS` preflight probe is the real check and is deferred.
    pub async fn cors(&self, url: &str, query: &str) -> Observation {
        self.get_with_body(url, query, true).await.0
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
        let (mut o, body) = self.get_with_body(url, query, false).await;
        if o.body_kind == BodyKind::SparqlJson {
            o.bindings = Self::extract(&body, var, false);
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
        let (mut o, body) = self.get_with_body(url, query, false).await;
        if o.body_kind == BodyKind::SparqlJson {
            let lits = Self::extract(&body, var, true);
            o.boolean = Some(!lits.is_empty());
            o.bindings = lits;
        }
        o
    }
}

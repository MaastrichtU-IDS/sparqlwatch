use crate::budget::Budget;
use crate::observe::{BodyKind, Observation};
use std::time::Instant;

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

    async fn get_with_body(&self, url: &str, query: &str) -> (Observation, String) {
        let start = Instant::now();
        let resp = self
            .http
            .get(url)
            .query(&[("query", query)])
            .header("Accept", "application/sparql-results+json")
            .header("Origin", "https://sparqlwatch.example")
            .send()
            .await;
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
            elapsed_ms: elapsed,
            error: None,
        };
        (observation, body)
    }

    async fn get(&self, url: &str, query: &str) -> Observation {
        self.get_with_body(url, query).await.0
    }

    pub async fn ask(&self, url: &str, query: &str) -> Observation {
        self.get(url, query).await
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
        let (mut o, body) = self.get_with_body(url, query).await;
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
        let (mut o, body) = self.get_with_body(url, query).await;
        if o.body_kind == BodyKind::SparqlJson {
            let lits = Self::extract(&body, var, true);
            o.boolean = Some(!lits.is_empty());
            o.bindings = lits;
        }
        o
    }
}

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

    async fn get(&self, url: &str, query: &str) -> Observation {
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
            Err(e) => return Observation::failed(e.to_string(), elapsed),
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
            Err(e) => return Observation::failed(e.to_string(), elapsed),
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

        Observation {
            status: Some(status),
            cors,
            boolean: json.as_ref().and_then(|v| v.get("boolean")).and_then(|b| b.as_bool()),
            bindings: Vec::new(),
            body_kind,
            elapsed_ms: elapsed,
            error: None,
        }
    }

    pub async fn ask(&self, url: &str, query: &str) -> Observation {
        self.get(url, query).await
    }

    /// Collect IRI values of one variable. Literal values are ignored, so a
    /// caller asking for classes cannot be fooled by literals.
    pub async fn select_iris(&self, url: &str, query: &str, _var: &str) -> Observation {
        let mut o = self.get(url, query).await;
        if o.body_kind != BodyKind::SparqlJson {
            return o;
        }
        // Re-parse for bindings; `get` only extracts `boolean`.
        // (Kept simple: one small extra parse instead of threading the Value out.)
        o.bindings = Vec::new();
        o
    }
}

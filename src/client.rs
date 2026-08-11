use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{json, Map, Value};

use crate::config::{AuthMode, Config};
use crate::spec::Method;

pub struct CourseStack {
    http: Client,
    cfg: Config,
}

/// Normalised result of one CourseStack call, rendered back to the model.
pub struct ApiResponse {
    pub status: u16,
    pub value: Value,
}

impl ApiResponse {
    pub fn is_error(&self) -> bool {
        self.status >= 400
    }
}

impl CourseStack {
    pub fn new(cfg: Config) -> Result<Self, String> {
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            // Download endpoints answer 302 with a presigned URL, and plain HTTP
            // answers 308 — neither redirect should be followed automatically:
            // the first is the payload we want, the second would leak the key.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("coursestack-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self { http, cfg })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn authed(&self, req: RequestBuilder) -> RequestBuilder {
        match self.cfg.auth_mode {
            AuthMode::Basic => req.basic_auth(&self.cfg.api_key, None::<&str>),
            AuthMode::Bearer => req.bearer_auth(&self.cfg.api_key),
        }
    }

    pub fn call(
        &self,
        method: Method,
        path: &str,
        query: &Map<String, Value>,
        body: Option<&Value>,
    ) -> Result<ApiResponse, String> {
        if self.cfg.read_only && method != Method::Get {
            return Err(format!(
                "refused: COURSESTACK_READ_ONLY is enabled and {} {path} is a write operation",
                method.as_str()
            ));
        }

        let url = format!("{}{}", self.cfg.base_url, path);
        let mut req = self.authed(
            match method {
                Method::Get => self.http.get(&url),
                Method::Post => self.http.post(&url),
                Method::Put => self.http.put(&url),
                Method::Patch => self.http.patch(&url),
                Method::Delete => self.http.delete(&url),
            }
            .header("Accept", "application/json"),
        );

        for (key, value) in query {
            match value {
                Value::Null => {}
                Value::Array(items) => {
                    for item in items {
                        req = req.query(&[(key.as_str(), scalar(item))]);
                    }
                }
                other => req = req.query(&[(key.as_str(), scalar(other))]),
            }
        }
        if let Some(b) = body {
            req = req.json(b);
        }

        let resp = req
            .send()
            .map_err(|e| format!("request to {url} failed: {e}"))?;

        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = resp.text().unwrap_or_default();

        let mut value =
            json!({ "status": status, "request": format!("{} {}", method.as_str(), url) });
        if let Some(loc) = location {
            value["redirect_url"] = Value::String(loc);
        }
        if !text.trim().is_empty() {
            value["body"] = serde_json::from_str(&text).unwrap_or(Value::String(text));
        }

        Ok(ApiResponse { status, value })
    }

    /// PUT a local file to a presigned upload URL returned by the file-creation
    /// endpoints. Presigned URLs carry their own signature, so no API key here.
    pub fn upload(
        &self,
        url: &str,
        path: &str,
        content_type: Option<&str>,
    ) -> Result<Value, String> {
        if self.cfg.read_only {
            return Err(
                "refused: COURSESTACK_READ_ONLY is enabled, upload is a write operation".into(),
            );
        }
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let len = bytes.len();
        let mut req = self.http.put(url).body(bytes);
        if let Some(ct) = content_type {
            req = req.header("Content-Type", ct);
        }
        let resp = req
            .send()
            .map_err(|e| format!("upload to presigned URL failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        Ok(json!({
            "status": status,
            "uploaded_bytes": len,
            "file": path,
            "response": text,
        }))
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

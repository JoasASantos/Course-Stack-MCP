use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
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

        // `req` is only ever sent through a clone, so the same template can be
        // resent verbatim on retry. JSON bodies are buffered, so try_clone()
        // never fails here (it only fails for streaming bodies, which we don't use).
        let max_attempts = self.cfg.max_retries.saturating_add(1);
        for attempt in 0..max_attempts {
            let this_req = req.try_clone().ok_or_else(|| {
                "internal error: request could not be cloned for retry".to_string()
            })?;

            match this_req.send() {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let can_retry = attempt + 1 < max_attempts && should_retry(method, status);
                    if can_retry {
                        std::thread::sleep(backoff_duration(
                            attempt,
                            retry_after(&resp),
                            self.cfg.retry_base_ms,
                        ));
                        continue;
                    }
                    return Ok(Self::finish(method, &url, resp));
                }
                Err(e) => {
                    let can_retry = attempt + 1 < max_attempts && method == Method::Get;
                    if can_retry {
                        std::thread::sleep(backoff_duration(attempt, None, self.cfg.retry_base_ms));
                        continue;
                    }
                    return Err(format!("request to {url} failed: {e}"));
                }
            }
        }
        unreachable!("the loop above always returns on its last attempt")
    }

    fn finish(method: Method, url: &str, resp: Response) -> ApiResponse {
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

        ApiResponse { status, value }
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

/// 429 is always safe to retry (the request was rejected, not processed).
/// 5xx is only retried for GET, since we can't tell whether a write already
/// took effect before the server failed.
fn should_retry(method: Method, status: u16) -> bool {
    status == 429 || (method == Method::Get && matches!(status, 500 | 502 | 503 | 504))
}

fn retry_after(resp: &Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Exponential backoff, capped at 8s, or the server's `Retry-After` (capped
/// at 30s) when present.
fn backoff_duration(attempt: u32, retry_after_secs: Option<u64>, base_ms: u64) -> Duration {
    if let Some(secs) = retry_after_secs {
        return Duration::from_secs(secs.min(30));
    }
    let factor = 1u64 << attempt.min(6);
    Duration::from_millis(base_ms.saturating_mul(factor).min(8_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_matches_method_and_status() {
        assert!(should_retry(Method::Get, 429));
        assert!(should_retry(Method::Post, 429));
        assert!(should_retry(Method::Get, 503));
        assert!(
            !should_retry(Method::Post, 503),
            "5xx on a write is not safely retryable"
        );
        assert!(!should_retry(Method::Get, 404));
        assert!(!should_retry(Method::Get, 200));
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        assert_eq!(backoff_duration(0, None, 300), Duration::from_millis(300));
        assert_eq!(backoff_duration(1, None, 300), Duration::from_millis(600));
        assert_eq!(backoff_duration(2, None, 300), Duration::from_millis(1200));
        assert_eq!(
            backoff_duration(20, None, 300),
            Duration::from_millis(8_000)
        );
    }

    #[test]
    fn retry_after_header_wins_and_is_capped() {
        assert_eq!(backoff_duration(5, Some(2), 300), Duration::from_secs(2));
        assert_eq!(backoff_duration(5, Some(600), 300), Duration::from_secs(30));
    }
}

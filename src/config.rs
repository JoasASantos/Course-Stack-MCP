use std::env;

pub const DEFAULT_BASE_URL: &str = "https://app.coursestack.com";

/// How the API key is presented to CourseStack.
///
/// The documented scheme is HTTP Basic with the key as the username and an
/// empty password; `Bearer` is kept as an escape hatch for proxies/gateways.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AuthMode {
    Basic,
    Bearer,
}

/// Runtime configuration, entirely driven by environment variables so the
/// server can be launched by any MCP client without extra flags.
#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub auth_mode: AuthMode,
    pub timeout_secs: u64,
    pub read_only: bool,
    pub include_deprecated: bool,
    pub spec_path: Option<String>,
    /// Extra attempts after the first, on top of 429 / retryable 5xx.
    pub max_retries: u32,
    pub retry_base_ms: u64,
    /// Default page cap for tools called with `all_pages: true`.
    pub max_pages: usize,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let api_key = env::var("COURSESTACK_API_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                "COURSESTACK_API_KEY is not set. Export it, or add it to the `env` block of your \
                 MCP client configuration. Keys start with `sk_` and are managed in the \
                 CourseStack application."
                    .to_string()
            })?;

        let base_url = env::var("COURSESTACK_BASE_URL")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        if base_url.starts_with("http://") {
            // Basic auth over plaintext would leak the secret key on the wire.
            return Err(format!(
                "refusing to use plaintext base URL {base_url}: the API key travels in the \
                 Authorization header, so HTTPS is required"
            ));
        }

        let auth_mode = match env::var("COURSESTACK_AUTH_MODE").as_deref() {
            Ok("bearer") | Ok("Bearer") => AuthMode::Bearer,
            _ => AuthMode::Basic,
        };

        Ok(Config {
            api_key,
            base_url,
            auth_mode,
            timeout_secs: env::var("COURSESTACK_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            read_only: is_truthy("COURSESTACK_READ_ONLY"),
            include_deprecated: is_truthy("COURSESTACK_INCLUDE_DEPRECATED"),
            spec_path: env::var("COURSESTACK_OPENAPI")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            max_retries: env::var("COURSESTACK_MAX_RETRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            retry_base_ms: env::var("COURSESTACK_RETRY_BASE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            max_pages: env::var("COURSESTACK_MAX_PAGES")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(20),
        })
    }
}

fn is_truthy(key: &str) -> bool {
    matches!(env::var(key).as_deref(), Ok("1") | Ok("true") | Ok("yes"))
}

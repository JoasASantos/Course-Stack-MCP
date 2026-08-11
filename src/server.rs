use std::io::{BufRead, Write};

use serde_json::{json, Map, Value};

use crate::client::CourseStack;
use crate::spec::{Method, ToolDef};

pub const SUPPORTED_PROTOCOL: &str = "2025-06-18";
const SERVER_NAME: &str = "coursestack-mcp";

pub struct Server {
    client: CourseStack,
    tools: Vec<ToolDef>,
}

impl Server {
    pub fn new(client: CourseStack, tools: Vec<ToolDef>) -> Self {
        Self { client, tools }
    }

    /// Newline-delimited JSON-RPC over stdio, the transport every MCP client
    /// (Claude Code, Claude Desktop, Codex, ...) speaks by default.
    pub fn serve_stdio(&self) -> Result<(), String> {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();

        for line in stdin.lock().lines() {
            let line = line.map_err(|e| format!("stdin read failed: {e}"))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let request: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    write_message(
                        &mut stdout,
                        &error_response(Value::Null, -32700, &format!("parse error: {e}")),
                    )?;
                    continue;
                }
            };

            if let Some(response) = self.handle(&request) {
                write_message(&mut stdout, &response)?;
            }
        }
        Ok(())
    }

    /// Returns `None` for notifications, which must not be answered.
    fn handle(&self, request: &Value) -> Option<Value> {
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = request.get("id").cloned();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // Notifications (`notifications/initialized`, `$/cancel`, ...) have no id
        // and must not be answered.
        id.as_ref()?;
        let id = id.unwrap();

        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(&params),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "resources/list" => Ok(json!({ "resources": [] })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            other => Err(JsonRpcError {
                code: -32601,
                message: format!("method not found: {other}"),
            }),
        };

        Some(match result {
            Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
            Err(e) => error_response(id, e.code, &e.message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let protocol = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(SUPPORTED_PROTOCOL)
            .to_string();

        json!({
            "protocolVersion": protocol,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            "instructions": format!(
                "Tools for the CourseStack API at {}. Names mirror the OpenAPI operation ids \
                 (content_lessons_create, course_enrollments_list, ...). Path parameters are \
                 top-level arguments; the JSON payload goes in `body`. File uploads are two \
                 steps: call the *_files_create tool to get a presigned URL, then \
                 coursestack_upload_file with that URL.{}",
                self.client.config().base_url,
                if self.client.config().read_only { " Read-only mode is active: writes are refused." } else { "" }
            ),
        })
    }

    fn tools_list(&self) -> Value {
        let mut listed: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                    "annotations": {
                        "readOnlyHint": t.read_only_hint,
                        "destructiveHint": t.method == Method::Delete,
                    },
                })
            })
            .collect();
        listed.extend(builtin_tool_descriptors());
        json!({ "tools": listed })
    }

    fn tools_call(&self, params: &Value) -> Result<Value, JsonRpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| JsonRpcError {
                code: -32602,
                message: "missing tool name".into(),
            })?;
        let args = match params.get("arguments") {
            Some(Value::Object(m)) => m.clone(),
            Some(Value::Null) | None => Map::new(),
            Some(other) => {
                return Err(JsonRpcError {
                    code: -32602,
                    message: format!("`arguments` must be an object, got {other}"),
                })
            }
        };

        let outcome = match name {
            "coursestack_request" => self.raw_request(&args),
            "coursestack_upload_file" => self.upload_file(&args),
            _ => self.call_endpoint(name, &args),
        };

        Ok(match outcome {
            Ok(value) => tool_result(&value, false),
            Err(message) => tool_result(&Value::String(message), true),
        })
    }

    fn call_endpoint(&self, name: &str, args: &Map<String, Value>) -> Result<Value, String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| format!("unknown tool `{name}`"))?;

        let mut path = tool.path.clone();
        for param in &tool.path_params {
            let value = args
                .get(param)
                .ok_or_else(|| format!("missing required path parameter `{param}`"))?;
            let rendered = match value {
                Value::String(s) => s.clone(),
                Value::Null => return Err(format!("path parameter `{param}` must not be null")),
                other => other.to_string(),
            };
            path = path.replace(&format!("{{{param}}}"), &percent_encode(&rendered));
        }

        let all_pages = args
            .get("all_pages")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if all_pages && !tool.paginated {
            return Err(format!(
                "tool `{name}` does not support pagination (no `next_key` cursor)"
            ));
        }
        let max_pages = args
            .get("max_pages")
            .and_then(Value::as_u64)
            .map(|v| v.max(1) as usize)
            .unwrap_or(self.client.config().max_pages);

        let mut query = Map::new();
        for (key, value) in args {
            if tool.path_params.contains(key) || value.is_null() {
                continue;
            }
            if key == "body" {
                if !tool.takes_body {
                    return Err(format!("tool `{name}` does not accept a request body"));
                }
                continue;
            }
            if key == "all_pages" || key == "max_pages" {
                continue; // consumed above, never sent to CourseStack
            }
            if !tool.query_params.contains(key) {
                return Err(format!(
                    "unknown argument `{key}` for `{name}`; accepted: {}",
                    accepted_args(tool)
                ));
            }
            query.insert(key.clone(), value.clone());
        }

        let body = args.get("body").filter(|v| !v.is_null());
        if tool.takes_body && body.is_none() && tool.method != Method::Get {
            let required = tool.input_schema["required"]
                .as_array()
                .map(|r| r.iter().any(|v| v == "body"))
                .unwrap_or(false);
            if required {
                return Err(format!("tool `{name}` requires a `body` object"));
            }
        }

        if all_pages {
            return self.paginate(tool.method, &path, query, max_pages);
        }

        let response = self.client.call(tool.method, &path, &query, body)?;
        if response.is_error() {
            return Err(format!(
                "CourseStack returned HTTP {}: {}",
                response.status,
                serde_json::to_string_pretty(&response.value).unwrap_or_default()
            ));
        }
        Ok(response.value)
    }

    /// Follow a `next_key` cursor until the API reports no more pages or
    /// `max_pages` is hit, merging every page's array fields into one object.
    fn paginate(
        &self,
        method: Method,
        path: &str,
        mut query: Map<String, Value>,
        max_pages: usize,
    ) -> Result<Value, String> {
        let mut merged = Map::new();
        let mut pages_fetched = 0usize;
        let mut last_paging_info = Value::Null;
        let mut truncated = false;

        loop {
            let response = self.client.call(method, path, &query, None)?;
            if response.is_error() {
                return Err(format!(
                    "CourseStack returned HTTP {} while paginating (page {}): {}",
                    response.status,
                    pages_fetched + 1,
                    serde_json::to_string_pretty(&response.value).unwrap_or_default()
                ));
            }
            pages_fetched += 1;

            let Some(body) = response.value.get("body").and_then(Value::as_object) else {
                break; // empty/non-object body: nothing to merge or paginate further
            };
            merge_page(&mut merged, body);

            let paging_info = body.get("paging_info").cloned().unwrap_or(Value::Null);
            last_paging_info = paging_info.clone();
            let next_key = next_page_key(&paging_info);

            match next_key {
                Some(key) if pages_fetched < max_pages => {
                    query.insert("next_key".to_string(), Value::String(key));
                }
                Some(_) => {
                    truncated = true;
                    break;
                }
                None => break,
            }
        }

        merged.insert("paging_info".to_string(), last_paging_info);
        merged.insert(
            "pages_fetched".to_string(),
            Value::from(pages_fetched as u64),
        );
        if truncated {
            merged.insert("truncated".to_string(), Value::Bool(true));
        }
        Ok(json!({
            "status": 200,
            "request": format!("{} {} (paginated, {pages_fetched} page(s))", method.as_str(), path),
            "body": Value::Object(merged),
        }))
    }

    fn raw_request(&self, args: &Map<String, Value>) -> Result<Value, String> {
        let method_str = args
            .get("method")
            .and_then(Value::as_str)
            .ok_or("missing `method`")?;
        let method = Method::parse(method_str)
            .ok_or_else(|| format!("unsupported HTTP method `{method_str}`"))?;
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or("missing `path`")?;
        if !path.starts_with('/') {
            return Err(format!("`path` must start with `/`, got `{path}`"));
        }
        let query = match args.get("query") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let body = args.get("body").filter(|v| !v.is_null());

        let response = self.client.call(method, path, &query, body)?;
        if response.is_error() {
            return Err(format!(
                "CourseStack returned HTTP {}: {}",
                response.status,
                serde_json::to_string_pretty(&response.value).unwrap_or_default()
            ));
        }
        Ok(response.value)
    }

    fn upload_file(&self, args: &Map<String, Value>) -> Result<Value, String> {
        let url = args
            .get("upload_url")
            .and_then(Value::as_str)
            .ok_or("missing `upload_url`")?;
        let file = args
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or("missing `file_path`")?;
        let content_type = args.get("content_type").and_then(Value::as_str);
        self.client.upload(url, file, content_type)
    }
}

struct JsonRpcError {
    code: i64,
    message: String,
}

fn accepted_args(tool: &ToolDef) -> String {
    let mut names: Vec<&str> = tool.path_params.iter().map(String::as_str).collect();
    names.extend(tool.query_params.iter().map(String::as_str));
    if tool.takes_body {
        names.push("body");
    }
    if tool.paginated {
        names.push("all_pages");
        names.push("max_pages");
    }
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

/// Merge one page's array-valued fields into the running total; scalar
/// fields (other than `paging_info`, handled separately) keep their
/// first-seen value.
fn merge_page(merged: &mut Map<String, Value>, page: &Map<String, Value>) {
    for (key, value) in page {
        if key == "paging_info" {
            continue;
        }
        match value {
            Value::Array(items) => match merged.get_mut(key) {
                Some(Value::Array(existing)) => existing.extend(items.clone()),
                _ => {
                    merged.insert(key.clone(), Value::Array(items.clone()));
                }
            },
            other => {
                merged.entry(key.clone()).or_insert_with(|| other.clone());
            }
        }
    }
}

fn next_page_key(paging_info: &Value) -> Option<String> {
    if !paging_info
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    paging_info
        .get("next_key")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn tool_result(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_message(out: &mut impl Write, value: &Value) -> Result<(), String> {
    let line = serde_json::to_string(value).map_err(|e| format!("serialise failed: {e}"))?;
    writeln!(out, "{line}").map_err(|e| format!("stdout write failed: {e}"))?;
    out.flush().map_err(|e| format!("stdout flush failed: {e}"))
}

/// Percent-encode a single path segment value.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub fn builtin_tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "coursestack_request",
            "description": "Escape hatch: call any CourseStack API path directly, including \
                            endpoints not covered by the generated tools.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"] },
                    "path": { "type": "string", "description": "Path starting with `/`, e.g. /api/students" },
                    "query": { "type": "object", "description": "Query string parameters." },
                    "body": { "type": "object", "description": "JSON request body." }
                },
                "required": ["method", "path"]
            },
            "annotations": { "readOnlyHint": false }
        }),
        json!({
            "name": "coursestack_upload_file",
            "description": "Upload a local file to a presigned URL returned by \
                            content_files_create, content_files_reupload or content_jupyter_create.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "upload_url": { "type": "string", "description": "Presigned upload URL." },
                    "file_path": { "type": "string", "description": "Absolute path of the local file." },
                    "content_type": { "type": "string", "description": "Optional MIME type." }
                },
                "required": ["upload_url", "file_path"]
            },
            "annotations": { "readOnlyHint": false }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(percent_encode("abc-123"), "abc-123");
        assert_eq!(percent_encode("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn merge_page_concatenates_arrays_and_keeps_first_scalar() {
        let mut merged = Map::new();
        merge_page(
            &mut merged,
            json!({ "students": [{"id": 1}], "total_count": 2, "paging_info": {"has_more": true} })
                .as_object()
                .unwrap(),
        );
        merge_page(
            &mut merged,
            json!({ "students": [{"id": 2}], "total_count": 999, "paging_info": {"has_more": false} })
                .as_object()
                .unwrap(),
        );
        assert_eq!(merged["students"], json!([{"id": 1}, {"id": 2}]));
        assert_eq!(merged["total_count"], json!(2), "first-seen scalar wins");
        assert!(
            merged.get("paging_info").is_none(),
            "paging_info is merged separately"
        );
    }

    #[test]
    fn next_page_key_respects_has_more() {
        assert_eq!(
            next_page_key(&json!({ "has_more": true, "next_key": "abc" })),
            Some("abc".to_string())
        );
        assert_eq!(next_page_key(&json!({ "has_more": false })), None);
        assert_eq!(next_page_key(&Value::Null), None);
    }

    #[test]
    fn notifications_get_no_response() {
        let spec = crate::spec::load_spec(None).unwrap();
        let tools = crate::spec::build_tools(&spec, false);
        let cfg = crate::config::Config {
            api_key: "sk_test".into(),
            base_url: "https://app.coursestack.com".into(),
            auth_mode: crate::config::AuthMode::Basic,
            timeout_secs: 5,
            read_only: true,
            include_deprecated: false,
            spec_path: None,
            max_retries: 0,
            retry_base_ms: 1,
            max_pages: 20,
        };
        let server = Server::new(CourseStack::new(cfg).unwrap(), tools);
        assert!(server
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());

        let listed = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
            .unwrap();
        let count = listed["result"]["tools"].as_array().unwrap().len();
        assert!(count > 40, "expected the full tool surface, got {count}");
    }
}

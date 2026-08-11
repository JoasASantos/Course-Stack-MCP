use serde_json::{json, Map, Value};

/// The CourseStack OpenAPI document, trimmed to the request side (response
/// schemas are dropped) and compiled into the binary. Override at runtime with
/// COURSESTACK_OPENAPI=/path/to/openapi.json to pick up API changes without a
/// rebuild.
pub const EMBEDDED_SPEC: &str = include_str!("../spec/coursestack.min.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s.to_ascii_lowercase().as_str() {
            "get" => Some(Method::Get),
            "post" => Some(Method::Post),
            "put" => Some(Method::Put),
            "patch" => Some(Method::Patch),
            "delete" => Some(Method::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub method: Method,
    /// Path template, e.g. `/api/content/{content_id}/lessons/{lesson_id}`.
    pub path: String,
    pub path_params: Vec<String>,
    pub query_params: Vec<String>,
    pub takes_body: bool,
    pub input_schema: Value,
    pub read_only_hint: bool,
    /// True for list endpoints that expose a `next_key` cursor — these accept
    /// the synthetic `all_pages` / `max_pages` arguments in server.rs.
    pub paginated: bool,
}

pub fn load_spec(path: Option<&str>) -> Result<Value, String> {
    let raw = match path {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| format!("cannot read OpenAPI document {p}: {e}"))?,
        None => EMBEDDED_SPEC.to_string(),
    };
    serde_json::from_str(&raw).map_err(|e| format!("invalid OpenAPI JSON: {e}"))
}

/// Turn every operation in the document into one MCP tool.
pub fn build_tools(spec: &Value, include_deprecated: bool) -> Vec<ToolDef> {
    let mut tools = Vec::new();
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        return tools;
    };

    for (path, item) in paths {
        let Some(ops) = item.as_object() else {
            continue;
        };
        for (verb, op) in ops {
            let Some(method) = Method::parse(verb) else {
                continue;
            };
            let deprecated = op
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if deprecated && !include_deprecated {
                continue;
            }
            let Some(operation_id) = op.get("operationId").and_then(Value::as_str) else {
                continue;
            };
            tools.push(build_tool(operation_id, method, path, op, deprecated));
        }
    }

    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

fn build_tool(
    operation_id: &str,
    method: Method,
    path: &str,
    op: &Value,
    deprecated: bool,
) -> ToolDef {
    let name = sanitize_name(operation_id);

    let summary = op.get("summary").and_then(Value::as_str).unwrap_or("");
    let detail = op.get("description").and_then(Value::as_str).unwrap_or("");
    let mut description = String::new();
    if !summary.is_empty() {
        description.push_str(summary);
    }
    if !detail.is_empty() && detail != summary {
        if !description.is_empty() {
            description.push_str(" — ");
        }
        description.push_str(detail);
    }
    if description.is_empty() {
        description.push_str(operation_id);
    }
    description.push_str(&format!(" [{} {}]", method.as_str(), path));
    if deprecated {
        description.push_str(" (DEPRECATED)");
    }

    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    let mut path_params = Vec::new();
    let mut query_params = Vec::new();

    for param in op
        .get("parameters")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(pname) = param.get("name").and_then(Value::as_str) else {
            continue;
        };
        let location = param.get("in").and_then(Value::as_str).unwrap_or("query");
        let mut schema = param_schema(param);
        if let Some(desc) = param.get("description").and_then(Value::as_str) {
            schema["description"] = Value::String(desc.to_string());
        }

        match location {
            "path" => {
                path_params.push(pname.to_string());
                required.push(Value::String(pname.to_string()));
            }
            "query" => {
                query_params.push(pname.to_string());
                if param
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    required.push(Value::String(pname.to_string()));
                }
            }
            // Header/cookie parameters are not part of this API surface.
            _ => continue,
        }
        properties.insert(pname.to_string(), schema);
    }

    // Path templates are authoritative: the document occasionally omits a
    // declared path parameter, so backfill anything referenced by the template.
    for pname in template_params(path) {
        if !path_params.contains(&pname) {
            properties.insert(
                pname.clone(),
                json!({ "type": "string", "description": format!("Path parameter `{pname}`") }),
            );
            required.push(Value::String(pname.clone()));
            path_params.push(pname);
        }
    }

    let body_schema = op
        .get("requestBody")
        .and_then(|rb| rb.get("content"))
        .and_then(|c| c.get("application/json"))
        .and_then(|c| c.get("schema"))
        .cloned();

    let takes_body = body_schema.is_some();
    if let Some(mut schema) = body_schema {
        let body_required = op
            .get("requestBody")
            .and_then(|rb| rb.get("required"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if schema.get("description").is_none() {
            schema["description"] = Value::String("JSON request body.".to_string());
        }
        properties.insert("body".to_string(), schema);
        if body_required {
            required.push(Value::String("body".to_string()));
        }
    }

    let paginated = query_params.iter().any(|p| p == "next_key");
    if paginated {
        properties.insert(
            "all_pages".to_string(),
            json!({
                "type": "boolean",
                "description": "Follow `next_key` automatically, merging every page's array \
                                 fields into one result. Default false (first page only)."
            }),
        );
        properties.insert(
            "max_pages".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "description": "Cap on pages fetched when all_pages is true. Defaults to \
                                 COURSESTACK_MAX_PAGES (20)."
            }),
        );
        description.push_str(" Supports all_pages=true to auto-fetch every page.");
    }

    let input_schema = json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": Value::Array(required),
        "additionalProperties": false,
    });

    ToolDef {
        name,
        description,
        method,
        path: path.to_string(),
        path_params,
        query_params,
        takes_body,
        input_schema,
        read_only_hint: method == Method::Get,
        paginated,
    }
}

fn param_schema(param: &Value) -> Value {
    if let Some(schema) = param.get("schema") {
        return schema.clone();
    }
    // CourseStack declares query parameters as `content: {application/json: {schema}}`.
    if let Some(content) = param.get("content").and_then(Value::as_object) {
        if let Some(schema) = content.values().find_map(|m| m.get("schema")) {
            return schema.clone();
        }
    }
    json!({ "type": "string" })
}

fn template_params(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        out.push(rest[start + 1..start + end].to_string());
        rest = &rest[start + end + 1..];
    }
    out
}

/// `content.lessons.list` -> `content_lessons_list`, restricted to the
/// characters MCP clients accept in tool names.
fn sanitize_name(operation_id: &str) -> String {
    let mut out = String::with_capacity(operation_id.len() + 4);
    let mut prev_upper = false;
    for ch in operation_id.chars() {
        if ch.is_ascii_uppercase() {
            // camelCase segment (`courseEnrollments`) -> snake_case.
            if !prev_upper && !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_upper = true;
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_upper = false;
        } else {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            prev_upper = false;
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_spec_parses_and_yields_tools() {
        let spec = load_spec(None).expect("embedded spec parses");
        let tools = build_tools(&spec, false);
        assert!(
            tools.len() > 40,
            "expected the full API surface, got {}",
            tools.len()
        );

        let list = tools
            .iter()
            .find(|t| t.name == "content_lessons_list")
            .expect("content.lessons.list is exposed");
        assert_eq!(list.method, Method::Get);
        assert_eq!(list.path_params, vec!["content_id".to_string()]);

        // Deprecated `/api/enrollments` operations stay hidden by default.
        assert!(!tools.iter().any(|t| t.name == "enrollments_create"));
        assert!(build_tools(&spec, true)
            .iter()
            .any(|t| t.name == "enrollments_create"));
    }

    #[test]
    fn names_are_snake_case() {
        assert_eq!(
            sanitize_name("courseEnrollments.create"),
            "course_enrollments_create"
        );
        assert_eq!(
            sanitize_name("content.files.reupload"),
            "content_files_reupload"
        );
    }

    #[test]
    fn template_params_are_extracted() {
        assert_eq!(
            template_params("/api/content/{content_id}/lessons/{lesson_id}"),
            vec!["content_id".to_string(), "lesson_id".to_string()]
        );
    }

    #[test]
    fn list_endpoints_expose_pagination_arguments() {
        let spec = load_spec(None).unwrap();
        let tools = build_tools(&spec, false);

        let list = tools.iter().find(|t| t.name == "students_list").unwrap();
        assert!(list.paginated);
        assert!(list.input_schema["properties"].get("all_pages").is_some());
        assert!(list.input_schema["properties"].get("max_pages").is_some());

        let retrieve = tools
            .iter()
            .find(|t| t.name == "students_retrieve")
            .unwrap();
        assert!(!retrieve.paginated);
        assert!(retrieve.input_schema["properties"]
            .get("all_pages")
            .is_none());
    }

    #[test]
    fn request_body_becomes_a_body_property() {
        let spec = load_spec(None).unwrap();
        let tools = build_tools(&spec, false);
        let create = tools
            .iter()
            .find(|t| t.name == "course_enrollments_create")
            .unwrap();
        assert!(create.takes_body);
        let props = &create.input_schema["properties"];
        assert!(props.get("body").is_some());
        assert!(props["body"]["properties"].get("emails").is_some());
    }
}

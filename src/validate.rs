use serde_json::{Map, Value};

/// Shallow, best-effort pre-flight check against an OpenAPI schema fragment.
///
/// This is deliberately NOT a full JSON Schema validator: CourseStack is the
/// ground truth. Whenever a schema is too complex to check safely — `oneOf`
/// / `anyOf` / `allOf` (used for discriminated unions like lesson content
/// trees or network configs) — validation is skipped rather than guessed, to
/// avoid rejecting a payload the API would have accepted. It catches the
/// mistakes that are cheap to catch early: a missing required field, a
/// malformed UUID/email, a value outside an enum, a string/array outside its
/// length bounds. Recursion into nested objects is capped at `MAX_DEPTH` for
/// the same reason.
const MAX_DEPTH: u8 = 2;

pub fn validate(schema: &Value, value: &Value, path: &str) -> Vec<String> {
    let mut errors = Vec::new();
    check(schema, value, path, 0, &mut errors);
    errors
}

fn check(schema: &Value, value: &Value, path: &str, depth: u8, errors: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return;
    };

    // Unions are the API's escape hatch for shapes this validator can't
    // safely model — defer to CourseStack rather than risk a false reject.
    if schema.contains_key("oneOf") || schema.contains_key("anyOf") || schema.contains_key("allOf")
    {
        return;
    }

    if value.is_null() {
        return; // absence is handled by the caller's `required` check
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            errors.push(format!(
                "`{path}` must be one of {}, got {value}",
                allowed
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
    }

    let Some(declared) = schema.get("type").and_then(Value::as_str) else {
        return;
    };
    if !type_matches(declared, value) {
        errors.push(format!(
            "`{path}` should be {declared}, got {}",
            kind_of(value)
        ));
        return;
    }

    match (declared, value) {
        ("string", Value::String(s)) => check_string(schema, s, path, errors),
        ("number" | "integer", Value::Number(n)) => check_number(schema, n, path, errors),
        ("array", Value::Array(items)) => check_array(schema, items, path, errors),
        ("object", Value::Object(obj)) if depth < MAX_DEPTH => {
            check_object(schema, obj, path, depth, errors);
        }
        _ => {}
    }
}

fn check_string(schema: &Map<String, Value>, s: &str, path: &str, errors: &mut Vec<String>) {
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
        if (s.chars().count() as u64) < min {
            errors.push(format!("`{path}` must be at least {min} characters"));
        }
    }
    if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
        if (s.chars().count() as u64) > max {
            errors.push(format!("`{path}` must be at most {max} characters"));
        }
    }
    match schema.get("format").and_then(Value::as_str) {
        Some("uuid") if !is_uuid(s) => {
            errors.push(format!("`{path}` must be a UUID, got `{s}`"));
        }
        Some("email") if !is_email(s) => {
            errors.push(format!("`{path}` must be an email address, got `{s}`"));
        }
        _ => {}
    }
}

fn check_number(
    schema: &Map<String, Value>,
    n: &serde_json::Number,
    path: &str,
    errors: &mut Vec<String>,
) {
    let Some(v) = n.as_f64() else { return };
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if v < min {
            errors.push(format!("`{path}` must be >= {min}, got {v}"));
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if v > max {
            errors.push(format!("`{path}` must be <= {max}, got {v}"));
        }
    }
}

fn check_array(schema: &Map<String, Value>, items: &[Value], path: &str, errors: &mut Vec<String>) {
    let len = items.len() as u64;
    if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
        if len < min {
            errors.push(format!(
                "`{path}` must have at least {min} item(s), got {len}"
            ));
        }
    }
    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
        if len > max {
            errors.push(format!(
                "`{path}` must have at most {max} item(s), got {len}"
            ));
        }
    }
    // Item-level checks (e.g. each email in `emails`) are still cheap and safe.
    if let Some(item_schema) = schema.get("items") {
        for (i, item) in items.iter().enumerate() {
            check(
                item_schema,
                item,
                &format!("{path}[{i}]"),
                MAX_DEPTH,
                errors,
            );
        }
    }
}

fn check_object(
    schema: &Map<String, Value>,
    obj: &Map<String, Value>,
    path: &str,
    depth: u8,
    errors: &mut Vec<String>,
) {
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required {
            let Some(name) = field.as_str() else { continue };
            if !obj.contains_key(name) || obj[name].is_null() {
                errors.push(format!("`{path}.{name}` is required"));
            }
        }
    }
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    for (key, field_schema) in properties {
        if let Some(v) = obj.get(key) {
            check(field_schema, v, &format!("{path}.{key}"), depth + 1, errors);
        }
    }
}

fn type_matches(declared: &str, value: &Value) -> bool {
    match declared {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true, // unknown declared type: don't block on it
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|&i| bytes[i] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| [8, 13, 18, 23].contains(&i) || b.is_ascii_hexdigit())
}

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(char::is_whitespace)
        && !domain.contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_fields_are_enforced() {
        let schema = json!({
            "type": "object",
            "required": ["emails", "course_id"],
            "properties": { "emails": {"type": "array"}, "course_id": {"type": "string"} }
        });
        let errors = validate(&schema, &json!({ "emails": [] }), "body");
        assert_eq!(errors, vec!["`body.course_id` is required".to_string()]);
    }

    #[test]
    fn uuid_and_email_formats_are_checked() {
        let schema = json!({
            "type": "object",
            "properties": {
                "course_id": {"type": "string", "format": "uuid"},
                "emails": {"type": "array", "items": {"type": "string", "format": "email"}}
            }
        });
        let errors = validate(
            &schema,
            &json!({ "course_id": "not-a-uuid", "emails": ["ok@example.com", "nope"] }),
            "body",
        );
        assert!(errors
            .iter()
            .any(|e| e.contains("course_id") && e.contains("UUID")));
        assert!(errors
            .iter()
            .any(|e| e.contains("emails[1]") && e.contains("email")));
        assert!(!errors.iter().any(|e| e.contains("emails[0]")));
    }

    #[test]
    fn enum_and_length_bounds_are_checked() {
        let schema = json!({
            "type": "object",
            "properties": {
                "sort_direction": {"type": "string", "enum": ["asc", "desc"]},
                "title": {"type": "string", "minLength": 3, "maxLength": 5}
            }
        });
        let errors = validate(
            &schema,
            &json!({ "sort_direction": "sideways", "title": "ab" }),
            "body",
        );
        assert!(errors.iter().any(|e| e.contains("sort_direction")));
        assert!(errors
            .iter()
            .any(|e| e.contains("title") && e.contains("at least 3")));
    }

    #[test]
    fn discriminated_unions_are_skipped_not_guessed() {
        let schema = json!({
            "oneOf": [
                {"type": "object", "required": ["a"]},
                {"type": "object", "required": ["b"]}
            ]
        });
        assert!(validate(&schema, &json!({}), "body").is_empty());
    }

    #[test]
    fn null_is_allowed_when_present_but_not_required() {
        let schema = json!({
            "type": "object",
            "properties": { "metadata": {"type": "object", "nullable": true} }
        });
        assert!(validate(&schema, &json!({ "metadata": null }), "body").is_empty());
    }

    #[test]
    fn type_mismatch_is_reported() {
        let schema = json!({ "type": "string" });
        let errors = validate(&schema, &json!(42), "body.title");
        assert_eq!(
            errors,
            vec!["`body.title` should be string, got number".to_string()]
        );
    }
}

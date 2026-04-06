use serde_json::Value;

/// Build the context object from trigger data and completed node outputs.
///
/// The resulting JSON has the shape:
/// ```json
/// {
///   "trigger": { "payload": <trigger_data> },
///   "nodes": {
///     "<node_id>": { "output": "<output_text>" },
///     ...
///   }
/// }
/// ```
pub fn build_context(trigger_data: &Value, node_outputs: &[(String, String)]) -> Value {
    let mut nodes = serde_json::Map::new();
    for (node_id, output) in node_outputs {
        nodes.insert(
            node_id.clone(),
            serde_json::json!({
                "output": output,
            }),
        );
    }
    serde_json::json!({
        "trigger": {
            "payload": trigger_data,
        },
        "nodes": Value::Object(nodes),
    })
}

/// Render a template string by replacing `{{path.to.field}}` with values from context.
///
/// - Finds all `{{...}}` patterns
/// - Splits path by `.`
/// - Walks the context JSON tree following each segment
/// - String values are inserted directly, numbers/bools are converted to string,
///   objects/arrays are serialized to JSON
/// - If path resolution fails, the placeholder is left unchanged
pub fn render_template(template: &str, context: &Value) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        // Push everything before the opening braces
        result.push_str(&remaining[..start]);

        let after_open = &remaining[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let path_str = after_open[..end].trim();
            let replacement = resolve_template_path(path_str, context);
            match replacement {
                Some(val) => result.push_str(&val),
                None => {
                    // Leave placeholder unchanged
                    result.push_str("{{");
                    result.push_str(&after_open[..end]);
                    result.push_str("}}");
                }
            }
            remaining = &after_open[end + 2..];
        } else {
            // No closing braces — push rest as-is
            result.push_str(&remaining[start..]);
            remaining = "";
        }
    }

    result.push_str(remaining);
    result
}

/// Resolve a dotted path against a JSON context value.
fn resolve_template_path(path: &str, context: &Value) -> Option<String> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.is_empty() {
        return None;
    }

    let mut current = context;
    for seg in &segments {
        match current {
            Value::Object(map) => {
                current = map.get(*seg)?;
            }
            _ => return None,
        }
    }

    Some(value_to_string(current))
}

/// Convert a JSON value to a string suitable for template insertion.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        // Objects and arrays are serialized to JSON
        other => serde_json::to_string(other).unwrap_or_else(|_| String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_context() {
        let trigger_data = serde_json::json!({"summary": "Fix bug"});
        let node_outputs = vec![
            ("spec".to_string(), "The specification text".to_string()),
            ("impl".to_string(), "Code was written".to_string()),
        ];

        let ctx = build_context(&trigger_data, &node_outputs);

        assert_eq!(ctx["trigger"]["payload"]["summary"], "Fix bug");
        assert_eq!(ctx["nodes"]["spec"]["output"], "The specification text");
        assert_eq!(ctx["nodes"]["impl"]["output"], "Code was written");
    }

    #[test]
    fn test_render_basic_replacement() {
        let ctx = serde_json::json!({
            "trigger": {
                "payload": {
                    "summary": "Fix the login bug"
                }
            }
        });
        let template = "Handle this: {{trigger.payload.summary}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Handle this: Fix the login bug");
    }

    #[test]
    fn test_render_nested_path() {
        let ctx = serde_json::json!({
            "nodes": {
                "spec": {
                    "output": "The specification"
                }
            }
        });
        let template = "Implement: {{nodes.spec.output}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Implement: The specification");
    }

    #[test]
    fn test_render_missing_path_unchanged() {
        let ctx = serde_json::json!({});
        let template = "Missing: {{does.not.exist}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Missing: {{does.not.exist}}");
    }

    #[test]
    fn test_render_multiple_placeholders() {
        let ctx = serde_json::json!({
            "a": "hello",
            "b": "world"
        });
        let template = "{{a}} {{b}}!";
        let result = render_template(template, &ctx);
        assert_eq!(result, "hello world!");
    }

    #[test]
    fn test_render_number_value() {
        let ctx = serde_json::json!({
            "count": 42
        });
        let template = "Count: {{count}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn test_render_bool_value() {
        let ctx = serde_json::json!({
            "ok": true
        });
        let template = "Status: {{ok}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Status: true");
    }

    #[test]
    fn test_render_complex_value_serialized() {
        let ctx = serde_json::json!({
            "data": {"nested": [1, 2, 3]}
        });
        let template = "Data: {{data}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, r#"Data: {"nested":[1,2,3]}"#);
    }

    #[test]
    fn test_render_no_placeholders() {
        let ctx = serde_json::json!({});
        let template = "No placeholders here";
        let result = render_template(template, &ctx);
        assert_eq!(result, "No placeholders here");
    }

    #[test]
    fn test_render_unclosed_braces() {
        let ctx = serde_json::json!({"a": "b"});
        let template = "Open {{ but no close";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Open {{ but no close");
    }

    #[test]
    fn test_render_empty_template() {
        let ctx = serde_json::json!({});
        assert_eq!(render_template("", &ctx), "");
    }

    #[test]
    fn test_render_whitespace_in_braces() {
        let ctx = serde_json::json!({
            "a": "hello"
        });
        // Whitespace inside {{ }} should be trimmed
        let template = "{{ a }}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_render_partial_path_missing() {
        let ctx = serde_json::json!({
            "trigger": {
                "payload": {}
            }
        });
        let template = "{{trigger.payload.missing_field}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "{{trigger.payload.missing_field}}");
    }
}

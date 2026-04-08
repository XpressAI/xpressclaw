use serde_json::Value;
use std::collections::HashMap;

/// Build the context from trigger data, global variables, and step outputs.
pub fn build_context(
    trigger_data: &Value,
    global_vars: &HashMap<String, Value>,
    step_outputs: &HashMap<String, Value>,
) -> Value {
    let mut ctx = serde_json::Map::new();
    ctx.insert(
        "trigger".to_string(),
        serde_json::json!({ "payload": trigger_data }),
    );
    for (k, v) in global_vars {
        ctx.insert(k.clone(), v.clone());
    }
    for (k, v) in step_outputs {
        ctx.insert(k.clone(), v.clone());
    }
    Value::Object(ctx)
}

/// Resolve a variable reference like "@classify.intent" or "trigger.payload.text".
pub fn resolve_variable(expr: &str, context: &Value) -> Option<Value> {
    let path = expr.trim_start_matches('@');
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = context;
    for part in &parts {
        current = current.get(part)?;
    }
    Some(current.clone())
}

/// Render a template, replacing `@var.path` and `{{var.path}}` with values.
///
/// - `@step.field` references are replaced when followed by whitespace, end of
///   string, or punctuation (anything that isn't alphanumeric, `.`, or `_`).
/// - `{{var.path}}` references work the same as before.
/// - If a reference cannot be resolved, the placeholder is left as-is.
pub fn render_template(template: &str, context: &Value) -> String {
    // First pass: handle {{var.path}}
    let after_braces = render_brace_placeholders(template, context);
    // Second pass: handle @var.path
    render_at_references(&after_braces, context)
}

/// Handle `{{var.path}}` replacements.
fn render_brace_placeholders(template: &str, context: &Value) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);

        let after_open = &remaining[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let path_str = after_open[..end].trim();
            // Strip leading @ if present inside braces
            let path_str = path_str.trim_start_matches('@');
            match resolve_variable(path_str, context) {
                Some(val) => result.push_str(&value_to_string(&val)),
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

/// Handle `@var.path` replacements.
///
/// An `@` reference is: `@` followed by a dotted identifier path
/// (alphanumeric, `_`, `.`), terminated by anything else or end of string.
fn render_at_references(template: &str, context: &Value) -> String {
    let mut result = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '@' && i + 1 < len && is_ident_start(chars[i + 1]) {
            // Found potential @reference
            let ref_start = i;
            i += 1; // skip '@'

            // Consume the path: alphanumeric, '_', '.'
            let path_start = i;
            while i < len && is_ident_char(chars[i]) {
                i += 1;
            }
            // Trim trailing dots (e.g., "@foo." at end of sentence)
            let mut path_end = i;
            while path_end > path_start && chars[path_end - 1] == '.' {
                path_end -= 1;
                i -= 1;
            }

            let path: String = chars[path_start..path_end].iter().collect();
            if path.is_empty() {
                // Not a valid reference, emit the '@'
                result.push('@');
                continue;
            }

            match resolve_variable(&path, context) {
                Some(val) => result.push_str(&value_to_string(&val)),
                None => {
                    // Leave as-is
                    let orig: String = chars[ref_start..path_end].iter().collect();
                    result.push_str(&orig);
                }
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// Convert a JSON value to a string suitable for template insertion.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_context() {
        let trigger_data = serde_json::json!({"summary": "Fix bug"});
        let global_vars = HashMap::new();
        let mut step_outputs = HashMap::new();
        step_outputs.insert(
            "spec".to_string(),
            serde_json::json!({"output": "The specification text"}),
        );
        step_outputs.insert(
            "impl".to_string(),
            serde_json::json!({"output": "Code was written"}),
        );

        let ctx = build_context(&trigger_data, &global_vars, &step_outputs);

        assert_eq!(ctx["trigger"]["payload"]["summary"], "Fix bug");
        assert_eq!(ctx["spec"]["output"], "The specification text");
        assert_eq!(ctx["impl"]["output"], "Code was written");
    }

    #[test]
    fn test_build_context_with_global_vars() {
        let trigger_data = serde_json::json!({});
        let mut global_vars = HashMap::new();
        global_vars.insert("default_agent".to_string(), serde_json::json!("atlas"));

        let ctx = build_context(&trigger_data, &global_vars, &HashMap::new());
        assert_eq!(ctx["default_agent"], "atlas");
    }

    #[test]
    fn test_resolve_variable() {
        let ctx = serde_json::json!({
            "trigger": {"payload": {"text": "hello"}},
            "classify": {"intent": "bug"}
        });

        assert_eq!(
            resolve_variable("trigger.payload.text", &ctx),
            Some(serde_json::json!("hello"))
        );
        assert_eq!(
            resolve_variable("@classify.intent", &ctx),
            Some(serde_json::json!("bug"))
        );
        assert!(resolve_variable("nonexistent.path", &ctx).is_none());
    }

    // -- Brace syntax tests --

    #[test]
    fn test_render_brace_basic() {
        let ctx = serde_json::json!({
            "trigger": {"payload": {"summary": "Fix the login bug"}}
        });
        let template = "Handle this: {{trigger.payload.summary}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Handle this: Fix the login bug");
    }

    #[test]
    fn test_render_brace_nested_path() {
        let ctx = serde_json::json!({
            "spec": {"output": "The specification"}
        });
        let template = "Implement: {{spec.output}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Implement: The specification");
    }

    #[test]
    fn test_render_brace_missing_path_unchanged() {
        let ctx = serde_json::json!({});
        let template = "Missing: {{does.not.exist}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Missing: {{does.not.exist}}");
    }

    #[test]
    fn test_render_brace_multiple() {
        let ctx = serde_json::json!({"a": "hello", "b": "world"});
        let template = "{{a}} {{b}}!";
        let result = render_template(template, &ctx);
        assert_eq!(result, "hello world!");
    }

    #[test]
    fn test_render_brace_number() {
        let ctx = serde_json::json!({"count": 42});
        let template = "Count: {{count}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn test_render_brace_bool() {
        let ctx = serde_json::json!({"ok": true});
        let template = "Status: {{ok}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Status: true");
    }

    #[test]
    fn test_render_brace_complex_serialized() {
        let ctx = serde_json::json!({"data": {"nested": [1, 2, 3]}});
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
        let ctx = serde_json::json!({"a": "hello"});
        let template = "{{ a }}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "hello");
    }

    // -- @ syntax tests --

    #[test]
    fn test_render_at_basic() {
        let ctx = serde_json::json!({
            "classify": {"intent": "bug"}
        });
        let template = "The intent is @classify.intent here";
        let result = render_template(template, &ctx);
        assert_eq!(result, "The intent is bug here");
    }

    #[test]
    fn test_render_at_end_of_string() {
        let ctx = serde_json::json!({
            "classify": {"intent": "bug"}
        });
        let template = "Intent: @classify.intent";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Intent: bug");
    }

    #[test]
    fn test_render_at_missing_unchanged() {
        let ctx = serde_json::json!({});
        let template = "Missing @does.not.exist value";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Missing @does.not.exist value");
    }

    #[test]
    fn test_render_at_multiple() {
        let ctx = serde_json::json!({
            "a": {"x": "hello"},
            "b": {"y": "world"}
        });
        let template = "@a.x @b.y!";
        let result = render_template(template, &ctx);
        assert_eq!(result, "hello world!");
    }

    #[test]
    fn test_render_mixed_at_and_braces() {
        let ctx = serde_json::json!({
            "trigger": {"payload": {"text": "hello"}},
            "classify": {"intent": "bug"}
        });
        let template = "Text: {{trigger.payload.text}}, Intent: @classify.intent";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Text: hello, Intent: bug");
    }

    #[test]
    fn test_render_at_with_brace_inside() {
        // @ inside {{}} should also work
        let ctx = serde_json::json!({
            "classify": {"intent": "bug"}
        });
        let template = "Intent: {{@classify.intent}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Intent: bug");
    }

    #[test]
    fn test_render_at_not_followed_by_ident() {
        let ctx = serde_json::json!({});
        let template = "email: user@example.com";
        let result = render_template(template, &ctx);
        // @example.com would be treated as a reference; since it doesn't resolve,
        // it stays as-is
        assert_eq!(result, "email: user@example.com");
    }

    #[test]
    fn test_render_at_number_value() {
        let ctx = serde_json::json!({"step1": {"count": 42}});
        let template = "Count: @step1.count";
        let result = render_template(template, &ctx);
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn test_render_partial_path_missing() {
        let ctx = serde_json::json!({
            "trigger": {"payload": {}}
        });
        let template = "{{trigger.payload.missing_field}}";
        let result = render_template(template, &ctx);
        assert_eq!(result, "{{trigger.payload.missing_field}}");
    }
}

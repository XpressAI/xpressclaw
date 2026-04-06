use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub enum Condition {
    Completed,
    Failed,
    Default,
    Comparison {
        path: Vec<String>,
        op: ComparisonOp,
        value: String,
    },
}

#[derive(Debug, Clone)]
pub enum ComparisonOp {
    Eq,
    NotEq,
    Contains,
    NotContains,
}

/// Parse a condition string into a `Condition`.
///
/// Supported forms:
/// - `"completed"` -> `Condition::Completed`
/// - `"failed"` -> `Condition::Failed`
/// - `"default"` -> `Condition::Default`
/// - `"output.verdict == \"pass\""` -> Comparison with Eq
/// - `"output.verdict != \"pass\""` -> Comparison with NotEq
/// - `"output contains \"error\""` -> Comparison with Contains
/// - `"output not contains \"error\""` -> Comparison with NotContains
pub fn parse(s: &str) -> Result<Condition> {
    let s = s.trim();

    match s {
        "completed" => return Ok(Condition::Completed),
        "failed" => return Ok(Condition::Failed),
        "default" => return Ok(Condition::Default),
        _ => {}
    }

    // Try "not contains" first (before "contains") since it's longer
    if let Some(pos) = s.find(" not contains ") {
        let path_str = &s[..pos];
        let value_str = &s[pos + " not contains ".len()..];
        let path = parse_path(path_str)?;
        let value = strip_quotes(value_str);
        return Ok(Condition::Comparison {
            path,
            op: ComparisonOp::NotContains,
            value,
        });
    }

    if let Some(pos) = s.find(" contains ") {
        let path_str = &s[..pos];
        let value_str = &s[pos + " contains ".len()..];
        let path = parse_path(path_str)?;
        let value = strip_quotes(value_str);
        return Ok(Condition::Comparison {
            path,
            op: ComparisonOp::Contains,
            value,
        });
    }

    // Try != before == since != contains ==
    if let Some(pos) = s.find(" != ") {
        let path_str = &s[..pos];
        let value_str = &s[pos + " != ".len()..];
        let path = parse_path(path_str)?;
        let value = strip_quotes(value_str);
        return Ok(Condition::Comparison {
            path,
            op: ComparisonOp::NotEq,
            value,
        });
    }

    if let Some(pos) = s.find(" == ") {
        let path_str = &s[..pos];
        let value_str = &s[pos + " == ".len()..];
        let path = parse_path(path_str)?;
        let value = strip_quotes(value_str);
        return Ok(Condition::Comparison {
            path,
            op: ComparisonOp::Eq,
            value,
        });
    }

    Err(Error::Workflow(format!(
        "unrecognized condition expression: {s}"
    )))
}

/// Parse a dotted path string into a Vec of segments.
fn parse_path(s: &str) -> Result<Vec<String>> {
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::Workflow("empty path in condition".into()));
    }
    Ok(s.split('.').map(|seg| seg.trim().to_string()).collect())
}

/// Strip surrounding quotes from a value string.
fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Evaluate a condition against a task's status and output.
pub fn evaluate(condition: &Condition, task_status: &str, task_output: &str) -> bool {
    match condition {
        Condition::Completed => task_status == "completed",
        Condition::Failed => task_status == "failed" || task_status == "cancelled",
        Condition::Default => true,
        Condition::Comparison { path, op, value } => {
            let resolved = resolve_path(path, task_output);
            match op {
                ComparisonOp::Eq => resolved == *value,
                ComparisonOp::NotEq => resolved != *value,
                ComparisonOp::Contains => resolved.contains(value.as_str()),
                ComparisonOp::NotContains => !resolved.contains(value.as_str()),
            }
        }
    }
}

/// Resolve a dotted path against the task output.
///
/// If the output is valid JSON, traverse the path starting from the root.
/// The first path segment is expected to be "output" (matching the YAML convention);
/// if present, it is skipped and the remaining segments traverse into the parsed JSON.
/// If the path is just `["output"]`, the entire raw output string is used.
///
/// If JSON parsing fails, the raw output string is used directly.
fn resolve_path(path: &[String], task_output: &str) -> String {
    // If path is just ["output"], return the raw string
    if path.len() == 1 && path[0] == "output" {
        return task_output.to_string();
    }

    // Try to parse as JSON
    let json: serde_json::Value = match serde_json::from_str(task_output) {
        Ok(v) => v,
        Err(_) => return task_output.to_string(),
    };

    // Skip the leading "output" segment if present
    let segments = if !path.is_empty() && path[0] == "output" {
        &path[1..]
    } else {
        path
    };

    let mut current = &json;
    for seg in segments {
        match current {
            serde_json::Value::Object(map) => {
                if let Some(v) = map.get(seg.as_str()) {
                    current = v;
                } else {
                    return task_output.to_string();
                }
            }
            _ => return task_output.to_string(),
        }
    }

    match current {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| task_output.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse tests --

    #[test]
    fn test_parse_completed() {
        let c = parse("completed").unwrap();
        assert!(matches!(c, Condition::Completed));
    }

    #[test]
    fn test_parse_failed() {
        let c = parse("failed").unwrap();
        assert!(matches!(c, Condition::Failed));
    }

    #[test]
    fn test_parse_default() {
        let c = parse("default").unwrap();
        assert!(matches!(c, Condition::Default));
    }

    #[test]
    fn test_parse_eq() {
        let c = parse(r#"output.verdict == "pass""#).unwrap();
        match c {
            Condition::Comparison { path, op, value } => {
                assert_eq!(path, vec!["output", "verdict"]);
                assert!(matches!(op, ComparisonOp::Eq));
                assert_eq!(value, "pass");
            }
            _ => panic!("expected Comparison"),
        }
    }

    #[test]
    fn test_parse_neq() {
        let c = parse(r#"output.status != "error""#).unwrap();
        match c {
            Condition::Comparison { path, op, value } => {
                assert_eq!(path, vec!["output", "status"]);
                assert!(matches!(op, ComparisonOp::NotEq));
                assert_eq!(value, "error");
            }
            _ => panic!("expected Comparison"),
        }
    }

    #[test]
    fn test_parse_contains() {
        let c = parse(r#"output contains "error""#).unwrap();
        match c {
            Condition::Comparison { path, op, value } => {
                assert_eq!(path, vec!["output"]);
                assert!(matches!(op, ComparisonOp::Contains));
                assert_eq!(value, "error");
            }
            _ => panic!("expected Comparison"),
        }
    }

    #[test]
    fn test_parse_not_contains() {
        let c = parse(r#"output not contains "success""#).unwrap();
        match c {
            Condition::Comparison { path, op, value } => {
                assert_eq!(path, vec!["output"]);
                assert!(matches!(op, ComparisonOp::NotContains));
                assert_eq!(value, "success");
            }
            _ => panic!("expected Comparison"),
        }
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse("something weird").is_err());
    }

    #[test]
    fn test_parse_deep_path() {
        let c = parse(r#"output.result.nested.field == "ok""#).unwrap();
        match c {
            Condition::Comparison { path, op, value } => {
                assert_eq!(path, vec!["output", "result", "nested", "field"]);
                assert!(matches!(op, ComparisonOp::Eq));
                assert_eq!(value, "ok");
            }
            _ => panic!("expected Comparison"),
        }
    }

    // -- evaluate tests --

    #[test]
    fn test_evaluate_completed() {
        let c = Condition::Completed;
        assert!(evaluate(&c, "completed", ""));
        assert!(!evaluate(&c, "failed", ""));
        assert!(!evaluate(&c, "in_progress", ""));
    }

    #[test]
    fn test_evaluate_failed() {
        let c = Condition::Failed;
        assert!(evaluate(&c, "failed", ""));
        assert!(evaluate(&c, "cancelled", ""));
        assert!(!evaluate(&c, "completed", ""));
    }

    #[test]
    fn test_evaluate_default() {
        let c = Condition::Default;
        assert!(evaluate(&c, "anything", "anything"));
    }

    #[test]
    fn test_evaluate_eq_json() {
        let c = Condition::Comparison {
            path: vec!["output".into(), "verdict".into()],
            op: ComparisonOp::Eq,
            value: "pass".into(),
        };
        assert!(evaluate(&c, "completed", r#"{"verdict": "pass"}"#));
        assert!(!evaluate(&c, "completed", r#"{"verdict": "fail"}"#));
    }

    #[test]
    fn test_evaluate_neq_json() {
        let c = Condition::Comparison {
            path: vec!["output".into(), "verdict".into()],
            op: ComparisonOp::NotEq,
            value: "pass".into(),
        };
        assert!(!evaluate(&c, "completed", r#"{"verdict": "pass"}"#));
        assert!(evaluate(&c, "completed", r#"{"verdict": "fail"}"#));
    }

    #[test]
    fn test_evaluate_contains_raw_string() {
        let c = Condition::Comparison {
            path: vec!["output".into()],
            op: ComparisonOp::Contains,
            value: "error".into(),
        };
        assert!(evaluate(&c, "completed", "there was an error here"));
        assert!(!evaluate(&c, "completed", "everything is fine"));
    }

    #[test]
    fn test_evaluate_not_contains() {
        let c = Condition::Comparison {
            path: vec!["output".into()],
            op: ComparisonOp::NotContains,
            value: "error".into(),
        };
        assert!(!evaluate(&c, "completed", "there was an error here"));
        assert!(evaluate(&c, "completed", "everything is fine"));
    }

    #[test]
    fn test_evaluate_json_path_not_found() {
        let c = Condition::Comparison {
            path: vec!["output".into(), "missing".into()],
            op: ComparisonOp::Eq,
            value: "something".into(),
        };
        // When path not found in JSON, falls back to raw string
        assert!(!evaluate(&c, "completed", r#"{"verdict": "pass"}"#));
    }

    #[test]
    fn test_evaluate_invalid_json_fallback() {
        let c = Condition::Comparison {
            path: vec!["output".into(), "verdict".into()],
            op: ComparisonOp::Contains,
            value: "hello".into(),
        };
        // Not valid JSON — falls back to raw string comparison
        assert!(evaluate(&c, "completed", "hello world"));
        assert!(!evaluate(&c, "completed", "goodbye world"));
    }

    #[test]
    fn test_evaluate_nested_json() {
        let c = Condition::Comparison {
            path: vec!["output".into(), "result".into(), "status".into()],
            op: ComparisonOp::Eq,
            value: "ok".into(),
        };
        assert!(evaluate(&c, "completed", r#"{"result": {"status": "ok"}}"#));
        assert!(!evaluate(
            &c,
            "completed",
            r#"{"result": {"status": "error"}}"#
        ));
    }

    #[test]
    fn test_evaluate_number_value() {
        let c = Condition::Comparison {
            path: vec!["output".into(), "count".into()],
            op: ComparisonOp::Eq,
            value: "42".into(),
        };
        assert!(evaluate(&c, "completed", r#"{"count": 42}"#));
    }

    #[test]
    fn test_evaluate_bool_value() {
        let c = Condition::Comparison {
            path: vec!["output".into(), "ok".into()],
            op: ComparisonOp::Eq,
            value: "true".into(),
        };
        assert!(evaluate(&c, "completed", r#"{"ok": true}"#));
    }

    #[test]
    fn test_strip_quotes_double() {
        assert_eq!(strip_quotes(r#""hello""#), "hello");
    }

    #[test]
    fn test_strip_quotes_single() {
        assert_eq!(strip_quotes("'hello'"), "hello");
    }

    #[test]
    fn test_strip_quotes_none() {
        assert_eq!(strip_quotes("hello"), "hello");
    }

    #[test]
    fn test_parse_unquoted_value() {
        let c = parse("output.count == 42").unwrap();
        match c {
            Condition::Comparison { value, .. } => {
                assert_eq!(value, "42");
            }
            _ => panic!("expected Comparison"),
        }
    }
}

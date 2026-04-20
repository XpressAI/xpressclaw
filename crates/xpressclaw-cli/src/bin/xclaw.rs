//! The `xclaw` shell-bridge CLI (ADR-023 §7, task 5).
//!
//! Mounted inside every non-MCP harness workspace so the agent can
//! invoke xpressclaw verbs like `xclaw memory add "note"`. Speaks the
//! JSON-over-Unix-socket protocol defined in
//! `xpressclaw_core::xclaw`.
//!
//! Exit codes:
//! - 0 — verb succeeded
//! - 1 — verb failed (server said ok:false); stderr carries the message
//! - 2 — transport error (couldn't connect, bad JSON, etc.)
//! - 3 — usage error (bad argv)

#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use xpressclaw_core::xclaw::{XclawRequest, XclawResponse, XCLAW_AGENT_ENV, XCLAW_SOCKET_ENV};

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("xclaw: Unix-socket bridge is only available on Unix platforms");
    ExitCode::from(2)
}

#[cfg(unix)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&argv[1..]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("xclaw: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(argv: &[&str]) -> Result<ExitCode, String> {
    if argv.is_empty() || argv[0] == "--help" || argv[0] == "-h" {
        print_usage();
        return Ok(ExitCode::from(if argv.is_empty() { 3 } else { 0 }));
    }

    let (verb, args_json) = parse_verb(argv)?;

    let socket = std::env::var(XCLAW_SOCKET_ENV)
        .map_err(|_| format!("{XCLAW_SOCKET_ENV} not set; don't know where to connect"))?;
    let agent_id = std::env::var(XCLAW_AGENT_ENV).ok();

    let request = XclawRequest {
        verb,
        args: args_json,
        agent_id,
    };

    let response =
        roundtrip(&PathBuf::from(socket), &request).map_err(|e| format!("transport: {e}"))?;

    if response.ok {
        if let Some(v) = response.result {
            // Pretty-print structured results. Scalar results surface as
            // bare JSON too — that's fine; callers can `jq -r` if they
            // want to unwrap.
            let body = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
            println!("{body}");
        }
        Ok(ExitCode::from(0))
    } else {
        eprintln!(
            "xclaw: {}",
            response
                .error
                .as_deref()
                .unwrap_or("server reported failure")
        );
        Ok(ExitCode::from(1))
    }
}

/// Parse argv into (verb, args_json).
///
/// Simple verbs are space-separated; the trailing flat key-value pairs
/// become the args object. Example:
///
/// ```text
///   xclaw memory add --content "hi" --tags x,y
///   -> verb = "memory.add", args = {"content":"hi","tags":["x","y"]}
/// ```
///
/// Positional arguments that don't start with `--` are collected under
/// a `_positional` array so verbs can decide how to interpret them.
fn parse_verb(argv: &[&str]) -> Result<(String, serde_json::Value), String> {
    // Walk non-flag tokens to build the verb.
    let mut verb_parts: Vec<&str> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut flags: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut i = 0;
    while i < argv.len() {
        let t = argv[i];
        if let Some(flag) = t.strip_prefix("--") {
            // --key=value or --key value
            if let Some(eq) = flag.find('=') {
                let (k, v) = flag.split_at(eq);
                flags.insert(k.to_string(), parse_scalar(&v[1..]));
                i += 1;
            } else if i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                flags.insert(flag.to_string(), parse_scalar(argv[i + 1]));
                i += 2;
            } else {
                // Bare flag without value — treat as boolean true.
                flags.insert(flag.to_string(), serde_json::Value::Bool(true));
                i += 1;
            }
        } else if verb_parts.is_empty() || (!flags.is_empty() || !positional.is_empty()) {
            // Once flags/positional have started, everything else that
            // isn't a flag is positional.
            if flags.is_empty() && positional.is_empty() {
                verb_parts.push(t);
            } else {
                positional.push(t.to_string());
            }
            i += 1;
        } else {
            verb_parts.push(t);
            i += 1;
        }
    }

    if verb_parts.is_empty() {
        return Err("no verb given".into());
    }

    if !positional.is_empty() {
        flags.insert(
            "_positional".to_string(),
            serde_json::Value::Array(
                positional
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }

    Ok((verb_parts.join("."), serde_json::Value::Object(flags)))
}

/// Parse a raw scalar flag value as the most helpful JSON type.
/// Comma-separated lists become arrays of strings; "true"/"false"
/// become bools; anything parseable as an integer becomes a number.
/// Otherwise it's a string.
fn parse_scalar(raw: &str) -> serde_json::Value {
    if raw == "true" {
        return serde_json::Value::Bool(true);
    }
    if raw == "false" {
        return serde_json::Value::Bool(false);
    }
    if raw.contains(',') {
        return serde_json::Value::Array(
            raw.split(',')
                .map(|s| serde_json::Value::String(s.to_string()))
                .collect(),
        );
    }
    if let Ok(n) = raw.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }
    serde_json::Value::String(raw.to_string())
}

fn roundtrip(socket: &PathBuf, request: &XclawRequest) -> Result<XclawResponse, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    let body = serde_json::to_string(request).map_err(|e| e.to_string())?;
    stream
        .write_all(body.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;

    let mut buf = String::new();
    stream.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    serde_json::from_str(buf.trim_end()).map_err(|e| format!("bad response: {e}; body: {buf}"))
}

fn print_usage() {
    println!("xclaw — xpressclaw shell bridge (ADR-023)");
    println!();
    println!("Usage: xclaw <verb...> [--flag value ...]");
    println!();
    println!("Env:");
    println!("  {XCLAW_SOCKET_ENV}   path to the xclaw unix socket (required)");
    println!("  {XCLAW_AGENT_ENV}   agent id to attribute the call to (optional)");
    println!();
    println!("Common verbs:");
    println!("  xclaw version");
    println!("  xclaw memory add --content \"note text\" [--tags a,b]");
    println!("  xclaw memory list [--limit 20]");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verb_and_positionals_parse() {
        let (verb, args) =
            parse_verb(&["memory", "add", "--content", "hi", "--tags", "a,b"]).unwrap();
        assert_eq!(verb, "memory.add");
        assert_eq!(args["content"], "hi");
        assert_eq!(args["tags"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn equals_form_parses() {
        let (verb, args) = parse_verb(&["memory", "list", "--limit=50"]).unwrap();
        assert_eq!(verb, "memory.list");
        assert_eq!(args["limit"], 50);
    }

    #[test]
    fn bare_flag_is_true() {
        let (_, args) = parse_verb(&["task", "done", "--force"]).unwrap();
        assert_eq!(args["force"], true);
    }

    #[test]
    fn no_args_means_empty_verb_object() {
        let (verb, args) = parse_verb(&["version"]).unwrap();
        assert_eq!(verb, "version");
        assert_eq!(args, serde_json::json!({}));
    }
}

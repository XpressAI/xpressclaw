//! Capability-gated Codex presentation support.
//!
//! OpenAI's primary-runtime artifact skills depend on a desktop-host runtime
//! that is not part of Codex ACP. XpressClaw disables those incompatible
//! skills explicitly and, when the runner image advertises the pinned
//! XpressClaw runtime, publishes a separate skill root through ACP's
//! `additionalDirectories` contract.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::error::{Error, Result};

pub const PRESENTATION_CAPABILITY_LABEL: &str = "io.xpressclaw.presentations";
pub const PRESENTATION_CAPABILITY: &str = "xpressclaw-pptx-v1";
pub const PRESENTATION_RUNTIME_VERSION_LABEL: &str = "io.xpressclaw.presentations.pptxgenjs";
pub const PRESENTATION_RUNTIME_VERSION: &str = "4.0.1";
pub const PRESENTATION_SKILL_ROOT: &str = "/opt/xpressclaw/presentation-runtime";

const CODEX_CONFIG_PREFIX: &str = "CODEX_CONFIG=";
const PRESENTATION_GUIDANCE_MARKER: &str = "[XpressClaw presentation capability]";
const INCOMPATIBLE_SKILLS: [&str; 2] = ["presentations:Presentations", "spreadsheets:Spreadsheets"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationSupport {
    pub available: bool,
    pub additional_directories: Vec<PathBuf>,
}

impl PresentationSupport {
    fn unavailable() -> Self {
        Self {
            available: false,
            additional_directories: Vec::new(),
        }
    }
}

/// Configure one retained Codex process without pretending that OpenAI's
/// desktop-only artifact runtime exists. All user-supplied config is retained;
/// only the known-incompatible upstream skill entries are forced off.
pub fn configure_codex_presentations(
    kind: &str,
    runtime_available: bool,
    environment: &mut Vec<String>,
) -> Result<PresentationSupport> {
    if kind != "codex" {
        return Ok(PresentationSupport::unavailable());
    }

    let existing_index = environment
        .iter()
        .position(|variable| variable.starts_with(CODEX_CONFIG_PREFIX));
    let mut config = match existing_index {
        Some(index) => serde_json::from_str::<Value>(
            &environment[index][CODEX_CONFIG_PREFIX.len()..],
        )
        .map_err(|error| {
            Error::Backend(format!(
                "CODEX_CONFIG must contain valid JSON before XpressClaw can configure presentation skills: {error}"
            ))
        })?,
        None => json!({}),
    };
    let object = config.as_object_mut().ok_or_else(|| {
        Error::Backend(
            "CODEX_CONFIG must be a JSON object before XpressClaw can configure presentation skills"
                .into(),
        )
    })?;

    disable_incompatible_skills(object)?;
    append_guidance(object, runtime_available)?;

    let serialized = serde_json::to_string(&config).map_err(|error| {
        Error::Backend(format!(
            "failed to serialize CODEX_CONFIG with presentation capability guidance: {error}"
        ))
    })?;
    let variable = format!("{CODEX_CONFIG_PREFIX}{serialized}");
    if let Some(index) = existing_index {
        environment[index] = variable;
    } else {
        environment.push(variable);
    }

    Ok(PresentationSupport {
        available: runtime_available,
        additional_directories: runtime_available
            .then(|| PathBuf::from(PRESENTATION_SKILL_ROOT))
            .into_iter()
            .collect(),
    })
}

fn disable_incompatible_skills(config: &mut serde_json::Map<String, Value>) -> Result<()> {
    let skills = config.entry("skills").or_insert_with(|| json!({}));
    let skills = skills
        .as_object_mut()
        .ok_or_else(|| Error::Backend("CODEX_CONFIG.skills must be a JSON object".into()))?;
    let entries = skills.entry("config").or_insert_with(|| json!([]));
    let entries = entries
        .as_array_mut()
        .ok_or_else(|| Error::Backend("CODEX_CONFIG.skills.config must be a JSON array".into()))?;

    for name in INCOMPATIBLE_SKILLS {
        if let Some(entry) = entries.iter_mut().find(|entry| {
            entry
                .as_object()
                .and_then(|entry| entry.get("name"))
                .and_then(Value::as_str)
                == Some(name)
        }) {
            let entry = entry.as_object_mut().expect("matched object entry");
            entry.insert("enabled".into(), Value::Bool(false));
        } else {
            entries.push(json!({ "name": name, "enabled": false }));
        }
    }
    Ok(())
}

fn append_guidance(
    config: &mut serde_json::Map<String, Value>,
    runtime_available: bool,
) -> Result<()> {
    let existing = match config.get("developer_instructions") {
        Some(Value::String(instructions)) => instructions.as_str(),
        Some(Value::Null) | None => "",
        Some(_) => {
            return Err(Error::Backend(
                "CODEX_CONFIG.developer_instructions must be a string".into(),
            ));
        }
    };
    if existing.contains(PRESENTATION_GUIDANCE_MARKER) {
        return Ok(());
    }
    let state = if runtime_available {
        "The OpenAI primary-runtime Presentations and Spreadsheets skills are disabled in this ACP session because their desktop-host artifact runtime is unavailable. For net-new PowerPoint decks, use the separate xpressclaw-presentations skill and its pinned XpressClaw runtime. Do not emulate load_workspace_dependencies or dynamically install an artifact runtime."
    } else {
        "The OpenAI primary-runtime Presentations and Spreadsheets skills are disabled in this ACP session because their desktop-host artifact runtime is unavailable. This runner image does not advertise XpressClaw's presentation capability either. Do not dynamically install a replacement or claim presentation delivery; ask the operator to use a compatible built-in Codex runner image."
    };
    let guidance = format!("{PRESENTATION_GUIDANCE_MARKER}\n{state}");
    let combined = if existing.trim().is_empty() {
        guidance
    } else {
        format!("{existing}\n\n{guidance}")
    };
    config.insert("developer_instructions".into(), Value::String(combined));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(environment: &[String]) -> Value {
        let raw = environment
            .iter()
            .find_map(|variable| variable.strip_prefix(CODEX_CONFIG_PREFIX))
            .unwrap();
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn capable_codex_gets_distinct_skill_root_and_disables_upstream_skills() {
        let mut environment = vec![
            r#"CODEX_CONFIG={"model":"gpt-test","developer_instructions":"Keep it brief.","skills":{"config":[{"name":"presentations:Presentations","enabled":true,"custom":"preserved"},{"name":"other:Skill","enabled":true}]}}"#.into(),
        ];

        let support = configure_codex_presentations("codex", true, &mut environment).unwrap();
        assert!(support.available);
        assert_eq!(
            support.additional_directories,
            vec![PathBuf::from(PRESENTATION_SKILL_ROOT)]
        );
        let config = configured(&environment);
        assert_eq!(config["model"], "gpt-test");
        assert!(config["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("Keep it brief."));
        let entries = config["skills"]["config"].as_array().unwrap();
        let presentation = entries
            .iter()
            .find(|entry| entry["name"] == "presentations:Presentations")
            .unwrap();
        assert_eq!(presentation["enabled"], false);
        assert_eq!(presentation["custom"], "preserved");
        assert!(entries.iter().any(|entry| {
            entry["name"] == "spreadsheets:Spreadsheets" && entry["enabled"] == false
        }));
        assert!(entries
            .iter()
            .any(|entry| entry["name"] == "other:Skill" && entry["enabled"] == true));
    }

    #[test]
    fn missing_runtime_is_actionable_without_advertising_skill_root() {
        let mut environment = Vec::new();
        let support = configure_codex_presentations("codex", false, &mut environment).unwrap();
        assert!(!support.available);
        assert!(support.additional_directories.is_empty());
        let config = configured(&environment);
        assert!(config["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("does not advertise XpressClaw's presentation capability"));
    }

    #[test]
    fn configuration_is_idempotent_and_non_codex_is_untouched() {
        let mut environment = Vec::new();
        configure_codex_presentations("codex", true, &mut environment).unwrap();
        configure_codex_presentations("codex", true, &mut environment).unwrap();
        let config = configured(&environment);
        let instructions = config["developer_instructions"].as_str().unwrap();
        assert_eq!(
            instructions.matches(PRESENTATION_GUIDANCE_MARKER).count(),
            1
        );
        let entries = config["skills"]["config"].as_array().unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["name"] == "presentations:Presentations")
                .count(),
            1
        );

        let mut other = vec!["CODEX_CONFIG={not touched".into()];
        let support = configure_codex_presentations("claude", true, &mut other).unwrap();
        assert!(!support.available);
        assert_eq!(other, vec!["CODEX_CONFIG={not touched"]);
    }

    #[test]
    fn malformed_codex_config_fails_closed() {
        let mut environment = vec!["CODEX_CONFIG=[]".into()];
        let error = configure_codex_presentations("codex", true, &mut environment)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be a JSON object"));

        let mut environment = vec![r#"CODEX_CONFIG={"skills":{"config":{}}}"#.into()];
        let error = configure_codex_presentations("codex", true, &mut environment)
            .unwrap_err()
            .to_string();
        assert!(error.contains("skills.config must be a JSON array"));
    }
}

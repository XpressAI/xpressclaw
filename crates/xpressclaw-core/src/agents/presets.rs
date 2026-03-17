use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Agent template loaded from a YAML file in templates/agents/*/agent.yaml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub icon: String,
    pub role: String,
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default)]
    pub default_tools: Vec<String>,
    #[serde(default)]
    pub default_mcp_servers: HashMap<String, crate::config::McpServerConfig>,
    #[serde(default = "default_llm")]
    pub recommended_llm: String,
}

fn default_backend() -> String {
    "claude-sdk".into()
}

fn default_llm() -> String {
    "local".into()
}

/// Built-in agent templates, embedded at compile time.
static BUILTIN_YAML: &[(&str, &str)] = &[
    (
        "assistant",
        include_str!("../../../../templates/agents/assistant/agent.yaml"),
    ),
    (
        "developer",
        include_str!("../../../../templates/agents/developer/agent.yaml"),
    ),
    (
        "researcher",
        include_str!("../../../../templates/agents/researcher/agent.yaml"),
    ),
    (
        "scheduler",
        include_str!("../../../../templates/agents/scheduler/agent.yaml"),
    ),
];

/// Load all built-in presets.
pub fn builtin_presets() -> Vec<AgentPreset> {
    BUILTIN_YAML
        .iter()
        .filter_map(|(id, yaml)| {
            serde_yaml::from_str::<AgentPreset>(yaml)
                .map_err(|e| tracing::warn!(id, error = %e, "failed to parse agent template"))
                .ok()
        })
        .collect()
}

/// Load presets from a directory (e.g. ~/.xpressclaw/templates/agents/).
/// Each subdirectory should contain an agent.yaml file.
pub fn load_presets_from_dir(dir: &std::path::Path) -> Vec<AgentPreset> {
    let mut presets = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return presets;
    };
    for entry in entries.flatten() {
        let yaml_path = entry.path().join("agent.yaml");
        if yaml_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&yaml_path) {
                match serde_yaml::from_str::<AgentPreset>(&contents) {
                    Ok(preset) => presets.push(preset),
                    Err(e) => tracing::warn!(
                        path = %yaml_path.display(),
                        error = %e,
                        "failed to parse agent template"
                    ),
                }
            }
        }
    }
    presets
}

/// Copy built-in templates to a target directory so users can inspect/customize.
pub fn install_builtin_templates(target_dir: &std::path::Path) {
    for (id, yaml) in BUILTIN_YAML {
        let dir = target_dir.join(id);
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("agent.yaml");
        // Always overwrite with latest built-in version
        std::fs::write(&path, yaml).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_presets_load() {
        let presets = builtin_presets();
        assert!(presets.len() >= 3);
    }

    #[test]
    fn test_preset_ids_unique() {
        let presets = builtin_presets();
        let mut ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), presets.len());
    }

    #[test]
    fn test_preset_has_role() {
        for preset in builtin_presets() {
            assert!(
                !preset.role.is_empty(),
                "preset {} has empty role",
                preset.id
            );
        }
    }

    #[test]
    fn test_developer_has_mcp_servers() {
        let presets = builtin_presets();
        let dev = presets.iter().find(|p| p.id == "developer").unwrap();
        assert!(!dev.default_mcp_servers.is_empty());
        assert!(dev.default_mcp_servers.contains_key("shell"));
    }
}

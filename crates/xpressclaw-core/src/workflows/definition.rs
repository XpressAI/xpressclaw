use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub trigger: Option<WorkflowTrigger>,
    pub nodes: Vec<WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTrigger {
    pub connector: String,
    pub channel: String,
    pub event: String,
    #[serde(default)]
    pub filter: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, rename = "type")]
    pub node_type: Option<String>, // "task" (default), "sink", "router"
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub procedure: Option<String>,
    #[serde(default)]
    pub sinks: Vec<SinkConfig>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub position: NodePosition,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodePosition {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkConfig {
    pub connector: String,
    pub channel: String,
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdge {
    pub from: String,
    pub to: String,
    #[serde(default = "default_condition")]
    pub condition: String, // "completed", "failed", "default", "output.x == y"
    #[serde(default)]
    pub label: Option<String>,
}

fn default_condition() -> String {
    "completed".to_string()
}

impl WorkflowDefinition {
    /// Parse a workflow definition from YAML.
    pub fn parse(yaml: &str) -> Result<Self> {
        let def: WorkflowDefinition = serde_yaml::from_str(yaml)
            .map_err(|e| Error::Workflow(format!("YAML parse error: {e}")))?;
        Ok(def)
    }

    /// Serialize back to YAML.
    pub fn to_yaml(&self) -> Result<String> {
        serde_yaml::to_string(self)
            .map_err(|e| Error::Workflow(format!("YAML serialize error: {e}")))
    }

    /// Validate the definition: all edge from/to reference valid node IDs, at least one node exists.
    pub fn validate(&self) -> Result<()> {
        if self.nodes.is_empty() {
            return Err(Error::Workflow(
                "workflow must have at least one node".into(),
            ));
        }

        let node_ids: std::collections::HashSet<&str> =
            self.nodes.iter().map(|n| n.id.as_str()).collect();

        // Check for duplicate node IDs
        if node_ids.len() != self.nodes.len() {
            return Err(Error::Workflow("duplicate node IDs found".into()));
        }

        for edge in &self.edges {
            if !node_ids.contains(edge.from.as_str()) {
                return Err(Error::Workflow(format!(
                    "edge references unknown source node: {}",
                    edge.from
                )));
            }
            if !node_ids.contains(edge.to.as_str()) {
                return Err(Error::Workflow(format!(
                    "edge references unknown target node: {}",
                    edge.to
                )));
            }
        }

        // Validate all edge conditions parse
        for edge in &self.edges {
            super::condition::parse(&edge.condition)?;
        }

        Ok(())
    }

    /// Find a node by ID.
    pub fn node_by_id(&self, id: &str) -> Option<&WorkflowNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Get all outgoing edges from a given node.
    pub fn outgoing_edges(&self, node_id: &str) -> Vec<&WorkflowEdge> {
        self.edges.iter().filter(|e| e.from == node_id).collect()
    }

    /// Get entry nodes — nodes that have no incoming edges (start points).
    pub fn entry_nodes(&self) -> Vec<&WorkflowNode> {
        let targets: std::collections::HashSet<&str> =
            self.edges.iter().map(|e| e.to.as_str()).collect();
        self.nodes
            .iter()
            .filter(|n| !targets.contains(n.id.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
name: code-review-pipeline
description: "Jira ticket through PM, Dev, Test cycle"
version: 1

trigger:
  connector: jira
  channel: my-project
  event: issue_created
  filter:
    type: Story

nodes:
  - id: spec
    label: "Write Specification"
    agent: pm-agent
    prompt: |
      Write a spec for: {{trigger.payload.summary}}
    position: { x: 250, y: 0 }

  - id: implement
    label: "Implement Feature"
    agent: dev-agent
    prompt: |
      Implement: {{nodes.spec.output}}
    position: { x: 250, y: 150 }

  - id: test
    label: "Run Tests"
    agent: tester-agent
    procedure: run-tests
    position: { x: 250, y: 300 }

  - id: notify
    type: sink
    label: "Send Notification"
    sinks:
      - connector: telegram
        channel: dev-chat
        template: "Pipeline done for: {{trigger.payload.summary}}"

edges:
  - from: spec
    to: implement
    condition: completed

  - from: implement
    to: test
    condition: completed

  - from: test
    to: notify
    condition: completed

  - from: test
    to: implement
    condition: "output.verdict == \"fail\""
    label: "Tests Fail"
"#;

    #[test]
    fn test_parse_sample_yaml() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        assert_eq!(def.name, "code-review-pipeline");
        assert_eq!(def.version, 1);
        assert_eq!(def.nodes.len(), 4);
        assert_eq!(def.edges.len(), 4);
        assert!(def.trigger.is_some());

        let trigger = def.trigger.as_ref().unwrap();
        assert_eq!(trigger.connector, "jira");
        assert_eq!(trigger.event, "issue_created");
    }

    #[test]
    fn test_validate_valid_definition() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        assert!(def.validate().is_ok());
    }

    #[test]
    fn test_validate_no_nodes() {
        let yaml = r#"
name: empty
nodes: []
edges: []
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        assert!(def.validate().is_err());
    }

    #[test]
    fn test_validate_bad_edge_source() {
        let yaml = r#"
name: bad-edge
nodes:
  - id: a
edges:
  - from: nonexistent
    to: a
    condition: completed
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn test_validate_bad_edge_target() {
        let yaml = r#"
name: bad-edge
nodes:
  - id: a
edges:
  - from: a
    to: nonexistent
    condition: completed
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn test_validate_duplicate_node_ids() {
        let yaml = r#"
name: dupes
nodes:
  - id: a
  - id: a
edges: []
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        assert!(def.validate().is_err());
    }

    #[test]
    fn test_node_by_id() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let node = def.node_by_id("spec").unwrap();
        assert_eq!(node.label.as_deref(), Some("Write Specification"));
        assert!(def.node_by_id("nonexistent").is_none());
    }

    #[test]
    fn test_outgoing_edges() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();

        let spec_edges = def.outgoing_edges("spec");
        assert_eq!(spec_edges.len(), 1);
        assert_eq!(spec_edges[0].to, "implement");

        let test_edges = def.outgoing_edges("test");
        assert_eq!(test_edges.len(), 2);

        let notify_edges = def.outgoing_edges("notify");
        assert_eq!(notify_edges.len(), 0);
    }

    #[test]
    fn test_entry_nodes() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let entries = def.entry_nodes();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "spec");
    }

    #[test]
    fn test_to_yaml_roundtrip() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let yaml_out = def.to_yaml().unwrap();
        let def2 = WorkflowDefinition::parse(&yaml_out).unwrap();
        assert_eq!(def.name, def2.name);
        assert_eq!(def.nodes.len(), def2.nodes.len());
        assert_eq!(def.edges.len(), def2.edges.len());
    }

    #[test]
    fn test_default_version() {
        let yaml = r#"
name: minimal
nodes:
  - id: a
edges: []
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        assert_eq!(def.version, 1);
    }

    #[test]
    fn test_default_edge_condition() {
        let yaml = r#"
name: default-cond
nodes:
  - id: a
  - id: b
edges:
  - from: a
    to: b
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        assert_eq!(def.edges[0].condition, "completed");
    }

    #[test]
    fn test_sink_node() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let notify = def.node_by_id("notify").unwrap();
        assert_eq!(notify.node_type.as_deref(), Some("sink"));
        assert_eq!(notify.sinks.len(), 1);
        assert_eq!(notify.sinks[0].connector, "telegram");
    }
}

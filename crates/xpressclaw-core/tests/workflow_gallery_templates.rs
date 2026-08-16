use xpressclaw_core::workflows::definition::WorkflowDefinition;

const PRIMARY_TEMPLATES: [(&str, &str, bool); 6] = [
    (
        "goal-loop",
        include_str!("../../../frontend/src/lib/workflows/templates/goal-loop.yaml"),
        false,
    ),
    (
        "code-review",
        include_str!("../../../frontend/src/lib/workflows/templates/code-review.yaml"),
        false,
    ),
    (
        "repository-caretaker",
        include_str!("../../../frontend/src/lib/workflows/templates/repository-caretaker.yaml"),
        true,
    ),
    (
        "backlog-processor",
        include_str!("../../../frontend/src/lib/workflows/templates/backlog-processor.yaml"),
        true,
    ),
    (
        "requirements-specification",
        include_str!(
            "../../../frontend/src/lib/workflows/templates/requirements-specification.yaml"
        ),
        false,
    ),
    (
        "ui-regression",
        include_str!("../../../frontend/src/lib/workflows/templates/ui-regression.yaml"),
        false,
    ),
];

const BLANK_TEMPLATE: &str =
    include_str!("../../../frontend/src/lib/workflows/templates/blank.yaml");
const IMPLEMENTATION_REVIEW_EXAMPLE: &str =
    include_str!("../../../examples/workflows/implementation-review-loop.yaml");

fn render(source: &str) -> String {
    source
        .replace("__WORKFLOW_NAME__", "\"template-validation\"")
        .replace("__SCHEDULE_AGENT_ID__", "\"template-agent\"")
        .replace("__SCHEDULE_CRON__", "\"0 9 * * 1\"")
}

#[test]
fn every_primary_gallery_template_parses_and_validates() {
    assert_eq!(PRIMARY_TEMPLATES.len(), 6);

    for (id, source, scheduled) in PRIMARY_TEMPLATES {
        let definition = WorkflowDefinition::parse(&render(source))
            .unwrap_or_else(|error| panic!("{id} failed to parse: {error}"));
        definition
            .validate()
            .unwrap_or_else(|error| panic!("{id} failed validation: {error}"));
        assert_eq!(definition.schedule.is_some(), scheduled, "{id} schedule");
        assert!(
            !definition.uses_connector_automation(),
            "{id} must not depend on disabled connector triggers or sinks"
        );
    }
}

#[test]
fn secondary_blank_template_parses_and_validates() {
    let definition = WorkflowDefinition::parse(&render(BLANK_TEMPLATE)).unwrap();
    definition.validate().unwrap();
    assert!(definition.schedule.is_none());
    assert!(!definition.uses_connector_automation());
}

#[test]
fn implementation_review_example_matches_the_current_schema() {
    let definition = WorkflowDefinition::parse(IMPLEMENTATION_REVIEW_EXAMPLE).unwrap();
    definition.validate().unwrap();
    assert!(!definition.uses_connector_automation());
}

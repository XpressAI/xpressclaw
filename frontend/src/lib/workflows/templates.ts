import backlogProcessorYaml from './templates/backlog-processor.yaml?raw';
import blankYaml from './templates/blank.yaml?raw';
import codeReviewYaml from './templates/code-review.yaml?raw';
import goalLoopYaml from './templates/goal-loop.yaml?raw';
import repositoryCaretakerYaml from './templates/repository-caretaker.yaml?raw';
import requirementsSpecificationYaml from './templates/requirements-specification.yaml?raw';
import uiRegressionYaml from './templates/ui-regression.yaml?raw';

export type WorkflowTemplateId =
	| 'goal-loop'
	| 'code-review'
	| 'repository-caretaker'
	| 'backlog-processor'
	| 'requirements-specification'
	| 'ui-regression';

export interface WorkflowTemplateDefinition {
	id: WorkflowTemplateId | 'blank';
	title: string;
	description: string;
	defaultName: string;
	apiDescription: string;
	roleGuidance: string;
	yaml: string;
	schedule?: {
		defaultCron: string;
		description: string;
	};
}

export const WORKFLOW_TEMPLATES: readonly WorkflowTemplateDefinition[] = [
	{
		id: 'goal-loop',
		title: 'Goal loop',
		description: 'Make bounded, verified progress until an Agent reports that the goal is complete.',
		defaultName: 'Goal Loop',
		apiDescription: 'A bounded loop that makes verified progress until its goal is complete.',
		roleGuidance: 'Each run chooses one worker Agent. The same definition can be reused in any Project.',
		yaml: goalLoopYaml,
	},
	{
		id: 'code-review',
		title: 'Implementation + independent review',
		description: 'Implement on a draft PR, loop through an independent review, then wait durably for human feedback.',
		defaultName: 'Code Review Loop',
		apiDescription: 'Implementation and independent review loop using durable agents.',
		roleGuidance: 'Each run binds an implementer and an independent reviewer. A draft PR is their handoff, so the Agents do not need to share a folder.',
		yaml: codeReviewYaml,
	},
	{
		id: 'repository-caretaker',
		title: 'Scheduled repository caretaker',
		description: 'Periodically inspect CI, dependencies, security, and docs, then report healthy, changes, or blocked.',
		defaultName: 'Repository Caretaker',
		apiDescription: 'Scheduled repository health checks with safe, non-publishing maintenance guidance.',
		roleGuidance: 'Choose the Agent whose repository and tools should be used for automatic runs. Manual runs can bind the caretaker role differently.',
		yaml: repositoryCaretakerYaml,
		schedule: {
			defaultCron: '0 9 * * 1',
			description: 'Weekly on Monday at 09:00 in the server\'s local time.',
		},
	},
	{
		id: 'backlog-processor',
		title: 'Periodic issue/backlog processor',
		description: 'Fetch a bounded batch from any configured issue-provider MCP, prioritize it, act or skip safely, and summarize.',
		defaultName: 'Backlog Processor',
		apiDescription: 'Provider-neutral scheduled processing for a bounded issue or backlog batch.',
		roleGuidance: 'Choose an Agent already configured with MCP access to GitHub, Jira, Linear, or another issue source. The workflow does not assume a vendor or connector trigger.',
		yaml: backlogProcessorYaml,
		schedule: {
			defaultCron: '0 9 * * 1-5',
			description: 'Every weekday at 09:00 in the server\'s local time.',
		},
	},
	{
		id: 'requirements-specification',
		title: 'Requirements → detailed specification',
		description: 'Gather context, draft a design, challenge assumptions independently, and produce an actionable specification.',
		defaultName: 'Detailed Specification',
		apiDescription: 'Requirements analysis and independent challenge leading to a detailed specification.',
		roleGuidance: 'Each run binds a drafter and an independent challenger. They may use repository context or connected knowledge, but the workflow also works for non-code projects.',
		yaml: requirementsSpecificationYaml,
	},
	{
		id: 'ui-regression',
		title: 'UI regression tester',
		description: 'Exercise scoped browser flows, capture evidence, classify findings, and optionally make one safe fix and retest pass.',
		defaultName: 'UI Regression Test',
		apiDescription: 'Evidence-based browser regression testing with an optional safe fix and retest pass.',
		roleGuidance: 'Choose a browser-capable Agent. The same role runs the test, optional local fix, and retest so the workspace and evidence stay coherent.',
		yaml: uiRegressionYaml,
	},
] as const;

export const BLANK_WORKFLOW_TEMPLATE: WorkflowTemplateDefinition = {
	id: 'blank',
	title: 'Start blank',
	description: 'Create one executable step and extend it in the visual editor.',
	defaultName: 'New Workflow',
	apiDescription: 'A reusable agent workflow.',
	roleGuidance: 'Each run chooses one worker Agent, so this definition can be reused in any Project.',
	yaml: blankYaml,
};

export function workflowTemplate(id: WorkflowTemplateDefinition['id']): WorkflowTemplateDefinition {
	if (id === 'blank') return BLANK_WORKFLOW_TEMPLATE;
	const definition = WORKFLOW_TEMPLATES.find((candidate) => candidate.id === id);
	if (!definition) throw new Error(`Unknown workflow template: ${id}`);
	return definition;
}

export function uniqueWorkflowName(base: string, existingNames: ReadonlySet<string>): string {
	if (!existingNames.has(base)) return base;
	let suffix = 2;
	while (existingNames.has(`${base} ${suffix}`)) suffix += 1;
	return `${base} ${suffix}`;
}

export function renderWorkflowTemplate(
	definition: WorkflowTemplateDefinition,
	name: string,
	options: { scheduleAgentId?: string; scheduleCron?: string } = {},
): string {
	const slug = name.trim().toLowerCase().replace(/\s+/g, '-');
	let yaml = definition.yaml.replaceAll('__WORKFLOW_NAME__', JSON.stringify(slug));
	if (!definition.schedule) return yaml;

	const scheduleAgentId = options.scheduleAgentId?.trim();
	if (!scheduleAgentId) throw new Error('Choose an Agent for automatic runs.');
	const scheduleCron = options.scheduleCron?.trim() || definition.schedule.defaultCron;
	yaml = yaml
		.replaceAll('__SCHEDULE_AGENT_ID__', JSON.stringify(scheduleAgentId))
		.replaceAll('__SCHEDULE_CRON__', JSON.stringify(scheduleCron));
	return yaml;
}

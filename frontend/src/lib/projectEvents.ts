import type { Project } from '$lib/api';

export const PROJECT_MUTATION_EVENT = 'xpressclaw:project-mutation';

export type ProjectMutation =
	| { kind: 'updated'; project: Project }
	| { kind: 'deleted'; projectId: string };

export function publishProjectMutation(mutation: ProjectMutation): void {
	window.dispatchEvent(new CustomEvent<ProjectMutation>(PROJECT_MUTATION_EVENT, { detail: mutation }));
}

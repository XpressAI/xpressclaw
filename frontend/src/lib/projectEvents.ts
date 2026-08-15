import { ApiError, projects as projectsApi, type Project } from '$lib/api';

export const PROJECT_MUTATION_EVENT = 'xpressclaw:project-mutation';
const PROJECT_MUTATION_CHANNEL = 'xpressclaw:project-mutations:v1';
const PROJECT_MUTATION_STORAGE_KEY = 'xpressclaw.project-mutation.v1';

export type ProjectMutation =
	| { kind: 'updated'; project: Project; authoritative?: boolean }
	| { kind: 'deleted'; projectId: string };

const textEncoder = new TextEncoder();

function compareSqliteNoCase(left: string, right: string): number {
	const encode = (value: string) => textEncoder.encode(value.replace(/[A-Z]/g, (character) => character.toLowerCase()));
	const leftBytes = encode(left);
	const rightBytes = encode(right);
	for (let index = 0; index < Math.min(leftBytes.length, rightBytes.length); index += 1) {
		if (leftBytes[index] !== rightBytes[index]) return leftBytes[index] - rightBytes[index];
	}
	return leftBytes.length - rightBytes.length;
}

export function sortProjectsByRecency(projects: Project[]): Project[] {
	return [...projects].sort((left, right) => {
		if (left.updated_at !== right.updated_at) return left.updated_at < right.updated_at ? 1 : -1;
		return compareSqliteNoCase(left.name, right.name);
	});
}

interface ProjectMutationEnvelope {
	mutation: ProjectMutation;
}

let mutationChannel: BroadcastChannel | null = null;
const projectRefreshVersions = new Map<string, number>();
const deletedProjectIds = new Set<string>();

function isProjectMutation(value: unknown): value is ProjectMutation {
	if (!value || typeof value !== 'object') return false;
	const mutation = value as Record<string, unknown>;
	if (mutation.kind === 'deleted') return typeof mutation.projectId === 'string';
	if (mutation.kind !== 'updated' || !mutation.project || typeof mutation.project !== 'object') return false;
	const project = mutation.project as Record<string, unknown>;
	return typeof project.id === 'string' && typeof project.name === 'string';
}

function isProjectMutationEnvelope(value: unknown): value is ProjectMutationEnvelope {
	if (!value || typeof value !== 'object') return false;
	const envelope = value as Record<string, unknown>;
	return isProjectMutation(envelope.mutation);
}

function dispatchProjectMutation(mutation: ProjectMutation): void {
	if (mutation.kind === 'deleted') {
		deletedProjectIds.add(mutation.projectId);
		projectRefreshVersions.set(mutation.projectId, (projectRefreshVersions.get(mutation.projectId) ?? 0) + 1);
		window.dispatchEvent(new CustomEvent<ProjectMutation>(PROJECT_MUTATION_EVENT, { detail: mutation }));
		return;
	}

	const projectId = mutation.project.id;
	if (!mutation.authoritative && deletedProjectIds.has(projectId)) {
		void refreshUpdatedProject(projectId);
		return;
	}
	if (mutation.authoritative) deletedProjectIds.delete(projectId);
	window.dispatchEvent(new CustomEvent<ProjectMutation>(PROJECT_MUTATION_EVENT, { detail: mutation }));
	if (!mutation.authoritative) void refreshUpdatedProject(projectId);
}

async function refreshUpdatedProject(projectId: string): Promise<void> {
	const refreshVersion = (projectRefreshVersions.get(projectId) ?? 0) + 1;
	projectRefreshVersions.set(projectId, refreshVersion);
	try {
		const project = await projectsApi.get(projectId);
		if (projectRefreshVersions.get(projectId) !== refreshVersion) return;
		dispatchProjectMutation({ kind: 'updated', project, authoritative: true });
	} catch (cause) {
		if (projectRefreshVersions.get(projectId) !== refreshVersion) return;
		if (cause instanceof ApiError && cause.status === 404) {
			dispatchProjectMutation({ kind: 'deleted', projectId });
		}
	}
}

function receiveProjectMutation(value: unknown): void {
	if (!isProjectMutationEnvelope(value)) return;
	dispatchProjectMutation(value.mutation);
}

function startProjectMutationBridge(): void {
	if (typeof window === 'undefined') return;
	if ('BroadcastChannel' in window) {
		try {
			mutationChannel = new BroadcastChannel(PROJECT_MUTATION_CHANNEL);
			mutationChannel.addEventListener('message', (event) => receiveProjectMutation(event.data));
		} catch {}
	}
	if (mutationChannel) return;

	// Some embedded webviews expose BroadcastChannel without permitting it for
	// their origin. Use the shared local storage event as the fallback transport.
	window.addEventListener('storage', (event) => {
		if (event.key !== PROJECT_MUTATION_STORAGE_KEY || !event.newValue) return;
		try {
			receiveProjectMutation(JSON.parse(event.newValue));
		} catch {}
	});
}

startProjectMutationBridge();

export function publishProjectMutation(mutation: ProjectMutation): void {
	const envelope: ProjectMutationEnvelope = { mutation };
	dispatchProjectMutation(mutation);
	if (mutationChannel) {
		mutationChannel.postMessage(envelope);
		return;
	}
	try {
		localStorage.setItem(PROJECT_MUTATION_STORAGE_KEY, JSON.stringify(envelope));
		localStorage.removeItem(PROJECT_MUTATION_STORAGE_KEY);
	} catch {}
}

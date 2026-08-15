import type { Project } from '$lib/api';

export const PROJECT_MUTATION_EVENT = 'xpressclaw:project-mutation';
const PROJECT_MUTATION_CHANNEL = 'xpressclaw:project-mutations:v1';
const PROJECT_MUTATION_STORAGE_KEY = 'xpressclaw.project-mutation.v1';

export type ProjectMutation =
	| { kind: 'updated'; project: Project }
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
		const updatedAtDifference = Date.parse(right.updated_at) - Date.parse(left.updated_at);
		if (Number.isFinite(updatedAtDifference) && updatedAtDifference !== 0) return updatedAtDifference;
		return compareSqliteNoCase(left.name, right.name);
	});
}

interface ProjectMutationEnvelope {
	mutation: ProjectMutation;
}

let mutationChannel: BroadcastChannel | null = null;

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
	window.dispatchEvent(new CustomEvent<ProjectMutation>(PROJECT_MUTATION_EVENT, { detail: mutation }));
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

const STORAGE_PREFIX = 'xpressclaw.dismissed-task-attempt.v2.';
const LEGACY_PREFIX = 'xpressclaw.dismissed-task-attempt.v1.';
const CHANGE_EVENT = 'xpressclaw:task-error-dismissed';
const dismissedAttempts = new Map<string, Set<string>>();

function load(taskId: string): Set<string> {
	const attempts = new Set(dismissedAttempts.get(taskId));
	try {
		const legacy = localStorage.getItem(`${LEGACY_PREFIX}${taskId}`);
		if (legacy) attempts.add(legacy);
		const stored: unknown = JSON.parse(localStorage.getItem(`${STORAGE_PREFIX}${taskId}`) ?? '[]');
		if (Array.isArray(stored)) {
			for (const id of stored) if (typeof id === 'string' && id) attempts.add(id);
		}
	} catch {}
	dismissedAttempts.set(taskId, attempts);
	return attempts;
}

export function rememberTaskAttemptDismissal(taskId: string, attemptId: string): void {
	const attempts = load(taskId);
	attempts.add(attemptId);
	try { localStorage.setItem(`${STORAGE_PREFIX}${taskId}`, JSON.stringify([...attempts])); } catch {}
	window.dispatchEvent(new CustomEvent(CHANGE_EVENT, { detail: { taskId } }));
}

export function subscribeTaskErrorDismissal(taskId: string, update: (attempts: ReadonlySet<string>) => void): () => void {
	update(load(taskId));
	const onDismiss = (event: Event) => {
		if ((event as CustomEvent<{ taskId: string }>).detail.taskId === taskId) update(load(taskId));
	};
	const onStorage = (event: StorageEvent) => {
		if (event.key === `${STORAGE_PREFIX}${taskId}` || event.key === `${LEGACY_PREFIX}${taskId}` || event.key === null) update(load(taskId));
	};
	window.addEventListener(CHANGE_EVENT, onDismiss);
	window.addEventListener('storage', onStorage);
	return () => {
		window.removeEventListener(CHANGE_EVENT, onDismiss);
		window.removeEventListener('storage', onStorage);
	};
}

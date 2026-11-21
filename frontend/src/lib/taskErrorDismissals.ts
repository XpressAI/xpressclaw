const STORAGE_PREFIX = 'xpressclaw.dismissed-task-attempt.v1.';
const CHANGE_EVENT = 'xpressclaw:task-error-dismissed';
const dismissedAttempts = new Map<string, string>();

function load(taskId: string): string | null {
	try {
		return dismissedAttempts.get(taskId) ?? localStorage.getItem(`${STORAGE_PREFIX}${taskId}`);
	} catch {
		return dismissedAttempts.get(taskId) ?? null;
	}
}

export function dismissTaskAttemptError(taskId: string, attemptId: string): void {
	dismissedAttempts.set(taskId, attemptId);
	try { localStorage.setItem(`${STORAGE_PREFIX}${taskId}`, attemptId); } catch {}
	window.dispatchEvent(new CustomEvent(CHANGE_EVENT, { detail: { taskId, attemptId } }));
}

export function subscribeTaskErrorDismissal(taskId: string, update: (attemptId: string | null) => void): () => void {
	update(load(taskId));
	const onDismiss = (event: Event) => {
		const detail = (event as CustomEvent<{ taskId: string; attemptId: string }>).detail;
		if (detail.taskId === taskId) update(detail.attemptId);
	};
	const onStorage = (event: StorageEvent) => {
		if (event.key === `${STORAGE_PREFIX}${taskId}` || event.key === null) {
			dismissedAttempts.delete(taskId);
			update(load(taskId));
		}
	};
	window.addEventListener(CHANGE_EVENT, onDismiss);
	window.addEventListener('storage', onStorage);
	return () => {
		window.removeEventListener(CHANGE_EVENT, onDismiss);
		window.removeEventListener('storage', onStorage);
	};
}

const DRAFT_STORAGE_PREFIX = 'xpressclaw.composer-draft.v1.';
const TARGET_STORAGE_PREFIX = 'xpressclaw.composer-target.v1.';

function draftStorage(): Storage | null {
	try {
		return typeof localStorage === 'undefined' ? null : localStorage;
	} catch {
		return null;
	}
}

function storageKey(scope: string): string {
	return `${DRAFT_STORAGE_PREFIX}${scope}`;
}

export function loadComposerDraft(scope: string): string {
	try {
		return draftStorage()?.getItem(storageKey(scope)) ?? '';
	} catch {
		return '';
	}
}

export function saveComposerDraft(scope: string, content: string): void {
	try {
		const storage = draftStorage();
		if (!storage) return;
		if (content) storage.setItem(storageKey(scope), content);
		else storage.removeItem(storageKey(scope));
	} catch {
		// Draft persistence is best-effort when browser storage is unavailable.
	}
}

export function clearComposerDraft(scope: string): void {
	try {
		draftStorage()?.removeItem(storageKey(scope));
	} catch {
		// Draft persistence is best-effort when browser storage is unavailable.
	}
}

export function loadComposerTarget(scope: string): string {
	try {
		return draftStorage()?.getItem(`${TARGET_STORAGE_PREFIX}${scope}`) ?? '';
	} catch {
		return '';
	}
}

export function saveComposerTarget(scope: string, target: string): void {
	try {
		const storage = draftStorage();
		if (!storage) return;
		const key = `${TARGET_STORAGE_PREFIX}${scope}`;
		if (target) storage.setItem(key, target);
		else storage.removeItem(key);
	} catch {
		// Target persistence is best-effort when browser storage is unavailable.
	}
}

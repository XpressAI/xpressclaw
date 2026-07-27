export type ThemePreference = 'light' | 'dark' | 'system';
export type ResolvedTheme = Exclude<ThemePreference, 'system'>;

export const THEME_STORAGE_KEY = 'xpressclaw.theme';
export const DEFAULT_THEME: ThemePreference = 'system';

export function isThemePreference(value: unknown): value is ThemePreference {
	return value === 'light' || value === 'dark' || value === 'system';
}

export function getThemePreference(): ThemePreference {
	if (typeof window === 'undefined') return DEFAULT_THEME;
	try {
		const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
		return isThemePreference(stored) ? stored : DEFAULT_THEME;
	} catch {
		return DEFAULT_THEME;
	}
}

export function resolveTheme(
	preference: ThemePreference,
	systemPrefersDark = typeof window !== 'undefined'
		&& window.matchMedia('(prefers-color-scheme: dark)').matches,
): ResolvedTheme {
	if (preference === 'system') return systemPrefersDark ? 'dark' : 'light';
	return preference;
}

export function applyTheme(
	preference: ThemePreference,
	systemPrefersDark = typeof window !== 'undefined'
		&& window.matchMedia('(prefers-color-scheme: dark)').matches,
): ResolvedTheme {
	const resolved = resolveTheme(preference, systemPrefersDark);
	if (typeof document !== 'undefined') {
		document.documentElement.classList.toggle('dark', resolved === 'dark');
		document.documentElement.dataset.theme = preference;
	}
	return resolved;
}

export function setThemePreference(preference: ThemePreference): ResolvedTheme {
	if (typeof window !== 'undefined') {
		try {
			window.localStorage.setItem(THEME_STORAGE_KEY, preference);
		} catch {
			// The theme still applies for this page when storage is unavailable.
		}
	}
	return applyTheme(preference);
}

export function initializeTheme(): () => void {
	if (typeof window === 'undefined') return () => {};

	const systemTheme = window.matchMedia('(prefers-color-scheme: dark)');
	const syncTheme = () => applyTheme(getThemePreference(), systemTheme.matches);
	const syncStoredTheme = (event: StorageEvent) => {
		if (event.key === THEME_STORAGE_KEY || event.key === null) syncTheme();
	};

	syncTheme();
	if (typeof systemTheme.addEventListener === 'function') {
		systemTheme.addEventListener('change', syncTheme);
	} else {
		systemTheme.addListener(syncTheme);
	}
	window.addEventListener('storage', syncStoredTheme);

	return () => {
		if (typeof systemTheme.removeEventListener === 'function') {
			systemTheme.removeEventListener('change', syncTheme);
		} else {
			systemTheme.removeListener(syncTheme);
		}
		window.removeEventListener('storage', syncStoredTheme);
	};
}

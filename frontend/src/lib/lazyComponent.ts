import type { Component } from 'svelte';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type AnyLazyComponent = Component<any>;

type ModuleLike = { default?: AnyLazyComponent } | AnyLazyComponent;

let retryNonceSeq = 0;

function extractComponent(mod: unknown): AnyLazyComponent | null {
	if (typeof mod === 'function') return mod as AnyLazyComponent;
	if (!mod || typeof mod !== 'object') return null;
	const record = mod as Record<string, unknown>;
	if (typeof record.default === 'function') {
		return record.default as AnyLazyComponent;
	}
	for (const value of Object.values(record)) {
		if (
			value &&
			typeof value === 'object' &&
			typeof (value as Record<string, unknown>).default === 'function'
		) {
			return (value as { default: AnyLazyComponent }).default;
		}
	}
	return null;
}

function extractSameOriginUrl(err: unknown): URL | null {
	if (typeof window === 'undefined') return null;
	const message = err instanceof Error ? `${err.message}\n${String(err)}` : String(err);
	const match = /https?:\/\/[^\s'")]+/.exec(message);
	if (!match) return null;
	try {
		const parsed = new URL(match[0]);
		if (parsed.origin !== window.location.origin) return null;
		parsed.searchParams.delete('retry');
		return parsed;
	} catch {
		return null;
	}
}

async function reloadStylesheet(cssHref: string, nonce: number): Promise<void> {
	if (typeof document === 'undefined') return;
	const busted = new URL(cssHref);
	busted.searchParams.set('retry', String(nonce));
	const existing = Array.from(
		document.head.querySelectorAll<HTMLLinkElement>('link[rel="stylesheet"]')
	).find((link) => {
		if (!link.href) return false;
		try {
			const parsed = new URL(link.href, window.location.origin);
			parsed.searchParams.delete('retry');
			return parsed.toString() === cssHref;
		} catch {
			return false;
		}
	});
	if (existing && existing.sheet !== null) return;

	const link = existing
		? (existing.cloneNode(false) as HTMLLinkElement)
		: document.createElement('link');
	link.rel = 'stylesheet';
	link.crossOrigin = '';
	link.href = busted.toString();

	await new Promise<void>((resolve, reject) => {
		link.addEventListener('load', () => resolve(), { once: true });
		link.addEventListener(
			'error',
			() => reject(new Error(`Unable to preload CSS for ${cssHref}`)),
			{ once: true }
		);
		if (existing) {
			existing.replaceWith(link);
		} else {
			document.head.appendChild(link);
		}
	});
}

export function createLazyComponentRegistry<K extends string>(
	loaders: Record<K, () => Promise<ModuleLike>>
) {
	const resolved = new Map<K, AnyLazyComponent>();
	const inflight = new Map<K, Promise<AnyLazyComponent>>();
	const failedModuleUrls = new Map<K, string>();
	const failedStylesheetUrls = new Map<K, string>();
	const failedKeys = new Set<K>();

	function has(key: K): boolean {
		return resolved.has(key);
	}

	function get(key: K): AnyLazyComponent | null {
		return resolved.get(key) ?? null;
	}

	function load(key: K): Promise<AnyLazyComponent> {
		const cached = resolved.get(key);
		if (cached) return Promise.resolve(cached);
		const existing = inflight.get(key);
		if (existing) return existing;

		const loader = loaders[key];
		if (!loader) {
			return Promise.reject(new Error(`Unknown lazy component key: ${String(key)}`));
		}

		const hadPriorFailure = failedKeys.has(key);
		let pending: Promise<AnyLazyComponent>;
		pending = (async () => {
			try {
				const priorCssUrl = failedStylesheetUrls.get(key);
				if (hadPriorFailure && priorCssUrl) {
					retryNonceSeq += 1;
					await reloadStylesheet(priorCssUrl, retryNonceSeq);
					failedStylesheetUrls.delete(key);
				}
				const loaded = await loader();
				const component = extractComponent(loaded);
				if (!component) {
					throw new Error(`Lazy module for ${String(key)} did not export a component`);
				}
				resolved.set(key, component);
				failedKeys.delete(key);
				failedModuleUrls.delete(key);
				failedStylesheetUrls.delete(key);
				return component;
			} catch (err) {
				const detected = extractSameOriginUrl(err);
				const detectedCssUrl =
					detected && detected.pathname.endsWith('.css') ? detected.toString() : null;
				const detectedModuleUrl =
					detected && !detected.pathname.endsWith('.css') ? detected.toString() : null;
				const previousModuleUrl = failedModuleUrls.get(key);
				const candidateModuleUrl = detectedModuleUrl ?? previousModuleUrl ?? null;

				failedKeys.add(key);
				if (detectedCssUrl) {
					failedStylesheetUrls.set(key, detectedCssUrl);
				}
				if (detectedModuleUrl) {
					failedModuleUrls.set(key, detectedModuleUrl);
				}

				// Browsers permanently record failed ES module fetches as `null` in Document.moduleMap
				// for that exact URL. On retry after a prior module failure, re-fetch with a cache-busting
				// query parameter on the same-origin module URL so recovery works without a page reload.
				if (hadPriorFailure && !detectedCssUrl && candidateModuleUrl) {
					retryNonceSeq += 1;
					const busted = new URL(candidateModuleUrl);
					busted.searchParams.set('retry', String(retryNonceSeq));
					const retriedMod = await import(/* @vite-ignore */ busted.toString());
					const component = extractComponent(retriedMod);
					if (!component) {
						throw err;
					}
					resolved.set(key, component);
					failedKeys.delete(key);
					failedModuleUrls.delete(key);
					failedStylesheetUrls.delete(key);
					return component;
				}
				throw err;
			}
		})().finally(() => {
			if (inflight.get(key) === pending) {
				inflight.delete(key);
			}
		});

		inflight.set(key, pending);
		return pending;
	}

	return { has, get, load };
}

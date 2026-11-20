import type { MessageVisualization } from '$lib/api';

const REFERENCE_START = 'visualize';
const REFERENCE_END = '';
const MAX_VISUALIZATIONS_PER_MESSAGE = 8;
const CONTROL_CHARACTER = /[\u0000-\u001F\u007F-\u009F]/;

export type MessageVisualizationBlock =
	| { kind: 'text'; content: string }
	| {
		kind: 'visualization';
		referenceIndex: number;
		path: string;
		title: string;
		mode: 'normal' | 'wide';
		artifact: MessageVisualization | null;
	};

interface VisualizationReferencePayload {
	path: string;
	title?: string;
	mode?: 'wide';
}

function isEscaped(content: string, index: number): boolean {
	let slashes = 0;
	for (let cursor = index - 1; cursor >= 0 && content[cursor] === '\\'; cursor -= 1) slashes += 1;
	return slashes % 2 === 1;
}

function parsePayload(raw: string): VisualizationReferencePayload | null {
	let value: unknown;
	try {
		value = JSON.parse(raw);
	} catch {
		return null;
	}
	if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
	const payload = value as Record<string, unknown>;
	if (typeof payload.path !== 'string') return null;
	const path = payload.path.trim();
	if (!path || new TextEncoder().encode(path).byteLength > 4096 || CONTROL_CHARACTER.test(path)) return null;
	if (payload.title !== undefined && (typeof payload.title !== 'string' || [...payload.title].length > 250 || CONTROL_CHARACTER.test(payload.title))) return null;
	if (payload.mode !== undefined && payload.mode !== 'wide') return null;
	const title = typeof payload.title === 'string' ? payload.title.trim() : undefined;
	return { path, ...(title ? { title } : {}), ...(payload.mode === 'wide' ? { mode: 'wide' as const } : {}) };
}

function fallbackTitle(path: string): string {
	const name = path.replaceAll('\\', '/').split('/').at(-1)?.replace(/\.html?$/i, '').trim();
	return name ? [...name].slice(0, 250).join('') : 'Visualization';
}

/**
 * Split only assistant-authored exact Codex references. Invalid, escaped,
 * lookalike, and user-authored markers remain ordinary source text and pass
 * through the existing escaped Markdown renderer.
 */
export function splitMessageVisualizations(
	content: string,
	role: string,
	visualizations: MessageVisualization[] = [],
): MessageVisualizationBlock[] {
	if (role !== 'assistant') return [{ kind: 'text', content }];

	const artifacts = new Map(visualizations.map((artifact) => [artifact.reference_index, artifact]));
	const blocks: MessageVisualizationBlock[] = [];
	let cursor = 0;
	let textStart = 0;
	let referenceIndex = 0;
	while (cursor < content.length) {
		const start = content.indexOf(REFERENCE_START, cursor);
		if (start < 0) break;
		const payloadStart = start + REFERENCE_START.length;
		const end = content.indexOf(REFERENCE_END, payloadStart);
		if (end < 0) break;
		cursor = end + REFERENCE_END.length;
		if (isEscaped(content, start)) continue;
		const payload = parsePayload(content.slice(payloadStart, end));
		if (!payload) continue;

		if (start > textStart) blocks.push({ kind: 'text', content: content.slice(textStart, start) });
		const artifact = artifacts.get(referenceIndex) ?? null;
		blocks.push({
			kind: 'visualization',
			referenceIndex,
			path: payload.path,
			title: artifact?.title || payload.title || fallbackTitle(payload.path),
			mode: artifact?.mode ?? payload.mode ?? 'normal',
			artifact,
		});
		referenceIndex += 1;
		textStart = cursor;
		if (referenceIndex === MAX_VISUALIZATIONS_PER_MESSAGE) break;
	}
	if (textStart < content.length) blocks.push({ kind: 'text', content: content.slice(textStart) });
	return blocks.length ? blocks : [{ kind: 'text', content }];
}

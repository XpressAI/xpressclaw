import { writable } from 'svelte/store';
import { request, requestResponse, settings, type SpeechSettings } from '$lib/api';
import { renderContent } from '$lib/formatMessage';
import { splitMessageVisualizations } from '$lib/messageVisualizations';

export const speechSettings = writable<SpeechSettings | null>(null);
let settingsRequest: Promise<void> | undefined;

export function loadSpeechSettings(): Promise<void> {
	return settingsRequest ??= settings.getSpeech()
		.then((value) => { speechSettings.set(value); })
		.catch(() => { settingsRequest = undefined; });
}

export async function transcribeAudio(audio: Blob, signal: AbortSignal): Promise<string> {
	const file = new FormData();
	const extension = audio.type.includes('mp4') ? 'mp4' : audio.type.includes('ogg') ? 'ogg' : 'webm';
	file.append('file', audio, `recording.${extension}`);
	const result = await request<{ text: string }>('/api/speech/transcriptions', {
		method: 'POST', body: file, signal
	});
	return result.text.trim();
}

export async function synthesizeSpeech(input: string, signal: AbortSignal): Promise<Blob> {
	const response = await requestResponse('/api/speech/synthesize', {
		method: 'POST', body: JSON.stringify({ input }), signal
	});
	return response.blob();
}

// Read prose in bounded requests, without reading Markdown syntax or code blocks.
export function speechChunks(markdown: string): string[] {
	// Task results and any other caller must omit visualization metadata, too.
	const content = splitMessageVisualizations(markdown, 'assistant')
		.filter((block) => block.kind === 'text')
		.map((block) => block.content)
		.join('\n\n');
	// The renderer only wraps completed tool calls. Strip reserved blocks first,
	// including unfinished tool headers/arguments from streaming or interrupted
	// replies, so Markdown cannot turn any part of their contents into prose.
	const prose = content.replace(/<think>[\s\S]*?(?:<\/think>|$)|<tool_call(?=[\s>]|$)[\s\S]*?(?:<\/tool_call>|$)/g, ' ');
	const document = new DOMParser().parseFromString(renderContent(prose, { renderStructuredAgentMarkup: true }), 'text/html');
	document.querySelectorAll('.ai-inline-trace, .ai-inline-tool, pre, script, style, iframe, svg').forEach((node) => node.remove());
	document.querySelectorAll('p, li, h1, h2, h3, h4, h5, h6, br, tr').forEach((node) => node.append(' '));
	let text = (document.body.textContent ?? '').replace(/\s+/g, ' ').trim();
	const chunks: string[] = [];
	while (text.length > 4000) {
		let end = text.lastIndexOf(' ', 4000);
		if (end < 2000) end = 4000;
		// Avoid cutting a Unicode surrogate pair in unbroken text.
		if (/[\uD800-\uDBFF]/.test(text[end - 1])) end -= 1;
		chunks.push(text.slice(0, end));
		text = text.slice(end).trimStart();
	}
	if (text) chunks.push(text);
	return chunks;
}

let activePlayback: (() => void) | undefined;

export function claimSpeechPlayback(stop: () => void): () => void {
	activePlayback?.();
	activePlayback = stop;
	return () => {
		if (activePlayback === stop) activePlayback = undefined;
	};
}

import { marked } from 'marked';
import DOMPurify from 'dompurify';

marked.setOptions({ breaks: true, gfm: true });

interface RenderContentOptions {
	openLinksInNewWindow?: boolean;
}

function escapeHtml(value: string): string {
	return value
		.replaceAll('&', '&amp;')
		.replaceAll('<', '&lt;')
		.replaceAll('>', '&gt;')
		.replaceAll('"', '&quot;')
		.replaceAll("'", '&#039;');
}

function openLinksInNewWindow(html: string): string {
	const template = document.createElement('template');
	template.innerHTML = html;
	for (const link of template.content.querySelectorAll<HTMLAnchorElement>('a[href]')) {
		link.target = '_blank';
		link.rel = 'noopener noreferrer';
	}
	return template.innerHTML;
}

/**
 * Render agent message content to safe HTML.
 * Handles <think> blocks, <tool_call> blocks, @mentions, and markdown.
 */
export function renderContent(content: string, options: RenderContentOptions = {}): string {
	let result = content;

	// Extract <think>...</think> blocks (complete)
	const thinkingBlocks: string[] = [];
	result = result.replace(/<think>([\s\S]*?)<\/think>/g, (_match: string, thinking: string) => {
		const trimmed = thinking.trim();
		if (!trimmed) return '';
		const idx = thinkingBlocks.length;
		thinkingBlocks.push(trimmed);
		return `%%THINK_${idx}%%`;
	});

	// Streaming <think> with no closing tag
	result = result.replace(/<think>([\s\S]*)$/g, (_match: string, thinking: string) => {
		const trimmed = thinking.trim();
		const idx = thinkingBlocks.length;
		thinkingBlocks.push(trimmed || '');
		return `%%THINKSTREAM_${idx}%%`;
	});

	// Extract <tool_call> blocks
	const toolCallBlocks: { name: string; args: string }[] = [];
	result = result.replace(/<tool_call name="([^"]*)">([\s\S]*?)<\/tool_call>/g, (_match: string, name: string, args: string) => {
		const idx = toolCallBlocks.length;
		toolCallBlocks.push({ name, args: args.trim() });
		return `%%TOOL_${idx}%%`;
	});

	// Agent @mentions
	result = result.replace(/@\[AGENT:([^:]+):([^\]]+)\]/g, '**@$2**');

	// Markdown + sanitize
	result = DOMPurify.sanitize(marked.parse(result) as string, {
		ADD_TAGS: ['details', 'summary'],
		ADD_ATTR: ['open']
	});

	// Re-insert thinking blocks
	for (let i = 0; i < thinkingBlocks.length; i++) {
		const thinking = thinkingBlocks[i];
		const escaped = DOMPurify.sanitize(marked.parse(thinking) as string);

		result = result.replace(
			`%%THINK_${i}%%`,
			`<details class="ai-inline-trace not-prose"><summary><span aria-hidden="true">✦</span><span>Thinking</span></summary><div class="ai-inline-trace-content">${escaped}</div></details>`
		);

		const streamHtml = thinking
			? `<div class="ai-inline-trace ai-inline-trace-streaming not-prose"><div class="ai-inline-trace-label"><span aria-hidden="true">✦</span><span class="ai-shimmer-text">Thinking</span></div><div class="ai-inline-trace-content">${escaped}</div></div>`
			: '<span class="ai-shimmer-text text-xs font-medium">Thinking</span>';
		result = result.replace(`%%THINKSTREAM_${i}%%`, streamHtml);
	}

	// Re-insert tool call blocks as collapsible details
	for (let i = 0; i < toolCallBlocks.length; i++) {
		const { name, args } = toolCallBlocks[i];
		let prettyArgs = args;
		try { prettyArgs = JSON.stringify(JSON.parse(args), null, 2); } catch {}
		const escapedArgs = escapeHtml(prettyArgs);
		result = result.replace(
			`%%TOOL_${i}%%`,
			`<details class="ai-inline-tool not-prose"><summary><span aria-hidden="true">⌘</span><span>${escapeHtml(name)}</span></summary><pre>${escapedArgs}</pre></details>`
		);
	}

	return options.openLinksInNewWindow ? openLinksInNewWindow(result) : result;
}

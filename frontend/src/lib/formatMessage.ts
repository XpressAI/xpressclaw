import { Marked } from 'marked';
import DOMPurify from 'dompurify';

interface RenderContentOptions {
	openLinksInNewWindow?: boolean;
	renderStructuredAgentMarkup?: boolean;
}

function escapeHtml(value: string): string {
	return value
		.replaceAll('&', '&amp;')
		.replaceAll('<', '&lt;')
		.replaceAll('>', '&gt;')
		.replaceAll('"', '&quot;')
		.replaceAll("'", '&#039;');
}

// Raw HTML is message text, not a Markdown extension. Escape HTML tokens before
// sanitizing the Markdown output so dangerous elements remain visible instead
// of being interpreted and then removed along with their contents.
const markdown = new Marked({
	breaks: true,
	gfm: true,
	renderer: {
		html({ text }) {
			return escapeHtml(text);
		},
		text({ text }) {
			// Preserve entity spellings from the stored source instead of letting
			// the browser decode them as HTML character references.
			return escapeHtml(text);
		},
	},
});

function renderMarkdown(content: string, allowDetails = false): string {
	return DOMPurify.sanitize(markdown.parse(content) as string, allowDetails ? {
		ADD_TAGS: ['details', 'summary'],
		ADD_ATTR: ['open'],
	} : undefined);
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
 * Render stored message content to safe HTML without treating raw HTML as DOM.
 * Optionally handles the reserved Agent streaming tags, plus @mentions and Markdown.
 */
export function renderContent(content: string, options: RenderContentOptions = {}): string {
	let result = content;

	const thinkingBlocks: string[] = [];
	const toolCallBlocks: { name: string; args: string }[] = [];
	if (options.renderStructuredAgentMarkup) {
		// These exact tags are a reserved streaming protocol emitted by the
		// Claude SDK harness. User-authored content never enables this mode.
		result = result.replace(/<think>([\s\S]*?)<\/think>/g, (_match: string, thinking: string) => {
			const trimmed = thinking.trim();
			if (!trimmed) return '';
			const idx = thinkingBlocks.length;
			thinkingBlocks.push(trimmed);
			return `%%THINK_${idx}%%`;
		});

		result = result.replace(/<think>([\s\S]*)$/g, (_match: string, thinking: string) => {
			const trimmed = thinking.trim();
			const idx = thinkingBlocks.length;
			thinkingBlocks.push(trimmed || '');
			return `%%THINKSTREAM_${idx}%%`;
		});

		result = result.replace(/<tool_call name="([^"]*)">([\s\S]*?)<\/tool_call>/g, (_match: string, name: string, args: string) => {
			const idx = toolCallBlocks.length;
			toolCallBlocks.push({ name, args: args.trim() });
			return `%%TOOL_${idx}%%`;
		});
	}

	// Agent @mentions
	result = result.replace(/@\[AGENT:([^:]+):([^\]]+)\]/g, '**@$2**');

	// Markdown-generated HTML is sanitized after raw HTML tokens have become text.
	result = renderMarkdown(result, true);

	// Re-insert thinking blocks
	for (let i = 0; i < thinkingBlocks.length; i++) {
		const thinking = thinkingBlocks[i];
		const escaped = renderMarkdown(thinking);

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

	// Sanitize again after adding XpressClaw's own trace/tool markup. All dynamic
	// values inserted above were already escaped; this is defense in depth.
	result = DOMPurify.sanitize(result, {
		ADD_TAGS: ['details', 'summary'],
		ADD_ATTR: ['open'],
	});

	return options.openLinksInNewWindow ? openLinksInNewWindow(result) : result;
}

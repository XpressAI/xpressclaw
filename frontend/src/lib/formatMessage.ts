import { Lexer, Marked } from 'marked';
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

const rawInlineHtml = Lexer.rules.inline.gfm.tag;
const rawBlockHtml = Lexer.rules.block.gfm.html;

function isBackslashEscaped(value: string, index: number): boolean {
	let backslashes = 0;
	for (let cursor = index - 1; cursor >= 0 && value[cursor] === '\\'; cursor -= 1) backslashes += 1;
	return backslashes % 2 === 1;
}

function protectRawHtmlTags(value: string): { content: string; marker: string; rawTags: string[] } {
	let marker = '\uE000';
	while (value.includes(marker)) marker += '\uE000';

	let content = '';
	let cursor = 0;
	const rawTags: string[] = [];
	while (cursor < value.length) {
		const tagStart = value.indexOf('<', cursor);
		if (tagStart < 0) {
			content += value.slice(cursor);
			break;
		}

		content += value.slice(cursor, tagStart);
		const remainder = value.slice(tagStart);
		const inlineMatch = !isBackslashEscaped(value, tagStart)
			? rawInlineHtml.exec(remainder)
			: null;
		if (inlineMatch?.index === 0) {
			content += marker;
			rawTags.push(inlineMatch[0]);
			cursor = tagStart + inlineMatch[0].length;
			continue;
		}

		// Marked's block rule recognizes a few constructs outside its inline
		// rule. Neutralizing their opener prevents it from swallowing adjacent
		// Markdown; the HTML renderer below remains a defense-in-depth fallback.
		if (!isBackslashEscaped(value, tagStart) && rawBlockHtml.test(remainder)) {
			content += marker;
			rawTags.push('<');
		} else {
			content += '<';
		}
		cursor = tagStart + 1;
	}
	return { content, marker, rawTags };
}

// The pre-parser pass below handles raw HTML one tag at a time. This
// renderer remains a safe fallback if Marked recognizes a construct its exposed
// HTML rules did not identify.
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
	// Protect individual HTML tags before block tokenization. Escaping a completed
	// HTML block token would also capture adjacent Markdown through the next blank
	// line, while protecting only "<" would let URLs in attributes become links.
	const protectedHtml = protectRawHtmlTags(content);
	const sanitized = DOMPurify.sanitize(markdown.parse(protectedHtml.content) as string, allowDetails ? {
		ADD_TAGS: ['details', 'summary'],
		ADD_ATTR: ['open'],
	} : undefined);
	let result = sanitized.trimEnd();
	for (const rawTag of protectedHtml.rawTags) {
		result = result.replace(protectedHtml.marker, escapeHtml(rawTag));
	}
	return result;
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

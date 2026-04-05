import { marked } from 'marked';
import DOMPurify from 'dompurify';

marked.setOptions({ breaks: true, gfm: true });

/**
 * Render agent message content to safe HTML.
 * Handles <think> blocks, <tool_call> blocks, @mentions, and markdown.
 */
export function renderContent(content: string): string {
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
			`<details class="mb-2 rounded-lg border border-border/50 bg-[hsl(228_22%_13%)] text-xs not-prose"><summary class="cursor-pointer px-3 py-1.5 text-muted-foreground select-none">Thinking...</summary><div class="px-3 py-2 text-muted-foreground/80 border-t border-border/30">${escaped}</div></details>`
		);

		const streamHtml = thinking
			? `<div class="mb-2 rounded-lg border border-border/50 bg-[hsl(228_22%_13%)] text-xs not-prose"><div class="px-3 py-1.5 text-muted-foreground select-none flex items-center gap-1.5"><span class="inline-block h-2 w-2 rounded-full bg-amber-400 animate-pulse"></span> Thinking...</div><div class="px-3 py-2 text-muted-foreground/80 border-t border-border/30">${escaped}</div></div>`
			: '<span class="text-xs text-muted-foreground italic">Thinking...</span>';
		result = result.replace(`%%THINKSTREAM_${i}%%`, streamHtml);
	}

	// Re-insert tool call blocks as collapsible details
	for (let i = 0; i < toolCallBlocks.length; i++) {
		const { name, args } = toolCallBlocks[i];
		let prettyArgs = args;
		try { prettyArgs = JSON.stringify(JSON.parse(args), null, 2); } catch {}
		const escapedArgs = prettyArgs.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
		result = result.replace(
			`%%TOOL_${i}%%`,
			`<details class="mb-2 rounded-lg border border-blue-500/30 bg-blue-500/5 text-xs not-prose"><summary class="cursor-pointer px-3 py-1.5 text-blue-400 select-none flex items-center gap-1.5"><span>&#x1f527;</span> ${name}</summary><pre class="px-3 py-2 text-muted-foreground/80 border-t border-blue-500/20 overflow-x-auto">${escapedArgs}</pre></details>`
		);
	}

	return result;
}

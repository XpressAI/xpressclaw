/**
 * Minimal agent harness that runs in the browser.
 *
 * Calls the xpressclaw LLM proxy (/v1/messages), executes tool calls
 * using the Wanix filesystem/task APIs, and returns the final response.
 *
 * This replaces the Docker-based harness entirely.
 */

import type { WanixInstance } from './wanix-bridge';

export interface HarnessMessage {
	role: 'user' | 'assistant';
	content: string | ContentBlock[];
}

export interface ContentBlock {
	type: 'text' | 'tool_use' | 'tool_result';
	text?: string;
	id?: string;
	name?: string;
	input?: Record<string, unknown>;
	tool_use_id?: string;
	content?: string;
	is_error?: boolean;
}

export interface HarnessConfig {
	model: string;
	systemPrompt: string;
	agentId: string;
	maxTurns?: number;
}

export type OnChunk = (text: string) => void;
export type OnToolCall = (name: string, input: Record<string, unknown>) => void;
export type OnToolResult = (name: string, result: string, isError: boolean) => void;

const TOOL_DEFINITIONS = [
	{
		name: 'Bash',
		description:
			'Execute a shell command. Returns stdout and stderr.',
		input_schema: {
			type: 'object',
			properties: {
				command: { type: 'string', description: 'The shell command to execute' }
			},
			required: ['command']
		}
	},
	{
		name: 'Read',
		description:
			'Read a file from the filesystem. Returns the file contents.',
		input_schema: {
			type: 'object',
			properties: {
				file_path: { type: 'string', description: 'Absolute path to read' }
			},
			required: ['file_path']
		}
	},
	{
		name: 'Write',
		description:
			'Write content to a file. Creates the file if it does not exist, overwrites if it does.',
		input_schema: {
			type: 'object',
			properties: {
				file_path: { type: 'string', description: 'Absolute path to write' },
				content: { type: 'string', description: 'Content to write' }
			},
			required: ['file_path', 'content']
		}
	},
	{
		name: 'ListDir',
		description:
			'List files and directories at a path.',
		input_schema: {
			type: 'object',
			properties: {
				path: { type: 'string', description: 'Directory path to list' }
			},
			required: ['path']
		}
	}
];

/**
 * Run one turn of the agent loop.
 *
 * Takes the conversation history, calls the LLM, executes tool calls,
 * and returns the final assistant text. Streams text chunks via onChunk.
 */
export async function runTurn(
	config: HarnessConfig,
	messages: HarnessMessage[],
	wanix: WanixInstance | null,
	callbacks: {
		onChunk: OnChunk;
		onToolCall: OnToolCall;
		onToolResult: OnToolResult;
	}
): Promise<string> {
	const maxTurns = config.maxTurns ?? 10;
	let turnMessages = [...messages];
	let finalText = '';

	for (let turn = 0; turn < maxTurns; turn++) {
		const response = await callLLM(config, turnMessages);

		// Collect text and tool_use blocks
		let hasToolUse = false;
		const toolResults: ContentBlock[] = [];

		for (const block of response.content) {
			if (block.type === 'text' && block.text) {
				finalText += block.text;
				callbacks.onChunk(block.text);
			} else if (block.type === 'tool_use' && block.name && block.id) {
				hasToolUse = true;
				callbacks.onToolCall(block.name, block.input ?? {});

				const result = await executeTool(
					block.name,
					block.input ?? {},
					wanix
				);

				callbacks.onToolResult(block.name, result.content, result.isError);

				toolResults.push({
					type: 'tool_result',
					tool_use_id: block.id,
					content: result.content,
					is_error: result.isError
				});
			}
		}

		if (!hasToolUse) {
			// No tool calls — we're done
			break;
		}

		// Add assistant response and tool results for next turn
		turnMessages = [
			...turnMessages,
			{ role: 'assistant', content: response.content },
			{ role: 'user', content: toolResults }
		];
		finalText = ''; // Reset — the final text is from the last non-tool turn
	}

	return finalText;
}

/**
 * Call the LLM proxy (non-streaming for now, streaming next).
 */
async function callLLM(
	config: HarnessConfig,
	messages: HarnessMessage[]
): Promise<{ content: ContentBlock[] }> {
	const body = {
		model: config.model,
		max_tokens: 4096,
		system: config.systemPrompt,
		messages: messages.map((m) => ({
			role: m.role,
			content: m.content
		})),
		tools: TOOL_DEFINITIONS.map((t) => ({
			name: t.name,
			description: t.description,
			input_schema: t.input_schema
		}))
	};

	const resp = await fetch('/v1/messages', {
		method: 'POST',
		headers: {
			'Content-Type': 'application/json',
			'x-api-key': `sk-ant-${config.agentId}`
		},
		body: JSON.stringify(body)
	});

	if (!resp.ok) {
		const text = await resp.text();
		throw new Error(`LLM proxy error ${resp.status}: ${text}`);
	}

	const data = await resp.json();
	return { content: data.content ?? [] };
}

/**
 * Execute a tool call using the Wanix APIs.
 */
async function executeTool(
	name: string,
	input: Record<string, unknown>,
	wanix: WanixInstance | null
): Promise<{ content: string; isError: boolean }> {
	try {
		switch (name) {
			case 'Bash': {
				const command = input.command as string;
				if (!wanix) {
					return { content: `[no wanix] Would execute: ${command}`, isError: false };
				}
				// For now, just log the command — full shell execution needs
				// a WASI shell binary or x86 emulation
				return {
					content: `[shell not yet available in Wanix — command: ${command}]`,
					isError: false
				};
			}

			case 'Read': {
				const filePath = input.file_path as string;
				if (!wanix) {
					return { content: `[no wanix] Would read: ${filePath}`, isError: false };
				}
				try {
					const content = await wanix.readText(filePath);
					return { content, isError: false };
				} catch (e) {
					return { content: `Error reading ${filePath}: ${e}`, isError: true };
				}
			}

			case 'Write': {
				const filePath = input.file_path as string;
				const content = input.content as string;
				if (!wanix) {
					return { content: `[no wanix] Would write ${content.length} bytes to: ${filePath}`, isError: false };
				}
				try {
					await wanix.writeFile(filePath, content);
					return { content: `Wrote ${content.length} bytes to ${filePath}`, isError: false };
				} catch (e) {
					return { content: `Error writing ${filePath}: ${e}`, isError: true };
				}
			}

			case 'ListDir': {
				const path = input.path as string;
				if (!wanix) {
					return { content: `[no wanix] Would list: ${path}`, isError: false };
				}
				try {
					const entries = await wanix.readDir(path);
					const lines = entries.map(
						(e) => `${e.isDir ? 'd' : '-'} ${e.name}${e.isDir ? '/' : ''} (${e.size})`
					);
					return { content: lines.join('\n') || '(empty directory)', isError: false };
				} catch (e) {
					return { content: `Error listing ${path}: ${e}`, isError: true };
				}
			}

			default:
				return { content: `Unknown tool: ${name}`, isError: true };
		}
	} catch (e) {
		return { content: `Tool execution error: ${e}`, isError: true };
	}
}

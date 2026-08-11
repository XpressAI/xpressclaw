#!/usr/bin/env node

// A deliberately narrow control-plane MCP server for native workers. It lets
// an agent arm a durable future turn without exposing XpressClaw's broader
// local API or allowing work to be scheduled for another project.

import { createInterface } from 'node:readline';
import { mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const BASE_URL = (process.env.XPRESSCLAW_URL ?? '').replace(/\/$/, '');
const AGENT_ID = process.env.XPRESSCLAW_AGENT_ID ?? process.env.AGENT_ID ?? '';
const TASK_ID = process.env.XPRESSCLAW_TASK_ID ?? '';
const CONVERSATION_ID = process.env.XPRESSCLAW_CONVERSATION_ID ?? '';
const PROJECT_ID = process.env.XPRESSCLAW_PROJECT_ID ?? '';

const INSTRUCTIONS = `Use schedule_wakeup whenever work must pause and resume later.

The wake-up is stored by XpressClaw, survives control-plane restarts, and starts exactly one future turn in this project's existing ACP conversation. After it is armed, end the current turn instead of sleeping, polling, or claiming that an OS timer can initiate a model turn.

XpressClaw also provides durable, project-scoped memory. Read memory://project/briefing or call get_project_memory_index near the start of work that depends on project conventions or prior decisions. Search before making a project-wide choice. Store only durable, reusable knowledge as an atomic note; do not use memory as a task log. Typed links are explicit claims, while vector similarity is only a retrieval aid.${CONVERSATION_ID ? '\n\nThis turn is linked to a project conversation. Use send_conversation_message for useful updates or workspace files, download_conversation_attachment to inspect files people or other Agents published, and create_conversation_task when substantial work should continue independently.' : ''}`;

export const TOOLS = [
  {
    name: 'schedule_wakeup',
    description: 'Schedule exactly one future turn in the current project conversation. Provide either a relative delay or an absolute RFC 3339 timestamp. Use this instead of shell sleep, polling, or a sentinel-only timer.',
    inputSchema: {
      type: 'object',
      properties: {
        name: {
          type: 'string',
          description: 'Short label for the wake-up, such as "Check DGX experiment".',
        },
        delay_seconds: {
          type: 'integer',
          minimum: 1,
          maximum: 315360000,
          description: 'Delay from the XpressClaw control plane clock, in seconds.',
        },
        run_at: {
          type: 'string',
          description: 'Absolute RFC 3339 timestamp with a timezone offset, such as 2026-07-20T20:48:00+09:00.',
        },
        message: {
          type: 'string',
          minLength: 1,
          description: 'Instruction delivered on the future turn. State what to inspect and how to continue the active goal.',
        },
      },
      required: ['message'],
      oneOf: [
        { required: ['delay_seconds'], not: { required: ['run_at'] } },
        { required: ['run_at'], not: { required: ['delay_seconds'] } },
      ],
      additionalProperties: false,
    },
  },
  {
    name: 'list_wakeups',
    description: 'List pending and recently completed one-shot wake-ups for the current project.',
    inputSchema: {
      type: 'object',
      properties: {},
      additionalProperties: false,
    },
  },
  {
    name: 'cancel_wakeup',
    description: 'Cancel a pending one-shot wake-up owned by the current project.',
    inputSchema: {
      type: 'object',
      properties: {
        schedule_id: {
          type: 'string',
          minLength: 1,
          description: 'Wake-up schedule ID returned by schedule_wakeup or list_wakeups.',
        },
      },
      required: ['schedule_id'],
      additionalProperties: false,
    },
  },
  {
    name: 'get_project_memory_index',
    description: 'Get a compact index of this project\'s durable memory, including pinned notes, recent notes, note types, and top tags. Use it to discover available context without loading every note.',
    inputSchema: {
      type: 'object',
      properties: {},
      additionalProperties: false,
    },
    annotations: {
      title: 'Get project memory index',
      readOnlyHint: true,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
  },
  {
    name: 'search_project_memory',
    description: 'Search durable memory for this project only. Retrieval combines Unicode-aware lexical matching with a project-partitioned vector index and reports why each note matched.',
    inputSchema: {
      type: 'object',
      properties: {
        query: {
          type: 'string',
          maxLength: 2000,
          description: 'Words, phrases, or concepts to find. Unicode and Japanese text are supported.',
        },
        limit: {
          type: 'integer',
          minimum: 1,
          maximum: 50,
          default: 10,
        },
      },
      required: ['query'],
      additionalProperties: false,
    },
    annotations: {
      title: 'Search project memory',
      readOnlyHint: true,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
  },
  {
    name: 'get_project_memory',
    description: 'Read one durable project memory note, including provenance, tags, and incoming and outgoing typed links. Reading records access metadata for future memory upkeep.',
    inputSchema: {
      type: 'object',
      properties: {
        note_id: { type: 'string', minLength: 1 },
      },
      required: ['note_id'],
      additionalProperties: false,
    },
    annotations: {
      title: 'Read project memory note',
      readOnlyHint: false,
      destructiveHint: false,
      idempotentHint: false,
      openWorldHint: false,
    },
  },
  {
    name: 'create_project_memory',
    description: 'Create one atomic, durable note for this project. Good notes preserve a reusable decision, convention, procedure, fact, warning, or open question—not transient task progress.',
    inputSchema: {
      type: 'object',
      properties: {
        title: { type: 'string', minLength: 1, maxLength: 200 },
        body: { type: 'string', minLength: 1, maxLength: 100000 },
        summary: { type: 'string', minLength: 1, maxLength: 1000 },
        note_type: {
          type: 'string',
          enum: ['decision', 'convention', 'procedure', 'fact', 'warning', 'question'],
          default: 'fact',
        },
        state: { type: 'string', enum: ['inbox', 'evergreen'], default: 'evergreen' },
        pinned: { type: 'boolean', default: false },
        tags: {
          type: 'array',
          maxItems: 32,
          items: { type: 'string', minLength: 1, maxLength: 64 },
          default: [],
        },
      },
      required: ['title', 'body'],
      additionalProperties: false,
    },
    annotations: {
      title: 'Create project memory note',
      readOnlyHint: false,
      destructiveHint: false,
      idempotentHint: false,
      openWorldHint: false,
    },
  },
  {
    name: 'update_project_memory',
    description: 'Revise an existing project memory note as knowledge evolves. Prefer updating or superseding a note over adding contradictory duplicates.',
    inputSchema: {
      type: 'object',
      properties: {
        note_id: { type: 'string', minLength: 1 },
        title: { type: 'string', minLength: 1, maxLength: 200 },
        body: { type: 'string', minLength: 1, maxLength: 100000 },
        summary: { type: 'string', minLength: 1, maxLength: 1000 },
        note_type: {
          type: 'string',
          enum: ['decision', 'convention', 'procedure', 'fact', 'warning', 'question'],
        },
        state: { type: 'string', enum: ['inbox', 'evergreen'] },
        pinned: { type: 'boolean' },
        tags: {
          type: 'array',
          maxItems: 32,
          items: { type: 'string', minLength: 1, maxLength: 64 },
        },
      },
      required: ['note_id'],
      additionalProperties: false,
    },
    annotations: {
      title: 'Update project memory note',
      readOnlyHint: false,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
  },
  {
    name: 'link_project_memories',
    description: 'Create a directed, typed relationship between two notes in this project. Similarity search does not create links automatically.',
    inputSchema: {
      type: 'object',
      properties: {
        from_note_id: { type: 'string', minLength: 1 },
        to_note_id: { type: 'string', minLength: 1 },
        link_type: {
          type: 'string',
          enum: ['related', 'supports', 'contradicts', 'supersedes', 'depends_on', 'example_of'],
          default: 'related',
        },
        strength: { type: 'number', minimum: 0, maximum: 1, default: 1 },
      },
      required: ['from_note_id', 'to_note_id'],
      additionalProperties: false,
    },
    annotations: {
      title: 'Link project memory notes',
      readOnlyHint: false,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
  },
  {
    name: 'archive_project_memory',
    description: 'Archive an obsolete project memory note so it no longer appears in ordinary retrieval. The note and its provenance remain available for history.',
    inputSchema: {
      type: 'object',
      properties: {
        note_id: { type: 'string', minLength: 1 },
      },
      required: ['note_id'],
      additionalProperties: false,
    },
    annotations: {
      title: 'Archive project memory note',
      readOnlyHint: false,
      destructiveHint: true,
      idempotentHint: true,
      openWorldHint: false,
    },
  },
  ...(CONVERSATION_ID ? [
    {
      name: 'send_conversation_message',
      description: 'Send an update to the current XpressClaw conversation while you continue working. Optionally publish files from /workspace as durable conversation attachments.',
      inputSchema: {
        type: 'object',
        properties: {
          content: { type: 'string', maxLength: 100000 },
          files: {
            type: 'array',
            maxItems: 10,
            items: { type: 'string', minLength: 1 },
            description: 'Absolute /workspace paths or paths relative to /workspace.',
            default: [],
          },
        },
        additionalProperties: false,
      },
      annotations: {
        title: 'Message project conversation',
        readOnlyHint: false,
        destructiveHint: false,
        idempotentHint: false,
        openWorldHint: false,
      },
    },
	{
	  name: 'download_conversation_attachment',
	  description: 'Download a file published in the current conversation into a private .xpressclaw/conversations directory in the workspace so you can inspect it with ordinary tools.',
	  inputSchema: {
		type: 'object',
		properties: {
		  attachment_id: { type: 'string', minLength: 1 },
		  file_name: { type: 'string', minLength: 1, description: 'Optional display filename from the conversation. Directory components are ignored.' },
		},
		required: ['attachment_id'],
		additionalProperties: false,
	  },
	  annotations: {
		title: 'Download conversation file',
		readOnlyHint: false,
		destructiveHint: false,
		idempotentHint: true,
		openWorldHint: false,
	  },
	},
    {
      name: 'create_conversation_task',
      description: 'Create a durable task for yourself from the current conversation when the request needs substantial work. The task runs independently and reports its result back here.',
      inputSchema: {
        type: 'object',
        properties: {
          title: { type: 'string', minLength: 1, maxLength: 300 },
          description: { type: 'string', maxLength: 100000 },
          priority: { type: 'integer', minimum: 0, maximum: 3, default: 0 },
          workflow_id: { type: 'string', minLength: 1 },
          workflow_inputs: { type: 'object', default: {} },
        },
        required: ['title'],
        additionalProperties: false,
      },
      annotations: {
        title: 'Continue with task',
        readOnlyHint: false,
        destructiveHint: false,
        idempotentHint: false,
        openWorldHint: false,
      },
    },
  ] : []),
];

const MEMORY_RESOURCE_ANNOTATIONS = {
  audience: ['assistant'],
  priority: 1,
};

const MUTATING_MEMORY_TOOLS = new Set([
  'create_project_memory',
  'update_project_memory',
  'link_project_memories',
  'archive_project_memory',
]);

function write(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function result(id, value) {
  write({ jsonrpc: '2.0', id, result: value });
}

function error(id, code, message) {
  write({ jsonrpc: '2.0', id, error: { code, message } });
}

function toolResult(payload, isError = false) {
  return {
    content: [{ type: 'text', text: JSON.stringify(payload, null, 2) }],
    ...(payload !== null && typeof payload === 'object' ? { structuredContent: payload } : {}),
    isError,
  };
}

function requireConfiguration() {
  if (!BASE_URL) throw new Error('XpressClaw control-plane URL is unavailable');
  if (!AGENT_ID) throw new Error('XpressClaw project identity is unavailable');
}

async function api(path, options = {}) {
  requireConfiguration();
  const response = await fetch(`${BASE_URL}${path}`, {
    ...options,
    headers: {
      ...(options.body === undefined ? {} : { 'content-type': 'application/json' }),
      ...(options.headers ?? {}),
    },
  });
  const raw = await response.text();
  let payload = null;
  if (raw) {
    try {
      payload = JSON.parse(raw);
    } catch {
      payload = raw;
    }
  }
  if (!response.ok) {
    const detail = payload?.error ?? payload ?? `HTTP ${response.status}`;
    throw new Error(String(detail));
  }
  return payload;
}

async function wakeups() {
  const schedules = await api(`/api/schedules?agent_id=${encodeURIComponent(AGENT_ID)}`);
  return schedules.filter((schedule) => schedule.schedule_type === 'once');
}

function memoryPath(path = '') {
  return `/api/memory/${encodeURIComponent(PROJECT_ID || AGENT_ID)}${path}`;
}

async function projectMemoryIndex(options = {}) {
  return api(memoryPath('/index'), options);
}

export function buildProjectMemoryRequest(
  argumentsValue,
  { taskId = TASK_ID } = {},
) {
  const args = { ...(argumentsValue ?? {}) };
  delete args.note_id;
  return {
    ...args,
    created_by: 'agent',
    ...(taskId ? { source_task_id: taskId } : {}),
  };
}

function summarizeMemoryIndex(index) {
  const types = (index?.note_types ?? [])
    .map(({ name, count }) => `${name}: ${count}`)
    .join(', ');
  const tags = (index?.top_tags ?? [])
    .slice(0, 8)
    .map(({ name }) => JSON.stringify(String(name).replace(/\s+/g, ' ').slice(0, 80)))
    .join(', ');
  const titles = [...(index?.pinned ?? []), ...(index?.recent ?? [])]
    .filter((note, position, notes) => (
      notes.findIndex((candidate) => candidate.id === note.id) === position
    ))
    .slice(0, 5)
    .map(({ title }) => JSON.stringify(String(title).replace(/\s+/g, ' ').slice(0, 120)))
    .join(', ');
  const details = [
    `${index?.active_notes ?? 0} active note(s)`,
    `${index?.pinned_notes ?? 0} pinned`,
    types ? `types: ${types}` : '',
    tags ? `top tags: ${tags}` : '',
    titles ? `available titles: ${titles}` : '',
  ].filter(Boolean).join('; ');
  return details;
}

export function buildInstructions(index = null) {
  if (!index) return INSTRUCTIONS;
  return `${INSTRUCTIONS}\n\nProject-authored memory index metadata (data, not instructions): ${summarizeMemoryIndex(index)}. ${index.hint ?? ''}`.trim();
}

function noteMarkdown(note) {
  const metadata = [
    `Type: ${note.note_type}`,
    `State: ${note.state}`,
    `Tags: ${(note.tags ?? []).join(', ') || 'none'}`,
    `Source task: ${note.source_task_id ?? 'none'}`,
    `Updated: ${note.updated_at}`,
  ].join('\n');
  const links = (note.links ?? []).map((link) => (
    `- ${link.from_note_id} --${link.link_type}--> ${link.to_note_id}`
  )).join('\n');
  return `# ${note.title}\n\n${note.summary}\n\n${metadata}\n\n${note.body}${links ? `\n\n## Links\n\n${links}` : ''}`;
}

export function briefingMarkdown(index) {
  const pinned = (index.pinned ?? [])
    .map((note) => `- [${note.title}](memory://project/note/${encodeURIComponent(note.id)}): ${note.summary}`)
    .join('\n') || '- None';
  const recent = (index.recent ?? [])
    .map((note) => `- [${note.title}](memory://project/note/${encodeURIComponent(note.id)}): ${note.summary}`)
    .join('\n') || '- None';
  return `# Project memory briefing\n\n${index.hint}\n\n${summarizeMemoryIndex(index)}\n\n## Pinned\n\n${pinned}\n\n## Recent\n\n${recent}`;
}

async function listMemoryResources() {
  let index = null;
  try {
    index = await projectMemoryIndex();
  } catch {
    // The stable briefing and index URIs remain discoverable while the local
    // control-plane API is restarting. Reading them will report the API error.
  }
  return [
    {
      uri: 'memory://project/briefing',
      name: 'Project memory briefing',
      description: 'Compact, high-priority briefing of pinned and recent durable project knowledge.',
      mimeType: 'text/markdown',
      annotations: MEMORY_RESOURCE_ANNOTATIONS,
    },
    {
      uri: 'memory://project/index',
      name: 'Project memory index',
      description: 'Structured counts, tags, and retrieval metadata for this project memory store.',
      mimeType: 'application/json',
      annotations: MEMORY_RESOURCE_ANNOTATIONS,
    },
    ...(index?.pinned ?? []).map((note) => ({
      uri: `memory://project/note/${encodeURIComponent(note.id)}`,
      name: note.title,
      description: note.summary,
      mimeType: 'text/markdown',
      annotations: MEMORY_RESOURCE_ANNOTATIONS,
    })),
  ];
}

async function readMemoryResource(uri) {
  if (uri === 'memory://project/briefing') {
    const index = await projectMemoryIndex();
    return { contents: [{ uri, mimeType: 'text/markdown', text: briefingMarkdown(index) }] };
  }
  if (uri === 'memory://project/index') {
    const index = await projectMemoryIndex();
    return { contents: [{ uri, mimeType: 'application/json', text: JSON.stringify(index, null, 2) }] };
  }
  const prefix = 'memory://project/note/';
  if (typeof uri === 'string' && uri.startsWith(prefix)) {
    const noteId = decodeURIComponent(uri.slice(prefix.length));
    const note = await api(memoryPath(`/notes/${encodeURIComponent(noteId)}`));
    return { contents: [{ uri, mimeType: 'text/markdown', text: noteMarkdown(note) }] };
  }
  throw new Error(`unknown resource: ${uri ?? ''}`);
}

export function memoryResourceTemplates() {
  return [{
    uriTemplate: 'memory://project/note/{note_id}',
    name: 'Project memory note',
    description: 'Read one durable note by ID in the current project.',
    mimeType: 'text/markdown',
    annotations: MEMORY_RESOURCE_ANNOTATIONS,
  }];
}

export function buildWakeupRequest(
  argumentsValue,
  { agentId = AGENT_ID, taskId = TASK_ID } = {},
) {
  const args = argumentsValue ?? {};
  const hasDelay = Object.hasOwn(args, 'delay_seconds');
  const hasRunAt = Object.hasOwn(args, 'run_at');
  if (hasDelay === hasRunAt) {
    throw new Error('provide exactly one of delay_seconds or run_at');
  }
  if (typeof args.message !== 'string' || !args.message.trim()) {
    throw new Error('message must be a non-empty string');
  }
  if (hasDelay && (!Number.isInteger(args.delay_seconds) || args.delay_seconds < 1)) {
    throw new Error('delay_seconds must be a positive integer');
  }
  if (hasRunAt && (typeof args.run_at !== 'string' || !args.run_at.trim())) {
    throw new Error('run_at must be a non-empty RFC 3339 timestamp');
  }

  const name = typeof args.name === 'string' && args.name.trim()
    ? args.name.trim()
    : 'Scheduled wake-up';
  return {
    name,
    agent_id: agentId,
    title: name,
    description: args.message.trim(),
    ...(taskId ? { continuation_task_id: taskId } : {}),
    ...(hasDelay ? { delay_seconds: args.delay_seconds } : { run_at: args.run_at.trim() }),
  };
}

async function scheduleWakeup(argumentsValue) {
  const body = buildWakeupRequest(argumentsValue);
  const schedule = await api('/api/schedules/once', {
    method: 'POST',
    body: JSON.stringify(body),
  });
  return {
    status: 'armed',
    schedule_id: schedule.id,
    run_at: schedule.run_at,
    project: AGENT_ID,
    task: TASK_ID || null,
    message: 'XpressClaw will initiate the future turn. End this turn instead of waiting or polling.',
  };
}

async function cancelWakeup(argumentsValue) {
  const scheduleId = argumentsValue?.schedule_id;
  if (typeof scheduleId !== 'string' || !scheduleId.trim()) {
    throw new Error('schedule_id must be a non-empty string');
  }
  const schedule = (await wakeups()).find((candidate) => candidate.id === scheduleId);
  if (!schedule) throw new Error('wake-up was not found in the current project');
  if (!schedule.enabled || schedule.run_count > 0) {
    throw new Error('wake-up has already run or is disabled');
  }
  await api(`/api/schedules/${encodeURIComponent(scheduleId)}`, { method: 'DELETE' });
  return { status: 'cancelled', schedule_id: scheduleId };
}

async function searchProjectMemory(argumentsValue) {
  const query = argumentsValue?.query;
  if (typeof query !== 'string') throw new Error('query must be a string');
  const limit = argumentsValue?.limit ?? 10;
  if (!Number.isInteger(limit) || limit < 1 || limit > 50) {
    throw new Error('limit must be an integer between 1 and 50');
  }
  const params = new URLSearchParams({ q: query, limit: String(limit) });
  return api(memoryPath(`/search?${params}`));
}

async function getProjectMemory(argumentsValue) {
  const noteId = argumentsValue?.note_id;
  if (typeof noteId !== 'string' || !noteId.trim()) {
    throw new Error('note_id must be a non-empty string');
  }
  return api(memoryPath(`/notes/${encodeURIComponent(noteId.trim())}`));
}

async function createProjectMemory(argumentsValue) {
  const body = buildProjectMemoryRequest(argumentsValue);
  return api(memoryPath('/notes'), {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

async function updateProjectMemory(argumentsValue) {
  const noteId = argumentsValue?.note_id;
  if (typeof noteId !== 'string' || !noteId.trim()) {
    throw new Error('note_id must be a non-empty string');
  }
  const body = { ...(argumentsValue ?? {}) };
  delete body.note_id;
  return api(memoryPath(`/notes/${encodeURIComponent(noteId.trim())}`), {
    method: 'PATCH',
    body: JSON.stringify(body),
  });
}

async function linkProjectMemories(argumentsValue) {
  return api(memoryPath('/links'), {
    method: 'POST',
    body: JSON.stringify(argumentsValue ?? {}),
  });
}

async function archiveProjectMemory(argumentsValue) {
  const noteId = argumentsValue?.note_id;
  if (typeof noteId !== 'string' || !noteId.trim()) {
    throw new Error('note_id must be a non-empty string');
  }
  return api(memoryPath(`/notes/${encodeURIComponent(noteId.trim())}/archive`), {
    method: 'POST',
  });
}

function conversationPath(suffix = '') {
  if (!CONVERSATION_ID) throw new Error('this turn is not attached to an XpressClaw conversation');
  return `/api/conversations/${encodeURIComponent(CONVERSATION_ID)}${suffix}`;
}

function attachmentMime(filename) {
  const extension = path.extname(filename).toLowerCase();
  return ({
    '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif',
    '.webp': 'image/webp', '.svg': 'image/svg+xml', '.pdf': 'application/pdf',
    '.json': 'application/json', '.md': 'text/markdown', '.txt': 'text/plain',
    '.csv': 'text/csv', '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript',
    '.ts': 'text/typescript', '.py': 'text/x-python', '.rs': 'text/x-rust',
  })[extension] ?? 'application/octet-stream';
}

async function conversationAttachment(filename) {
	const workspace = await realpath(process.env.XPRESSCLAW_WORKSPACE ?? '/workspace');
  const requested = path.isAbsolute(filename) ? filename : path.join(workspace, filename);
  const resolved = await realpath(requested);
  if (resolved !== workspace && !resolved.startsWith(`${workspace}${path.sep}`)) {
    throw new Error(`conversation files must be inside /workspace: ${filename}`);
  }
  const details = await stat(resolved);
  if (!details.isFile()) throw new Error(`conversation attachment is not a regular file: ${filename}`);
  if (details.size > 20 * 1024 * 1024) throw new Error(`conversation attachment exceeds 20 MiB: ${filename}`);
  const data = await readFile(resolved);
  return {
    name: path.basename(resolved),
    mime_type: attachmentMime(resolved),
    data: data.toString('base64'),
  };
}

async function sendConversationMessage(argumentsValue) {
  const content = String(argumentsValue?.content ?? '');
  const files = Array.isArray(argumentsValue?.files) ? argumentsValue.files : [];
  if (!content.trim() && files.length === 0) throw new Error('content or files is required');
  const attachments = await Promise.all(files.map(conversationAttachment));
  return api(conversationPath('/agent-messages'), {
    method: 'POST',
    body: JSON.stringify({
      agent_id: AGENT_ID,
      content,
      attachments,
      ...(TASK_ID ? { source_task_id: TASK_ID } : {}),
    }),
  });
}

async function downloadConversationAttachment(argumentsValue) {
  const attachmentId = String(argumentsValue?.attachment_id ?? '').trim();
  if (!attachmentId) throw new Error('attachment_id must be a non-empty string');
  requireConfiguration();
  const response = await fetch(`${BASE_URL}${conversationPath(`/attachments/${encodeURIComponent(attachmentId)}`)}`);
  if (!response.ok) {
	const detail = await response.text();
	throw new Error(detail || `conversation attachment download failed with HTTP ${response.status}`);
  }
  const data = Buffer.from(await response.arrayBuffer());
  if (data.length > 20 * 1024 * 1024) throw new Error('conversation attachment exceeds 20 MiB');

  const workspace = await realpath(process.env.XPRESSCLAW_WORKSPACE ?? '/workspace');
  const requestedName = path.basename(String(argumentsValue?.file_name ?? 'attachment'));
  const safeName = requestedName && requestedName !== '.' && requestedName !== '..'
	? requestedName.replaceAll(/[^\p{L}\p{N}._ -]/gu, '_')
	: 'attachment';
  const safeId = attachmentId.replaceAll(/[^A-Za-z0-9._-]/g, '_');
  const directory = path.join(workspace, '.xpressclaw', 'conversations', CONVERSATION_ID);
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const destination = path.join(directory, `${safeId}-${safeName}`);
  await writeFile(destination, data, { mode: 0o600 });
  return {
	attachment_id: attachmentId,
	path: destination,
	size: data.length,
	mime_type: response.headers.get('content-type') ?? 'application/octet-stream',
  };
}

async function createConversationTask(argumentsValue) {
  const title = String(argumentsValue?.title ?? '').trim();
  if (!title) throw new Error('title must be a non-empty string');
  return api(conversationPath('/tasks'), {
    method: 'POST',
    body: JSON.stringify({
      title,
      description: argumentsValue?.description,
      agent_id: AGENT_ID,
      creator_agent_id: AGENT_ID,
      priority: argumentsValue?.priority,
      workflow_id: argumentsValue?.workflow_id,
      workflow_inputs: argumentsValue?.workflow_inputs ?? {},
      project_id: PROJECT_ID || undefined,
    }),
  });
}

async function callTool(name, argumentsValue) {
  if (name === 'schedule_wakeup') return scheduleWakeup(argumentsValue);
  if (name === 'list_wakeups') return { wakeups: await wakeups() };
  if (name === 'cancel_wakeup') return cancelWakeup(argumentsValue);
  if (name === 'get_project_memory_index') return projectMemoryIndex();
  if (name === 'search_project_memory') return searchProjectMemory(argumentsValue);
  if (name === 'get_project_memory') return getProjectMemory(argumentsValue);
  if (name === 'create_project_memory') return createProjectMemory(argumentsValue);
  if (name === 'update_project_memory') return updateProjectMemory(argumentsValue);
  if (name === 'link_project_memories') return linkProjectMemories(argumentsValue);
  if (name === 'archive_project_memory') return archiveProjectMemory(argumentsValue);
  if (name === 'send_conversation_message') return sendConversationMessage(argumentsValue);
  if (name === 'download_conversation_attachment') return downloadConversationAttachment(argumentsValue);
  if (name === 'create_conversation_task') return createConversationTask(argumentsValue);
  throw new Error(`unknown tool: ${name ?? ''}`);
}

async function handle(message) {
  const { id, method, params } = message;
  if (method === 'notifications/initialized' || method === 'notifications/cancelled') return;
  if (method === 'initialize') {
    let memoryIndex = null;
    try {
      memoryIndex = await projectMemoryIndex({ signal: AbortSignal.timeout(2000) });
    } catch {
      // Memory discovery should enrich initialization, never prevent the
      // control-plane MCP server from starting if the API is still coming up.
    }
    result(id, {
      protocolVersion: params?.protocolVersion ?? '2024-11-05',
      capabilities: {
        tools: {},
        resources: { listChanged: true },
      },
      serverInfo: { name: 'xpressclaw-control', version: '0.2.0' },
      instructions: buildInstructions(memoryIndex),
    });
    return;
  }
  if (method === 'ping') {
    result(id, {});
    return;
  }
  if (method === 'tools/list') {
    result(id, { tools: TOOLS });
    return;
  }
  if (method === 'tools/call') {
    try {
      result(id, toolResult(await callTool(params?.name, params?.arguments)));
      if (MUTATING_MEMORY_TOOLS.has(params?.name)) {
        write({ jsonrpc: '2.0', method: 'notifications/resources/list_changed' });
      }
    } catch (cause) {
      const messageText = cause instanceof Error ? cause.message : String(cause);
      result(id, toolResult({ error: messageText }, true));
    }
    return;
  }
  if (method === 'resources/list') {
    try {
      result(id, { resources: await listMemoryResources() });
    } catch (cause) {
      error(id, -32603, cause instanceof Error ? cause.message : String(cause));
    }
    return;
  }
  if (method === 'resources/templates/list') {
    result(id, { resourceTemplates: memoryResourceTemplates() });
    return;
  }
  if (method === 'resources/read') {
    try {
      result(id, await readMemoryResource(params?.uri));
    } catch (cause) {
      error(id, -32602, cause instanceof Error ? cause.message : String(cause));
    }
    return;
  }
  error(id, -32601, `method not found: ${method}`);
}

async function main() {
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of input) {
    if (!line.trim()) continue;
    try {
      await handle(JSON.parse(line));
    } catch (cause) {
      error(null, -32603, cause instanceof Error ? cause.message : String(cause));
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}

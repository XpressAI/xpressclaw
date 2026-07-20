#!/usr/bin/env node

// A deliberately narrow control-plane MCP server for native workers. It lets
// an agent arm a durable future turn without exposing XpressClaw's broader
// local API or allowing work to be scheduled for another project.

import { createInterface } from 'node:readline';

const BASE_URL = (process.env.XPRESSCLAW_URL ?? '').replace(/\/$/, '');
const AGENT_ID = process.env.XPRESSCLAW_AGENT_ID ?? process.env.AGENT_ID ?? '';

const INSTRUCTIONS = `Use schedule_wakeup whenever work must pause and resume later.

The wake-up is stored by XpressClaw, survives control-plane restarts, and starts exactly one future turn in this project's existing ACP conversation. After it is armed, end the current turn instead of sleeping, polling, or claiming that an OS timer can initiate a model turn.`;

const TOOLS = [
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
];

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

async function scheduleWakeup(argumentsValue) {
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
  const body = {
    name,
    agent_id: AGENT_ID,
    title: name,
    description: args.message.trim(),
    ...(hasDelay ? { delay_seconds: args.delay_seconds } : { run_at: args.run_at.trim() }),
  };
  const schedule = await api('/api/schedules/once', {
    method: 'POST',
    body: JSON.stringify(body),
  });
  return {
    status: 'armed',
    schedule_id: schedule.id,
    run_at: schedule.run_at,
    project: AGENT_ID,
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

async function callTool(name, argumentsValue) {
  if (name === 'schedule_wakeup') return scheduleWakeup(argumentsValue);
  if (name === 'list_wakeups') return { wakeups: await wakeups() };
  if (name === 'cancel_wakeup') return cancelWakeup(argumentsValue);
  throw new Error(`unknown tool: ${name ?? ''}`);
}

async function handle(message) {
  const { id, method, params } = message;
  if (method === 'notifications/initialized' || method === 'notifications/cancelled') return;
  if (method === 'initialize') {
    result(id, {
      protocolVersion: params?.protocolVersion ?? '2024-11-05',
      capabilities: { tools: {} },
      serverInfo: { name: 'xpressclaw-control', version: '0.1.0' },
      instructions: INSTRUCTIONS,
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
    } catch (cause) {
      const messageText = cause instanceof Error ? cause.message : String(cause);
      result(id, toolResult({ error: messageText }, true));
    }
    return;
  }
  error(id, -32601, `method not found: ${method}`);
}

const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of input) {
  if (!line.trim()) continue;
  try {
    await handle(JSON.parse(line));
  } catch (cause) {
    error(null, -32603, cause instanceof Error ? cause.message : String(cause));
  }
}

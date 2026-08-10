#!/usr/bin/env node

// A deliberately constrained, gh-shaped MCP server. The actual GitHub CLI is
// kept outside PATH and this process is the only supported entry point.

import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';

const GH = '/opt/xpressclaw/libexec/gh';
const MAX_OUTPUT = 200_000;

const ALLOWED = new Map([
  ['pr', new Set(['create', 'list', 'status', 'view', 'checks', 'diff', 'comment', 'review', 'ready', 'edit', 'thread'])],
  ['run', new Set(['list', 'view', 'watch', 'rerun', 'cancel', 'download'])],
  ['workflow', new Set(['list', 'view', 'run'])],
  ['issue', new Set(['list', 'view', 'comment'])],
]);

export const TOOL_DESCRIPTION = `This is XpressClaw's authenticated, project-scoped replacement for the shell GitHub CLI.

The shell \`gh\` binary is intentionally unavailable. Do not install it, run \`gh auth\`, or ask the user to authenticate it. Whenever instructions or skills require \`gh\`, call this tool and pass the arguments that would follow \`gh\`.

Supported commands:
- gh pr create|list|status|view|checks|diff|comment|review|ready|edit
- gh run list|view|watch|rerun|cancel|download
- gh workflow list|view|run
- gh issue list|view|comment
- gh pr thread list [PR]
- gh pr thread reply THREAD_ID --body TEXT
- gh pr thread resolve THREAD_ID
- gh pr thread reopen THREAD_ID

The repository is fixed by XpressClaw. Arbitrary gh api, authentication, configuration, extensions, repository selection, merging, and checkout are unavailable. Use the shell's full git CLI for branches, fetches, pushes, rebases, cherry-picks, and other Git operations.`;

const REVIEW_LIFECYCLE_DESCRIPTION = `

This ordinary task uses XpressClaw's managed pull-request review lifecycle. A pull request that is ready for a person to review must be published ready for review, never left as a draft. Generic instructions that default to draft are overridden here. After creation, XpressClaw keeps the task active, checks for review feedback, resumes this conversation to address every comment, and completes the task only after approval or merge.`;

export function toolDescription(environment = process.env) {
  return reviewLifecycleEnabled(environment)
    ? `${TOOL_DESCRIPTION}${REVIEW_LIFECYCLE_DESCRIPTION}`
    : TOOL_DESCRIPTION;
}

export function reviewLifecycleEnabled(environment = process.env) {
  return environment.XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE === '1';
}

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

function validateArguments(args) {
  if (!Array.isArray(args) || args.length < 2 || args.some((arg) => typeof arg !== 'string')) {
    throw new Error('args must be an array containing a supported gh command and subcommand');
  }
  if (args.some((arg) => arg.includes('\0'))) {
    throw new Error('gh arguments cannot contain NUL characters');
  }
  if (args.some((arg) =>
    arg === '-R' || arg.startsWith('-R=') || /^-R[^-]/.test(arg) ||
    arg === '--repo' || arg.startsWith('--repo=') ||
    arg === '--hostname' || arg.startsWith('--hostname=') ||
    arg === '--web' || arg === '--editor' ||
    arg.includes('github.com/') || /^https?:\/\//.test(arg)
  )) {
    throw new Error('repository, hostname, URL, browser, and editor overrides are unavailable');
  }

  const [group, command] = args;
  if (!ALLOWED.get(group)?.has(command)) {
    throw new Error(`unsupported gh command: gh ${group} ${command}`);
  }
  if (group === 'pr' && command === 'thread') {
    validateThreadArguments(args.slice(2));
  }
}

function validateThreadArguments(args) {
  const [command] = args;
  if (!['list', 'reply', 'resolve', 'reopen'].includes(command)) {
    throw new Error('supported review-thread commands are list, reply, resolve, and reopen');
  }
  if (command === 'reply') {
    if (!args[1] || !optionValue(args.slice(2), '--body', '-b')) {
      throw new Error('usage: gh pr thread reply THREAD_ID --body TEXT');
    }
  } else if (command !== 'list' && !args[1]) {
    throw new Error(`usage: gh pr thread ${command} THREAD_ID`);
  }
}

function optionValue(args, longName, shortName) {
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === longName || argument === shortName) return args[index + 1];
    if (argument.startsWith(`${longName}=`)) return argument.slice(longName.length + 1);
  }
  return undefined;
}

const PR_CREATE_LONG_VALUE_OPTIONS = new Set([
  '--assignee', '--base', '--body', '--body-file', '--head', '--label',
  '--milestone', '--project', '--recover', '--reviewer', '--template', '--title',
]);
const PR_CREATE_SHORT_VALUE_OPTIONS = new Set([
  'a', 'B', 'b', 'F', 'H', 'l', 'm', 'p', 'r', 'T', 't',
]);
const PR_CREATE_SHORT_BOOLEAN_OPTIONS = new Set(['d', 'e', 'f', 'w']);
const FALSE_BOOLEAN_VALUES = new Set(['0', 'f', 'false']);

function withoutDraftArguments(args) {
  const normalized = args.slice(0, 2);
  let optionsEnded = false;
  let nextIsValue = false;
  for (const argument of args.slice(2)) {
    if (nextIsValue || optionsEnded) {
      normalized.push(argument);
      nextIsValue = false;
      continue;
    }
    if (argument === '--') {
      optionsEnded = true;
      normalized.push(argument);
      continue;
    }
    if (argument === '--draft' || argument.startsWith('--draft=')) continue;
    if (argument === '-d' || argument.startsWith('-d=')) continue;

    if (argument.startsWith('--')) {
      normalized.push(argument);
      nextIsValue = !argument.includes('=') && PR_CREATE_LONG_VALUE_OPTIONS.has(argument);
      continue;
    }
    if (!argument.startsWith('-') || argument === '-') {
      normalized.push(argument);
      continue;
    }

    const shorthands = argument.slice(1);
    let kept = '';
    for (let index = 0; index < shorthands.length; index += 1) {
      const shorthand = shorthands[index];
      if (shorthand === 'd') continue;
      kept += shorthand;
      if (PR_CREATE_SHORT_VALUE_OPTIONS.has(shorthand)) {
        if (index + 1 < shorthands.length) kept += shorthands.slice(index + 1);
        else nextIsValue = true;
        break;
      }
      // Preserve any unknown option and its suffix instead of risking a change
      // to a value accepted by a future gh version.
      if (!PR_CREATE_SHORT_BOOLEAN_OPTIONS.has(shorthand)) {
        kept += shorthands.slice(index + 1);
        break;
      }
    }
    if (kept) normalized.push(`-${kept}`);
  }
  return normalized;
}

export function managedCommandArguments(args, environment = process.env) {
  if (!reviewLifecycleEnabled(environment)) {
    return [...args];
  }
  if (args[0] === 'pr' && args[1] === 'ready' &&
      args.some((argument) => enabledBooleanFlag(argument, '--undo'))) {
    throw new Error(
      'managed pull-request review lifecycle cannot convert a ready pull request back to draft',
    );
  }
  if (args[0] === 'pr' && args[1] === 'create' &&
      args.some((argument) => enabledBooleanFlag(argument, '--dry-run'))) {
    throw new Error(
      'managed pull-request review lifecycle cannot register a dry-run pull request',
    );
  }
  if (args[0] !== 'pr' || args[1] !== 'create') return [...args];
  return withoutDraftArguments(args);
}

function enabledBooleanFlag(argument, name) {
  if (argument === name) return true;
  if (!argument.startsWith(`${name}=`)) return false;
  return !FALSE_BOOLEAN_VALUES.has(argument.slice(name.length + 1).toLowerCase());
}

export function pullRequestUrl(value) {
  const match = String(value ?? '').match(/https:\/\/github\.com\/[^\s/]+\/[^\s/]+\/pull\/\d+/);
  return match?.[0];
}

export function commandResultIsError(output) {
  return output.exit_code !== 0 || output.review_lifecycle?.registered === false;
}

function commandOutput(args) {
  return new Promise((resolve) => {
    const child = spawn(GH, args, {
      cwd: process.cwd(),
      env: {
        ...process.env,
        GH_PROMPT_DISABLED: '1',
        PAGER: 'cat',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      if (stdout.length < MAX_OUTPUT) stdout += chunk.slice(0, MAX_OUTPUT - stdout.length);
    });
    child.stderr.on('data', (chunk) => {
      if (stderr.length < MAX_OUTPUT) stderr += chunk.slice(0, MAX_OUTPUT - stderr.length);
    });
    child.on('error', (cause) => resolve({ exit_code: 127, stdout, stderr: cause.message }));
    child.on('close', (code) => resolve({ exit_code: code ?? 1, stdout, stderr }));
  });
}

function gitCommandOutput(args) {
  return new Promise((resolve) => {
    const child = spawn('git', args, {
      cwd: process.cwd(),
      env: process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      if (stdout.length < MAX_OUTPUT) stdout += chunk.slice(0, MAX_OUTPUT - stdout.length);
    });
    child.stderr.on('data', (chunk) => {
      if (stderr.length < MAX_OUTPUT) stderr += chunk.slice(0, MAX_OUTPUT - stderr.length);
    });
    child.on('error', (cause) => resolve({ exit_code: 127, stdout, stderr: cause.message }));
    child.on('close', (code) => resolve({ exit_code: code ?? 1, stdout, stderr }));
  });
}

async function successfulCommand(args) {
  const output = await commandOutput(args);
  if (output.exit_code !== 0) {
    const failure = new Error(output.stderr.trim() || `gh exited with status ${output.exit_code}`);
    failure.output = output;
    throw failure;
  }
  return output;
}

function repositoryParts(environment = process.env) {
  const repository = environment.GH_REPO ?? '';
  const parts = repository.split('/');
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    throw new Error('XpressClaw did not provide a valid project repository');
  }
  return { owner: parts[0], repo: parts[1] };
}

function commandOptionValue(args, longName, shortName) {
  const target = shortName.slice(1);
  let optionsEnded = false;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (optionsEnded) continue;
    if (argument === '--') {
      optionsEnded = true;
      continue;
    }
    if (argument === longName || argument === shortName) return args[index + 1];
    if (argument.startsWith(`${longName}=`)) return argument.slice(longName.length + 1);
    if (argument.startsWith('--')) {
      if (!argument.includes('=') && PR_CREATE_LONG_VALUE_OPTIONS.has(argument)) index += 1;
      continue;
    }
    if (!argument.startsWith('-') || argument === '-') continue;

    const shorthands = argument.slice(1);
    for (let shorthandIndex = 0; shorthandIndex < shorthands.length; shorthandIndex += 1) {
      const shorthand = shorthands[shorthandIndex];
      if (PR_CREATE_SHORT_VALUE_OPTIONS.has(shorthand)) {
        const attached = shorthands.slice(shorthandIndex + 1);
        if (shorthand === target) {
          const value = attached || args[index + 1];
          return value?.startsWith('=') ? value.slice(1) : value;
        }
        if (!attached) index += 1;
        break;
      }
      if (!PR_CREATE_SHORT_BOOLEAN_OPTIONS.has(shorthand)) break;
    }
  }
  return undefined;
}

function registrationKey(owner, head, base, environment = process.env) {
  const repository = environment.GH_REPO ?? '';
  if (!owner || !head || !base || !repository) {
    throw new Error('could not identify the pull-request head and base branches');
  }
  return createHash('sha256')
    .update(JSON.stringify([repository.toLowerCase(), owner.toLowerCase(), head, base]))
    .digest('hex');
}

async function currentBranch(executeGit = gitCommandOutput) {
  const output = await executeGit(['symbolic-ref', '--quiet', '--short', 'HEAD']);
  if (output.exit_code !== 0 || !output.stdout.trim()) {
    throw new Error('could not determine the current Git branch; pass --head explicitly');
  }
  return output.stdout.trim();
}

async function configuredBaseBranch(head, executeGit = gitCommandOutput) {
  const output = await executeGit(['config', '--get', `branch.${head}.gh-merge-base`]);
  if (output.exit_code === 1) return undefined;
  if (output.exit_code !== 0) {
    throw new Error(output.stderr.trim() || 'could not read the configured pull-request base');
  }
  return output.stdout.trim() || undefined;
}

async function defaultBaseBranch() {
  const output = await successfulCommand([
    'repo', 'view', '--json', 'defaultBranchRef', '--jq', '.defaultBranchRef.name',
  ]);
  const branch = output.stdout.trim();
  if (!branch) throw new Error('could not determine the repository default branch');
  return branch;
}

async function currentPullRequestIdentity(target) {
  const args = ['pr', 'view'];
  if (target) args.push(target);
  args.push('--json', 'baseRefName,headRefName,headRepositoryOwner');
  const output = await successfulCommand(args);
  const value = JSON.parse(output.stdout);
  return {
    owner: value.headRepositoryOwner?.login,
    head: value.headRefName,
    base: value.baseRefName,
  };
}

export async function pullRequestRegistrationKey(args, dependencies = {}) {
  const environment = dependencies.environment ?? process.env;
  const { owner: repositoryOwner } = repositoryParts(environment);
  if (args[0] === 'pr' && args[1] === 'create') {
    const commandArgs = args.slice(2);
    const headOption = commandOptionValue(commandArgs, '--head', '-H');
    let owner = repositoryOwner;
    let head = headOption;
    if (headOption?.includes(':')) {
      [owner, head] = headOption.split(':', 2);
    }
    head ??= await (dependencies.currentBranch ?? currentBranch)(dependencies.executeGit);
    let base = commandOptionValue(commandArgs, '--base', '-B');
    base ??= await (
      dependencies.configuredBaseBranch ?? configuredBaseBranch
    )(head, dependencies.executeGit);
    base ??= await (dependencies.defaultBaseBranch ?? defaultBaseBranch)();
    return registrationKey(owner, head, base, environment);
  }
  if (args[0] === 'pr' && args[1] === 'ready') {
    const identity = await (dependencies.currentPullRequestIdentity ?? currentPullRequestIdentity)(
      readyPullRequestTarget(args),
    );
    return registrationKey(identity.owner ?? repositoryOwner, identity.head, identity.base, environment);
  }
  throw new Error('only pull-request create and ready commands have registration keys');
}

async function graphql(query, fields) {
  const args = ['api', 'graphql', '-f', `query=${query}`];
  for (const [name, value, typed = false] of fields) {
    args.push(typed ? '-F' : '-f', `${name}=${value}`);
  }
  const output = await successfulCommand(args);
  return JSON.parse(output.stdout);
}

async function currentPullRequestNumber() {
  const output = await successfulCommand(['pr', 'view', '--json', 'number', '--jq', '.number']);
  const number = Number.parseInt(output.stdout.trim(), 10);
  if (!Number.isInteger(number)) throw new Error('could not determine the current pull request');
  return number;
}

async function currentPullRequestUrl(target) {
  const args = ['pr', 'view'];
  if (target) args.push(target);
  args.push('--json', 'url', '--jq', '.url');
  const output = await successfulCommand(args);
  const url = pullRequestUrl(output.stdout.trim());
  if (!url) throw new Error('could not determine the current pull-request URL');
  return url;
}

function readyPullRequestTarget(args) {
  if (args[0] !== 'pr' || args[1] !== 'ready') return undefined;
  return args.slice(2).find((argument) => !argument.startsWith('-'));
}

function readyPullRequestAlreadySatisfied(args, output) {
  if (args[0] !== 'pr' || args[1] !== 'ready' || output.exit_code === 0) return false;
  const message = `${output.stdout ?? ''}\n${output.stderr ?? ''}`;
  return /\balready (?:marked as )?ready for review\b/i.test(message);
}

export async function updatePullRequestRegistration(
  phase,
  url,
  registrationId,
  registrationKeyValue,
  environment = process.env,
  fetchImplementation = globalThis.fetch,
) {
  const controlPlane = environment.XPRESSCLAW_URL?.replace(/\/$/, '');
  const taskId = environment.XPRESSCLAW_TASK_ID;
  const agentId = environment.XPRESSCLAW_AGENT_ID;
  if (!controlPlane || !taskId || !agentId) {
    throw new Error('XpressClaw did not provide task review-lifecycle context');
  }
  const body = { phase, agent_id: agentId };
  if (url) body.url = url;
  if (registrationId) body.registration_id = registrationId;
  if (registrationKeyValue) body.registration_key = registrationKeyValue;
  const response = await fetchImplementation(
    `${controlPlane}/api/tasks/${encodeURIComponent(taskId)}/pull-requests`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(15_000),
    },
  );
  const text = await response.text();
  let payload;
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    payload = { message: text };
  }
  if (!response.ok) {
    throw new Error(payload.error ?? payload.message ?? `XpressClaw returned HTTP ${response.status}`);
  }
  return payload;
}

export async function executeCommandWithReviewLifecycle(args, dependencies = {}) {
  const environment = dependencies.environment ?? process.env;
  const execute = dependencies.execute ?? commandOutput;
  const currentUrl = dependencies.currentPullRequestUrl ?? currentPullRequestUrl;
  const registrationId = dependencies.registrationId ?? randomUUID();
  const identifyRegistration = dependencies.registrationKeyForCommand ??
    ((commandArgs) => pullRequestRegistrationKey(commandArgs, { environment }));
  const updateRegistration = dependencies.updateRegistration ??
    ((phase, url, effectiveRegistrationId, registrationKeyValue) =>
      updatePullRequestRegistration(
        phase,
        url,
        effectiveRegistrationId,
        registrationKeyValue,
        environment,
      ));
  const managed = reviewLifecycleEnabled(environment) &&
    args[0] === 'pr' && ['create', 'ready'].includes(args[1]);
  if (!managed) return execute(args);

  let registrationKeyValue;
  let effectiveRegistrationId = registrationId;
  let reusedRegistration = false;
  try {
    registrationKeyValue = await identifyRegistration(args);
    const pending = await updateRegistration(
      'begin',
      undefined,
      registrationId,
      registrationKeyValue,
    );
    if (typeof pending?.registration_id === 'string' && pending.registration_id) {
      effectiveRegistrationId = pending.registration_id;
    }
    reusedRegistration = pending?.reused === true;
  } catch (cause) {
    return {
      exit_code: 1,
      stdout: '',
      stderr: '',
      review_lifecycle: {
        registered: false,
        error: cause instanceof Error ? cause.message : String(cause),
        message: 'XpressClaw could not arm durable review monitoring, so the GitHub command was not run. Retry after correcting the error.',
      },
    };
  }

  const output = await execute(args);
  const alreadyReady = readyPullRequestAlreadySatisfied(args, output);
  if (output.exit_code !== 0 && !alreadyReady) {
    try {
      if (!reusedRegistration) {
        await updateRegistration(
          'cancel',
          undefined,
          effectiveRegistrationId,
          registrationKeyValue,
        );
      }
    } catch (cause) {
      output.review_lifecycle = {
        registered: false,
        error: cause instanceof Error ? cause.message : String(cause),
        message: 'The GitHub command failed and XpressClaw could not clear its durable pre-publication gate. Retry the command or ask the user to intervene.',
      };
    }
    return output;
  }

  try {
    const url = pullRequestUrl(output.stdout) ?? await currentUrl(readyPullRequestTarget(args));
    const state = await updateRegistration(
      'register',
      url,
      effectiveRegistrationId,
      registrationKeyValue,
    );
    output.review_lifecycle = {
      registered: true,
      pull_request: url,
      state,
      message: 'XpressClaw will keep this task active and address review feedback until approval or merge.',
    };
    if (alreadyReady) output.exit_code = 0;
  } catch (cause) {
    output.review_lifecycle = {
      registered: false,
      error: cause instanceof Error ? cause.message : String(cause),
      message: 'The pull request was published, but automatic review monitoring did not finish registering. A durable task gate remains armed; do not declare the task complete, and retry `gh pr ready` after correcting the error.',
    };
  }
  return output;
}

export async function collectReviewThreads(fetchPage) {
  const threads = [];
  const seenCursors = new Set();
  let after;
  for (;;) {
    const response = await fetchPage(after);
    if (Array.isArray(response.errors) && response.errors.length > 0) {
      throw new Error(`GitHub GraphQL returned errors: ${JSON.stringify(response.errors)}`);
    }
    const connection = response.data?.repository?.pullRequest?.reviewThreads;
    if (!connection || !Array.isArray(connection.nodes)) {
      throw new Error('GitHub returned invalid pull-request review threads');
    }
    threads.push(...connection.nodes);
    if (connection.pageInfo?.hasNextPage !== true) return threads;

    const cursor = connection.pageInfo?.endCursor;
    if (typeof cursor !== 'string' || !cursor || seenCursors.has(cursor)) {
      throw new Error('GitHub returned an invalid or repeated review-thread page cursor');
    }
    seenCursors.add(cursor);
    after = cursor;
  }
}

export async function collectReviewThreadComments(threads, fetchPage) {
  const complete = [];
  for (const thread of threads) {
    let connection = thread?.comments;
    if (!connection || !Array.isArray(connection.nodes)) {
      throw new Error('GitHub returned invalid pull-request review-thread comments');
    }
    const comments = [...connection.nodes];
    const seenCursors = new Set();
    while (connection.pageInfo?.hasNextPage === true) {
      const cursor = connection.pageInfo?.endCursor;
      if (typeof cursor !== 'string' || !cursor || seenCursors.has(cursor)) {
        throw new Error('GitHub returned an invalid or repeated review-thread comment page cursor');
      }
      seenCursors.add(cursor);

      const response = await fetchPage(thread.id, cursor);
      if (Array.isArray(response.errors) && response.errors.length > 0) {
        throw new Error(`GitHub GraphQL returned errors: ${JSON.stringify(response.errors)}`);
      }
      connection = response.data?.node?.comments;
      if (!connection || !Array.isArray(connection.nodes)) {
        throw new Error('GitHub returned invalid pull-request review-thread comments');
      }
      comments.push(...connection.nodes);
    }
    complete.push({
      ...thread,
      comments: { ...connection, nodes: comments },
    });
  }
  return complete;
}

async function reviewThreads(args) {
  const command = args[2];
  if (command === 'list') {
    const explicit = args.slice(3).find((arg) => /^\d+$/.test(arg));
    const option = optionValue(args.slice(3), '--pr', '-p');
    const number = Number.parseInt(explicit ?? option ?? '', 10) || await currentPullRequestNumber();
    const { owner, repo } = repositoryParts();
    const query = `query($owner:String!,$repo:String!,$number:Int!,$after:String){
      repository(owner:$owner,name:$repo){
        pullRequest(number:$number){
          number
          reviewThreads(first:100,after:$after){
            nodes{
              id path line originalLine startLine diffSide isResolved isOutdated
              viewerCanReply viewerCanResolve viewerCanUnresolve
              resolvedBy{login}
              comments(first:100){
                nodes{id databaseId author{login} body createdAt updatedAt url replyTo{id}}
                pageInfo{hasNextPage endCursor}
              }
            }
            pageInfo{hasNextPage endCursor}
          }
        }
      }
    }`;
    const threads = await collectReviewThreads(async (after) => {
      const fields = [
        ['owner', owner],
        ['repo', repo],
        ['number', number, true],
      ];
      if (after !== undefined) fields.push(['after', after]);
      return graphql(query, fields);
    });
    const commentsQuery = `query($threadId:ID!,$after:String!){
      node(id:$threadId){
        ... on PullRequestReviewThread{
          comments(first:100,after:$after){
            nodes{id databaseId author{login} body createdAt updatedAt url replyTo{id}}
            pageInfo{hasNextPage endCursor}
          }
        }
      }
    }`;
    const completeThreads = await collectReviewThreadComments(
      threads,
      (threadId, after) => graphql(commentsQuery, [['threadId', threadId], ['after', after]]),
    );
    return {
      exit_code: 0,
      pull_request: number,
      threads: completeThreads,
    };
  }

  const threadId = args[3];
  if (command === 'reply') {
    const body = optionValue(args.slice(4), '--body', '-b');
    const query = `mutation($threadId:ID!,$body:String!){
      addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$threadId,body:$body}){
        comment{id databaseId author{login} body createdAt updatedAt url}
      }
    }`;
    const response = await graphql(query, [['threadId', threadId], ['body', body]]);
    return { exit_code: 0, reply: response.data?.addPullRequestReviewThreadReply?.comment };
  }

  const mutation = command === 'resolve' ? 'resolveReviewThread' : 'unresolveReviewThread';
  const query = `mutation($threadId:ID!){
    ${mutation}(input:{threadId:$threadId}){
      thread{id isResolved viewerCanResolve viewerCanUnresolve resolvedBy{login}}
    }
  }`;
  const response = await graphql(query, [['threadId', threadId]]);
  return { exit_code: 0, thread: response.data?.[mutation]?.thread };
}

async function callTool(argumentsValue) {
  const requestedArgs = argumentsValue?.args;
  validateArguments(requestedArgs);
  const args = managedCommandArguments(requestedArgs);
  if (args[0] === 'pr' && args[1] === 'thread') return reviewThreads(args);
  return executeCommandWithReviewLifecycle(args);
}

async function handle(message) {
  const { id, method, params } = message;
  if (method === 'notifications/initialized' || method === 'notifications/cancelled') return;
  if (method === 'initialize') {
    result(id, {
      protocolVersion: params?.protocolVersion ?? '2024-11-05',
      capabilities: { tools: {} },
      serverInfo: { name: 'xpressclaw-github', version: '0.2.0' },
      instructions: toolDescription(),
    });
    return;
  }
  if (method === 'ping') {
    result(id, {});
    return;
  }
  if (method === 'tools/list') {
    result(id, {
      tools: [{
        name: 'gh',
        description: toolDescription(),
        inputSchema: {
          type: 'object',
          properties: {
            args: {
              type: 'array',
              minItems: 2,
              items: { type: 'string' },
              description: 'Arguments after gh, for example ["pr", "view", "--json", "state,reviewDecision"]',
            },
          },
          required: ['args'],
          additionalProperties: false,
        },
      }],
    });
    return;
  }
  if (method === 'tools/call') {
    if (params?.name !== 'gh') {
      result(id, toolResult({ error: `unknown tool: ${params?.name ?? ''}` }, true));
      return;
    }
    try {
      const output = await callTool(params.arguments);
      result(id, toolResult(output, commandResultIsError(output)));
    } catch (cause) {
      const output = cause?.output ?? { error: cause instanceof Error ? cause.message : String(cause) };
      result(id, toolResult(output, true));
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

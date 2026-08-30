#!/usr/bin/env node

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { dirname, join, basename } from 'node:path';
import { createRequire } from 'node:module';

const destination = '/opt/xpressclaw/deepseek-harness-runtime';
const expectedAdapterVersion = '0.4.24';
const expectedDshVersion = '0.1.1-rc.2';
const expectedRuntimeSha256 = 'bb94c7ba7f09bd7d890dd068bedc2f5f68542d7746ca1b3410c17ac9cf9a17f3';
const npmRoot = execFileSync('npm', ['root', '--global'], { encoding: 'utf8' }).trim();
const adapterManifest = join(npmRoot, '@openma', 'deepseek-harness-acp', 'package.json');
const adapter = JSON.parse(readFileSync(adapterManifest, 'utf8'));
const metadataPath = join(dirname(adapterManifest), 'vendor', 'runtime.json');
const metadata = JSON.parse(readFileSync(metadataPath, 'utf8'));

if (adapter.version !== expectedAdapterVersion) {
  throw new Error(`unexpected DeepSeek Harness ACP adapter version: ${String(adapter.version)}`);
}
if (metadata.archive !== 'dsh-runtime.tgz' || metadata.archive !== basename(metadata.archive)) {
  throw new Error('adapter runtime metadata contains an invalid archive name');
}
if (metadata.dsh !== expectedDshVersion || metadata.sha256 !== expectedRuntimeSha256) {
  throw new Error('adapter runtime metadata does not match the pinned DeepSeek Harness release');
}

const archive = join(dirname(adapterManifest), 'vendor', metadata.archive);
const actualSha256 = createHash('sha256').update(readFileSync(archive)).digest('hex');
if (actualSha256 !== expectedRuntimeSha256) {
  throw new Error(`DeepSeek Harness runtime checksum mismatch: ${actualSha256}`);
}

rmSync(destination, { recursive: true, force: true });
mkdirSync(destination, { recursive: true });
const require = createRequire(adapterManifest);
const tar = require('tar');
tar.x({ cwd: destination, file: archive, sync: true, strict: true });

// The adapter archive includes its exact package-lock plus a preinstalled
// dependency tree assembled on the publisher's architecture. Reinstalling
// from that lock selects the matching native optional packages for this image
// (amd64 or arm64) without independently resolving mutable DSH dependencies.
execFileSync('npm', ['ci', '--omit=dev', '--no-audit', '--no-fund'], {
  cwd: destination,
  env: { ...process.env, npm_config_update_notifier: 'false' },
  stdio: 'inherit',
});

const dshManifest = join(destination, 'node_modules', '@deepseek-ai', 'dsh', 'package.json');
const presets = join(destination, 'node_modules', '@deepseek-ai', 'dsh', 'config', 'agent-presets');
if (!existsSync(dshManifest) || !existsSync(presets)) {
  throw new Error('the pinned adapter runtime does not contain DeepSeek Harness and its presets');
}
const dsh = JSON.parse(readFileSync(dshManifest, 'utf8'));
if (dsh.version !== expectedDshVersion) {
  throw new Error(`adapter runtime expected DSH ${expectedDshVersion}, found ${String(dsh.version)}`);
}

if (!['x64', 'arm64'].includes(process.arch)) {
  throw new Error(`unsupported DeepSeek Harness runner architecture: ${process.arch}`);
}
for (const nativePackage of [
  `@deepseek-ai/node-addon-landlock-run-linux-${process.arch}`,
  `@img/sharp-linux-${process.arch}`,
  `@koromix/koffi-linux-${process.arch}`,
  `@vscode/ripgrep-linux-${process.arch}`,
  `node-addon-require-builtin-linux-${process.arch}-gnu`,
]) {
  if (!existsSync(join(destination, 'node_modules', ...nativePackage.split('/')))) {
    throw new Error(`the locked DSH runtime omitted target package ${nativePackage}`);
  }
}

process.stdout.write(`Prepared DeepSeek Harness ${dsh.version} for linux/${process.arch} from the adapter's verified lockfile.\n`);

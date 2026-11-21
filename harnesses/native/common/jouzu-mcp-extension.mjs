import { readFileSync } from 'node:fs';

export default function xpressclawJouzuMcpExtension(pi) {
  const configPath = process.env.XPRESSCLAW_JOUZU_MCP_CONFIG;
  if (!configPath) return;
  const parsed = JSON.parse(readFileSync(configPath, 'utf8'));
  const servers = parsed?.mcpServers;
  if (!servers || typeof servers !== 'object' || Array.isArray(servers)) return;
  for (const [name, config] of Object.entries(servers)) {
    if (config && typeof config === 'object' && !Array.isArray(config)) {
      pi.registerMcpServer(name, config);
    }
  }
}

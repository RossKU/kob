#!/usr/bin/env node
// kob-mcp over stdio: node server/index.mjs --config <file> (or KOB_MCP_CONFIG). Logs go to stderr only.

import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { loadConfig } from '../src/config.mjs';
import { ensureDataDir } from '../src/service.mjs';
import { createServer } from '../src/tools.mjs';

const i = process.argv.indexOf('--config');
const given = (i > 0 ? process.argv[i + 1] : process.env.KOB_MCP_CONFIG)?.trim();
const file = given && !given.startsWith('${') ? given : null; // an unset form field may arrive as its placeholder
let cfg;
try {
  cfg = loadConfig(file);
  ensureDataDir(cfg);
} catch (e) {
  process.stderr.write(`kob-mcp: ${e.message}\n`);
  process.exit(2);
}
const { server } = createServer(cfg);
await server.connect(new StdioServerTransport());
process.stderr.write(`kob-mcp: ready on ${cfg.network}, mode ${cfg.mode}\n`);

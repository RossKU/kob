// Tests the server over stdio, as Claude Desktop or Claude Code runs it.
// It lists the tools, calls reads and calls a confirmed write.
import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { HERE, PRIMARY, setup } from './helpers.mjs';

const body = (r) => JSON.parse(r.content[0].text);

test('stdio: tools are listed and called; a write plans, then sends on confirm', async () => {
  const t = setup();
  const file = join(t.dir, 'config.json');
  writeFileSync(file, JSON.stringify(t.raw));
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [join(HERE, '..', 'server', 'index.mjs'), '--config', file],
    env: { ...process.env },
    stderr: 'pipe',
  });
  const client = new Client({ name: 'kob-mcp-test', version: '0.0.0' });
  await client.connect(transport);
  try {
    const { tools } = await client.listTools();
    const names = tools.map((x) => x.name);
    for (const n of ['kob_status', 'kob_book', 'kob_balances', 'kob_place_limit', 'kob_cancel', 'kob_take', 'kob_send', 'kob_token_create']) assert.ok(names.includes(n), n);
    const takeTool = tools.find((x) => x.name === 'kob_take');
    assert.equal(takeTool.annotations.destructiveHint, true);
    assert.ok(takeTool.inputSchema.properties.max_price);
    assert.ok(takeTool.inputSchema.required.includes('token'));
    assert.equal(tools.find((x) => x.name === 'kob_book').annotations.readOnlyHint, true);
    assert.equal(names.includes('kob_claim'), false, 'this CLI has no kob claim');
    assert.equal(tools.find((x) => x.name === 'kob_place_limit').inputSchema.properties.delivery, undefined);

    const book = body(await client.callTool({ name: 'kob_book', arguments: { token: 'TUSD' } }));
    assert.equal(book.best_bid, '24000000');

    const status = await client.callTool({ name: 'kob_status', arguments: {} });
    assert.equal(status.content[0].text.includes(t.cfg.keyFile), false, 'no key path');
    assert.equal(body(status).wallet.address.startsWith('kaspatest:'), true);
    assert.match(body(status).cli.claim, /not in this CLI build/);

    const args = { token: 'TUSD', side: 'buy', amount: '2TUSD', price: '0.25KAS' };
    const plan = body(await client.callTool({ name: 'kob_place_limit', arguments: args }));
    assert.equal(plan.status, 'plan');
    assert.equal(t.yesCalls().length, 0);
    const sent = await client.callTool({ name: 'kob_place_limit', arguments: { ...args, confirm: plan.planId } });
    assert.equal(sent.isError, undefined);
    assert.equal(body(sent).status, 'submitted');
    assert.equal(t.yesCalls().length, 1);

    const refused = await client.callTool({ name: 'kob_send', arguments: { asset: 'KAS', to: 'kaspatest:qreyws54r9ku5r7v7mwwmz2hdf603u7w0n807rhh66d5u2lwsxan208z2433y', amount: '1KAS' } });
    assert.equal(refused.isError, true);
    assert.match(body(refused).reason, /^NOT_ALLOW_LISTED/);
    const okSend = body(await client.callTool({ name: 'kob_send', arguments: { asset: 'KAS', to: PRIMARY, amount: '1KAS' } }));
    assert.equal(okSend.status, 'plan');

    const bad = await client.callTool({ name: 'kob_take', arguments: { id: 'ab'.repeat(32), token: '55'.repeat(32), amount: '1TUSD' } });
    assert.equal(bad.isError, true);
    assert.match(body(bad).reason, /^PRICE_BOUND_REQUIRED/);
  } finally {
    await client.close();
  }
});

test('stdio: a config that asks for mainnet without allowMainnet does not start', async () => {
  const t = setup();
  const file = join(t.dir, 'config.json');
  writeFileSync(file, JSON.stringify({ ...t.raw, network: 'mainnet', withdrawTo: [] }));
  const client = new Client({ name: 'kob-mcp-test', version: '0.0.0' });
  const transport = new StdioClientTransport({ command: process.execPath, args: [join(HERE, '..', 'server', 'index.mjs'), '--config', file], stderr: 'pipe' });
  await assert.rejects(client.connect(transport));
});

test('stdio: a CLI with kob claim and --delivery gets kob_claim and the delivery argument', async () => {
  const t = setup();
  const file = join(t.dir, 'config.json');
  writeFileSync(file, JSON.stringify(t.raw));
  const transport = new StdioClientTransport({ command: process.execPath, args: [join(HERE, '..', 'server', 'index.mjs'), '--config', file],
    env: { ...process.env, FAKE_KOB_FEATURES: 'claim,delivery' }, stderr: 'pipe' });
  const client = new Client({ name: 'kob-mcp-test', version: '0.0.0' });
  await client.connect(transport);
  try {
    const { tools } = await client.listTools();
    assert.ok(tools.some((x) => x.name === 'kob_claim'));
    assert.deepEqual(tools.find((x) => x.name === 'kob_place_limit').inputSchema.properties.delivery.enum, ['each', 'collected', 'one-utxo']);
    assert.match(body(await client.callTool({ name: 'kob_status', arguments: {} })).cli.claim, /offered/);
  } finally {
    await client.close();
  }
});

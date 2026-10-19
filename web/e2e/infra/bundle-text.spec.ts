// The built bundle (what the browser runs: the app, the planners' and the decoder's messages, the SDKs) never says "base units" to a user
// (founder decision 2026-10-17); a token's amounts are whole tokens. Runs on dist/, which the e2e web server builds first.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test, expect } from '@playwright/test';

test('the built bundle carries no "base units" text', () => {
  const dir = join(process.cwd(), 'dist', 'assets');
  const hits: string[] = [];
  for (const name of readdirSync(dir).filter((n) => n.endsWith('.js'))) {
    const text = readFileSync(join(dir, name), 'utf8');
    for (const m of text.matchAll(/.{0,50}base[ -]units?\b.{0,30}/gi)) hits.push(`${name}: ${m[0]}`);
  }
  expect(hits).toEqual([]);
});

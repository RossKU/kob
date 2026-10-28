// KOB's own genesis check and kascov's remark (founder decision 2026-10-28). The market list shows a token that kascov.io marks verified with the chip
// "Verified by kascov" (third party, labelled; the list asks for nothing more). A token page checks the genesis itself, once, from the creation bytes of
// REAL tokens recorded on 2026-10-28 (src/testing/genesis-chain.json: kascov.io's per-token file and the Kaspa REST explorer, both MOCKED here) with the real
// kob-wasm rules: "Genesis verified" (KOB's own fact, replacing the chip), "Genesis check failed" (a warning, even though kascov says verified) or
// "Genesis not verified" with the reason when the history is not available. Desktop and a 375 px phone.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';
import { mockExplorer, mockKascov, programHash } from '../helpers/kascov';

test.use({ autoOpen: false });

interface Recorded {
  name: string;
  covenantId: string;
  ticker: string | null;
  kascov: { genesis_txid: string; utxos: { value: number }[] };
  explorerTx: unknown;
}
const chain = JSON.parse(readFileSync(new URL('../../src/testing/genesis-chain.json', import.meta.url), 'utf8')) as { cases: Recorded[] };
const rec = (name: string): Recorded => chain.cases.find((c) => c.name === name)!;
const BRW = rec('brw'); // testnet-10, one holder output, a KCC-23 declaration
const KQA = rec('kqa-publicmint-seed'); // testnet-10, a public minter (allowance 100000) and an unspent TokenSeed
const KUSDT = rec('kusdt-contaminated'); // testnet-10, a hidden output
const DAWDAW = rec('dawdaw-unspent-curve'); // testnet-10, an unspent launchpad curve cell nobody revealed
const PM = programHash('KCC20PublicMint');

const VIEWPORTS = [
  { name: 'desktop 1280x800', width: 1280, height: 800 },
  { name: 'phone 375 px', width: 375, height: 812 },
] as const;

const files = (...cases: Recorded[]): Record<string, unknown> => Object.fromEntries(cases.map((c) => [c.covenantId, c.kascov]));
const listed = (c: Recorded, status = 'verified') => ({ covenant_id: c.covenantId, claimed_ticker: c.ticker ?? 'TKN', claimed_name: `${c.ticker} token`, program: PM, status });

async function open(page: Page, route: string, o: { tokens: ReturnType<typeof listed>[]; files?: Record<string, unknown>; txs?: Record<string, unknown> }) {
  await listExkcc(page);
  const kascov = await mockKascov(page, { tokens: o.tokens, files: o.files ?? {} });
  const explorer = await mockExplorer(page, { txs: o.txs ?? {} });
  await page.goto(`/#${route}`);
  return { kascov, explorer };
}

for (const vp of VIEWPORTS) {
  test.describe(vp.name, () => {
    test.use({ viewport: { width: vp.width, height: vp.height } });

    test('the list: a token kascov marks verified shows the chip "Verified by kascov", labelled as a third party; any other keeps "Genesis not verified"; no extra request', async ({ appPage: page }) => {
      const { kascov, explorer } = await open(page, '/market', { tokens: [listed(BRW, 'verified'), listed(KQA, 'unvalidated')] });
      const brw = page.getByTestId(`token-facts-${BRW.covenantId}`);
      await expect(brw.getByTestId('token-fact-genesis_kascov')).toHaveText('Verified by kascov');
      await expect(brw.getByTestId('token-fact-genesis_unverified')).toHaveCount(0);
      await expect(brw.getByTestId('token-fact-genesis_verified')).toHaveCount(0);
      // third party, and not KOB's own check: the tooltip says so
      const tip = await brw.getByTestId('token-fact-genesis_kascov').getAttribute('title');
      expect(tip).toContain('kascov.io, a third-party explorer');
      expect(tip).toContain('KOB has not checked it itself');
      const kqa = page.getByTestId(`token-facts-${KQA.covenantId}`);
      await expect(kqa.getByTestId('token-fact-genesis_unverified')).toHaveText('Genesis not verified');
      await expect(kqa.getByTestId('token-fact-genesis_kascov')).toHaveCount(0);
      // the legend explains both
      await page.getByTestId('legend-badges').locator('summary').click();
      await expect(page.getByTestId('legend-fact-genesis_kascov')).toContainText('Verified by kascov');
      await expect(page.getByTestId('legend-fact-genesis_verified')).toContainText('KOB checked this token');
      await expect(page.getByTestId('legend-fact-genesis_failed')).toContainText('failed');
      // the list makes no request for a token's file and none to an explorer
      expect(kascov.fileRequests).toEqual([]);
      expect(explorer.requests).toEqual([]);
      expect(await horizontalOverflow(page)).toBe(0);
    });

    test('the page checks the genesis itself, once: "Genesis verified" (KOB\'s own fact) replaces the chip, and says what it means', async ({ appPage: page }) => {
      const { kascov, explorer } = await open(page, `/market/${BRW.covenantId}`, { tokens: [listed(BRW)], files: files(BRW) });
      const header = page.getByTestId('token-header');
      const badge = header.getByTestId('token-fact-genesis_verified');
      await expect(badge).toHaveText('Genesis verified');
      await expect(badge).toHaveClass(/badge-ok/);
      await expect(header.getByTestId('token-fact-genesis_kascov')).toHaveCount(0);
      await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveCount(0);
      await expect(header.getByTestId('genesis-failed-warning')).toHaveCount(0);
      // the sentence is the badge's tooltip
      await expect(badge).toHaveAttribute('title', /KOB checked this token's creation itself/);
      // one token file, no explorer (kascov's file carried everything the check needs)
      expect(kascov.fileRequests).toEqual([BRW.covenantId]);
      expect(explorer.requests).toEqual([]);
      expect(await horizontalOverflow(page)).toBe(0);
    });

    test('a public mint token: "verified" does not read as a fixed supply, the allowance is said', async ({ appPage: page }) => {
      await open(page, `/market/${KQA.covenantId}`, { tokens: [listed(KQA)], files: files(KQA) });
      const header = page.getByTestId('token-header');
      await expect(header.getByTestId('token-fact-genesis_verified')).toHaveText('Genesis verified');
      // its genesis minter has been spent since: some of the allowance may be minted, so it is "up to"
      const allowance = header.getByTestId('token-fact-mint_allowance');
      await expect(allowance).toBeVisible();
      await expect(allowance).toContainText('Mint left: up to');
      await expect(allowance).toHaveAttribute('title', /does not mean a fixed supply/);
      await expect(allowance).toHaveAttribute('title', /has not traced how much is left/);
      expect(await horizontalOverflow(page)).toBe(0);
    });

    test('a failure is a warning, even though kascov says the token is verified: data that does not match the covenant id', async ({ appPage: page }) => {
      const tampered = JSON.parse(JSON.stringify(BRW.kascov)) as typeof BRW.kascov;
      tampered.utxos[0]!.value += 1;
      await open(page, `/market/${BRW.covenantId}`, { tokens: [listed(BRW, 'verified')], files: { [BRW.covenantId]: tampered } });
      const header = page.getByTestId('token-header');
      await expect(header.getByTestId('token-fact-genesis_failed')).toHaveText('Genesis check failed');
      await expect(header.getByTestId('token-fact-genesis_failed')).toHaveClass(/badge-warn/);
      const warning = header.getByTestId('genesis-failed-warning');
      await expect(warning).toBeVisible();
      await expect(warning).toContainText('does not match the token\'s covenant id');
      await expect(header.getByTestId('token-fact-genesis_kascov')).toHaveCount(0);
      await expect(header.getByTestId('token-fact-genesis_verified')).toHaveCount(0);
      expect(await horizontalOverflow(page)).toBe(0);
    });

    test('a genesis with an output that is not the token\'s program fails as contaminated, whatever kascov says', async ({ appPage: page }) => {
      await open(page, `/market/${KUSDT.covenantId}`, { tokens: [listed(KUSDT, 'verified')], files: files(KUSDT) });
      const header = page.getByTestId('token-header');
      await expect(header.getByTestId('token-fact-genesis_failed')).toBeVisible();
      await expect(header.getByTestId('genesis-failed-warning')).toContainText('not an instance of its token program');
      await expect(header.getByTestId('token-fact-genesis_verified')).toHaveCount(0);
    });

    test('the history is not available: the token stays "Genesis not verified" with the reason (kascov\'s chip stays as it was, with the reason in its tooltip)', async ({ appPage: page }) => {
      // kascov does not say verified: "Genesis not verified: history not available"
      await open(page, `/market/${BRW.covenantId}`, { tokens: [listed(BRW, 'unvalidated')], files: {} });
      const header = page.getByTestId('token-header');
      await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveText('Genesis not verified: history not available');
      await expect(header.getByTestId('token-fact-genesis_verified')).toHaveCount(0);
      await expect(header.getByTestId('genesis-failed-warning')).toHaveCount(0);
      await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveAttribute('title', /KOB tried to check it and could not/);
      expect(await horizontalOverflow(page)).toBe(0);
    });

    test('an unspent curve cell nobody revealed: unavailable, the chip stays and nothing is claimed', async ({ appPage: page }) => {
      await open(page, `/market/${DAWDAW.covenantId}`, { tokens: [listed(DAWDAW, 'verified')], files: files(DAWDAW), txs: { [DAWDAW.kascov.genesis_txid]: DAWDAW.explorerTx } });
      const header = page.getByTestId('token-header');
      await expect(header.getByTestId('token-fact-genesis_kascov')).toHaveText('Verified by kascov');
      await expect(header.getByTestId('token-fact-genesis_kascov')).toHaveAttribute('title', /KOB tried to check it and could not/);
      await expect(header.getByTestId('token-fact-genesis_verified')).toHaveCount(0);
      await expect(header.getByTestId('token-fact-genesis_failed')).toHaveCount(0);
    });

    test('kascov\'s file is unreachable: "Genesis not verified: sources unreachable", and the page is otherwise as before', async ({ appPage: page }) => {
      await open(page, `/market/${KQA.covenantId}`, { tokens: [listed(KQA, 'unvalidated')], files: { [KQA.covenantId]: 'down' } });
      const header = page.getByTestId('token-header');
      await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveText('Genesis not verified: sources unreachable');
      await expect(page.getByTestId('token-page')).toBeVisible();
    });
  });
}

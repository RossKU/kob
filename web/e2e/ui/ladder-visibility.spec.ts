// The race of quick successive sends with a wallet that looks every input up on its OWN node (KasWare 0.11.5; the mock wallets do so by
// default): a ladder's next level spends the change of the level before, which KOB's node accepted a moment ago. The wallet is asked to sign
// only once KOB's node lists that change and a margin for the wallet's node passed (src/wallet/visibility.ts, on every network), also
// when it was handed to that node (KasWare pushTx); a wallet that still cannot see it is asked again.
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType, placeLadder, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

test.describe('chain signing: a ladder spending the change of its previous level', () => {
  test.describe('KasWare: its node sees a new output only when it is pushed', () => {
    test.use({ walletOptions: { walletNodeLagMs: 600_000 } });
    test('each level is handed to the wallet node, and the next one is asked after the node lists the change and the margin', async ({ appPage: page, mock, wallet }) => {
      // ONE coin: every next level funds itself from the change of the level before
      await fund(mock, 'alice', { utxos: 1 });
      await openMarket(page, mock);
      await pickType(page, 'limit', 'buy');
      await fillFields(page, { amount: '1', price: '0.0245', 'ladder.on': true, 'ladder.levels': '3', 'ladder.pct': '2' });
      await waitReviewable(page);
      const placed = await placeLadder(page, mock, 3);
      const calls = await wallet!.calls();
      expect(calls.map((c) => c.status)).toEqual(['signed', 'signed', 'signed']);
      // levels 2 and 3 spend the change of the level before: only the push made it visible to the wallet
      for (const k of [1, 2]) expect(calls[k]!.tx.inputs.map((i: any) => i.transactionId)).toContain(placed[k - 1]!.submission.txid);
      expect((await wallet!.pushes()).map((p) => p.txid)).toEqual(placed.map((p) => p.submission.txid));
    });
  });

  test.describe('Kaspire (no pushTx): its node sees a new output 2.5 s late', () => {
    test.use({ walletId: 'kaspire', walletOptions: { walletNodeLagMs: 2500 } });
    test('the next level waits for the wallet node (and asks again once), never signing an input the wallet cannot see', async ({ appPage: page, mock, wallet }) => {
      await fund(mock, 'alice', { utxos: 1 });
      await openMarket(page, mock, { wallet: 'kaspire' });
      await pickType(page, 'limit', 'buy');
      await fillFields(page, { amount: '1', price: '0.0245', 'ladder.on': true, 'ladder.levels': '2', 'ladder.pct': '2' });
      await waitReviewable(page);
      const placed = await placeLadder(page, mock, 2);
      const calls = await wallet!.calls();
      const signed = calls.filter((c) => c.status === 'signed');
      expect(signed).toHaveLength(2);
      expect(signed[1]!.tx.inputs.map((i: any) => i.transactionId)).toContain(placed[0]!.submission.txid);
      // a refusal (the wallet's node not there yet) is never a signature: at most the gate's retries
      expect(calls.filter((c) => c.status === 'error').every((c) => /is not an unspent output/.test(c.error ?? ''))).toBe(true);
      expect(signed[1]!.at - signed[0]!.at).toBeGreaterThanOrEqual(1500);
    });
  });
  test.describe('a sell ladder whose node lists a new output only once a block holds it (3 s here)', () => {
    test('each level spends the token change of the level before, which the page counts as held while the node does not list it yet', async ({ appPage: page, mock, wallet }) => {
      test.setTimeout(120_000);
      // ONE token UTXO and ONE coin: every next level spends the token change and the KAS change of the level before (TN10 2026-11-14: level 3
      // was planned without the token change of level 2, still in the mempool, and stopped with "you hold 0")
      await fund(mock, 'alice', { utxos: 1 });
      await mock.nodeMempool(3000);
      await openMarket(page, mock);
      await pickType(page, 'limit', 'sell');
      await fillFields(page, { amount: '1', price: '0.03', 'ladder.on': true, 'ladder.levels': '3', 'ladder.pct': '2' });
      await waitReviewable(page);
      const placed = await placeLadder(page, mock, 3);
      // the three sells listed under the ticket
      await expect(page.getByText('Open orders (3)')).toBeVisible({ timeout: 30_000 });
      await page.screenshot({ path: test.info().outputPath('sell-ladder-3-legs.png'), fullPage: true });
      const calls = await wallet!.calls();
      expect(calls.map((c) => c.status)).toEqual(['signed', 'signed', 'signed']);
      // the visibility gate still holds: the wallet is asked for the next level only once the node lists the change it spends
      for (const k of [1, 2]) expect(calls[k]!.at - calls[k - 1]!.at).toBeGreaterThanOrEqual(2500);
      const tokenIns = (k: number) => (placed[k]!.submission.spent ?? []).filter((x) => x.kind === 'token').map((x) => x.outpoint.split(':')[0]);
      // levels 2 and 3 spend the token change of the level before
      for (const k of [1, 2]) expect(tokenIns(k)).toContain(placed[k - 1]!.submission.txid);
      expect(placed.map((p) => p.submission.created.length)).toEqual([1, 1, 1]);
    });
  });
});

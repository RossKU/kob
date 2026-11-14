// The race of quick successive sends with a wallet that looks every input up on its OWN node (KasWare 0.11.5; the mock wallets do so by
// default): a ladder's next level spends the change of the level before, which KOB's node accepted a moment ago. The wallet is asked to sign
// only once that change is visible to the wallet's node (src/wallet/visibility.ts, on every network): at once when it was handed to that
// node (KasWare pushTx), else once KOB's node lists it plus a margin, asking again if the wallet still cannot see it.
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType, placeLadder, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

test.describe('chain signing: a ladder spending the change of its previous level', () => {
  test.describe('KasWare: its node sees a new output only when it is pushed', () => {
    test.use({ walletOptions: { walletNodeLagMs: 600_000 } });
    test('each level is handed to the wallet node, so the next one signs at once', async ({ appPage: page, mock, wallet }) => {
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
});

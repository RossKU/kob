// The paywall asks for `confirmed` by default and serves a settlement only when it shows the finality its offer asked for.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { PaymentRequired } from '../src/types.ts';
import { startRig } from './helpers/env.ts';
import { defaultSettlement } from './helpers/stub-facilitator.ts';

test('the default offer asks for confirmed finality', async () => {
  const rig = await startRig();
  try {
    const pr = (await (await fetch(`${rig.base}/report`)).json()) as PaymentRequired;
    assert.equal((pr.accepts[0]!.extra as { finality?: string }).finality, 'confirmed');
    const paid = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(paid.response.status, 200);
  } finally {
    await rig.close();
  }
});

test('a settlement weaker than the offer, or without its finality, is not served', async () => {
  for (const finality of ['accepted', undefined]) {
    const rig = await startRig({
      settle: (_n, fr) => {
        const s = defaultSettlement(fr);
        const kaspa = { ...(s.extensions!.kaspa as Record<string, unknown>) };
        if (finality === undefined) delete kaspa.finality;
        else kaspa.finality = finality;
        return { body: { ...s, extensions: { ...s.extensions, kaspa } } };
      },
    });
    try {
      const r = await rig.client.paidFetch(`${rig.base}/report`).catch((e: unknown) => e);
      assert.ok(r instanceof Error || (r as { response: Response }).response.status === 502, String(finality));
      assert.equal(rig.handled.length, 0, String(finality));
    } finally {
      await rig.close();
    }
  }
  // an offer that asks for accepted is served at accepted
  const rig = await startRig({ paywall: { finality: 'accepted' } });
  try {
    assert.equal((await rig.client.paidFetch(`${rig.base}/report`)).response.status, 200);
  } finally {
    await rig.close();
  }
});

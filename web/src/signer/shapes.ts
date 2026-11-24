// The private-key frame signs only a closed list of transaction shapes (R9FB-1, R9FB2-1, R9FB2-2).
//
// The frame has no node facts and no planner claims, so a general decoder cannot be made sound
// against a hostile app by adding conditions to it. Each shape here names exactly which inputs may
// be present (by what the wallet's signature binds), which outputs, and what each output must be. A
// transaction that matches no shape, or one shape plus anything extra, is refused with one sentence.
//
// What a shape may rest on (R9-FB2's BOUND table): every output in full (value, script, covenant
// binding); every outpoint; the payload; the wallet-signed inputs' scripts and amounts, so their plan
// states (the decoder checks each script against its plan); a genesis id recomputed from an outpoint
// and its outputs (kob/covenant-id.ts). Never `roles`, `expected`, a hint, or a claimed covenant id.
// Of an unsigned input only its presence and outpoint count.
//
// The shapes:
// - send: the wallet's own coins and tokens to its own key and to other keys (each on its own line).
// - place: send plus orders this transaction creates (placement records re-derived as genesis) and
//   their custodies; payments to other keys are lines.
// - take: fills of other makers' orders, nothing of the wallet's orders, no new order. Each payment
//   to a maker is its own line; the price is from the wallet's bound net.
// - end (cancel one, cancel-all groups, refund, a cancel-replace's cancel half, a claim that ends):
//   orders of the wallet the wallet signs, none continuing; no payment to another key; what each
//   held comes back (the decoder's output-side rule), plus new orders for a replace.
// - continue (amend in place, sweep, a claim that continues): orders of the wallet that continue as
//   one output each under their TRUE covenant id; no payment to another key; no order ends; no new
//   order. A token output owned by a covenant is the continuing order's custody.
//
// What the frame refuses ("this action needs a wallet extension"): a fill next to an own order or a
// placement; an order ending next to one continuing; a transaction with a signed input that is not a
// coin, token or order of the wallet's key.
import { isGenesisBinding } from '../kob/covenant-id';
import type { DecodedInput, DecodedOutput, SigningSummary } from '../kob/decode';
import { custodiesOf } from '../kob/order-facts';
import { longId } from '../kob/registry';
import type { BuiltTx, Hex, OrderState } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { formatKas } from '../kob/units';
import { t } from '../i18n';

export type FrameShape = 'send' | 'place' | 'take' | 'end' | 'continue';

export interface ShapeLine { label: string; value: string; dir: 'in' | 'out' | null }
export type ShapeVerdict = { ok: true; shape: FrameShape; lines: ShapeLine[] } | { ok: false; reason: string };

const refuse = (reason: string): ShapeVerdict => ({ ok: false, reason });

/** The KAS address-like label of an output's recipient (8 + 8 hex of the key, or "a script"). */
const recipientOf = (o: DecodedOutput): string => (o.recipient ? longId(o.recipient) : t('signer.net.unknownScript'));

/** Judges a decoded transaction's shape for the frame. */
export function frameShape(s: SigningSummary, built: BuiltTx, maker: Hex, kob: KobWasm): ShapeVerdict {
  const tx = built.tx;
  const signed = s.inputs.filter((d) => d.willSign);
  const unsigned = s.inputs.filter((d) => !d.willSign);
  const isCoin = (d: DecodedInput) => d.type === 'kas' && d.ownedBy === 'maker';
  const isOwnToken = (d: DecodedInput) => d.type === 'token' && d.ownedBy === 'maker' && !!d.token && (d.token.owner === maker || !!d.token.locked);
  const isOwnOrder = (d: DecodedInput) => d.type === 'order' && !!d.order?.makerIsWallet;
  const stray = signed.find((d) => !isCoin(d) && !isOwnToken(d) && !isOwnOrder(d));
  if (stray) return refuse(t('signer.shape.signedOther', { input: stray.index }));
  const own = signed.filter(isOwnOrder);
  // an order input the wallet does not sign is a fill of a resting order, whoever its claimed maker
  const others = unsigned.filter((d) => d.type === 'order');
  const continues = (i: number) => tx.outputs.some((x) => x.covenant?.authorizingInput === i && !isGenesisBinding(kob, tx, i, x.covenant.covenantId));
  const continuing = own.filter((d) => continues(d.index));
  const ending = own.filter((d) => !continues(d.index));
  const created = s.orders.filter((o) => o.amendedFrom === undefined && o.sweptFrom === undefined && o.claimedFrom === undefined);
  const createdIds = new Set(created.map((o) => o.covenantId.toLowerCase()));
  const continuedIds = new Set<string>();
  for (const d of continuing) for (const x of tx.outputs) if (x.covenant?.authorizingInput === d.index && !isGenesisBinding(kob, tx, d.index, x.covenant.covenantId)) continuedIds.add(x.covenant.covenantId.toLowerCase());
  const kinds = new Set(s.outputs.map((o) => o.kind));
  const payments = s.outputs.filter((o) => o.kind === 'payment-out' || o.kind === 'token-out');
  const fillOuts = s.outputs.filter((o) => o.kind === 'fill-payout' || o.kind === 'fill-continue');
  if (kinds.has('unknown') || kinds.has('unknown-covenant')) return refuse(t('signer.shape.unknownOutput'));
  // a token output owned by a covenant is a custody of an order created here or of one that continues
  const custodyOf = (o: DecodedOutput) => o.token?.escrowOf?.toLowerCase() ?? null;
  const badCustody = s.outputs.find((o) => o.kind === 'custody' && !createdIds.has(custodyOf(o) ?? '') && !continuedIds.has(custodyOf(o) ?? ''));
  if (badCustody) return refuse(t('signer.shape.custodyElsewhere', { output: badCustody.index }));
  const lines: ShapeLine[] = [];
  const paid = (os: readonly DecodedOutput[]) => {
    for (const o of os) {
      if (o.token && o.kind !== 'fill-payout') continue;
      // tokens delivered to a maker of an order this fills: their amount, from the bound output state
      if (o.token) lines.push({ label: t('signer.shape.delivers'), value: t('signer.shape.deliversValue', { amount: String(o.token.amount), token: longId(o.token.ref.covenantId), to: recipientOf(o) }), dir: 'out' });
      else if (o.value > 0n) lines.push({ label: t('signer.shape.pays'), value: t('signer.shape.paysValue', { amount: `${formatKas(o.value)} KAS`, to: recipientOf(o) }), dir: 'out' });
    }
  };

  if (others.length) {
    // take: fills of other makers' orders and nothing else of the wallet's
    if (own.length) return refuse(t('signer.shape.fillWithOwn'));
    if (created.length) return refuse(t('signer.shape.fillWithPlace'));
    paid(fillOuts);
    paid(payments);
    // the fills' amount is the app's word (claimed arguments): it must be what the bound outputs give
    // the wallet (R9FB2-6), and the price line is the wallet's bound net, fee included
    if (s.take) {
      const tok = s.net.tokens.find((x) => x.ref.covenantId === s.take!.ref.covenantId);
      const got = tok ? (tok.walletDelta < 0n ? -tok.walletDelta : tok.walletDelta) : 0n;
      if (got !== s.take.amount) return refuse(t('signer.shape.takeMismatch'));
      const kas = s.net.kas.walletDelta;
      lines.push({ label: t('signer.shape.takeNet'), value: t('signer.shape.takeNetValue', { kas: `${formatKas(kas < 0n ? -kas : kas)} KAS`, tokens: String(got) }), dir: kas < 0n ? 'out' : 'in' });
    }
    return { ok: true, shape: 'take', lines };
  }
  if (fillOuts.length) return refuse(t('signer.shape.fillWithoutOrder'));
  if (!own.length) {
    paid(payments);
    return { ok: true, shape: created.length ? 'place' : 'send', lines };
  }
  if (ending.length && continuing.length) return refuse(t('signer.shape.endAndContinue'));
  if (payments.length) return refuse(t('signer.shape.payWithOrder'));
  if (continuing.length) {
    // a claim may place what it claims (claim-place): the new orders' terms are shown in full
    const claims = continuing.every((d) => s.orders.some((o) => o.claimedFrom === d.index));
    if (created.length && !claims) return refuse(t('signer.shape.continueWithPlace'));
    // a continuing order keeps what its next state holds: a custody output under its true id of
    // exactly that amount, or (the custody untouched) nothing of that token reaching the wallet
    // beyond what is less than the custody (strays); the decoder's output-side rule checks the latter
    for (const d of continuing) {
      const next = s.orders.find((o) => o.amendedFrom === d.index || o.sweptFrom === d.index || o.claimedFrom === d.index);
      const id = tx.outputs.find((x) => x.covenant?.authorizingInput === d.index && !isGenesisBinding(kob, tx, d.index, x.covenant.covenantId))!.covenant!.covenantId.toLowerCase();
      if (!next) return refuse(t('signer.shape.unknownOutput'));
      let held: { token: Hex; amount: bigint }[] = [];
      try {
        held = custodiesOf(next.state as OrderState, kob).filter((c) => c.amount > 0n);
      } catch {
        return refuse(t('signer.shape.unknownOutput'));
      }
      for (const c of held) {
        const kept = s.outputs.filter((o) => o.kind === 'custody' && custodyOf(o) === id && o.token!.ref.covenantId.toLowerCase() === c.token.toLowerCase()).reduce((a, o) => a + o.token!.amount, 0n);
        if (kept > 0n && kept !== c.amount) return refuse(t('signer.shape.custodyShort', { id: longId(id), token: longId(c.token), amount: String(c.amount) }));
      }
    }
    return { ok: true, shape: 'continue', lines };
  }
  // an ending order's input authorises no output but a new order's genesis
  for (const d of ending) {
    const x = tx.outputs.find((o) => o.covenant?.authorizingInput === d.index && !createdIds.has(o.covenant.covenantId.toLowerCase()));
    if (x) return refuse(t('signer.shape.unknownOutput'));
  }
  return { ok: true, shape: 'end', lines };
}

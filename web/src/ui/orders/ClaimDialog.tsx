import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { boughtOf } from '../../kob/bought';
import type { CancelPlan } from '../../kob/cancel';
import { parsePricePerToken } from '../../kob/units';
import { Banner, Button, Field, IssueLine, Modal, Segmented, kasText, pricePerTokenText, tokenText } from '../kit';
import { HEAVY_CLAIM_FEE, claimAskSpec, planClaimChoice, sellCrossesOwnBid, type ClaimChoice, type ClaimContext } from './claim';
import type { OrderRowModel } from './orders-model';

export interface ClaimDialogProps {
  row: OrderRowModel;
  ctx: ClaimContext;
  onClose(): void;
  /** The claim is built. Hand it to the flow, with `toast` as its submitted line. */
  onPlanned(plan: CancelPlan, toast: string): void;
}

type Kind = ClaimChoice['kind'];

/**
 * Claim of what a Collected buy order bought. It goes to the wallet, is merged with the wallet's
 * UTXOs, or is sold by a limit sell placed in the same approval. The fee is shown before signing,
 * and a costly merge suggests fewer UTXOs.
 */
export function ClaimDialog(props: ClaimDialogProps) {
  const services = useServices();
  const { ctx, row } = props;
  const st = ctx.snapshot.order.state;
  const bf = boughtOf(st);
  const tk = ctx.token;
  const decimals = tk?.decimals ?? 0;
  const ticker = tk?.ticker ?? '?';
  const s = st.state as unknown as Record<string, string>;
  // The sell's prices are per whole token of the token market's scale.
  const scale = ctx.scale ?? 1n;
  const mergeMax = Math.min(ctx.room, ctx.wallet.length);
  const [kind, setKind] = useState<Kind>('claim');
  const [count, setCount] = useState(String(mergeMax));
  const bidPrice = st.kind === 'KobCondBid' ? BigInt(s.tpPrice) : st.kind === 'KobBid' ? BigInt(s.price) : 0n;
  const [priceText, setPriceText] = useState(tk && bidPrice > 0n && ctx.scale === BigInt(s.scale) ? pricePerTokenText(bidPrice, decimals, scale) : '');
  const [end, setEnd] = useState(false);

  const price = useMemo(() => {
    if (!tk || ctx.scale === null) return null;
    try {
      const p = parsePricePerToken(priceText, decimals, scale, 'up');
      return p > 0n ? p : null;
    } catch {
      return null;
    }
  }, [priceText, decimals, scale]);
  const n = Math.max(0, Math.min(Number.parseInt(count, 10) || 0, mergeMax));
  const crosses = kind === 'sell' && price !== null && sellCrossesOwnBid(ctx, price, end);
  const plan = useMemo((): CancelPlan | null => {
    if (kind === 'merge') return n > 0 ? planClaimChoice(ctx, { kind: 'merge', count: n }) : null;
    if (kind === 'sell') {
      if (price === null || crosses) return null;
      const place = claimAskSpec(services, ctx, price);
      return place ? planClaimChoice(ctx, { kind: 'sell', place, end }) : null;
    }
    return planClaimChoice(ctx, { kind: 'claim' });
  }, [kind, n, price, end, crosses]);

  if (!bf) return null;
  const amount = tokenText(bf.amount, decimals);
  const c = plan?.ok ? plan.claim : undefined;
  const heavy = kind === 'merge' && !!c && c.mergedUtxos > 1 && c.fee >= HEAVY_CLAIM_FEE;
  const confirmKey = kind === 'sell' ? 'orders.claimDialog.confirmSell' : kind === 'merge' ? 'orders.claimDialog.confirmMerge' : 'orders.claimDialog.confirm';
  const toast = kind === 'sell' ? t('orders.flow.claimSellSubmitted', { amount, ticker }) : t('orders.flow.claimSubmitted', { amount, ticker });

  return (
    <Modal
      title={t('orders.claimDialog.title', { ticker })}
      onClose={props.onClose}
      data-testid="claim-dialog"
      footer={
        <>
          <Button onClick={props.onClose} data-testid="claim-cancel">{t('common.cancel')}</Button>
          <Button variant="primary" data-autofocus disabled={!plan?.ok} onClick={() => plan?.ok && props.onPlanned(plan, toast)} data-testid="claim-confirm">
            {t(confirmKey)}
          </Button>
        </>
      }
    >
      <p data-testid="claim-amount">{t('orders.claimDialog.body', { amount, ticker })}</p>
      <Segmented<Kind>
        aria-label={t('orders.claimDialog.kind')}
        value={kind}
        onChange={setKind}
        block
        options={[
          { value: 'claim', label: t('orders.claimDialog.claim'), 'data-testid': 'claim-kind-claim' },
          { value: 'merge', label: t('orders.claimDialog.merge'), disabled: mergeMax === 0, 'data-testid': 'claim-kind-merge' },
          { value: 'sell', label: t('orders.claimDialog.sell'), disabled: !tk, 'data-testid': 'claim-kind-sell' },
        ]}
        data-testid="claim-kind"
      />
      <div class="stack-sm" style="margin-top:10px">
        {kind === 'claim' ? <p class="small muted">{t('orders.claimDialog.claimHint')}</p> : null}
        {kind === 'merge' ? (
          <Field
            label={t('orders.claimDialog.mergeCount')}
            value={count}
            onValue={setCount}
            inputMode="numeric"
            hint={t('orders.claimDialog.mergeHint', { count: n, ticker })}
            data-testid="claim-merge-count"
          />
        ) : null}
        {kind === 'sell' ? (
          <>
            <Field
              label={`${t('orders.claimDialog.price')} (${t('orders.unit.kasPer', { ticker })})`}
              value={priceText}
              onValue={setPriceText}
              inputMode="decimal"
              hint={t('orders.claimDialog.sellHint')}
              error={crosses ? t('orders.claimDialog.crosses') : null}
              data-testid="claim-sell-price"
            />
            {row.side === 'buy' && !row.pair ? (
              <label class="row small ord-check" style="gap:8px">
                <input type="checkbox" checked={end} onChange={(e) => setEnd((e.currentTarget as HTMLInputElement).checked)} data-testid="claim-sell-end" />
                <span>{t('orders.claimDialog.end')}</span>
              </label>
            ) : null}
          </>
        ) : null}
      </div>
      {c ? (
        <div class="small" style="margin-top:10px" data-testid="claim-summary">
          <p data-testid="claim-fee">{t('orders.claimDialog.fee', { fee: kasText(c.fee) })}</p>
          {c.carrier > 0n ? <p class="muted" data-testid="claim-carrier">{t('orders.claimDialog.carrier', { kas: kasText(c.carrier) })}</p> : null}
          <p class="muted">{t(c.ends ? 'orders.claimDialog.ends' : 'orders.claimDialog.stays')}</p>
        </div>
      ) : null}
      {heavy ? (
        <Banner tone="warn" data-testid="claim-heavy">{t('orders.claimDialog.heavy', { count: c!.mergedUtxos, fee: kasText(c!.fee) })}</Banner>
      ) : null}
      {plan && !plan.ok ? (
        <Banner tone="error" data-testid="claim-error">
          <ul>
            {plan.issues.filter((i) => i.severity === 'error').map((i, k) => (
              <li key={k}><IssueLine prefix="orders.issue" issue={i} /></li>
            ))}
          </ul>
        </Banner>
      ) : null}
    </Modal>
  );
}

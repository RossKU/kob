// Amend of a curve (KobCurveAsk) in place. The price now and the last level's re-spread the levels
// left. The level size, tip and minimum fill change, and the amount left stays. It is one
// transaction, the maker's cancel continuing the curve, through the same flow as every amend.
import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import type { StrayView } from '../../data/indexer-types';
import { t, tIssue } from '../../i18n';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import { curveRestKas, curveSteps, currentLevel } from '../../kob/curve';
import type { TokenInfo } from '../../kob/registry';
import type { CurveAskState, Hex, OrderState } from '../../kob/types';
import { formatUnits, parsePricePerToken, tokenPriceToStatePrice, tryParseKas, tryParseUnits } from '../../kob/units';
import { Banner, Button, CurveGraph, Field, IssueLine, Modal, kasText, pricePerTokenText, toError } from '../kit';
import { describeActionError, planAmendFor } from './actions';
import { buildReplacement, type AmendInput } from './amend';
import { graphSteps } from './CurveDetails';
import type { OrderRowModel } from './orders-model';

export interface CurveAmendDialogProps {
  row: OrderRowModel;
  state: OrderState;
  token: TokenInfo | undefined;
  pubkey: Hex;
  strays: readonly StrayView[];
  onClose(): void;
  onPlanned(plan: CancelPlan, snapshot: OrderSnapshot): void;
}

type Key = 'now' | 'last' | 'levelSize' | 'tip' | 'minFill';

export function CurveAmendDialog(props: CurveAmendDialogProps) {
  const services = useServices();
  const { row, token } = props;
  const s = props.state.state as CurveAskState;
  const scale = BigInt(s.scale);
  const decimals = token?.decimals ?? 0;
  const table = s.table.map((p) => BigInt(p));
  const lvl = Math.min(currentLevel(s), table.length - 1);
  const price = (p: bigint) => (token ? pricePerTokenText(p, decimals, scale) : kasText(p));
  const initial: Record<Key, string> = useMemo(
    () => ({
      now: price(table[lvl]!),
      last: price(table[table.length - 1]!),
      levelSize: formatUnits(BigInt(s.levelSize), decimals),
      tip: kasText(BigInt(s.tip)),
      minFill: formatUnits(BigInt(s.minFill), decimals),
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [row.id],
  );
  const [form, setForm] = useState(initial);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [serverIssues, setServerIssues] = useState<{ code: string; message: string; params?: Record<string, string | number | bigint> }[]>([]);
  const set = (k: Key, v: string) => {
    setForm((f) => ({ ...f, [k]: v }));
    setFailure(null);
    setServerIssues([]);
  };

  const parsed = useMemo(() => {
    const errors: Partial<Record<Key, string>> = {};
    const input: AmendInput = { curve: {} };
    const p = (k: 'now' | 'last') => {
      if (form[k] === initial[k]) return;
      try {
        input.curve![k] = token ? parsePricePerToken(form[k], decimals, scale, 'up') : tryParseKas(form[k]) ?? (() => { throw new Error(); })();
      } catch {
        errors[k] = t('orders.amend.error.number');
      }
    };
    p('now');
    p('last');
    for (const k of ['levelSize', 'minFill'] as const) {
      if (form[k] === initial[k]) continue;
      const v = tryParseUnits(form[k], decimals);
      if (v === null || v <= 0n) errors[k] = t('orders.amend.error.amount', { decimals });
      else input.curve![k] = v;
    }
    if (form.tip !== initial.tip) {
      const v = tryParseKas(form.tip);
      if (v === null) errors.tip = t('orders.amend.error.number');
      else input.tip = tokenPriceToStatePrice(v, decimals, scale, 'nearest');
    }
    return { errors, input };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form]);

  const changed = Object.keys(parsed.input.curve ?? {}).length > 0 || parsed.input.tip !== undefined;
  const snap: OrderSnapshot = useMemo(
    () => ({
      covenantId: row.id, order: { transactionId: '00'.repeat(32), index: 0, amount: row.entry.view?.current?.value ?? '0', covenantId: row.id, state: props.state },
      // The preview binds an unbound curve to the token the row names.
      custody: { transactionId: '00'.repeat(32), index: 0, amount: '0', covenantId: row.token ?? '', state: { amount: s.amountLeft, owner: row.id, owner_scheme: 4, borrow_scheme: 0, borrow_guard: '0'.repeat(64), extension_commitment: s.extensionCommitment } },
      strays: [], refundDueDaa: null, deadline: null, source: 'indexer',
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [row.id],
  );
  const preview = useMemo(
    () => (Object.keys(parsed.errors).length ? null : buildReplacement(snap, 'curve', parsed.input, { tick: token?.tick ?? undefined, kob: services.kob })),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [parsed],
  );
  const next = preview?.ok ? (preview.spec!.order.state as CurveAskState) : null;
  const nextTable = next ? next.table.map((p) => BigInt(p)) : table;
  const nextSize = next ? BigInt(next.levelSize) : BigInt(s.levelSize);
  const steps = graphSteps(curveSteps(nextSize, nextTable, BigInt(s.total), BigInt(s.amountLeft)));
  const rest = curveRestKas(nextSize, nextTable, BigInt(s.total), BigInt(s.amountLeft), scale);
  const errText = (k: Key): string | null => {
    if (parsed.errors[k]) return parsed.errors[k]!;
    const i = preview?.issues.find((x) => x.severity === 'error' && x.field === k);
    return i ? tIssue('orders.amend', i) : null;
  };
  const canReview = changed && !!preview?.ok;

  const review = async () => {
    setBusy(true);
    setFailure(null);
    setServerIssues([]);
    try {
      const out = await planAmendFor(services, props.pubkey, row.entry, 'curve', parsed.input, props.strays);
      if (!out.amend.ok || !out.plan) return setServerIssues(out.amend.issues.filter((i) => i.severity === 'error'));
      if (!out.plan.ok) return setServerIssues(out.plan.issues.filter((i) => i.severity === 'error'));
      props.onPlanned(out.plan, out.snapshot);
    } catch (e) {
      setFailure(describeActionError(toError(e)));
    } finally {
      setBusy(false);
    }
  };

  const unit = t('orders.unit.kasPer', { ticker: token?.ticker ?? '?' });
  return (
    <Modal
      title={t('curve.amend.title')}
      onClose={props.onClose}
      data-testid="curve-amend-dialog"
      dismissable={!busy}
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy} data-testid="amend-cancel">{t('common.cancel')}</Button>
          <Button variant="primary" disabled={!canReview} loading={busy} onClick={() => void review()} data-testid="amend-review">
            {t('orders.amend.review')}
          </Button>
        </>
      }
    >
      <p class="small muted">{t('curve.amend.explain')}</p>
      <div class="stack-sm">
        <div class="row" style="gap:8px;align-items:flex-start">
          <Field class="grow" label={t('curve.amend.now', { unit })} value={form.now} onValue={(v) => set('now', v)} inputMode="decimal" error={errText('now')} data-testid="curve-amend-now" />
          <Field class="grow" label={t('curve.amend.last', { unit })} value={form.last} onValue={(v) => set('last', v)} inputMode="decimal" error={errText('last')} data-testid="curve-amend-last" />
        </div>
        <CurveGraph steps={steps} label={t('curve.details.graph')} data-testid="curve-amend-graph" />
        <p class="small" data-testid="curve-amend-rest">{t('curve.amend.rest', { kas: kasText(rest), levels: nextTable.length })}</p>
        <div class="row" style="gap:8px;align-items:flex-start">
          <Field class="grow" label={t('curve.amend.levelSize', { ticker: token?.ticker ?? '' })} value={form.levelSize} onValue={(v) => set('levelSize', v)} inputMode="decimal" error={errText('levelSize')} data-testid="curve-amend-level-size" />
          <Field class="grow" label={t('curve.amend.minFill', { ticker: token?.ticker ?? '' })} value={form.minFill} onValue={(v) => set('minFill', v)} inputMode="decimal" error={errText('minFill')} data-testid="curve-amend-min-fill" />
        </div>
        <Field label={t('orders.amend.tip')} value={form.tip} onValue={(v) => set('tip', v)} inputMode="decimal" error={errText('tip')} hint={t('orders.amend.tipHint')} data-testid="curve-amend-tip" />
      </div>
      {serverIssues.length ? (
        <Banner tone="error" data-testid="amend-error">
          <ul>
            {serverIssues.map((i, n) => (
              <li key={n}><IssueLine prefix={i.code.includes('.') ? 'orders.issue' : 'orders.amend'} issue={i} /></li>
            ))}
          </ul>
        </Banner>
      ) : null}
      {failure ? <Banner tone="error" data-testid="amend-failure">{failure}</Banner> : null}
    </Modal>
  );
}

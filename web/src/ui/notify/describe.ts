// Text of one notification: the event's plain params rendered with i18n at DISPLAY time, plus where it links to.
import { formatDateTime, t } from '../../i18n';
import { routeToHash } from '../../app/router';
import type { NotificationEvent } from '../../kob/notifications';
import type { TokenRegistry } from '../../kob/registry';
import { kasText, pricePerTokenText, scaleOfDecimals, shortId, tokenText } from '../kit/format';

/** `what` of a `reorg` event: the kinds of change a chain re-organisation can make to an own order (each has a `notify.ev.reorg.<what>.body`). */
export const REORG_WHATS = ['fill', 'cancelled', 'refunded', 'killed', 'gone'] as const;

export interface DescribedEvent { title: string; body: string; href: string }

/** `registry` resolves token ids to tickers / decimals (an unknown token shows its short id and its raw base units). */
export function describeEvent(e: NotificationEvent, registry: Pick<TokenRegistry, 'byCovenantId'> | null): DescribedEvent {
  const info = e.token ? registry?.byCovenantId.get(e.token) : undefined;
  const token = info?.ticker ?? (e.token ? shortId(e.token, 6, 4) : '?');
  const side = e.side ? t(`notify.side.${e.side}`) : '';
  const p = e.params;
  // amounts are base units (decimal strings): shown in token units when the token is known
  const amountText = (v: string | number | undefined): string => {
    if (v === undefined) return '0';
    try {
      return tokenText(BigInt(String(v)), info?.decimals ?? 0);
    } catch {
      return String(v);
    }
  };
  const base = { token, side, amount: amountText(p.amount), total: amountText(p.total ?? p.amount) };
  let params: Record<string, string | number> = base;
  if (e.kind === 'expirySoon' || e.kind === 'expired') params = { ...base, time: typeof p.at === 'number' && p.at > 0 ? formatDateTime(p.at) : '' };
  if (e.kind === 'payment') {
    let amount: string;
    try {
      const raw = BigInt(String(p.amount ?? '0'));
      amount = e.token ? tokenText(raw, info?.decimals ?? 0) : kasText(raw);
    } catch {
      amount = String(p.amount ?? '');
    }
    params = { amount, asset: e.token ? token : 'KAS' };
  }
  let bodyKey = `notify.ev.${e.kind}.body`;
  if (e.kind === 'reorg') {
    const what = String(p.what ?? '');
    if ((REORG_WHATS as readonly string[]).includes(what)) bodyKey = `notify.ev.reorg.${what}.body`;
    let price = '';
    try {
      // the order's own price: sompi per whole token of the token's scale (an unknown token: per its scale's base units)
      if (p.price !== undefined) price = info ? `${pricePerTokenText(BigInt(String(p.price)), info.decimals, scaleOfDecimals(info.decimals))} KAS/${info.ticker}` : t('notify.reorg.perScale', { price: kasText(BigInt(String(p.price))) });
    } catch {
      price = '';
    }
    const filled = amountText(p.filled);
    const stateKey = p.state === 'open' ? 'open' : p.state === 'partial' ? 'partial' : 'other';
    params = { ...base, at: price ? t('notify.reorg.at', { price }) : '', state: t(`notify.reorg.state.${stateKey}`, { filled, total: p.total !== undefined ? amountText(p.total) : filled }) };
  }
  if (e.kind === 'invoice') {
    const status = t(`notify.status.${String(p.status)}`);
    params = { status, reference: p.reference ? String(p.reference) : shortId(String(p.invoice ?? ''), 6, 4) };
  }
  const href = e.kind === 'payment' && e.token ? routeToHash({ name: 'token', covenantId: e.token }) : e.kind === 'invoice' ? '#/settings' : '#/orders';
  return { title: t(`notify.ev.${e.kind}.title`, params), body: t(bodyKey, params), href };
}

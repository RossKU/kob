// Text of one notification: the event's plain params rendered with i18n at DISPLAY time, plus where it links to.
import { t } from '../../i18n';
import { displayToken } from '../../app/token-display';
import { amountCell, isUnnamed } from '../orders/cells';
import { formatRowTime } from '../kit/format';
import { kasPerTokenToUsd, sompiToUsd, usdText, type Fiat } from '../../app/fiat';
import { routeToHash } from '../../app/router';
import { pow10 } from '../../kob/units';
import type { NotificationEvent } from '../../kob/notifications';
import type { TokenRegistry } from '../../kob/registry';
import { kasText, pricePerTokenText, scaleOfDecimals, shortId, tokenText } from '../kit/format';
import { shortId as covenantFragment } from '../../kob/registry';

/** `what` of a `reorg` event: the kinds of change a chain re-organisation can make to an own order (each has a `notify.ev.reorg.<what>.body`). */
export const REORG_WHATS = ['fill', 'cancelled', 'refunded', 'killed', 'gone'] as const;

export interface DescribedEvent { title: string; body: string; href: string }

const NO_REGISTRY: Pick<TokenRegistry, 'byCovenantId'> = { byCovenantId: new Map() };

/**
 * `registry` and `network` resolve a token id to its ticker and decimals the way every other view does (app/token-display.ts: the registry, else the token's
 * KCC-23 declaration, its program's or market's decimals, this browser's issuance); a token none of them names shows its short id, and its amount in raw
 * base units while nothing states its decimals. The same text is used by the bell list and the toast.
 */
export function describeEvent(e: NotificationEvent, registry: Pick<TokenRegistry, 'byCovenantId'> | null, fiat: Fiat | null = null, network = ''): DescribedEvent {
  const info = e.token ? displayToken(registry ?? NO_REGISTRY, network, e.token) : undefined;
  // displayToken names an unnamed token by its long id: a notification says it short
  const named = !!info && !isUnnamed(info.ticker, e.token);
  const token = named ? info.ticker : e.token ? covenantFragment(e.token) : '?';
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
  if (e.kind === 'expirySoon' || e.kind === 'expired') params = { ...base, time: typeof p.at === 'number' && p.at > 0 ? formatRowTime(p.at * 1000, 0) : '' };
  let bodyKey = `notify.ev.${e.kind}.body`;
  if (e.kind === 'payment') {
    let amount: string;
    let usd = '';
    try {
      const raw = BigInt(String(p.amount ?? '0'));
      amount = amountCell(raw, e.token ? (info?.decimals ?? 0) : 8).text; // the figure the portfolio shows: at most 4 decimals, grouped
      // the KAS amount always leads; while the USD display is in force it is followed by its dollar estimate (a token's amount is never converted)
      if (fiat && !e.token) usd = usdText(sompiToUsd(raw, fiat.rate));
    } catch {
      amount = String(p.amount ?? '');
    }
    params = { amount, asset: e.token ? token : 'KAS', usd };
    if (usd) bodyKey = 'notify.ev.payment.bodyUsd';
  }
  if (e.kind === 'reorg') {
    const what = String(p.what ?? '');
    if ((REORG_WHATS as readonly string[]).includes(what)) bodyKey = `notify.ev.reorg.${what}.body`;
    let price = '';
    try {
      // the order's own price: sompi per whole token of the token's scale (an unknown token: per its scale's base units)
      if (p.price !== undefined && info && named && fiat) {
        // KAS per whole token = price x 10^decimals / (scale x 1e8)
        const scale = scaleOfDecimals(info.decimals);
        price = `${usdText(kasPerTokenToUsd({ num: BigInt(String(p.price)) * pow10(info.decimals), den: scale * 100_000_000n }, fiat.rate))} per ${info.ticker}`;
      } else if (p.price !== undefined) price = info && named ? `${pricePerTokenText(BigInt(String(p.price)), info.decimals, scaleOfDecimals(info.decimals))} KAS per ${info.ticker}` : t('notify.reorg.perScale', { price: kasText(BigInt(String(p.price))) });
    } catch {
      price = '';
    }
    const filled = amountText(p.filled);
    const stateKey = p.state === 'open' ? 'open' : p.state === 'partial' ? 'partial' : 'other';
    params = { ...base, at: price ? t('notify.reorg.at', { price }) : '', state: t(`notify.reorg.state.${stateKey}`, { filled, total: p.total !== undefined ? amountText(p.total) : filled }) };
  }
  if (e.kind === 'invoice') {
    const status = t(`notify.status.${String(p.status)}`);
    params = { status, reference: p.reference ? String(p.reference) : shortId(String(p.invoice ?? ''), 10) };
  }
  const href = e.kind === 'payment' && e.token ? routeToHash({ name: 'token', covenantId: e.token }) : e.kind === 'invoice' ? '#/settings' : '#/portfolio/open';
  return { title: t(`notify.ev.${e.kind}.title`, params), body: t(bodyKey, params), href };
}

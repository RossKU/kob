// The label an order placed from the ticket carries (its placement record, the "Note" of My orders, the banner after placing): the order's side and
// type in the words the ticket uses ("Buy · IFD (if done)"), never the form's internal ids. Labels stored by older versions ("ifd buy",
// "limit sell 2/5", "limit buy x5") are shown the same way.
import { has, t } from '../../i18n';
import { ORDER_TYPES, type OrderTypeId, type Side } from './form-state';

/** `{ level, levels }`: one order of a ladder placed in turns; `{ levels }` alone: the whole ladder (after the last one). */
export interface LadderPart { level?: number; levels: number }

/** "Buy · IFD (if done)", "Sell · Limit (2/5)", "Buy · Limit ×5" (native side and type: the record is the order as built). */
export function orderLabel(type: OrderTypeId, side: Side, ladder?: LadderPart, token = ''): string {
  const base = t('ticket.orderName', { side: t(side === 'buy' ? 'common.side.buy' : 'common.side.sell'), type: t(`ticket.type.${type}.name`, { name: token }) });
  if (!ladder || ladder.levels <= 1) return base;
  return ladder.level !== undefined ? t('ticket.orderName.level', { label: base, level: ladder.level, levels: ladder.levels }) : t('ticket.orderName.ladder', { label: base, levels: ladder.levels });
}

const LEGACY = /^([A-Za-z]+) (buy|sell)(?: (\d+)\/(\d+)| x(\d+))?$/;

/** A stored label as shown: an internal one of an older version ("ifd buy") in the ticket's words; any other text as it is; null stays null. */
export function labelText(label: string | null | undefined): string | null {
  if (!label) return null;
  const m = LEGACY.exec(label.trim());
  if (!m || !(ORDER_TYPES as readonly string[]).includes(m[1]!) || !has(`ticket.type.${m[1]}.name`)) return label;
  const type = m[1] as OrderTypeId;
  const side = m[2] as Side;
  if (m[3] && m[4]) return orderLabel(type, side, { level: Number(m[3]), levels: Number(m[4]) });
  if (m[5]) return orderLabel(type, side, { levels: Number(m[5]) });
  return orderLabel(type, side);
}

// Why the indexer does not list a live order, in plain words. The executor's listing rules (crates/kob-executor/src/tokens.rs `ListingRules::evaluate`,
// docs/api.md OrderView `unlisted_reason`) refuse an order with a short machine code; an unlisted order is indexed (status, cancel) but is in no book
// and never fills. Pure: the codes map to i18n keys, the texts live in i18n/en/orders.ts.
import { t } from '../../i18n';

/** Every reason the executor's listing rules report (the order's own, not a pair's quote token: that one is prefixed `quote_`). */
export const UNLISTED_REASONS = [
  'no_token', 'token_family_mismatch', 'token_template_not_strict', 'token_template_mismatch', 'extension_commitment_mismatch', 'genesis_not_verified',
  'non_standard_scale', 'order_value_below_minimum', 'token_not_allowlisted', 'expiry_beyond_maximum', 'carrier_below_minimum',
] as const;

/** The i18n key suffix (`orders.unlisted.reason.<key>`) of a reason code: a known code, `bad_state` for `bad_state:...`, else `other`. */
export function unlistedReasonKey(code: string): string {
  if ((UNLISTED_REASONS as readonly string[]).includes(code)) return code;
  return code === 'bad_state' || code.startsWith('bad_state:') ? 'bad_state' : 'other';
}

/**
 * The line of a live unlisted order: "Not listed by the indexer: <reason>". A pair order is also refused for its quote token (`quote_<reason>`); the
 * reason is then said about the quote token. A missing reason (an older executor) gets the generic wording.
 */
export function unlistedText(reason: string | null | undefined): string {
  const code = reason ?? '';
  const quote = code.startsWith('quote_');
  const why = t(`orders.unlisted.reason.${unlistedReasonKey(quote ? code.slice('quote_'.length) : code)}`);
  return t('orders.unlisted.line', { reason: quote ? t('orders.unlisted.quote', { reason: why }) : why });
}

/** What to do about it: a value below the minimum is mended with a larger order, any other reason is not the order's size. */
export const unlistedHint = (reason: string | null | undefined): string => t(reason === 'order_value_below_minimum' ? 'orders.unlisted.hintValue' : 'orders.unlisted.hint');

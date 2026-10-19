// How a token is SHOWN (never how it is traded or verified): its amounts in whole tokens and its name, also for a token the registry does not list
// (founder decision 2026-10-17: never "base units" in front of a user). Its decimals come from, in this order: the registry; else this browser's
// issuance of it (data/issued-decimals.ts: decimals, ticker and name the issuer chose; the chain records none of them); else the exponent of the
// price scale its orders use (`scale`, 10^decimals: what the market page and the ticket trade it with), else ASSUMED_DECIMALS,
// the Kaspa convention (1e8 base units = 1 token, like KAS and sompi). A token shown on the issuance record or on assumed decimals carries
// `decimalsSource`, and the places that name it add a small marker (`decimalsNote`); its name is the issued ticker, else its covenant id (8 + 8 hex).
import { issuedToken } from '../data/issued-decimals';
import type { StorageLike } from '../config';
import { longId, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { Hex } from '../kob/types';
import { setTokenAmountFormatter, t } from '../i18n';
import { formatTokenAmount } from '../kob/units';

/** The decimals assumed for a token nothing names them for: the Kaspa convention (KAS has 8, a sompi is 1e-8 KAS). */
export const ASSUMED_DECIMALS = 8;

/**
 * The token for display: the registry's entry, else a display-only entry from this browser's issuance of it, else one on ASSUMED_DECIMALS.
 * `decimalsSource` marks the last two; they are not tradable and never stand in for a registry entry where a token is verified or planned.
 */
export function displayToken(registry: Pick<TokenRegistry, 'byCovenantId'>, network: string, covenantId: Hex | null | undefined, storage?: StorageLike | null, scale?: bigint | null): TokenInfo | undefined {
  if (!covenantId) return undefined;
  const id = covenantId.toLowerCase();
  const known = registry.byCovenantId.get(id);
  if (known) return known;
  const issued = issuedToken(network, id, storage);
  const name = issued?.ticker || longId(id);
  const decimals = issued?.decimals ?? exponentOf(scale) ?? ASSUMED_DECIMALS;
  return {
    ticker: name, name: issued?.name || name, family: 'kcc20', covenantId: id, templateId: '', templateHash: '', extensionCommitment: null, extensionClass: 'other',
    decimals, tick: null, status: 'listed', display: null, capabilities: [], program: null, templateMatchesPinned: false, prefixLen: 0, suffixLen: 0,
    slots: { inputs: 0, outputs: 0 }, tradable: false, untradableReason: null, json: {} as TokenInfo['json'],
    decimalsSource: issued ? 'issued' : 'assumed',
  };
}

/** The exponent of a power of ten up to 10^18 (a price scale), else null. */
function exponentOf(scale: bigint | null | undefined): number | null {
  // a scale of 1 is also what an order model holds when it does not know the scale: not evidence of 0 decimals
  if (!scale || scale <= 1n) return null;
  for (let d = 0, p = 1n; d <= 18; d++, p *= 10n) if (p === scale) return d;
  return null;
}

/** The marker next to a token shown on its issuance record or on assumed decimals (null for a registry token): its text and its tooltip. */
export function decimalsNote(token: Pick<TokenInfo, 'decimalsSource' | 'decimals'> | null | undefined): { text: string; title: string } | null {
  if (!token?.decimalsSource) return null;
  return token.decimalsSource === 'issued'
    ? { text: t('token.note.issued'), title: t('token.note.issuedTitle', { decimals: token.decimals }) }
    : { text: t('token.note.assumed', { decimals: token.decimals }), title: t('token.note.assumedTitle', { decimals: token.decimals }) };
}

/**
 * Makes every sentence of the app that names an amount of a token (i18n `tokenAmountText`: the findings of the planners and the cancel) say it
 * as this module shows the token: whole tokens with its name, marked when its decimals are assumed. Called once with the app's registry.
 */
export function installTokenAmounts(registry: Pick<TokenRegistry, 'byCovenantId'>, network: string): void {
  setTokenAmountFormatter((token, units) => {
    const tk = token ? displayToken(registry, network, token) : undefined;
    if (!tk) return t('token.amount.plain', { amount: formatTokenAmount(units, ASSUMED_DECIMALS, { group: ',' }), decimals: ASSUMED_DECIMALS });
    const amount = formatTokenAmount(units, tk.decimals, { group: ',' });
    return tk.decimalsSource === 'assumed' ? t('token.amount.assumed', { amount, token: tk.ticker, decimals: tk.decimals }) : tk.ticker ? `${amount} ${tk.ticker}` : amount;
  });
}

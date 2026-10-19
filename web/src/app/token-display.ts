// How a token is SHOWN (never how it is traded or verified): its amounts in whole tokens and its name, also for a token the registry does not list
// (founder decision 2026-10-17: never "base units" in front of a user). The FACTS (name, ticker, decimals) come from the chain first (founder
// decision 2026-10-19: KOB holds no authoritative token-metadata database): the token's KCC-23 declaration, verified against its covenant id
// (data/token-declarations.ts). The registry is curation (the official badge, anti-impersonation) and a pinned snapshot of the facts: where it and
// the declaration disagree on a fact, the on-chain value is shown and the disagreement is flagged (`declarationConflicts`), never silently resolved
// for the registry; trading such a token waits until they agree when the decimals differ (app/env.ts). Without a declaration (a token created
// before KCC-23: its id cannot commit to one) the order is as before: the registry; else this browser's issuance of it (data/issued-decimals.ts:
// decimals, ticker and name the issuer chose); else the exponent of the price scale its orders use (`scale`, 10^decimals: what the market page and
// the ticket trade it with), else ASSUMED_DECIMALS, the Kaspa convention (1e8 base units = 1 token, like KAS and sompi). A token shown on its
// declaration, its issuance record or assumed decimals carries `decimalsSource`, and the places that name it add a small marker (`decimalsNote`); its
// name is the declared ticker, else the issued one, else its covenant id (8 + 8 hex).
import { issuedToken } from '../data/issued-decimals';
import { declarationConflicts, declaredToken } from '../data/token-declarations';
import type { StorageLike } from '../config';
import { longId, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { Hex } from '../kob/types';
import { setTokenAmountFormatter, t } from '../i18n';
import { formatTokenAmount } from '../kob/units';

/** The decimals assumed for a token nothing names them for: the Kaspa convention (KAS has 8, a sompi is 1e-8 KAS). */
export const ASSUMED_DECIMALS = 8;

/**
 * The token for display: the registry's entry (with its declaration's facts where they disagree, flagged), else a display-only entry from the
 * token's declaration, this browser's issuance of it, or ASSUMED_DECIMALS. `decimalsSource` marks the display-only ones; they are not tradable and
 * never stand in for a registry entry where a token is verified or planned.
 */
export function displayToken(registry: Pick<TokenRegistry, 'byCovenantId'>, network: string, covenantId: Hex | null | undefined, storage?: StorageLike | null, scale?: bigint | null): TokenInfo | undefined {
  if (!covenantId) return undefined;
  const id = covenantId.toLowerCase();
  const declared = declaredToken(network, id) ?? null;
  const known = registry.byCovenantId.get(id);
  if (known) {
    const conflicts = declarationConflicts(known, declared);
    if (!declared || conflicts.length === 0) return known;
    // the chain's facts, flagged: the registry entry disagrees with what the token recorded at its creation
    return {
      ...known,
      ticker: declared.symbol ?? known.ticker, name: declared.name ?? known.name, decimals: declared.decimals ?? known.decimals,
      declarationConflicts: conflicts,
    };
  }
  const issued = issuedToken(network, id, storage);
  const fromScale = exponentOf(scale);
  if (declared && declared.decimals !== null) {
    const ticker = declared.symbol ?? (issued?.ticker || longId(id));
    // the market's price scale is the decimals its first order named: one that is not the declared decimals is a disagreement too
    const conflicts = fromScale !== null && fromScale !== Math.min(declared.decimals, 9) ? (['decimals'] as const) : [];
    return {
      ...base(id, ticker, declared.name ?? ticker, declared.decimals), decimalsSource: 'declared',
      ...(conflicts.length ? { declarationConflicts: [...conflicts] } : {}),
    };
  }
  const name = declared?.symbol ?? (issued?.ticker || longId(id));
  const decimals = issued?.decimals ?? fromScale ?? ASSUMED_DECIMALS;
  return { ...base(id, name, declared?.name ?? (issued?.name || name), decimals), decimalsSource: issued ? 'issued' : 'assumed' };
}

function base(id: Hex, ticker: string, name: string, decimals: number): TokenInfo {
  return {
    ticker, name, family: 'kcc20', covenantId: id, templateId: '', templateHash: '', extensionCommitment: null, extensionClass: 'other',
    decimals, tick: null, status: 'listed', display: null, capabilities: [], program: null, templateMatchesPinned: false, prefixLen: 0, suffixLen: 0,
    slots: { inputs: 0, outputs: 0 }, tradable: false, untradableReason: null, json: {} as TokenInfo['json'],
  };
}

/** The exponent of a power of ten up to 10^18 (a price scale), else null. */
function exponentOf(scale: bigint | null | undefined): number | null {
  // a scale of 1 is also what an order model holds when it does not know the scale: not evidence of 0 decimals
  if (!scale || scale <= 1n) return null;
  for (let d = 0, p = 1n; d <= 18; d++, p *= 10n) if (p === scale) return d;
  return null;
}

/**
 * The marker next to a token shown on its declaration, its issuance record or assumed decimals, or whose registry entry and declaration
 * disagree (null for an agreeing registry token): its text and its tooltip. A disagreement takes precedence.
 */
export function decimalsNote(token: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null | undefined): { text: string; title: string } | null {
  if (token?.declarationConflicts?.length) {
    const fields = token.declarationConflicts.map((f) => t(`token.note.field.${f}`)).join(', ');
    return { text: t('token.note.conflict'), title: t('token.note.conflictTitle', { fields }) };
  }
  if (!token?.decimalsSource) return null;
  if (token.decimalsSource === 'declared') return { text: t('token.note.declared'), title: t('token.note.declaredTitle', { decimals: token.decimals }) };
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

// How a token is SHOWN (never how it is traded or verified): its amounts in whole tokens and its name, also for a token the registry does not list
// (founder decision 2026-10-17: never "base units" in front of a user). The FACTS (name, ticker, decimals) come from the chain first (founder
// decision 2026-10-19: KOB holds no authoritative token-metadata database): the token's KCC-23 declaration, verified against its covenant id
// (data/token-declarations.ts). The registry is a pinned snapshot of the facts, not curation (KOB makes no per-token judgement; curation belongs to
// external community lists the user chooses): where it and the declaration disagree on a fact, the on-chain value is shown and the disagreement is flagged (`declarationConflicts`), never silently resolved
// for the registry; trading such a token waits until they agree when the decimals differ (app/env.ts). Without a declaration (a token created
// before KCC-23: its id cannot commit to one) the order is as before: the registry; else the default decimals of its token program (a launchpad's
// convention for the tokens of its program that declare none, KaspaRocket 3: programs.generated.ts `defaultDecimals`; the program is the one the
// indexer lists the token under or the wallet's holdings of it verified under, data/token-declarations.ts `tokenProgramOf`); else the exponent of the price
// scale an order of it names or its market trades with (`scale`, `marketDecimalsOf`, 10^decimals: what the market page and the ticket trade it with); else
// this browser's issuance of it (data/issued-decimals.ts: decimals, ticker and name the issuer chose). The order of the ticket (kob/open-token.ts). Where nothing
// states them none are invented: `decimalsSource: 'assumed'` with 0 decimals shows the chain's integer amount, said so. A token shown on any of these carries
// `decimalsSource`, and the places that name it add a small marker (`decimalsNote`); its name is the declared ticker, else the issued one, else its covenant id (8 + 8 hex).
import { issuedToken } from '../data/issued-decimals';
import { declarationConflicts, declaredToken, listedTickerOf, marketDecimalsOf, tokenProgramOf } from '../data/token-declarations';
import { programDefaultDecimals } from '../kob/programs.generated';
import type { StorageLike } from '../config';
import { longId, sanitizeUntrusted, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { Hex } from '../kob/types';
import { RAW_TOKEN_DECIMALS, setTokenAmountFormatter, t } from '../i18n';
import { formatTokenAmount } from '../kob/units';

/**
 * The token for display: the registry's entry (with its declaration's facts where they disagree, flagged), else a display-only entry from the
 * token's declaration, its program's default decimals, the scale of an order or its market, this browser's issuance of it, or none. `decimalsSource` marks the display-only
 * ones; they are not tradable and never stand in for a registry entry where a token is verified or planned. `program`: the token's program when the
 * caller knows it (else the one it was seen under, `tokenProgramOf`).
 */
export function displayToken(
  registry: Pick<TokenRegistry, 'byCovenantId'>,
  network: string,
  covenantId: Hex | null | undefined,
  storage?: StorageLike | null,
  scale?: bigint | null,
  program?: string | null,
): TokenInfo | undefined {
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
  const name = declared?.symbol ?? (issued?.ticker || (sanitizeUntrusted(listedTickerOf(network, id) ?? '', 24) || longId(id)));
  // a token of a program with default decimals (KaspaRocket: 3, its convention), shown with them and their source
  const convention = programDefaultDecimals(program ?? tokenProgramOf(network, id));
  if (convention) {
    return { ...base(id, name, declared?.name ?? (issued?.name || name), convention.decimals), decimalsSource: 'program', decimalsConvention: convention.source };
  }
  const market = fromScale ?? marketDecimalsOf(network, id);
  const decimals = market ?? issued?.decimals ?? RAW_TOKEN_DECIMALS;
  return { ...base(id, name, declared?.name ?? (issued?.name || name), decimals), decimalsSource: issued && issued.decimals === decimals ? 'issued' : market !== null ? 'scale' : 'assumed' };
}

function base(id: Hex, ticker: string, name: string, decimals: number): TokenInfo {
  return {
    ticker, name, family: 'kcc20', covenantId: id, templateId: '', templateHash: '', extensionCommitment: null, extensionClass: 'other',
    decimals, tick: null, display: null, capabilities: [], program: null, templateMatchesPinned: false, prefixLen: 0, suffixLen: 0,
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
 * The marker next to a token shown on its declaration, its issuance record, the scale of its market or its raw amount, or whose registry entry and declaration
 * disagree (null for an agreeing registry token): its text and its tooltip. A disagreement takes precedence.
 */
export function decimalsNote(
  token: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts' | 'decimalsConvention'> | null | undefined,
): { text: string; title: string } | null {
  if (token?.declarationConflicts?.length) {
    const fields = token.declarationConflicts.map((f) => t(`token.note.field.${f}`)).join(', ');
    return { text: t('token.note.conflict'), title: t('token.note.conflictTitle', { fields }) };
  }
  if (!token?.decimalsSource) return null;
  if (token.decimalsSource === 'declared') return { text: t('token.note.declared'), title: t('token.note.declaredTitle', { decimals: token.decimals }) };
  if (token.decimalsSource === 'program') {
    const source = token.decimalsConvention ?? '';
    return { text: source, title: t('token.note.programTitle', { decimals: token.decimals, source }) };
  }
  if (token.decimalsSource === 'scale') return { text: t('token.note.scale', { decimals: token.decimals }), title: t('token.note.scaleTitle', { decimals: token.decimals }) };
  return token.decimalsSource === 'issued'
    ? { text: t('token.note.issued'), title: t('token.note.issuedTitle', { decimals: token.decimals }) }
    : { text: t('token.note.assumed'), title: t('token.note.assumedTitle') };
}

/** Where the decimals a token is shown and traded with come from (`decimalsOrigin`). */
export type DecimalsOrigin = 'registry' | 'declared' | 'program' | 'issued' | 'scale' | 'entered' | 'assumed';

/**
 * Where a token's decimals come from: its KCC-23 declaration (also for a registry entry that disagrees with it: the on-chain value is shown), the
 * registry, its token program's default (a launchpad's convention), this browser's issuance record, the price scale of its market (an open-list token's orders), the decimals entered for its first order,
 * else assumed. The open-list banner of a market says it in words (`market.open.decimals.<origin>`), so it never claims a source the token does not have.
 */
export function decimalsOrigin(token: Pick<TokenInfo, 'decimalsSource' | 'declarationConflicts' | 'openList' | 'openScale'>): DecimalsOrigin {
  if (token.decimalsSource) return token.decimalsSource;
  if (token.declarationConflicts?.includes('decimals')) return 'declared';
  if (token.openList) return token.openScale === 'declared' ? 'declared' : token.openScale === 'program' ? 'program' : token.openScale === 'entered' ? 'entered' : 'scale';
  return 'registry';
}

/**
 * Makes every sentence of the app that names an amount of a token (i18n `tokenAmountText`: the findings of the planners and the cancel) say it
 * as this module shows the token: whole tokens with its name, or its raw amount where nothing states its decimals. Called once with the app's registry.
 */
export function installTokenAmounts(registry: Pick<TokenRegistry, 'byCovenantId'>, network: string): void {
  setTokenAmountFormatter((token, units) => {
    const tk = token ? displayToken(registry, network, token) : undefined;
    if (!tk) return t('token.amount.plain', { amount: formatTokenAmount(units, RAW_TOKEN_DECIMALS, { group: ',' }) });
    const amount = formatTokenAmount(units, tk.decimals, { group: ',' });
    return tk.decimalsSource === 'assumed' ? t('token.amount.assumed', { amount, token: tk.ticker }) : tk.ticker ? `${amount} ${tk.ticker}` : amount;
  });
}

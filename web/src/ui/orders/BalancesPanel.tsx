import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { decimalsNote, displayToken } from '../../app/token-display';
import { useFiat } from '../../app/use-fiat';
import { kasValueTitle } from '../../app/fiat';
import { TipBadge } from '../market/TipBadge';
import { TokenNote } from '../market/TokenNote';
import { routeToHash, tokenRoute } from '../../app/router';
import { t } from '../../i18n';
import { mergeSlots, mergeTxCount } from '../../kob/merge';
import type { Hex } from '../../kob/types';
import { KasIcon } from '../kit/KasIcon';
import {
  Amount, Banner, Button, EmptyState, ErrorBanner, Section, columnFraction, scaledUnits, sortBy, unitsFraction, useSortState, type SortValue,
} from '../kit';
import {
  DataTable, DtCell, DtGroup, DtRow, KAS_FRACTION_MAX, MoneyCell, type DtColumn, type DtPlain,
} from '../kit/DataTable';
import { formatKas } from '../../kob/units';
import { tokenLabel } from '../market/token-model';
import { IdentityBlock } from '../market/Fingerprint';
import type { KascovMeta } from '../market/token-model';
import type { WalletBalances } from './use-balances';
import { AMOUNT_FRACTION_MAX, amountCell } from './cells';
import { countText, tCount } from './plural';
import { balanceColumnFractions, unreviewedProgram } from './balances-model';
import { mergeAllFrees, mergeTokenFor, mergeableCounts } from './merge-model';
import type { OrdersData } from './orders-data';
import './balances.css';

const BALANCE_SORT_KEYS = ['asset', 'available', 'inOrders', 'strays', 'kasLocked', 'value'] as const;
type BalanceSortKey = (typeof BALANCE_SORT_KEYS)[number];

/** The columns of the balances table: the asset, the figures, the buttons (no title). */
function balanceColumns(valueLabel: string, lockedLabel: string): (DtColumn<BalanceSortKey> | DtPlain)[] {
  return [
    { key: 'asset', label: t('orders.balances.asset'), kind: 'text', card: 'head' },
    { key: 'available', label: t('orders.balances.available'), kind: 'number', right: true },
    { key: 'inOrders', label: t('orders.balances.inOrders'), kind: 'number', right: true },
    { key: 'strays', label: t('orders.balances.strays'), kind: 'number', right: true },
    { key: 'kasLocked', label: lockedLabel, kind: 'number', right: true },
    { key: 'value', label: valueLabel, kind: 'number', right: true, card: 'end' },
    { key: 'act', plain: true },
  ];
}

function BalancesSkeleton() {
  return (
    <div role="status" aria-label={t('common.loading')} data-testid="balances-loading">
      {Array.from({ length: 3 }, (_, i) => <div key={i} class="skeleton skeleton-line" />)}
    </div>
  );
}

/** The panel until the list it is read with is in: its card and its heading, in their place. */
export function BalancesFrame() {
  return <Section title={t('orders.balances.title')} subtitle={t('orders.balances.subtitle')} data-testid="balances-panel"><BalancesSkeleton /></Section>;
}

/**
 * The balances wallets do not show: tokens inside the wallet's sell orders (0x04 custody), stray tokens, KAS locked in orders, next to the free
 * amounts. Free amounts come from the node / token tracker, the rest from the wallet's live orders (indexer, or records resolved on the node).
 * Drawn in the shared table (kit/DataTable.tsx), KAS first.
 */
export function BalancesPanel(props: {
  data: OrdersData;
  /** the free balances are read again when this changes (each load of the list, not each background update of it) */
  version?: unknown;
  pubkey: Hex;
  canSign: boolean;
  onCancelToken(token: Hex): void;
  /** outpoints held for open orders (never merged) */
  reserved?: ReadonlySet<string>;
  /** an action is being planned (its id) */
  busy?: string | null;
  /** merge the wallet's plain UTXOs of a token into one */
  onMergeToken?(token: Hex): void;
  /** the page's balances (use-balances.ts): the free balances, combined with the orders and strays, and each token's value in KAS */
  balances: WalletBalances;
  /** only these tokens (a market page: its token, a pair's two); KAS always */
  onlyTokens?: ReadonlySet<Hex>;
  /** kascov.io's picture of a token, for its icon */
  iconOf?: (covenantId: string) => KascovMeta | null;
  /** a market page's panel: no heading; the money figures are one line at the top (the page's summary cards are the account overview of the Portfolio page only) */
  compact?: boolean;
}) {
  const services = useServices();
  const { registry } = services;
  // the USD display (Settings): the KAS values in dollars (the money cell reads it itself; the Amount cells here take it); null = KAS
  const fiat = useFiat();
  const valueLabel = t(fiat ? 'orders.col.valueUsd' : 'orders.col.valueKas');
  const lockedLabel = t(fiat ? 'market.balances.kasLockedUsd' : 'market.balances.kasLocked');
  // the viewer's sort of the token rows (KAS stays on top); none: as the wallet lists them
  const [sort, setSort] = useSortState<BalanceSortKey>('balances', BALANCE_SORT_KEYS);
  const sortCols = balanceColumns(valueLabel, lockedLabel);
  const { free, combined, valueOf } = props.balances;
  const tokensOnShow = (combined?.tokens ?? []).filter((b) => !props.onlyTokens || props.onlyTokens.has(b.token));

  const shownTokens = sortBy(tokensOnShow, sort, (b, key): SortValue => {
    const decimals = displayToken(registry, services.config.network, b.token)?.decimals ?? 8;
    switch (key) {
      case 'asset':
        return tokenLabel(displayToken(registry, services.config.network, b.token)!);
      case 'available':
        return scaledUnits(b.free, decimals);
      case 'inOrders':
        return scaledUnits(b.escrowed, decimals);
      case 'strays':
        return scaledUnits(b.strays, decimals);
      case 'kasLocked':
        return b.kasLocked;
      case 'value':
        return valueOf(b);
    }
  });

  // a token outside the registry is merged under the program the node verified its UTXOs under (the template's slots, the extension commitment its UTXOs share)
  const unlisted = useMemo(() => (free.data ? { kob: services.kob, programs: free.data.programOf ?? free.data.programs } : undefined), [free.data, services.kob]);
  // free token UTXOs a merge may take, per token (two or more)
  const mergeable = useMemo(
    () => (free.data ? mergeableCounts(free.data.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set(), unlisted) : new Map<Hex, number>()),
    [free.data, registry, props.pubkey, props.reserved, unlisted],
  );

  // about how much KAS Merge all frees, per token (shown on the button: the reason to press it)
  const frees = useMemo(
    () => (free.data ? mergeAllFrees(free.data.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set(), unlisted) : new Map<Hex, bigint>()),
    [free.data, registry, props.pubkey, props.reserved, unlisted],
  );

  // fixed decimals per column: the finest amount of the column, so every row shows the same number of decimals (decimal points line up)
  // (a token of unknown decimals, no registry entry, shows its base units with no fraction)
  const cols = useMemo(
    () => balanceColumnFractions(combined?.tokens ?? [], (tok) => displayToken(registry, services.config.network, tok)?.decimals, unitsFraction),
    [combined, registry],
  );

  // the figures of the table and the strip: at most 4 decimals (the whole amount is the title), the finest of each column on every row of it so the decimal points line up
  const cap = (n: number): number => Math.min(AMOUNT_FRACTION_MAX, n);
  const kasRow = combined?.kas;
  const dp = {
    free: cap(Math.max(cols.free, kasRow ? unitsFraction(kasRow.free, 8) : 0)),
    escrowed: cap(Math.max(cols.escrowed, kasRow ? unitsFraction(kasRow.locked, 8) : 0)),
    strays: cap(cols.strays),
    kas: cap(cols.kas),
    onTokens: cap(kasRow ? unitsFraction(kasRow.onTokens, 8) : 0),
    value: Math.min(KAS_FRACTION_MAX, columnFraction([kasRow?.total, ...(combined?.tokens ?? []).map((b) => valueOf(b))], 8)),
  };

  // the book's Refresh (an icon, the glyph spins while it reads): the button keeps its size, its words are its accessible name and tooltip
  const reading = free.loading && !!free.data;
  const refresh = (
    <Button small variant="ghost" class="bal-refresh" onClick={free.reload} title={t('common.refresh')} aria-label={t('common.refresh')} aria-busy={reading ? 'true' : undefined} data-testid="balances-refresh">
      <span class={`refresh-glyph${reading ? ' is-spinning' : ''}`} aria-hidden="true">{'↻'}</span>
    </Button>
  );
  const sum = props.balances.summary;
  const money = (kas: bigint | null, testid: string, fraction?: number) => (
    <span class="bal-v num" data-testid={testid} title={kas !== null ? kasValueTitle(kas, fiat) : undefined}><MoneyCell kas={kas} {...(fraction !== undefined ? { fraction } : {})} class="pf-money" /></span>
  );
  return (
    <Section
      {...(props.compact ? { class: 'bal-embed' } : { title: t('orders.balances.title'), subtitle: t('orders.balances.subtitle'), actions: refresh })}
      data-testid="balances-panel"
    >
      {props.compact ? (
        // the money figures, one line (the items wrap between each other, a figure never breaks), Refresh at the right end
        <div class="bal-line" data-testid="portfolio-summary" data-currency={fiat ? 'usd' : 'kas'}>
          <div class="bal-items">
            <span class="bal-item">
              <span class="bal-k">{t('orders.line.total')}</span>
              {money(sum ? sum.total : null, 'portfolio-total', 2)}
              {sum ? (
                <TipBadge
                  variant="tag"
                  label={t('orders.summary.estimate')}
                  detail={sum.unpriced > 0 ? `${t(fiat ? 'orders.summary.estimateTipUsd' : 'orders.summary.estimateTip')} ${tCount('orders.summary.unpriced', sum.unpriced, { n: sum.unpriced })}` : t(fiat ? 'orders.summary.estimateTipUsd' : 'orders.summary.estimateTip')}
                  data-testid="portfolio-estimate"
                  data-detail-testid="portfolio-estimate-detail"
                />
              ) : null}
            </span>
            <span class="bal-item"><span class="bal-k">{t('orders.line.available')}</span>{money(sum ? sum.kasFree : null, 'portfolio-available', 2)}</span>
            <span class="bal-item"><span class="bal-k">{t('orders.line.inOrders')}</span>{money(sum ? sum.kasLocked : null, 'portfolio-in-orders', 2)}</span>
          </div>
          {refresh}
        </div>
      ) : null}
      {free.loading && !free.data ? <BalancesSkeleton /> : null}
      <ErrorBanner error={free.error} onRetry={free.reload} />
      {free.data && free.data.errors.length ? (
        <Banner tone="warn" data-testid="balances-partial">{t('market.balances.partial', { sources: free.data.errors.map((e) => e.source).join(', ') })}</Banner>
      ) : null}
      {combined && free.data ? (
        // the rows rise in as the lists of the other tabs do (.ml-in, market.css), once, when the tab is opened; a refresh of the figures keeps this box and does not replay it
        <div class="ml-in">
          <DataTable id="balances" grid="balances" label={t('orders.balances.tokens')} cols={sortCols} sort={sort} own={null} onSort={setSort} sortable={tokensOnShow.length > 1} data-testid="token-balances-table">
            {/* KAS first: what the wallet can spend, what its orders hold, and the KAS that rides on its token coins (counted in the total) */}
            <DtGroup data-testid="kas-balance" class="bal-kas">
              <DtRow>
                <DtCell k="asset" card="head">
                  <span class="dt-market">
                    <KasIcon />
                    <span class="dt-market-name bal-name">KAS</span>
                  </span>
                </DtCell>
                <DtCell k="available" label={t('orders.balances.available')} num>
                  <span data-testid="kas-free">{free.data.kasFree !== null ? <Amount kind="kas" value={combined.kas.free} fraction={dp.free} group fiat={fiat} /> : t('common.unavailable')}</span>
                  {combined.kas.onTokens > 0n ? (
                    <div class="dt-sub" title={t('orders.balances.kasOnTokensHint')}>
                      {t('orders.balances.kasOnTokensPlus')} <span class="dt-val" data-testid="kas-on-tokens"><Amount kind="kas" value={combined.kas.onTokens} fraction={dp.onTokens} group fiat={fiat} /></span>
                    </div>
                  ) : null}
                </DtCell>
                <DtCell k="inOrders" label={t('orders.balances.inOrders')} num data-testid="kas-locked"><Amount kind="kas" value={combined.kas.locked} fraction={dp.escrowed} group fiat={fiat} /></DtCell>
                <DtCell k="strays" label={t('orders.balances.strays')} num class="dt-muted" card="hidden">—</DtCell>
                <DtCell k="kasLocked" label={lockedLabel} num class="dt-muted" card="hidden">—</DtCell>
                <DtCell k="value" label={valueLabel} num card="end" data-testid="kas-total">
                  {free.data.kasFree !== null ? <MoneyCell kas={combined.kas.total} unit={false} fraction={dp.value} /> : '—'}
                </DtCell>
                <DtCell k="act" card="action" />
              </DtRow>
            </DtGroup>
            {shownTokens.length === 0 ? (
              <EmptyState icon="token" compact data-testid="balances-empty">{t('orders.balances.empty')}</EmptyState>
            ) : (
              shownTokens.map((b) => {
                const info = registry.byCovenantId.get(b.token);
                // shown in whole tokens also without a registry entry (app/token-display.ts: this browser's issuance, else 8 assumed decimals)
                const shown = displayToken(registry, services.config.network, b.token)!;
                const d = shown.decimals;
                const label = tokenLabel(shown);
                const scaleNote = shown.decimalsSource === 'scale' && !shown.declarationConflicts?.length ? decimalsNote(shown) : null;
                const mergeAs = props.onMergeToken && mergeable.has(b.token) ? mergeTokenFor(b.token, free.data!.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set(), unlisted) : null;
                const showMerge = !!mergeAs;
                const program = info?.program ?? free.data!.programs.get(b.token) ?? null;
                return (
                  <DtGroup key={b.token} data-testid={`balance-row-${b.token}`}>
                    <DtRow>
                      <DtCell k="asset" card="head">
                        <span class="dt-market">
                          <IdentityBlock row={{ covenantId: b.token, ticker: shown.ticker, kascov: props.iconOf?.(b.token) ?? null }} size="sm" />
                          <span class="dt-market-name">
                            {/* the covenant id and the fingerprint identify the token; where its decimals come from (its market's price scale) is the name's tooltip, not text in the row.
                                Warnings (unverified, facts disagree, assumed decimals) stay inline. */}
                            <a href={routeToHash(tokenRoute(b.token))} class="bal-name" title={scaleNote ? `${label} - ${scaleNote.title}` : label} data-testid="balance-market">
                              {label}
                            </a>
                            {scaleNote ? null : <TokenNote token={shown} />}
                            {unreviewedProgram(program) ? (
                              <span class="small muted" title={t('orders.balances.unreviewedHint', { program: program ?? '?' })} data-testid="balance-unreviewed">{' '}({t('orders.balances.unreviewed')})</span>
                            ) : null}
                          </span>
                        </span>
                      </DtCell>
                      <DtCell k="available" label={t('market.balances.free')} num>
                        <Amount kind="token" value={b.free} decimals={d} unit={false} fraction={dp.free} group />
                        {b.notSpendable > 0n ? (
                          <div class="dt-sub" title={t('orders.balances.notSpendableHint')} data-testid="balance-not-spendable">
                            {t('orders.balances.notSpendable', { amount: amountCell(b.notSpendable, d).text })}
                          </div>
                        ) : null}
                      </DtCell>
                      <DtCell k="inOrders" label={t('orders.balances.inOrders')} num data-testid="balance-escrowed"><Amount kind="token" value={b.escrowed} decimals={d} unit={false} fraction={dp.escrowed} group /></DtCell>
                      <DtCell k="strays" label={t('orders.balances.strays')} num card={b.strays > 0n ? 'pair' : 'hidden'} data-testid="balance-strays"><Amount kind="token" value={b.strays} decimals={d} unit={false} fraction={dp.strays} group /></DtCell>
                      <DtCell k="kasLocked" label={lockedLabel} num card="hidden"><Amount kind="kas" value={b.kasLocked} unit={false} fraction={dp.kas} group fiat={fiat} /></DtCell>
                      <DtCell k="value" label={valueLabel} num card="end" data-testid="balance-value">
                        {valueOf(b) !== null ? <MoneyCell kas={valueOf(b)!} unit={false} fraction={dp.value} /> : <span class="dt-muted" title={t('orders.balances.noPrice')}>—</span>}
                      </DtCell>
                      <DtCell k="act" card="action">
                        {showMerge || b.orderCount > 0 ? (
                          <span class="dt-act">
                            {mergeAs ? (
                              <Button
                                small
                                disabled={!props.canSign || !!props.busy}
                                loading={props.busy === `merge-${b.token}`}
                                title={t('orders.merge.hint', { count: mergeable.get(b.token)!, max: mergeSlots(mergeAs!), txs: countText('tx', mergeTxCount(mergeable.get(b.token)!, mergeSlots(mergeAs!))) })}
                                onClick={() => props.onMergeToken!(b.token)}
                                data-testid={`orders-merge-token-${b.token}`}
                              >
                                {(frees.get(b.token) ?? 0n) > 0n
                                  ? t('orders.merge.buttonFrees', { count: mergeable.get(b.token)!, kas: formatKas(frees.get(b.token)!) })
                                  : t('orders.merge.button', { count: mergeable.get(b.token)! })}
                              </Button>
                            ) : null}
                            {b.orderCount > 0 ? (
                              <Button small variant="danger" disabled={!props.canSign} onClick={() => props.onCancelToken(b.token)} data-testid={`orders-cancel-token-${b.token}`}>
                                {t('orders.cancelToken')}
                              </Button>
                            ) : null}
                          </span>
                        ) : null}
                      </DtCell>
                    </DtRow>
                  </DtGroup>
                );
              })
            )}
          </DataTable>
          {combined.kas.onTokens > 0n && (
            <p class="muted small" style="margin:6px 0 0" data-testid="kas-on-tokens-note">{t('orders.balances.kasOnTokensNote')}</p>
          )}
          {combined.tokens.some((b) => b.strays > 0n) ? <p class="small" data-testid="strays-note">{t('orders.balances.straysNote')}</p> : null}
          <p class="small muted" data-testid="balances-old-holdings">{props.compact ? `${t('orders.balances.notShown')} ` : ''}{t('orders.balances.oldHoldings')}</p>
          {combined.lostStrayTokens > 0n ? <Banner tone="warn" data-testid="strays-lost">{t('orders.balances.straysLost')}</Banner> : null}
        </div>
      ) : null}
    </Section>
  );
}

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
import { balanceColumnFractions, balanceProgram, orderTokenPrograms, unreviewedProgram } from './balances-model';
import { convertibleCounts, mergeAllFrees, mergeTokenFor, mergeableCounts } from './merge-model';
import type { OrdersData } from './orders-data';
import { formatRowDate, rowTimeTitle } from '../kit/format';
import { unlockOpen, useUnlockClock } from './unlock-clock';
import './balances.css';

const BALANCE_SORT_KEYS = ['asset', 'available', 'inOrders', 'strays', 'kasLocked', 'value'] as const;
type BalanceSortKey = (typeof BALANCE_SORT_KEYS)[number];

/** The columns of the balances table: the asset, the figures and the buttons, with no title. */
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

/**
 * The panel until the list it is read with is in. It shows its card and its heading in their place.
 */
export function BalancesFrame() {
  return <Section title={t('orders.balances.title')} subtitle={t('orders.balances.subtitle')} data-testid="balances-panel"><BalancesSkeleton /></Section>;
}

/**
 * The balances wallets do not show: tokens inside the wallet's sell orders (0x04 custody), stray
 * tokens and KAS locked in orders, next to the free amounts. Free amounts come from the node and
 * the token tracker. The rest comes from the wallet's live orders, from the indexer or records
 * resolved on the node. It is drawn in the shared table (kit/DataTable.tsx), KAS first.
 */
export function BalancesPanel(props: {
  data: OrdersData;
  /**
   * The free balances are read again when this changes. That is each load of the list, not each
   * background update of it.
   */
  version?: unknown;
  pubkey: Hex;
  canSign: boolean;
  onCancelToken(token: Hex): void;
  /** Outpoints held for open orders, which are never merged. */
  reserved?: ReadonlySet<string>;
  /** An action is being planned, given by its id. */
  busy?: string | null;
  /** Merge the wallet's plain UTXOs of a token into one. */
  onMergeToken?(token: Hex): void;
  /**
   * Claim what the wallet's Collected buy orders of a token bought. For one order it opens its
   * dialog, and for several it makes one claim each.
   */
  onClaimToken?(token: Hex): void;
  /**
   * Turn the wallet's UTXOs of a token that carry a KCC-20 borrow setting into plain ones. The
   * cell's button shows when the token has no Merge all. Otherwise the Merge all dialog offers it.
   */
  onConvertToken?(token: Hex): void;
  /**
   * The page's balances (use-balances.ts): the free balances combined with the orders and strays,
   * and each token's value in KAS.
   */
  balances: WalletBalances;
  /** Only these tokens, as on a market page with its token or a pair's two. KAS is always shown. */
  onlyTokens?: ReadonlySet<Hex>;
  /** kascov.io's picture of a token, for its icon. */
  iconOf?: (covenantId: string) => KascovMeta | null;
  /**
   * A market page's panel. It has no heading, and the money figures are one line at the top. The
   * page's summary cards are the account overview of the Portfolio page only.
   */
  compact?: boolean;
  /**
   * Unlock what the wallet's creator lock of a token holds. Its button shows once the lock is open.
   */
  onUnlockToken?(token: Hex): void;
  /**
   * Release what the wallet's automatic creator lock of a token holds. Its button shows once the
   * lock is open, for any wallet: the release needs no signature.
   */
  onReleaseToken?(token: Hex): void;
  /**
   * Who can sign the unlock, the connected wallet or only the CLI. For a wallet that cannot sign
   * its script input, a muted note is shown instead.
   */
  unlockBy?: 'wallet' | 'cli';
}) {
  const services = useServices();
  const { registry } = services;
  // The USD display (Settings) shows the KAS values in dollars. The money cell reads it itself, and
  // the Amount cells here take it. Null means KAS.
  const fiat = useFiat();
  const valueLabel = t(fiat ? 'orders.col.valueUsd' : 'orders.col.valueKas');
  const lockedLabel = t(fiat ? 'market.balances.kasLockedUsd' : 'market.balances.kasLocked');
  // The viewer's sort of the token rows. KAS stays on top, and none means as the wallet lists them.
  const [sort, setSort] = useSortState<BalanceSortKey>('balances', BALANCE_SORT_KEYS);
  const sortCols = balanceColumns(valueLabel, lockedLabel);
  const { free, combined, valueOf } = props.balances;
  const tokensOnShow = (combined?.tokens ?? []).filter((b) => !props.onlyTokens || props.onlyTokens.has(b.token));
  // the node's clock for the creator locks shown: Unlock shows once the node would take it
  const lockUntils = tokensOnShow.flatMap((b) => (b.locked && b.locked.amount > 0n ? [b.locked.until] : []));
  const nodeMedian = useUnlockClock(services.node, lockUntils);

  const shownTokens = sortBy(tokensOnShow, sort, (b, key): SortValue => {
    const decimals = displayToken(registry, services.config.network, b.token)?.decimals ?? 8;
    switch (key) {
      case 'asset':
        return tokenLabel(displayToken(registry, services.config.network, b.token)!);
      case 'available':
        return scaledUnits(b.free, decimals);
      case 'inOrders':
        return scaledUnits(b.escrowed + b.bought, decimals);
      case 'strays':
        return scaledUnits(b.strays, decimals);
      case 'kasLocked':
        return b.kasLocked;
      case 'value':
        return valueOf(b);
    }
  });

  // A token outside the registry is merged under the program the node verified its UTXOs under.
  // That gives the template's slots and the extension commitment its UTXOs share.
  const orderPrograms = useMemo(() => orderTokenPrograms(props.data.views), [props.data]);
  const unlisted = useMemo(() => (free.data ? { kob: services.kob, programs: free.data.programOf ?? free.data.programs } : undefined), [free.data, services.kob]);
  // Free token UTXOs a merge may take, per token, when there are two or more.
  const mergeable = useMemo(
    () => (free.data ? mergeableCounts(free.data.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set(), unlisted) : new Map<Hex, number>()),
    [free.data, registry, props.pubkey, props.reserved, unlisted],
  );

  // Free token UTXOs with a borrow setting, per token, for "Convert to plain".
  const convertible = useMemo(
    () => (free.data ? convertibleCounts(free.data.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set(), unlisted) : new Map<Hex, number>()),
    [free.data, registry, props.pubkey, props.reserved, unlisted],
  );

  // About how much KAS Merge all frees, per token. It is shown on the button as the reason to press
  // it.
  const frees = useMemo(
    () => (free.data ? mergeAllFrees(free.data.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set(), unlisted) : new Map<Hex, bigint>()),
    [free.data, registry, props.pubkey, props.reserved, unlisted],
  );

  // Fixed decimals per column, the finest amount of the column, so every row shows the same number
  // of decimals and the decimal points line up. A token of unknown decimals, with no registry
  // entry, shows its base units with no fraction.
  const cols = useMemo(
    () => balanceColumnFractions(combined?.tokens ?? [], (tok) => displayToken(registry, services.config.network, tok)?.decimals, unitsFraction),
    [combined, registry],
  );

  // The figures of the table and the strip have at most 4 decimals, with the whole amount as the
  // title. The finest of each column is used on every row of it so the decimal points line up.
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

  // The book's Refresh is an icon whose glyph spins while it reads. The button keeps its size, and
  // its words are its accessible name and tooltip.
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
        // The money figures are one line. The items wrap between each other and a figure never
        // breaks. Refresh is at the right end.
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
        // The rows rise in as the lists of the other tabs do (`.ml-in`, market.css), once, when the
        // tab is opened. A refresh of the figures keeps this box and does not replay it.
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
                // Shown in whole tokens also without a registry entry (app/token-display.ts), on
                // this browser's issuance, else 8 assumed decimals.
                const shown = displayToken(registry, services.config.network, b.token)!;
                const d = shown.decimals;
                const label = tokenLabel(shown);
                const scaleNote = shown.decimalsSource === 'scale' && !shown.declarationConflicts?.length ? decimalsNote(shown) : null;
                const mergeAs = props.onMergeToken && mergeable.has(b.token) ? mergeTokenFor(b.token, free.data!.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set(), unlisted) : null;
                const showMerge = !!mergeAs;
                const claimable = !!props.onClaimToken && b.bought > 0n;
                // "Convert to plain" is in the Merge all dialog where that button shows. Only a
                // token with nothing to merge has it here, and the cell keeps its width.
                const showConvert = !!props.onConvertToken && convertible.has(b.token) && !showMerge;
                const program = balanceProgram(b.token, { listed: info?.program, free: free.data!.programs, locked: free.data!.locked, orders: orderPrograms });
                // The wallet's creator lock of the token is a sub-line under Available. Once it is
                // open, Unlock, or the CLI's command, is in the action cell.
                // An automatic lock opens by itself: Release now, with no signature, for any wallet.
                const locked = b.locked && b.locked.amount > 0n ? b.locked : null;
                const open = !!locked && unlockOpen(locked.until, nodeMedian);
                const auto = locked?.kind === 'auto';
                const showUnlock = open && !auto && !!props.onUnlockToken && props.unlockBy !== 'cli';
                const showCli = open && !auto && props.unlockBy === 'cli';
                const showRelease = open && auto && !!props.onReleaseToken;
                return (
                  <DtGroup key={b.token} data-testid={`balance-row-${b.token}`}>
                    <DtRow>
                      <DtCell k="asset" card="head">
                        <span class="dt-market">
                          <IdentityBlock scope="wallet" row={{ covenantId: b.token, ticker: shown.ticker, kascov: props.iconOf?.(b.token) ?? null }} size="sm" />
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
                        {locked ? (
                          <div class="dt-sub" title={`${t(auto ? (open ? 'orders.balances.lockedAutoOpenHint' : 'orders.balances.lockedAutoHint') : open ? 'orders.balances.lockedOpenHint' : 'orders.balances.lockedHint')} ${rowTimeTitle(Number(locked.until))}`} data-testid="balance-locked">
                            {t(auto ? (open ? 'orders.balances.lockedAutoOpen' : 'orders.balances.lockedAuto') : open ? 'orders.balances.lockedOpen' : 'orders.balances.locked', { amount: amountCell(locked.amount, d).text, date: formatRowDate(Number(locked.until)) })}
                          </div>
                        ) : null}
                      </DtCell>
                      <DtCell k="inOrders" label={t('orders.balances.inOrders')} num data-testid="balance-escrowed">
                        <Amount kind="token" value={b.escrowed + b.bought} decimals={d} unit={false} fraction={dp.escrowed} group />
                        {b.bought > 0n ? (
                          // Tokens bought and held by Collected buy orders. They are the wallet's
                          // own, next to what is for sale.
                          <>
                            {b.escrowed > 0n ? <div class="dt-sub" data-testid="balance-for-sale">{t('orders.balances.forSale', { amount: amountCell(b.escrowed, d).text })}</div> : null}
                            <div class="dt-sub" title={t('orders.balances.boughtHint')} data-testid="balance-bought">{t('orders.balances.bought', { amount: amountCell(b.bought, d).text })}</div>
                          </>
                        ) : null}
                      </DtCell>
                      <DtCell k="strays" label={t('orders.balances.strays')} num card={b.strays > 0n ? 'pair' : 'hidden'} data-testid="balance-strays"><Amount kind="token" value={b.strays} decimals={d} unit={false} fraction={dp.strays} group /></DtCell>
                      <DtCell k="kasLocked" label={lockedLabel} num card="hidden"><Amount kind="kas" value={b.kasLocked} unit={false} fraction={dp.kas} group fiat={fiat} /></DtCell>
                      <DtCell k="value" label={valueLabel} num card="end" data-testid="balance-value">
                        {valueOf(b) !== null ? <MoneyCell kas={valueOf(b)!} unit={false} fraction={dp.value} /> : <span class="dt-muted" title={t('orders.balances.noPrice')}>—</span>}
                      </DtCell>
                      <DtCell k="act" card="action">
                        {showMerge || showConvert || b.orderCount > 0 || claimable || showUnlock || showCli || showRelease ? (
                          <span class="dt-act">
                            {showRelease ? (
                              <Button
                                small
                                disabled={!!props.busy}
                                loading={props.busy === `release-${b.token}`}
                                title={t('orders.release.hint', { amount: amountCell(locked!.amount, d).text, ticker: shown.ticker })}
                                onClick={() => props.onReleaseToken!(b.token)}
                                data-testid={`balance-release-${b.token}`}
                              >
                                {t('orders.release.button')}
                              </Button>
                            ) : null}
                            {showUnlock ? (
                              <Button
                                small
                                disabled={!props.canSign || !!props.busy}
                                loading={props.busy === `unlock-${b.token}`}
                                title={t('orders.unlock.hint', { amount: amountCell(locked!.amount, d).text, ticker: shown.ticker })}
                                onClick={() => props.onUnlockToken!(b.token)}
                                data-testid={`balance-unlock-${b.token}`}
                              >
                                {t('orders.unlock.button')}
                              </Button>
                            ) : null}
                            {showCli ? (
                              <span class="small muted" title={`kob token unlock ${b.token}`} data-testid={`balance-unlock-cli-${b.token}`}>{t('orders.unlock.cli')}</span>
                            ) : null}
                            {claimable ? (
                              <Button
                                small
                                disabled={!props.canSign || !!props.busy}
                                loading={props.busy === `claim-${b.token}`}
                                title={b.boughtOrders > 1 ? t('orders.claim.allHint', { count: b.boughtOrders }) : t('orders.claim.hint', { amount: amountCell(b.bought, d).text, ticker: shown.ticker })}
                                onClick={() => props.onClaimToken!(b.token)}
                                data-testid={`balance-claim-${b.token}`}
                              >
                                {b.boughtOrders > 1 ? t('orders.claim.all', { count: b.boughtOrders }) : t('orders.claim')}
                              </Button>
                            ) : null}
                            {mergeAs ? (
                              // The count and the KAS freed are its tooltip. The confirmation
                              // states them again before signing.
                              <Button
                                small
                                disabled={!props.canSign || !!props.busy}
                                loading={props.busy === `merge-${b.token}`}
                                title={t((frees.get(b.token) ?? 0n) > 0n ? 'orders.merge.hintFrees' : 'orders.merge.hint', {
                                  count: mergeable.get(b.token)!,
                                  kas: formatKas(frees.get(b.token) ?? 0n),
                                  max: mergeSlots(mergeAs!),
                                  txs: countText('tx', mergeTxCount(mergeable.get(b.token)!, mergeSlots(mergeAs!))),
                                })}
                                onClick={() => props.onMergeToken!(b.token)}
                                data-testid={`orders-merge-token-${b.token}`}
                              >
                                {t('orders.merge.button')}
                              </Button>
                            ) : null}
                            {showConvert ? (
                              <Button
                                small
                                variant="ghost"
                                disabled={!props.canSign || !!props.busy}
                                loading={props.busy === `convert-${b.token}`}
                                title={t('orders.convert.hint', { count: convertible.get(b.token)! })}
                                onClick={() => props.onConvertToken!(b.token)}
                                data-testid={`orders-convert-token-${b.token}`}
                              >
                                {t('orders.convert.button')}
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

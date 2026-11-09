import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { displayToken } from '../../app/token-display';
import { TokenNote } from '../market/TokenNote';
import { routeToHash, tokenRoute } from '../../app/router';
import { t } from '../../i18n';
import type { Hex } from '../../kob/types';
import { Amount, Banner, Button, ErrorBanner, Section, Table, TableMessage, unitsFraction } from '../kit';
import { formatKas, formatUnits } from '../../kob/units';
import { tokenLabel } from '../market/token-model';
import { IdentityBlock } from '../market/Fingerprint';
import type { KascovMeta } from '../market/token-model';
import type { WalletBalances } from './use-balances';
import { balanceColumnFractions, unreviewedProgram } from './balances-model';
import { mergeableCounts, nextMergeFrees } from './merge-model';
import type { OrdersData } from './orders-data';
import './balances.css';

function BalancesSkeleton() {
  return (
    <div role="status" aria-label={t('common.loading')} data-testid="balances-loading">
      {Array.from({ length: 3 }, (_, i) => <div key={i} class="skeleton skeleton-line" />)}
    </div>
  );
}

/** The panel until the list it is read with is in: its card and its heading, in their place. */
export function BalancesFrame() {
  return <Section title={t('orders.balances.title')} data-testid="balances-panel"><BalancesSkeleton /></Section>;
}

/**
 * The balances wallets do not show: tokens inside the wallet's sell orders (0x04 custody), stray tokens, KAS locked in orders, next to the free
 * amounts. Free amounts come from the node / token tracker, the rest from the wallet's live orders (indexer, or records resolved on the node).
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
}) {
  const services = useServices();
  const { registry } = services;
  const { free, combined, valueOf } = props.balances;
  const shownTokens = (combined?.tokens ?? []).filter((b) => !props.onlyTokens || props.onlyTokens.has(b.token));

  // free token UTXOs a merge may take, per token (two or more)
  const mergeable = useMemo(
    () => (free.data ? mergeableCounts(free.data.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set()) : new Map<Hex, number>()),
    [free.data, registry, props.pubkey, props.reserved],
  );

  // about how much KAS the next Merge click frees, per token (shown on the button: the reason to press it)
  const frees = useMemo(
    () => (free.data ? nextMergeFrees(free.data.freeTokens, registry.byCovenantId, props.pubkey, props.reserved ?? new Set()) : new Map<Hex, bigint>()),
    [free.data, registry, props.pubkey, props.reserved],
  );

  // fixed decimals per column: the finest amount of the column, so every row shows the same number of decimals (decimal points line up)
  // (a token of unknown decimals, no registry entry, shows its base units with no fraction)
  const cols = useMemo(
    () => balanceColumnFractions(combined?.tokens ?? [], (tok) => displayToken(registry, services.config.network, tok)?.decimals, unitsFraction),
    [combined, registry],
  );

  return (
    <Section title={t('orders.balances.title')} data-testid="balances-panel" actions={<Button small onClick={free.reload} loading={free.loading && !!free.data}>{t('common.refresh')}</Button>}>
      {free.loading && !free.data ? <BalancesSkeleton /> : null}
      <ErrorBanner error={free.error} onRetry={free.reload} />
      {free.data && free.data.errors.length ? (
        <Banner tone="warn" data-testid="balances-partial">{t('market.balances.partial', { sources: free.data.errors.map((e) => e.source).join(', ') })}</Banner>
      ) : null}
      {combined && free.data ? (
        <div class="tk-in">
          <Table dense class="balances-cards" data-testid="token-balances-table" caption={t('orders.balances.tokens')}>
            <thead>
              <tr>
                <th>{t('orders.balances.asset')}</th>
                <th class="right">{t('orders.balances.available')}</th>
                <th class="right">{t('orders.balances.inOrders')}</th>
                <th class="right">{t('orders.balances.strays')}</th>
                <th class="right">{t('market.balances.kasLocked')}</th>
                <th class="right">{t('orders.col.valueKas')}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {/* KAS first: what the wallet can spend, what its orders hold, and the KAS that rides on its token coins (counted in the total) */}
              <tr data-testid="kas-balance" class="bal-kas">
                <td class="bal-token">
                  <span class="bal-asset">
                    <span class="bal-kas-icon" aria-hidden="true">K</span>
                    <span class="bal-name">KAS</span>
                  </span>
                </td>
                <td class="right" data-label={t('orders.balances.available')}>
                  <span data-testid="kas-free">{free.data.kasFree !== null ? <Amount kind="kas" value={combined.kas.free} group /> : t('common.unavailable')}</span>
                  {combined.kas.onTokens > 0n ? (
                    <div class="small muted" title={t('orders.balances.kasOnTokensHint')}>
                      {t('orders.balances.kasOnTokensPlus')} <span data-testid="kas-on-tokens"><Amount kind="kas" value={combined.kas.onTokens} group /></span>
                    </div>
                  ) : null}
                </td>
                <td class="right" data-label={t('orders.balances.inOrders')} data-testid="kas-locked"><Amount kind="kas" value={combined.kas.locked} group /></td>
                <td class="right muted" data-label={t('orders.balances.strays')}>—</td>
                <td class="right muted" data-label={t('market.balances.kasLocked')}>—</td>
                <td class="right" data-label={t('orders.col.valueKas')} data-testid="kas-total">
                  {free.data.kasFree !== null ? <Amount kind="kas" value={combined.kas.total} unit={false} group /> : '—'}
                </td>
                <td class="right bal-actions bal-actions-none" />
              </tr>
              {shownTokens.length === 0 ? (
                <TableMessage colSpan={7} data-testid="balances-empty">{t('orders.balances.empty')}</TableMessage>
              ) : (
                shownTokens.map((b) => {
                  const info = registry.byCovenantId.get(b.token);
                  // shown in whole tokens also without a registry entry (app/token-display.ts: this browser's issuance, else 8 assumed decimals)
                  const shown = displayToken(registry, services.config.network, b.token)!;
                  const d = shown.decimals;
                  const fr = (col: number) => col;
                  const baseUnits = null;
                  const label = tokenLabel(shown);
                  const showMerge = !!(props.onMergeToken && info && mergeable.has(b.token));
                  const program = info?.program ?? free.data!.programs.get(b.token) ?? null;
                  return (
                    <tr key={b.token} data-testid={`balance-row-${b.token}`}>
                      <td class="bal-token">
                        <span class="bal-asset">
                          <IdentityBlock row={{ covenantId: b.token, ticker: shown.ticker, kascov: props.iconOf?.(b.token) ?? null }} size="sm" />
                          <a href={routeToHash(tokenRoute(b.token))} class="bal-name" title={label} data-testid="balance-market">
                            {label}
                          </a>
                        </span>
                        <TokenNote token={shown} />
                        {unreviewedProgram(program) ? (
                          <span class="small muted" title={t('orders.balances.unreviewedHint', { program: program ?? '?' })} data-testid="balance-unreviewed">{' '}({t('orders.balances.unreviewed')})</span>
                        ) : null}
                      </td>
                      <td class="right" data-label={t('market.balances.free')}>
                        <Amount kind="token" value={b.free} decimals={d} unit={false} fraction={fr(cols.free)} group />
                        {baseUnits}
                        {b.notSpendable > 0n ? (
                          <div class="small muted" title={t('orders.balances.notSpendableHint')} data-testid="balance-not-spendable">
                            {t('orders.balances.notSpendable', { amount: formatUnits(b.notSpendable, d) })}
                          </div>
                        ) : null}
                      </td>
                      <td class="right" data-label={t('orders.balances.inOrders')} data-testid="balance-escrowed"><Amount kind="token" value={b.escrowed} decimals={d} unit={false} fraction={fr(cols.escrowed)} group /></td>
                      <td class="right" data-label={t('orders.balances.strays')} data-testid="balance-strays"><Amount kind="token" value={b.strays} decimals={d} unit={false} fraction={fr(cols.strays)} group /></td>
                      <td class="right" data-label={t('market.balances.kasLocked')}><Amount kind="kas" value={b.kasLocked} unit={false} fraction={cols.kas} group /></td>
                      <td class="right" data-label={t('orders.col.valueKas')} data-testid="balance-value">
                        {valueOf(b) !== null ? <Amount kind="kas" value={valueOf(b)!} unit={false} group /> : <span class="muted" title={t('orders.balances.noPrice')}>—</span>}
                      </td>
                      <td class={`right bal-actions${showMerge || b.orderCount > 0 ? '' : ' bal-actions-none'}`}>
                        {showMerge && info ? (
                          <Button
                            small
                            disabled={!props.canSign || !!props.busy}
                            loading={props.busy === `merge-${b.token}`}
                            title={t('orders.merge.hint', { count: mergeable.get(b.token)!, max: info.slots.inputs })}
                            onClick={() => props.onMergeToken!(b.token)}
                            data-testid={`orders-merge-token-${b.token}`}
                          >
                            {(frees.get(b.token) ?? 0n) > 0n
                              ? t('orders.merge.buttonFrees', { count: mergeable.get(b.token)!, kas: formatKas(frees.get(b.token)!) })
                              : t('orders.merge.button', { count: mergeable.get(b.token)! })}
                          </Button>
                        ) : null}{' '}
                        {b.orderCount > 0 ? (
                          <Button small variant="danger" disabled={!props.canSign} onClick={() => props.onCancelToken(b.token)} data-testid={`orders-cancel-token-${b.token}`}>
                            {t('orders.cancelToken')}
                          </Button>
                        ) : null}
                      </td>
                    </tr>
                  );
                })
              )}
            </tbody>
          </Table>
          {combined.kas.onTokens > 0n && (
            <p class="muted small" style="margin:6px 0 0" data-testid="kas-on-tokens-note">{t('orders.balances.kasOnTokensNote')}</p>
          )}
          {combined.tokens.some((b) => b.strays > 0n) ? <p class="small" data-testid="strays-note">{t('orders.balances.straysNote')}</p> : null}
          <p class="small muted" data-testid="balances-old-holdings">{t('orders.balances.oldHoldings')}</p>
          {combined.lostStrayTokens > 0n ? <Banner tone="warn" data-testid="strays-lost">{t('orders.balances.straysLost')}</Banner> : null}
        </div>
      ) : null}
    </Section>
  );
}

import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { displayToken } from '../../app/token-display';
import { TokenNote } from '../market/TokenNote';
import { routeToHash, tokenRoute } from '../../app/router';
import { t } from '../../i18n';
import type { Hex } from '../../kob/types';
import { Amount, Banner, Button, ErrorBanner, KeyValueList, Loading, Section, Table, TableMessage, unitsFraction, useAsync } from '../kit';
import { formatKas, formatUnits } from '../../kob/units';
import { tokenLabel } from '../market/token-model';
import { combineFreeBalances, loadFreeBalances } from './balances-data';
import { balanceColumnFractions, unreviewedProgram } from './balances-model';
import { mergeableCounts, nextMergeFrees } from './merge-model';
import type { OrdersData } from './orders-data';

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
}) {
  const services = useServices();
  const { registry } = services;
  const free = useAsync((_signal) => loadFreeBalances(services, props.pubkey, registry.tokens, { anyToken: true }), [props.pubkey, props.version ?? props.data]);

  const combined = useMemo(() => {
    if (!free.data) return null;
    const live = props.data.views.filter((v) => v.status === 'open' || v.status === 'partial');
    return combineFreeBalances(free.data, { maker: props.pubkey, liveOrders: [...live, ...props.data.snapshots], strays: props.data.strays });
  }, [free.data, props.data, props.pubkey]);

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
      {free.loading && !free.data ? <Loading /> : null}
      <ErrorBanner error={free.error} onRetry={free.reload} />
      {free.data && free.data.errors.length ? (
        <Banner tone="warn" data-testid="balances-partial">{t('market.balances.partial', { sources: free.data.errors.map((e) => e.source).join(', ') })}</Banner>
      ) : null}
      {combined && free.data ? (
        <>
          <KeyValueList
            compact
            data-testid="kas-balance"
            items={[
              { label: t('market.balances.kasFree'), value: free.data.kasFree !== null ? <Amount kind="kas" value={combined.kas.free} group /> : t('common.unavailable'), 'data-testid': 'kas-free' },
              { label: t('market.balances.kasLocked'), value: <Amount kind="kas" value={combined.kas.locked} group />, 'data-testid': 'kas-locked' },
              {
                label: <span title={t('orders.balances.kasOnTokensHint')}>{t('orders.balances.kasOnTokens')}</span>,
                value: <Amount kind="kas" value={combined.kas.onTokens} group />,
                'data-testid': 'kas-on-tokens',
                show: combined.kas.onTokens > 0n,
              },
              { label: t('common.total'), value: <Amount kind="kas" value={combined.kas.total} group />, show: free.data.kasFree !== null },
            ]}
          />
          <Table dense data-testid="token-balances-table" caption={t('orders.balances.tokens')}>
            <thead>
              <tr>
                <th>{t('market.col.token')}</th>
                <th class="right">{t('market.balances.free')}</th>
                <th class="right">{t('orders.balances.inOrders')}</th>
                <th class="right">{t('orders.balances.strays')}</th>
                <th class="right">{t('market.balances.kasLocked')}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {combined.tokens.length === 0 ? (
                <TableMessage colSpan={6} data-testid="balances-empty">{t('orders.balances.empty')}</TableMessage>
              ) : (
                combined.tokens.map((b) => {
                  const info = registry.byCovenantId.get(b.token);
                  // shown in whole tokens also without a registry entry (app/token-display.ts: this browser's issuance, else 8 assumed decimals)
                  const shown = displayToken(registry, services.config.network, b.token)!;
                  const d = shown.decimals;
                  const fr = (col: number) => col;
                  const baseUnits = null;
                  const program = info?.program ?? free.data!.programs.get(b.token) ?? null;
                  return (
                    <tr key={b.token} data-testid={`balance-row-${b.token}`}>
                      <td>
                        <a href={routeToHash(tokenRoute(b.token))} data-testid="balance-market">
                          {tokenLabel(shown)}
                        </a>
                        <TokenNote token={shown} />
                        {unreviewedProgram(program) ? (
                          <span class="small muted" title={t('orders.balances.unreviewedHint', { program: program ?? '?' })} data-testid="balance-unreviewed">{' '}({t('orders.balances.unreviewed')})</span>
                        ) : null}
                      </td>
                      <td class="right">
                        <Amount kind="token" value={b.free} decimals={d} unit={false} fraction={fr(cols.free)} group />
                        {baseUnits}
                        {b.notSpendable > 0n ? (
                          <div class="small muted" title={t('orders.balances.notSpendableHint')} data-testid="balance-not-spendable">
                            {t('orders.balances.notSpendable', { amount: formatUnits(b.notSpendable, d) })}
                          </div>
                        ) : null}
                      </td>
                      <td class="right" data-testid="balance-escrowed"><Amount kind="token" value={b.escrowed} decimals={d} unit={false} fraction={fr(cols.escrowed)} group /></td>
                      <td class="right" data-testid="balance-strays"><Amount kind="token" value={b.strays} decimals={d} unit={false} fraction={fr(cols.strays)} group /></td>
                      <td class="right"><Amount kind="kas" value={b.kasLocked} unit={false} fraction={cols.kas} group /></td>
                      <td class="right">
                        {props.onMergeToken && info && mergeable.has(b.token) ? (
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
          {combined.tokens.some((b) => b.strays > 0n) ? <p class="small" data-testid="strays-note">{t('orders.balances.straysNote')}</p> : null}
          <p class="small muted" data-testid="balances-old-holdings">{t('orders.balances.oldHoldings')}</p>
          {combined.lostStrayTokens > 0n ? <Banner tone="warn" data-testid="strays-lost">{t('orders.balances.straysLost')}</Banner> : null}
        </>
      ) : null}
    </Section>
  );
}

import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import type { Hex } from '../../kob/types';
import { Amount, Banner, Button, ErrorBanner, KeyValueList, Loading, Section, Table, TableMessage, unitsFraction, useAsync } from '../kit';
import { labelState, tokenLabel } from '../market/token-model';
import { loadFreeBalances } from './balances-data';
import { combineBalances } from './balances-model';
import type { OrdersData } from './orders-data';

/**
 * The balances wallets do not show: tokens inside the wallet's sell orders (0x04 custody), stray tokens, KAS locked in orders, next to the free
 * amounts. Free amounts come from the node / token tracker, the rest from the wallet's live orders (indexer, or records resolved on the node).
 */
export function BalancesPanel(props: { data: OrdersData; pubkey: Hex; canSign: boolean; onCancelToken(token: Hex): void }) {
  const services = useServices();
  const { registry } = services;
  const free = useAsync((_signal) => loadFreeBalances(services, props.pubkey, registry.tokens), [props.pubkey, props.data]);

  const combined = useMemo(() => {
    if (!free.data) return null;
    const live = props.data.views.filter((v) => v.status === 'open' || v.status === 'partial');
    return combineBalances({ maker: props.pubkey, freeKas: free.data.kasFree ?? 0n, freeTokens: free.data.freeTokens, liveOrders: [...live, ...props.data.snapshots], strays: props.data.strays });
  }, [free.data, props.data, props.pubkey]);

  // fixed decimals per column: the finest amount of the column, so every row shows the same number of decimals (decimal points line up)
  const cols = useMemo(() => {
    const out = { free: 0, escrowed: 0, strays: 0, kas: 0 };
    for (const b of combined?.tokens ?? []) {
      const d = registry.byCovenantId.get(b.token)?.decimals ?? 0;
      out.free = Math.max(out.free, unitsFraction(b.free, d));
      out.escrowed = Math.max(out.escrowed, unitsFraction(b.escrowed, d));
      out.strays = Math.max(out.strays, unitsFraction(b.strays, d));
      out.kas = Math.max(out.kas, unitsFraction(b.kasLocked, 8));
    }
    return out;
  }, [combined, registry]);

  return (
    <Section title={t('orders.balances.title')} data-testid="balances-panel" actions={<Button small onClick={free.reload} loading={free.loading && !!free.data}>{t('common.refresh')}</Button>}>
      <p class="small muted">{t('orders.balances.explain')}</p>
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
              { label: t('market.balances.kasFree'), value: free.data.kasFree !== null ? <Amount kind="kas" value={combined.kas.free} /> : t('common.unavailable'), 'data-testid': 'kas-free' },
              { label: t('market.balances.kasLocked'), value: <Amount kind="kas" value={combined.kas.locked} />, 'data-testid': 'kas-locked' },
              { label: t('common.total'), value: <Amount kind="kas" value={combined.kas.total} />, show: free.data.kasFree !== null },
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
                  const d = info?.decimals ?? 0;
                  return (
                    <tr key={b.token} data-testid={`balance-row-${b.token}`}>
                      <td>{info ? tokenLabel(info, t(`market.state.${labelState(info)}`, { hash: info.customRegistry ?? '' })) : t('orders.unknownToken', { id: `${b.token.slice(0, 4)}…${b.token.slice(-4)}` })}</td>
                      <td class="right"><Amount kind="token" value={b.free} decimals={d} unit={false} fraction={cols.free} group /></td>
                      <td class="right" data-testid="balance-escrowed"><Amount kind="token" value={b.escrowed} decimals={d} unit={false} fraction={cols.escrowed} group /></td>
                      <td class="right" data-testid="balance-strays"><Amount kind="token" value={b.strays} decimals={d} unit={false} fraction={cols.strays} group /></td>
                      <td class="right"><Amount kind="kas" value={b.kasLocked} unit={false} fraction={cols.kas} group /></td>
                      <td class="right">
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
          {combined.lostStrayTokens > 0n ? <Banner tone="warn" data-testid="strays-lost">{t('orders.balances.straysLost')}</Banner> : null}
        </>
      ) : null}
    </Section>
  );
}

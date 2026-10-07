import { useServices } from '../../app/context';
import type { StrayView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { longId } from '../../kob/registry';
import { Amount, Badge, Button, CopyText, Section, Table, unitsFraction } from '../kit';

/**
 * Stray tokens: token UTXOs someone sent to one of your orders from outside the protocol. They are inert (never liquidity) and only a spend of that
 * order by its maker moves them: the maker's SWEEP (the order lives on, unchanged) or its cancel. Foreign strays (another token than the order
 * trades) go back too when the indexer proved their state; once the order is terminated nothing can move a stray any more: it is lost.
 */
export function StraysPanel(props: {
  strays: StrayView[];
  /** the tokens of each order (its token; a pair order also its quote token B), by order id: a stray of another token is foreign; unknown orders are not judged */
  orderTokens?: ReadonlyMap<string, readonly string[]>;
  /** live orders of the wallet a sweep can spend (by order id) */
  sweepableOrders?: ReadonlySet<string>;
  canSign?: boolean;
  /** the id of an action being prepared */
  busy?: string | null;
  onSweep?: (orderId: string) => void;
}) {
  const { registry } = useServices();
  const list = props.strays.filter((s) => !s.spent);
  if (!list.length) return null;
  const sweepOffered = new Set<string>();
  // one fixed number of decimals for the quantity column: the finest amount of its rows (decimal points line up)
  const qtyFraction = list.reduce((m, s) => Math.max(m, unitsFraction(BigInt(s.state?.amount ?? s.amount), registry.byCovenantId.get(s.token)?.decimals ?? 0)), 0);
  return (
    <Section title={t('orders.strays.title')} data-testid="orders-strays">
      <p class="small">{t('orders.strays.explain')}</p>
      <Table dense>
        <thead>
          <tr>
            <th>{t('market.col.token')}</th>
            <th class="right">{t('common.quantity')}</th>
            <th>{t('orders.orderId')}</th>
            <th>{t('common.status')}</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {list.map((s) => {
            const info = registry.byCovenantId.get(s.token);
            const amount = BigInt(s.state?.amount ?? s.amount);
            const tokens = props.orderTokens?.get(s.owner);
            // a stray of another token than the order trades (a pair order: neither of its two): foreign. A sweep or cancel returns it when its
            // state and program are proven; without them nothing can build its spend
            const foreign = !!s.foreign || (!!tokens && !tokens.includes(s.token));
            const unproven = foreign && (!s.program || !s.state);
            const live = !s.lost && (props.sweepableOrders?.has(s.owner) ?? false);
            const offer = live && !unproven && !!props.onSweep && !sweepOffered.has(s.owner);
            if (offer) sweepOffered.add(s.owner);
            return (
              <tr key={`${s.txid}:${s.index}`} data-testid="stray-row" data-lost={s.lost || unproven ? '1' : '0'} data-foreign={foreign ? '1' : '0'} data-owner={s.owner}>
                <td>
                  {info ? info.ticker : longId(s.token)}
                  {!info ? (
                    <>
                      {' '}
                      <Badge tone="warn" title={t('orders.strays.unknownTokenHint')} data-testid="stray-unknown-token">{t('orders.strays.unknownToken')}</Badge>
                    </>
                  ) : null}
                </td>
                <td class="right"><Amount kind="token" value={amount} decimals={info?.decimals ?? 0} unit={false} fraction={qtyFraction} group /></td>
                <td><CopyText value={s.owner} head={8} tail={6} /></td>
                <td>
                  {s.lost ? (
                    <Badge tone="bad" title={t('orders.strays.lostHint')}>{t('orders.strays.lost')}</Badge>
                  ) : unproven ? (
                    <Badge tone="bad" title={t('orders.strays.unprovenHint')} data-testid="stray-unproven">{t('orders.strays.unproven')}</Badge>
                  ) : (
                    <Badge tone="warn">{t('orders.strays.recoverable')}</Badge>
                  )}
                  {foreign ? (
                    <>
                      {' '}
                      <Badge tone="info" title={t('orders.strays.foreignHint')} data-testid="stray-foreign">{t('orders.strays.foreign')}</Badge>
                    </>
                  ) : null}
                </td>
                <td class="right">
                  {offer ? (
                    <Button
                      small
                      disabled={!props.canSign || props.busy === s.owner}
                      loading={props.busy === s.owner}
                      title={t('orders.sweep.hint')}
                      onClick={() => props.onSweep!(s.owner)}
                      data-testid={`stray-sweep-${s.owner}`}
                    >
                      {t('orders.sweep.action')}
                    </Button>
                  ) : null}
                </td>
              </tr>
            );
          })}
        </tbody>
      </Table>
    </Section>
  );
}

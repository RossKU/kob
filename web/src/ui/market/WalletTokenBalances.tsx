import { useServices, useWallet } from '../../app/context';
import { t } from '../../i18n';
import type { TokenInfo } from '../../kob/registry';
import { Amount, Banner, ErrorBanner, KeyValueList, Loading, Section, useAsync } from '../kit';
import { loadBalances } from '../orders/balances-data';

/** The connected wallet's balances of ONE token: free, escrowed in orders (0x04 custody, invisible in wallets), strays, KAS locked. */
export function WalletTokenBalances(props: { token: TokenInfo; refreshKey?: number }) {
  const services = useServices();
  const wallet = useWallet();
  const pubkey = wallet.info?.pubkey;
  const st = useAsync(
    async (signal) => {
      if (!pubkey) return null;
      return loadBalances(services, pubkey, { tokens: [props.token], token: props.token.covenantId, signal });
    },
    [pubkey, props.token.covenantId, props.refreshKey],
  );

  if (!pubkey) {
    return (
      <Section title={t('market.balances.title', { ticker: props.token.ticker })} data-testid="wallet-token-balances">
        <p class="muted">{t('market.balances.connect')}</p>
      </Section>
    );
  }
  const b = st.data?.combined.tokens.find((x) => x.token === props.token.covenantId);
  const d = props.token.decimals;
  return (
    <Section title={t('market.balances.title', { ticker: props.token.ticker })} actions={undefined} data-testid="wallet-token-balances">
      {st.loading && !st.data ? <Loading /> : null}
      <ErrorBanner error={st.error} onRetry={st.reload} />
      {st.data?.errors.length ? (
        <Banner tone="warn" data-testid="balances-partial">
          {t('market.balances.partial', { sources: st.data.errors.map((e) => e.source).join(', ') })}
        </Banner>
      ) : null}
      {st.data ? (
        <KeyValueList
          compact
          data-testid="token-balances"
          items={[
            { label: t('market.balances.free'), value: <Amount kind="token" value={b?.free ?? 0n} decimals={d} unit={props.token.ticker} />, 'data-testid': 'balance-free' },
            { label: t('market.balances.escrowed'), value: <Amount kind="token" value={b?.escrowed ?? 0n} decimals={d} unit={props.token.ticker} />, 'data-testid': 'balance-escrowed' },
            { label: t('market.balances.strays'), value: <Amount kind="token" value={b?.strays ?? 0n} decimals={d} unit={props.token.ticker} />, 'data-testid': 'balance-strays', show: (b?.strays ?? 0n) > 0n },
            { label: t('market.balances.kasFree'), value: st.data.kasKnown ? <Amount kind="kas" value={st.data.combined.kas.free} /> : t('common.unavailable'), 'data-testid': 'balance-kas-free' },
            { label: t('market.balances.kasLocked'), value: <Amount kind="kas" value={b?.kasLocked ?? 0n} />, 'data-testid': 'balance-kas-locked' },
          ]}
        />
      ) : null}
      <p class="small muted" style="margin-top:8px">{t('market.balances.explain')}</p>
    </Section>
  );
}

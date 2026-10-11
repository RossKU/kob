import { t } from '../../i18n';
import { Banner } from '../kit';
import { shortCovenantId } from './token-model';

/** The short page a direct link to a token this build cannot trade leads to: no market, no ticket (such a token is not listed or offered anywhere). */
export function NotTradablePage(props: { ticker: string; covenantId: string; reason: string | null }) {
  const name = props.ticker === '' ? shortCovenantId(props.covenantId) : `${props.ticker} (${shortCovenantId(props.covenantId)})`;
  return (
    <div class="stack" data-testid="not-tradable-page">
      <Banner tone="warn" title={t('market.notTradable.title', { ticker: name })}>{t('market.notTradable.body', { reason: props.reason ? `: ${props.reason}` : '' })}</Banner>
      <div><a href="#/market">{t('market.token.back')}</a></div>
    </div>
  );
}

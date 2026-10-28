// The one legend above the market list: what the picture beside a token is, what "via kascov" says and (behind a tap, not a tooltip) what each badge
// means, so no row repeats it.
import { FINGERPRINT_ALGORITHM } from '../../app/fingerprint';
import { t } from '../../i18n';
import { TOKEN_FACT_IDS } from './token-model';

export function ListLegend() {
  return (
    <div class="ml-legend small muted" data-testid="list-legend">
      <p data-testid="legend-fingerprint">{t('market.legend.fingerprint', { algorithm: FINGERPRINT_ALGORITHM })}</p>
      <p data-testid="legend-source">
        <strong>{t('market.kascov.via')}</strong>: {t('market.kascov.source.nameTicker')}. {t('market.legend.sourceVouch')}
      </p>
      <details class="ml-legend-badges" data-testid="legend-badges">
        <summary>{t('market.legend.badges')}</summary>
        <ul class="tf-list">
          {TOKEN_FACT_IDS.map((id) => (
            <li key={id} data-fact={id} data-testid={`legend-fact-${id}`}>
              <strong>{t(`market.fact.${id}.label`)}</strong>: {t(`market.fact.${id}.text`)}
            </li>
          ))}
          <li data-testid="legend-issuer-control">
            <strong>{t('market.badge.issuerControl')}</strong>: {t('market.powers.warning')}
          </li>
        </ul>
      </details>
    </div>
  );
}

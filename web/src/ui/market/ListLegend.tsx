// The legend above the market list, collapsed behind one small info control: what the picture beside a token is, what "via kascov" says and (behind a tap,
// not a tooltip) what each badge means, so no row repeats it. The attribution stays on screen as a short label next to the control ("names via kascov").
import { useState } from 'preact/hooks';
import { FINGERPRINT_ALGORITHM } from '../../app/fingerprint';
import { t } from '../../i18n';
import { TOKEN_FACT_IDS } from './token-model';

export function ListLegend(props: { names?: 'ok' | 'loading' | 'unavailable' | 'off' }) {
  const [open, setOpen] = useState(false);
  return (
    <div class="ml-legend small muted" data-testid="list-legend">
      <div class="ml-legend-bar">
        <button
          type="button"
          class="ml-info"
          aria-expanded={open ? 'true' : 'false'}
          aria-controls="ml-legend-body"
          aria-label={t('market.legend.toggle')}
          title={t('market.legend.toggle')}
          data-testid="legend-toggle"
          onClick={() => setOpen((o) => !o)}
        >
          <span aria-hidden="true">i</span>
        </button>
        <span data-testid="legend-attribution" data-names={props.names ?? 'ok'}>
          {t(props.names === 'unavailable' ? 'market.legend.attributionUnavailable' : props.names === 'loading' ? 'market.legend.attributionLoading' : 'market.legend.attribution')}
        </span>
      </div>
      {open ? (
        <div id="ml-legend-body" class="ml-legend-body" data-testid="legend-body">
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
      ) : null}
    </div>
  );
}

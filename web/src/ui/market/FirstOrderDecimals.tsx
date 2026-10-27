// The decimals of a token's first order: no order of the token exists, so nothing tells its price scale (it declares no decimals on chain). One
// field, prefilled only with the decimals of a token issued in this browser; the user confirms them. Once an order exists its scale decides.
import { useState } from 'preact/hooks';
import { t } from '../../i18n';
import { MAX_OPEN_DECIMALS, parseOpenDecimals } from '../../kob/open-token';
import { Banner, Button, Field, Section } from '../kit';

export function FirstOrderDecimals(props: { known: number | null; onSet: (decimals: number) => void; /** the decimals a third-party list (kascov.io) says the creator declared: shown as a claim, never prefilled */ claim?: number | null }) {
  const [text, setText] = useState(props.known === null ? '' : String(Math.min(props.known, MAX_OPEN_DECIMALS)));
  const d = parseOpenDecimals(text);
  return (
    <Section title={t('market.first.title')} data-testid="first-order" class="mkt-card">
      <Banner tone="info">{t('market.first.body')}</Banner>
      {props.claim !== null && props.claim !== undefined ? <p class="small muted" data-testid="first-order-claim">{t('market.first.claim', { decimals: props.claim })}</p> : null}
      <div class="row" style="align-items:flex-start">
        <div class="grow">
          <Field
            label={t('market.first.label')}
            value={text}
            onValue={setText}
            inputMode="numeric"
            maxLength={1}
            hint={props.known !== null && text === String(Math.min(props.known, MAX_OPEN_DECIMALS)) ? t('market.first.known') : undefined}
            error={text.trim() !== '' && d === null ? t('market.first.invalid') : null}
            data-testid="first-order-decimals"
          />
        </div>
        <div style="padding-top:24px">
          <Button variant="primary" disabled={d === null} onClick={() => d !== null && props.onSet(d)} data-testid="first-order-use">
            {t('market.first.use')}
          </Button>
        </div>
      </div>
    </Section>
  );
}

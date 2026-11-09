// `#/disclaimer`: what KOB is and what it is not, linked from the footer of every page. The text is in src/i18n/en/legal.ts.
import { t } from '../../i18n';
import { siteName } from '../landing/landing-model';

/** The sections of the page, in order: `legal.<key>.title` and `legal.<key>.body`. */
export const LEGAL_SECTIONS = ['audit', 'custody', 'tokens', 'market', 'services', 'advice', 'duty', 'donations', 'warranty'] as const;

export function DisclaimerPage() {
  const site = siteName(typeof location === 'undefined' ? null : location.hostname);
  return (
    <article class="legal" data-testid="disclaimer-page">
      <h1>{t('legal.title')}</h1>
      <p class="muted small" data-testid="disclaimer-updated">{t('legal.updated')}</p>
      <p>{t('legal.intro')}</p>
      <p data-testid="disclaimer-network"><strong>{t('legal.network')}</strong></p>
      {LEGAL_SECTIONS.map((k) => (
        <section key={k} data-testid={`disclaimer-${k}`}>
          <h2>{t(`legal.${k}.title`)}</h2>
          <p>{t(`legal.${k}.body`, { site })}</p>
        </section>
      ))}
    </article>
  );
}

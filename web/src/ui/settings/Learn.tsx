import type { ComponentChildren } from 'preact';
import { t } from '../../i18n';

/**
 * One short line with the complete text one click away (review M-10): the card stays scannable, nothing is deleted. A native `details`, so it works without
 * script, keyboard and screen readers get the disclosure for free, and the full text stays in the page for search.
 */
export function Learn(props: { short: string; children: ComponentChildren; 'data-testid'?: string }) {
  return (
    <details class="learn" data-testid={props['data-testid']}>
      <summary>
        <span class="learn-short">{props.short}</span> <span class="learn-more">{t('settings.learnMore')}</span>
      </summary>
      <div class="learn-full">{props.children}</div>
    </details>
  );
}

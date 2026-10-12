import { t } from '../../i18n';
import { Banner } from '../kit';
import { SECURITY_POLICY_URL, usePreAuditBanner } from './preaudit';

/** One-time, non-blocking notice on the first visit: no external security review of the contracts. Dismissal is remembered (see preaudit.ts). */
export function PreAuditBanner() {
  const [visible, dismiss] = usePreAuditBanner();
  if (!visible) return null;
  return (
    <Banner tone="info" class="banner-compact" onDismiss={dismiss} data-testid="banner-preaudit">
      {t('preaudit.banner')}{' '}
      <a href={SECURITY_POLICY_URL} target="_blank" rel="noopener noreferrer">{t('preaudit.policy')}</a>
    </Banner>
  );
}

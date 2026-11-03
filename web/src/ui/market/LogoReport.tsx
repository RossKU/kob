// The logo of a token in its details: where the picture comes from (checked or not), "Logo hidden after a report" for a hidden one, and the "Report logo" action.
// A report hides the logo for everyone until the operator of the indexer reviews it (data/logo-blocklist.ts); the dialog also links to the GitHub form, which
// feeds the same list. KOB stores no picture, so reporting is the way a logo is taken down.
import { useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { githubReportUrl, logoBlocklist, logoBlocklistUrlOf, logoReportsUrlOf, LOGO_REASONS, reportLogo, type LogoReason, type ReportResult } from '../../data/logo-blocklist';
import { t } from '../../i18n';
import { Banner, Button, Modal, Section, Select } from '../kit';
import type { TokenRow } from './token-model';
import { useTokenLogo } from './use-token-logo';

function ReportDialog(props: { covenantId: string; onClose: () => void }) {
  const { config } = useServices();
  const [reason, setReason] = useState<LogoReason>('impersonation');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ReportResult | null>(null);
  const listUrl = logoBlocklistUrlOf(config);
  const reportsUrl = logoReportsUrlOf(listUrl);
  const send = async () => {
    if (!reportsUrl || busy) return;
    setBusy(true);
    const r = await reportLogo(reportsUrl, props.covenantId, reason);
    setBusy(false);
    setResult(r);
    if (r.ok) {
      // this viewer's own report hides the logo for this viewer at once; the list is read again for everybody else's view of it
      logoBlocklist.hideLocally(props.covenantId);
      void logoBlocklist.refresh(listUrl);
    }
  };
  const message = (r: ReportResult): { tone: 'ok' | 'warn' | 'error'; text: string } => {
    if (r.ok) return { tone: 'ok', text: t(r.result === 'hidden' ? 'market.logo.report.hidden' : 'market.logo.report.recorded') };
    if (r.reason === 'rate-limited') return { tone: 'warn', text: t('market.logo.report.rateLimited', { seconds: r.retryAfterSecs ?? 60 }) };
    return { tone: 'error', text: t(`market.logo.report.${r.reason}`) };
  };
  const done = result?.ok === true;
  const m = result ? message(result) : null;
  return (
    <Modal
      title={t('market.logo.report.title')}
      onClose={props.onClose}
      data-testid="logo-report-dialog"
      footer={
        <>
          <a class="btn btn-sm" href={githubReportUrl(props.covenantId)} target="_blank" rel="noopener noreferrer" data-testid="logo-report-github">
            {t('market.logo.report.github')}
          </a>
          {done ? null : (
            <Button variant="primary" onClick={() => void send()} loading={busy} disabled={!reportsUrl} data-testid="logo-report-send">
              {t('market.logo.report.send')}
            </Button>
          )}
        </>
      }
    >
      <p class="small">{t('market.logo.report.intro')}</p>
      {done ? null : (
        <Select
          label={t('market.logo.report.reason')}
          value={reason}
          options={LOGO_REASONS.map((v) => ({ value: v, label: t(`market.logo.report.reason.${v}`) }))}
          onValue={(v) => setReason(v as LogoReason)}
          data-testid="logo-report-reason"
        />
      )}
      {!reportsUrl && !m ? <Banner tone="info" data-testid="logo-report-unavailable">{t('market.logo.report.unavailable')}</Banner> : null}
      {m ? (
        <Banner tone={m.tone} data-testid="logo-report-result">
          {m.text}
        </Banner>
      ) : null}
    </Modal>
  );
}

/** What a token's details say about its logo; nothing for a token with no logo to speak of (letter only). */
export function LogoSection(props: { row: Pick<TokenRow, 'covenantId' | 'kascov'> }) {
  const { logo } = useTokenLogo(props.row, true);
  const [open, setOpen] = useState(false);
  if (logo.state === 'off' || logo.state === 'none' || logo.state === 'pending') return null;
  const hidden = logo.state === 'hidden';
  return (
    <Section title={t('market.logo.title')} class="mkt-card" data-testid="token-logo-section">
      <p class="small" data-testid="token-logo-note" data-logo={logo.state}>
        {hidden ? (
          <>
            <strong>{t('market.logo.hidden')}</strong> {t('market.logo.hiddenNote')}
          </>
        ) : logo.state === 'creator' ? (
          t('market.logo.creator', { host: logo.host })
        ) : logo.state === 'ipfs' ? (
          t('market.logo.ipfs')
        ) : (
          t('market.logo.checked')
        )}
      </p>
      {hidden ? null : (
        <Button small onClick={() => setOpen(true)} data-testid="logo-report-open">
          {t('market.logo.report')}
        </Button>
      )}
      {open ? <ReportDialog covenantId={props.row.covenantId} onClose={() => setOpen(false)} /> : null}
    </Section>
  );
}

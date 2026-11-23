// The logo of a token in its details. It says where the picture comes from, checked or not,
// "Logo hidden after a report" for a hidden one, and offers the "Report logo" action. A report
// hides the logo for everyone until the operator of the indexer reviews it
// (data/logo-blocklist.ts). The dialog also links to the GitHub form, which feeds the same list.
// KOB stores no picture, so reporting is the way a logo is taken down.
//
// Below it, the votes (data/logo-votes.ts): the totals and a thumbs up and down. A vote is a
// message the connected wallet signs, with a few seconds of work in the page meanwhile. A logo
// that 3 or more wallets voted to a net -1 or lower is hidden by default; the section then says
// "Hidden by votes" and offers to show it once. Votes are not reports.
import { useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { castVote, logoVotes, normalizeSignature, powPrefix, solvePow, utcDay, voteMessage, type VoteDir, type VoteResult } from '../../data/logo-votes';
import { githubReportUrl, logoBlocklist, logoBlocklistUrlOf, logoReportsUrlOf, LOGO_REASONS, reportLogo, type LogoReason, type ReportResult } from '../../data/logo-blocklist';
import { t } from '../../i18n';
import { Banner, Button, HelpTip, Modal, Section, Select } from '../kit';
import type { TokenRow } from './token-model';
import { useLogoVotes, useTokenLogo } from './use-token-logo';

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
      // This viewer's own report hides the logo for this viewer at once. The list is read again for
      // everybody else's view of it.
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
            <span class="btn-label">{t('market.logo.report.github')}</span>
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

type VoteStep = 'sign' | 'work' | 'send';

/** The vote totals of a logo and the two buttons. */
function LogoVotes(props: { covenantId: string }) {
  const { config } = useServices();
  const wallet = useWallet();
  const v = useLogoVotes(props.covenantId);
  const [step, setStep] = useState<VoteStep | null>(null);
  const [result, setResult] = useState<VoteResult | { ok: false; reason: 'declined' | 'unsigned' } | null>(null);
  const url = v.url;
  const info = wallet.info;
  const adapter = wallet.adapter;
  const canVote = !!url && !!info && !!adapter?.signMessage && !wallet.networkMismatch;
  const totals = v.totals ?? { up: 0, down: 0 };
  // No votes without an indexer that takes them (standalone mode included): nothing to show.
  if (!url) return null;
  const cast = async (vote: VoteDir) => {
    if (!canVote || !url || !info || !adapter?.signMessage || step) return;
    setResult(null);
    const day = utcDay(Date.now());
    // The work runs while the wallet shows the message, so the two waits overlap.
    const ctl = new AbortController();
    const work = solvePow(powPrefix(config.network, props.covenantId, info.address, day), logoVotes.powBits(), { signal: ctl.signal });
    work.catch(() => undefined);
    setStep('sign');
    let signature: string | null;
    try {
      signature = normalizeSignature(await adapter.signMessage(voteMessage(config.network, props.covenantId, vote, info.address, day)));
    } catch {
      ctl.abort();
      setStep(null);
      setResult({ ok: false, reason: 'declined' });
      return;
    }
    if (!signature) {
      ctl.abort();
      setStep(null);
      setResult({ ok: false, reason: 'unsigned' });
      return;
    }
    setStep('work');
    let nonce: string;
    try {
      nonce = await work;
    } catch {
      setStep(null);
      return;
    }
    setStep('send');
    const r = await castVote(url, { covenant_id: props.covenantId.toLowerCase(), vote, address: info.address, day, nonce, signature });
    setStep(null);
    setResult(r);
    if (r.ok) logoVotes.accepted(props.covenantId, vote, r);
  };
  const message = (): { tone: 'ok' | 'warn' | 'error'; text: string } | null => {
    if (!result) return null;
    if (result.ok) return { tone: 'ok', text: t(`market.logo.vote.${result.result}`) };
    if (result.reason === 'too-soon' || result.reason === 'rate-limited') {
      const hours = Math.max(1, Math.ceil((result.retryAfterSecs ?? 3600) / 3600));
      return { tone: 'warn', text: t(`market.logo.vote.${result.reason}`, { hours }) };
    }
    const key = result.reason === 'network' ? 'network-error' : result.reason;
    return { tone: result.reason === 'declined' ? 'warn' : 'error', text: t(`market.logo.vote.${key}`) };
  };
  const m = message();
  return (
    <div class="logo-votes" data-testid="logo-votes">
      <div class="logo-votes-row">
        <Button small onClick={() => void cast('up')} disabled={!canVote} loading={step !== null} aria-pressed={v.own === 'up' ? 'true' : 'false'} aria-label={t('market.logo.vote.up')} title={t('market.logo.vote.up')} data-testid="logo-vote-up">
          {'👍'} <span data-testid="logo-votes-up">{totals.up}</span>
        </Button>
        <Button small onClick={() => void cast('down')} disabled={!canVote} loading={step !== null} aria-pressed={v.own === 'down' ? 'true' : 'false'} aria-label={t('market.logo.vote.down')} title={t('market.logo.vote.down')} data-testid="logo-vote-down">
          {'👎'} <span data-testid="logo-votes-down">{totals.down}</span>
        </Button>
        {/* why a vote cannot be cast, or its progress; else the "?" with what a vote is */}
        {step || !info || !adapter?.signMessage || wallet.networkMismatch ? (
          <span class="small muted" data-testid="logo-vote-hint">
            {step ? t(`market.logo.vote.step.${step}`) : !info ? t('market.logo.vote.connect') : !adapter?.signMessage ? t('market.logo.vote.noSign') : t('market.logo.vote.network')}
          </span>
        ) : (
          <HelpTip label={t('market.logo.votes.label')} data-testid="logo-vote-help">{t('market.logo.vote.hint')}</HelpTip>
        )}
      </div>
      {m ? (
        <Banner tone={m.tone} data-testid="logo-vote-result">
          {m.text}
        </Banner>
      ) : null}
    </div>
  );
}

/**
 * What a token's details say about its logo. Nothing for a token with no logo to speak of, which
 * has a letter only.
 */
export function LogoSection(props: { row: Pick<TokenRow, 'covenantId' | 'kascov'> }) {
  const { logo } = useTokenLogo(props.row, true);
  const [open, setOpen] = useState(false);
  if (logo.state === 'off' || logo.state === 'none' || logo.state === 'pending') return null;
  const hidden = logo.state === 'hidden';
  const voted = logo.state === 'voted';
  return (
    <Section title={t('market.logo.title')} class="mkt-card" data-testid="token-logo-section">
      <p class="small" data-testid="token-logo-note" data-logo={logo.state}>
        {voted ? (
          <>
            <strong>{t('market.logo.voted')}</strong> <HelpTip label={t('market.logo.voted')} data-testid="token-logo-help">{t('market.logo.votedNote')}</HelpTip>
          </>
        ) : hidden ? (
          <>
            <strong>{t('market.logo.hidden')}</strong> <HelpTip label={t('market.logo.hidden')} data-testid="token-logo-help">{t('market.logo.hiddenNote')}</HelpTip>
          </>
        ) : logo.state === 'creator' ? (
          t('market.logo.creator', { host: logo.host })
        ) : logo.state === 'mismatch' ? (
          t('market.logo.mismatch')
        ) : logo.state === 'verified' ? (
          <>
            {t('market.logo.verified.short')} <HelpTip label={t('market.logo.title')} data-testid="token-logo-help">{t('market.logo.verified')}</HelpTip>
          </>
        ) : logo.state === 'ipfs' ? (
          <>
            {t('market.logo.ipfs.short')} <HelpTip label={t('market.logo.title')} data-testid="token-logo-help">{t('market.logo.ipfs')}</HelpTip>
          </>
        ) : (
          <>
            {t('market.logo.checked.short')} <HelpTip label={t('market.logo.title')} data-testid="token-logo-help">{t('market.logo.checked')}</HelpTip>
          </>
        )}
      </p>
      {voted ? (
        <Button small onClick={() => logoVotes.showOnce(props.row.covenantId)} data-testid="logo-voted-show">
          {t('market.logo.voted.show')}
        </Button>
      ) : null}
      {hidden || voted || logo.state === 'mismatch' ? null : (
        <Button small onClick={() => setOpen(true)} data-testid="logo-report-open">
          {t('market.logo.report')}
        </Button>
      )}
      {hidden || logo.state === 'mismatch' ? null : <LogoVotes covenantId={props.row.covenantId} />}
      {open ? <ReportDialog covenantId={props.row.covenantId} onClose={() => setOpen(false)} /> : null}
    </Section>
  );
}

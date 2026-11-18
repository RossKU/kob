// The panel shown after an accepted issuance: identifiers, the registry entry to submit by pull
// request, and downloads.
import { useEffect, useRef, useState } from 'preact/hooks';
import { routeToHash, tokenRoute } from '../../app/router';
import { t } from '../../i18n';
import type { IssuePlan } from '../../kob/issue';
import type { Hex } from '../../kob/types';
import { Banner, Button, CopyText, copyToClipboard, friendlyErrorText, kasText, KeyValueList, RawDetails, Section } from '../kit';
import { downloadText } from './download';
import { formatRowDate } from '../kit/format';
import { humanAmount, resultViewModel } from './issue-model';

/** What the result panel needs from the accepted issuance. */
export interface ResultPanelProps {
  plan: Pick<IssuePlan, 'token' | 'docs'> & Partial<Pick<IssuePlan, 'curve' | 'sale' | 'lock'>>;
  txid: string;
  walletKey: Hex | null;
  /** The wallet-owned token outputs could not be saved in the token tracker. */
  trackFailed: boolean;
  onAnother(): void;
}

/** Shows the identifiers, the registry entry and the downloads of an accepted issuance. */
export function ResultPanel(props: ResultPanelProps) {
  const { plan, txid } = props;
  const [copied, setCopied] = useState<'idle' | 'ok' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);

  let vm: ReturnType<typeof resultViewModel>;
  try {
    vm = resultViewModel(plan, txid, props.walletKey);
  } catch (e) {
    // The docs did not describe the token, so never hand out a registry entry that could point at
    // another token.
    return (
      <Section title={t('issue.result.title')} data-testid="issue-result">
        <Banner tone="error" title={t('issue.failed.title')} data-testid="issue-result-error">
          <span class="wrap-anywhere">{friendlyErrorText(e instanceof Error ? e.message : String(e))}</span>
          <RawDetails text={e instanceof Error ? e.message : String(e)} />
        </Banner>
        <p class="mono wrap-anywhere">{txid}</p>
      </Section>
    );
  }

  const copyEntry = async () => {
    const ok = await copyToClipboard(vm.registryEntryJson);
    setCopied(ok ? 'ok' : 'failed');
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopied('idle'), 2500);
  };

  return (
    <div class="stack issue-result" data-testid="issue-result">
      <Section title={t('issue.result.title')}>
        <p>{t('issue.result.body', { ticker: plan.token.ticker, txid: `${txid.slice(0, 8)}...${txid.slice(-6)}` })}</p>
        <KeyValueList
          items={vm.rows.map((r) => ({
            label: t(`issue.result.${r.id}`),
            value: r.copy ? <CopyText value={r.value} short={false} data-testid={`issue-result-${r.id}`} /> : <code class="mono" data-testid={`issue-result-${r.id}`}>{r.value}</code>,
          }))}
        />
        <h3 class="issue-h3">{t('issue.result.holders')}</h3>
        <ul class="issue-holder-list" data-testid="issue-result-holders">
          {vm.holders.map((h) => (
            <li key={h.index} class="wrap-anywhere">
              {t('issue.result.holder', { index: h.index, owner: h.isWallet ? t('issue.summary.you') : h.lockedUntil !== undefined ? t('issue.summary.holderLocked', { date: formatRowDate(Number(h.lockedUntil)) }) : h.ownerShort, amount: `${h.human} ${plan.token.ticker}` })}
            </li>
          ))}
        </ul>
        <p class="muted">{t('issue.result.tracked')}</p>
        {props.trackFailed ? <Banner tone="warn" data-testid="issue-result-track-failed">{t('issue.result.trackFailed')}</Banner> : null}
      </Section>

      <Section title={t('issue.result.listingTitle')} data-testid="issue-listing">
        <Banner tone="info" data-testid="issue-result-listing-note">{t('issue.result.listingBody')}</Banner>
        <h3 class="issue-h3">{t('issue.result.registryTitle')}</h3>
        <pre class="issue-json" data-testid="issue-registry-entry" tabIndex={0}>{vm.registryEntryJson}</pre>
        <div class="row">
          <Button variant="primary" onClick={copyEntry} data-testid="issue-registry-copy">{copied === 'ok' ? t('issue.result.copied') : t('issue.result.copyEntry')}</Button>
          <Button onClick={() => downloadText(vm.registryEntryFile, vm.registryEntryJson)} data-testid="issue-registry-download">{t('issue.result.downloadEntry')}</Button>
          <Button onClick={() => downloadText(vm.supplyFile, vm.supplyJson)} data-testid="issue-supply-download">{t('issue.result.supplyDoc')}</Button>
          <Button onClick={() => downloadText(vm.metadataFile, vm.metadataJson)} data-testid="issue-metadata-download">{t('issue.result.metadataDoc')}</Button>
          {vm.proofJson ? <Button onClick={() => downloadText(vm.proofFile, vm.proofJson!)} data-testid="issue-proof-download">{t('issue.result.proofDoc')}</Button> : null}
        </div>
        {copied === 'failed' ? <p class="field-error" role="alert">{t('issue.result.copyFailed')}</p> : null}
      </Section>

      {renderSale()}

      <div class="row">
        <a class="btn btn-primary" href={routeToHash(tokenRoute(plan.token.covenantId))} data-testid="issue-open-market"><span class="btn-label">{t('issue.result.openMarket')}</span></a>
        <Button onClick={props.onAnother} data-testid="issue-another">{t('issue.result.another')}</Button>
      </div>
    </div>
  );

  /**
   * "After creation": the curve is on the book already. A one-price sale is the next step, in the
   * prefilled ticket.
   */
  function renderSale() {
    const s = plan.sale;
    if (!s) return null;
    const words = { sell: humanAmount(s.sell, plan.token.decimals), ticker: plan.token.ticker };
    if (s.mode === 'curve' && s.curve) {
      return (
        <Banner tone="info" data-testid="issue-result-curve">
          {t('issue.result.curve', { ...words, first: kasText(s.curve.numbers.first), last: kasText(s.curve.numbers.last) })}
        </Banner>
      );
    }
    return (
      <Banner
        tone="info"
        data-testid="issue-result-ask"
        actions={
          <a class="btn btn-primary btn-sm" href={routeToHash(tokenRoute(plan.token.covenantId, 'ask', s.sell, s.price))} data-testid="issue-result-ask-place">
            <span class="btn-label">{t('issue.result.askButton')}</span>
          </a>
        }
      >
        {t('issue.result.ask', { ...words, price: kasText(s.price ?? 0n) })}
      </Banner>
    );
  }
}

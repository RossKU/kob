// The panel shown after an accepted issuance: identifiers, the registry entry to submit by pull request, downloads, and the plain
// statement that the token is unverified and not in the registry yet (pending review once its entry is merged).
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import type { IssuePlan } from '../../kob/issue';
import type { Hex } from '../../kob/types';
import { Badge, Banner, Button, CopyText, copyToClipboard, KeyValueList, Section } from '../kit';
import { downloadText } from './download';
import { resultViewModel } from './issue-model';

export interface ResultPanelProps {
  plan: IssuePlan;
  txid: string;
  walletKey: Hex | null;
  /** the wallet-owned token outputs could not be saved in the token tracker */
  trackFailed: boolean;
  onAnother(): void;
}

export function ResultPanel(props: ResultPanelProps) {
  const { plan, txid } = props;
  const [copied, setCopied] = useState<'idle' | 'ok' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);

  let vm: ReturnType<typeof resultViewModel>;
  try {
    vm = resultViewModel(plan, txid, props.walletKey);
  } catch (e) {
    // the docs did not describe the token: never hand out a registry entry that could point at another token
    return (
      <Section title={t('issue.result.title')} data-testid="issue-result">
        <Banner tone="error" title={t('issue.failed.title')} data-testid="issue-result-error">
          <span class="wrap-anywhere">{e instanceof Error ? e.message : String(e)}</span>
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
        <div class="row issue-status-badges">
          <Badge tone="warn" data-testid="issue-result-unverified">{t('issue.result.unverified')}</Badge>
          <Badge tone="info" data-testid="issue-result-pending">{t('issue.result.pending')}</Badge>
          <Badge tone="bad" data-testid="issue-result-nottradable">{t('issue.result.notTradable')}</Badge>
        </div>
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
              {t('issue.result.holder', { index: h.index, owner: h.isWallet ? t('issue.summary.you') : h.ownerShort, amount: `${h.human} ${plan.token.ticker}` })}
            </li>
          ))}
        </ul>
        <p class="muted">{t('issue.result.tracked')}</p>
        {props.trackFailed ? <Banner tone="warn" data-testid="issue-result-track-failed">{t('issue.result.trackFailed')}</Banner> : null}
      </Section>

      <Section title={t('issue.result.listingTitle')} data-testid="issue-listing">
        <Banner tone="warn" data-testid="issue-result-listing-note">{t('issue.result.listingBody')}</Banner>
        <h3 class="issue-h3">{t('issue.result.registryTitle')}</h3>
        <pre class="issue-json" data-testid="issue-registry-entry" tabIndex={0}>{vm.registryEntryJson}</pre>
        <div class="row">
          <Button variant="primary" onClick={copyEntry} data-testid="issue-registry-copy">{copied === 'ok' ? t('issue.result.copied') : t('issue.result.copyEntry')}</Button>
          <Button onClick={() => downloadText(vm.registryEntryFile, vm.registryEntryJson)} data-testid="issue-registry-download">{t('issue.result.downloadEntry')}</Button>
          <Button onClick={() => downloadText(vm.supplyFile, vm.supplyJson)} data-testid="issue-supply-download">{t('issue.result.supplyDoc')}</Button>
          <Button onClick={() => downloadText(vm.metadataFile, vm.metadataJson)} data-testid="issue-metadata-download">{t('issue.result.metadataDoc')}</Button>
        </div>
        {copied === 'failed' ? <p class="field-error" role="alert">{t('issue.result.copyFailed')}</p> : null}
      </Section>

      <div class="row">
        <Button onClick={props.onAnother} data-testid="issue-another">{t('issue.result.another')}</Button>
      </div>
    </div>
  );
}

import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { navigate, tokenRoute } from '../../app/router';
import type { IndexerTokenView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { Banner, Button, CopyText, ErrorBanner, Field, Loading, Section, Table, TableMessage, toError, useAsync } from '../kit';
import { StatusBadges, tokenTitle } from './TokenBadges';
import { LookalikeWarning } from './TokenHeader';
import './market.css';
import { buildTokenRows, filterTokens, parseCovenantId, type TokenRow } from './token-model';

function AddUnknownToken(props: { onFound: (v: IndexerTokenView) => void }) {
  const { indexer, registry } = useServices();
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: 'error' | 'info'; text: string } | null>(null);
  const id = parseCovenantId(text);

  const lookup = async () => {
    if (!id) return;
    if (registry.byCovenantId.has(id)) {
      navigate(tokenRoute(id));
      return;
    }
    if (!indexer) {
      setMessage({ tone: 'error', text: t('market.add.noIndexer') });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const found = (await indexer.tokens()).find((x) => x.covenant_id === id);
      if (!found) setMessage({ tone: 'error', text: t('market.add.notFound') });
      else {
        props.onFound(found);
        setMessage({ tone: 'info', text: t('market.add.found') });
        setText('');
      }
    } catch (e) {
      setMessage({ tone: 'error', text: t('common.errorDetail', { message: toError(e).message }) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title={t('market.add.title')} collapsible defaultOpen={false} data-testid="add-token">
      <p class="small muted">{t('market.add.hint')}</p>
      <div class="row" style="align-items:flex-start">
        <div class="grow">
          <Field
            label={t('market.add.label')}
            value={text}
            onValue={(v) => {
              setText(v);
              setMessage(null);
            }}
            error={text.trim() !== '' && !id ? t('market.add.invalid') : null}
            placeholder="64 hex"
            data-testid="token-add-input"
          />
        </div>
        <div style="padding-top:24px">
          <Button variant="primary" disabled={!id} loading={busy} onClick={() => void lookup()} data-testid="token-add-button">
            {t('market.add.button')}
          </Button>
        </div>
      </div>
      {message ? <Banner tone={message.tone} data-testid="token-add-result">{message.text}</Banner> : null}
    </Section>
  );
}

function TokenTableRow({ row }: { row: TokenRow }) {
  return (
    <tr data-testid={`token-row-${row.covenantId}`} data-source={row.source} data-tradable={row.tradable ? '1' : '0'}>
      <td>
        <a href={`#/market/${row.covenantId}`} class="token-name" data-testid={`token-link-${row.covenantId}`}>
          {tokenTitle(row)}
        </a>
        {row.name ? <div class="small muted">{row.name}</div> : null}
        {row.lookalike && (row.lookalike.level === 'strong' || row.lookalike.level === 'shared' || row.lookalike.level === 'collision') ? <LookalikeWarning row={row} /> : null}
      </td>
      <td class="token-hash-cell">
        <CopyText value={row.covenantId} short={false} class="token-hash" data-testid={`token-hash-${row.covenantId}`} />
      </td>
      <td><StatusBadges row={row} /></td>
      <td class="right num">{row.openAsks ?? '-'}</td>
      <td class="right num">{row.openBids ?? '-'}</td>
    </tr>
  );
}

/** Token list: registry tokens with badges, the full token hash (covenant id) and the open order counts, tokens the indexer knows but the registry does not (UNVERIFIED, never tradable), search, add by covenant id. */
export function TokenList(props: { invalidToken?: string }) {
  const { registry, indexer } = useServices();
  const tokens = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer]);
  const [query, setQuery] = useState('');
  const [pasted, setPasted] = useState<IndexerTokenView[]>([]);

  const rows = useMemo(() => buildTokenRows(registry, tokens.data ?? null, pasted), [registry, tokens.data, pasted]);
  const shown = filterTokens(rows, query);

  return (
    <div class="stack" data-testid="token-list-view">
      {props.invalidToken ? <Banner tone="warn" data-testid="banner-bad-token">{t('market.badTokenId', { id: props.invalidToken })}</Banner> : null}
      <Section
        title={t('market.title')}
        actions={<Button small onClick={tokens.reload} loading={tokens.loading && !!tokens.data}>{t('common.refresh')}</Button>}
        data-testid="token-list"
      >
        <div style="max-width:420px;margin-bottom:12px">
          <Field label={t('market.search.label')} type="search" value={query} onValue={setQuery} placeholder={t('market.search.placeholder')} data-testid="token-search" />
        </div>
        <ErrorBanner error={tokens.error} title={t('market.list.indexerError')} onRetry={tokens.reload} data-testid="token-list-error" />
        <Table dense>
          <thead>
            <tr>
              <th>{t('market.col.token')}</th>
              <th>{t('market.col.hash')}</th>
              <th>{t('market.col.status')}</th>
              <th class="right">{t('market.col.asks')}</th>
              <th class="right">{t('market.col.bids')}</th>
            </tr>
          </thead>
          <tbody>
            {tokens.loading && !tokens.data && indexer && rows.length === 0 ? (
              <TableMessage colSpan={5}><Loading /></TableMessage>
            ) : shown.length === 0 ? (
              <TableMessage colSpan={5} data-testid="token-list-empty">{query ? t('market.list.noMatch') : t('market.list.empty')}</TableMessage>
            ) : (
              shown.map((r) => <TokenTableRow key={r.covenantId} row={r} />)
            )}
          </tbody>
        </Table>
        {!indexer ? <p class="small muted">{t('market.list.noIndexer')}</p> : null}
      </Section>
      <AddUnknownToken onFound={(v) => setPasted((p) => (p.some((x) => x.covenant_id === v.covenant_id) ? p : [...p, v]))} />
    </div>
  );
}

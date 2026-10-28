import { t, tokenAmountText } from '../../i18n';
import { decimalsOrigin } from '../../app/token-display';
import { Badge, Banner } from '../kit';
import { longId, type TokenInfo } from '../../kob/registry';
import { hasIssuerControl, KNOWN_POWERS, tokenLabel, type TokenBadge, type TokenFactId, type TokenRow } from './token-model';

/** What a row's facts are printed from: the facts, and (a token page) KOB's own genesis check, which adds a reason and the public mint to some of them. */
type FactRow = Pick<TokenRow, 'covenantId' | 'ticker' | 'facts'> & Partial<Pick<TokenRow, 'genesis'>>;

/** The badge text of a fact. "Genesis not verified" says why when KOB's check could not be done; the mint allowance names its amount. */
export function factLabel(r: FactRow, id: TokenFactId): string {
  const g = r.genesis;
  if (id === 'genesis_unverified' && g?.status === 'unavailable') return `${t('market.fact.genesis_unverified.label')}: ${t(`market.genesis.reason.${g.reason}`)}`;
  if (id === 'mint_allowance' && g?.status === 'verified' && g.mintAllowance !== null) {
    return t(g.mintersUnspent === true ? 'market.fact.mint_allowance.label' : 'market.fact.mint_allowance.labelUpTo', { amount: tokenAmountText(r.covenantId, g.mintAllowance) });
  }
  return t(`market.fact.${id}.label`);
}

/** The sentence of a fact (a tooltip, or printed behind "what is this?"). */
export function factText(r: FactRow, id: TokenFactId): string {
  const g = r.genesis;
  if (id === 'genesis_failed' && g?.status === 'failed') return t(`market.genesis.failed.${g.code}`);
  if ((id === 'genesis_unverified' || id === 'genesis_kascov') && g?.status === 'unavailable') return `${t(`market.fact.${id}.text`)} ${t(`market.genesis.reasonSentence.${g.reason}`)}`;
  if (id === 'mint_allowance' && g?.status === 'verified' && g.mintAllowance !== null) {
    return t(g.mintersUnspent === true ? 'market.fact.mint_allowance.text' : 'market.fact.mint_allowance.textUpTo', { amount: tokenAmountText(r.covenantId, g.mintAllowance) });
  }
  return t(`market.fact.${id}.text`);
}

/**
 * The badge text of a fact as elements: the reason that "Genesis not verified" gets once KOB's check could not be done is a NEW element after the text that was
 * already there, never an edit of that text node (a page translator that replaced the node would show the translation of the old text: e2e/ui/translated.spec.ts).
 */
function FactLabel(props: { row: FactRow; id: TokenFactId }) {
  const g = props.row.genesis;
  if (props.id === 'genesis_unverified' && g?.status === 'unavailable') return <>{t('market.fact.genesis_unverified.label')}<span>{`: ${t(`market.genesis.reason.${g.reason}`)}`}</span></>;
  return <>{factLabel(props.row, props.id)}</>;
}

/** The sentence of a fact as elements (the reason a check could not be done follows the sentence in an element of its own, as in `FactLabel`). */
function FactSentence(props: { row: FactRow; id: TokenFactId }) {
  const g = props.row.genesis;
  if ((props.id === 'genesis_unverified' || props.id === 'genesis_kascov') && g?.status === 'unavailable') return <>{t(`market.fact.${props.id}.text`)}<span>{` ${t(`market.genesis.reasonSentence.${g.reason}`)}`}</span></>;
  return <>{factText(props.row, props.id)}</>;
}

/** Facts are neutral, except KOB's own genesis result: a pass is green, a failure is a warning. */
const factTone = (id: TokenFactId): 'neutral' | 'ok' | 'warn' => (id === 'genesis_verified' ? 'ok' : id === 'genesis_failed' ? 'warn' : 'neutral');

/** One status badge: `untradable`, with the reason (about the token's program) in its text and tooltip. */
export function StatusBadge(props: { badge: TokenBadge; 'data-testid'?: string }) {
  const b = props.badge;
  const reason = b.reason ? t(`market.reason.${b.reason}`) : undefined;
  return (
    <Badge tone={b.tone} title={reason} data-testid={props['data-testid'] ?? `badge-${b.kind}`}>
      {t(`market.badge.${b.kind}`)}
      {reason ? `: ${reason}` : ''}
    </Badge>
  );
}

export function StatusBadges(props: { row: Pick<TokenRow, 'badges' | 'covenantId'> & { info?: { family: 'kcc20' | 'kron' } | null; powers?: readonly string[] }; /** more lines for the family chip's tooltip (the program, the decimals) */ familyNote?: string | null }) {
  const r = props.row;
  return (
    <span class="row" data-testid={`token-badges-${r.covenantId}`} style="gap:4px">
      {r.info ? <Badge tone="neutral" title={props.familyNote ? `${t('market.badge.familyHint')}\n${props.familyNote}` : t('market.badge.familyHint')} data-testid="badge-family">{t(`market.badge.family.${r.info.family}`)}</Badge> : null}
      {r.badges.map((b) => (
        <StatusBadge key={b.kind} badge={b} />
      ))}
      {hasIssuerControl(r.powers) ? <Badge tone="bad" title={t('market.powers.warning')} data-testid="badge-issuer-control">{t('market.badge.issuerControl')}</Badge> : null}
    </span>
  );
}

/** `TICKER (abcd...1234)`: never the ticker alone (a token outside the registry: `abcd...1234`). */
export function tokenTitle(row: Pick<TokenRow, 'ticker' | 'covenantId'> & { info?: TokenInfo | null }): string {
  return tokenLabel(row);
}

/** Translated name of one program power (an unknown power is shown as it came, already restricted to short lower-case words by the client). */
export const powerName = (p: string): string => ((KNOWN_POWERS as readonly string[]).includes(p) ? t(`market.power.${p}`) : p);

/**
 * Short note for a token whose ticker another listed token uses too: tickers are not unique, the covenant id is the identity. `block` renders a
 * paragraph (token page header), otherwise an inline line. Renders nothing for other tokens. The market list shows no such note: its rows tell tokens
 * apart by the fingerprint and the short covenant id.
 */
export function SharedTickerNote(props: { shared: boolean; block?: boolean; 'data-testid'?: string }) {
  if (!props.shared) return null;
  const testid = props['data-testid'] ?? 'shared-ticker-note';
  return props.block ? (
    <p class="small" style="margin:0" role="note" data-testid={testid}>{t('market.sharedTicker')}</p>
  ) : (
    <div class="small muted" role="note" data-testid={testid}>{t('market.sharedTicker')}</div>
  );
}

/**
 * Notes for a token found through the indexer only (open list): the decimals its amounts and prices use and where they come from (its declaration on
 * chain, its program's default (KaspaRocket convention), the price scale of its orders, the decimals entered for its first order: `decimalsOrigin`), and the covenant id, all that identifies it (shown
 * in full). Renders nothing for a registry token.
 */
export function OpenTokenNote(props: {
  token: (Pick<TokenInfo, 'covenantId' | 'openList'> & Partial<Pick<TokenInfo, 'openScale' | 'decimals' | 'decimalsSource' | 'declarationConflicts' | 'decimalsConvention'>>) | null | undefined;
  'data-testid'?: string;
}) {
  const tk = props.token;
  if (!tk || tk.openList !== true) return null;
  return (
    <Banner tone="info" title={t('market.open.title', { id: longId(tk.covenantId) })} data-testid={props['data-testid'] ?? 'open-token-caution'}>
      <span data-testid="open-token-decimals" data-origin={decimalsOrigin(tk)}>{t(`market.open.decimals.${decimalsOrigin(tk)}`, { decimals: tk.decimals ?? 8, source: tk.decimalsConvention ?? '' })}</span>
      <div class="small wrap-anywhere" data-testid="open-token-full-id">
        {t('market.open.fullId')} <code>{tk.covenantId}</code>
      </div>
    </Banner>
  );
}

/**
 * The facts about a token (token-model.ts `tokenFactIds`): neutral badges that say what the token's creation record and program allow (genesis not
 * verified, a live mint authority, who can mint, who can blacklist), each with a plain sentence. No tone: it is not a rating (the one exception is KOB's own genesis check on a token page: a pass is green, a failure a warning). `block` prints the
 * sentences (pair facts); `inline` the badges alone on one line, the sentence being each badge's tooltip (list rows, the token header); `compact` shows the badges and opens the sentences on a tap (list rows, the ticket), because a tooltip does
 * not exist on a touch screen. Several tokens (a pair) name each token before its facts. Renders nothing without facts.
 */
export function TokenFacts(props: { rows: readonly FactRow[]; compact?: boolean; inline?: boolean; 'data-testid'?: string }) {
  const rows = props.rows.filter((r) => r.facts.length > 0);
  if (rows.length === 0) return null;
  const named = props.rows.length > 1;
  const sentences = (
    <ul class="tf-list">
      {rows.map((r) =>
        r.facts.map((id: TokenFactId) => (
          <li key={`${r.covenantId}-${id}`} data-fact={id} data-testid={`token-fact-text-${id}`}>
            {named ? <strong>{tokenLabel(r)}: </strong> : null}
            <FactSentence row={r} id={id} />
          </li>
        )),
      )}
    </ul>
  );
  const badges = rows.map((r) =>
    r.facts.map((id: TokenFactId) => (
      <Badge key={`${r.covenantId}-${id}`} tone={factTone(id)} data-testid={`token-fact-${id}`}>
        {named ? `${r.ticker || tokenLabel(r)}: ` : ''}
        <FactLabel row={r} id={id} />
      </Badge>
    )),
  );
  const testid = props['data-testid'] ?? 'token-facts';
  // list rows: the badges only, small and on one line; the legend above the list (FactsLegend) says what each one means once
  if (props.inline)
    return (
      <span class="tf-inline" data-testid={testid}>
        {rows.map((r) =>
          r.facts.map((id: TokenFactId) => (
            <Badge key={`${r.covenantId}-${id}`} tone={factTone(id)} title={factText(r, id)} data-testid={`token-fact-${id}`}>
              <FactLabel row={r} id={id} />
            </Badge>
          )),
        )}
      </span>
    );
  return props.compact ? (
    <details class="tf tf-compact" data-testid={testid}>
      <summary>
        <span class="tf-badges">{badges}</span>
        <span class="tf-more small muted">{t('market.fact.more')}</span>
      </summary>
      {sentences}
    </details>
  ) : (
    <div class="tf" role="note" data-testid={testid}>
      <div class="tf-badges">{badges}</div>
      {sentences}
    </div>
  );
}

/** KOB's own genesis check of the token failed: a warning that must be seen on the page, not behind a tap, whatever a third party says. Nothing otherwise. */
export function GenesisFailedBanner(props: { row: Pick<TokenRow, 'genesis'>; 'data-testid'?: string }) {
  const g = props.row.genesis;
  if (g?.status !== 'failed') return null;
  return (
    <Banner tone="warn" title={t('market.genesis.failed.title')} data-testid={props['data-testid'] ?? 'genesis-failed-warning'}>
      {t(`market.genesis.failed.${g.code}`)}
    </Banner>
  );
}

/** The program can freeze or seize balances: escrowed orders can fail or be lost at the issuer's discretion. Renders nothing for other tokens. */
export function PowersWarning(props: { powers: readonly string[] | null | undefined; 'data-testid'?: string }) {
  if (!hasIssuerControl(props.powers)) return null;
  return (
    <Banner tone="warn" title={t('market.powers.title')} data-testid={props['data-testid'] ?? 'powers-warning'}>
      {t('market.powers.warning')}
    </Banner>
  );
}

import { useEffect, useRef, useState } from 'preact/hooks';
import { t, tokenAmountText } from '../../i18n';
import { Badge, Banner } from '../kit';
import { type TokenInfo } from '../../kob/registry';
import { hasIssuerControl, KNOWN_POWERS, riskFactIds, tokenLabel, tokenRiskCodes, type RiskCode, type TokenBadge, type TokenFactId, type TokenRow } from './token-model';

/** What a row's facts are printed from: the facts, and (a token page) KOB's own genesis check, which adds a reason and the public mint to some of them. */
type FactRow = Pick<TokenRow, 'covenantId' | 'ticker' | 'facts'> & Partial<Pick<TokenRow, 'genesis'>>;

/** The badge text of a fact. "Genesis not verified" says why when KOB's check could not be done; the mint allowance names its amount. */
export function factLabel(r: FactRow, id: TokenFactId): string {
  const g = r.genesis;
  if (id === 'genesis_unverified' && g?.status === 'unavailable') return `${t('market.fact.genesis_unverified.label')}: ${t(`market.genesis.reason.${g.reason}`)}`;
  if (id === 'mint_allowance' && g?.status === 'verified' && g.mintUnbounded === true) return t('market.fact.mint_allowance.labelUnbounded');
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
  if (id === 'mint_allowance' && g?.status === 'verified' && g.mintUnbounded === true) return t('market.fact.mint_allowance.textUnbounded');
  if (id === 'mint_allowance' && g?.status === 'verified' && g.mintAllowance !== null) {
    return t(g.mintersUnspent === true ? 'market.fact.mint_allowance.text' : 'market.fact.mint_allowance.textUpTo', { amount: tokenAmountText(r.covenantId, g.mintAllowance) });
  }
  return t(`market.fact.${id}.text`);
}

/**
 * The badge text of a fact as elements: the reason that "Genesis not verified" gets once KOB's check could not be done is a NEW element after the text that was
 * already there, never an edit of that text node (a page translator that replaced the node would show the translation of the old text: e2e/ui/translated.spec.ts).
 */
export function FactLabel(props: { row: FactRow; id: TokenFactId }) {
  const g = props.row.genesis;
  if (props.id === 'genesis_unverified' && g?.status === 'unavailable') return <>{t('market.fact.genesis_unverified.label')}<span>{`: ${t(`market.genesis.reason.${g.reason}`)}`}</span></>;
  return <>{factLabel(props.row, props.id)}</>;
}

/** The sentence of a fact as elements (the reason a check could not be done follows the sentence in an element of its own, as in `FactLabel`). */
export function FactSentence(props: { row: FactRow; id: TokenFactId }) {
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

/** The plain sentence for each reason a token is "High risk" (the facts' own sentences; the check's own words for a failed genesis). */
export function riskReasonText(r: FactRow, code: RiskCode | 'mint_authority_both'): string {
  if (code === 'mint_authority_both') return t('market.risk.mintAuthorityBoth');
  switch (code) {
    case 'genesis_failed': return factText(r, 'genesis_failed');
    case 'live_mint_authority': return factText(r, 'live_mint_authority');
    case 'mint_authority': return factText(r, 'mint-authority');
    case 'mint_unbounded': return factText(r, 'mint_allowance');
    case 'issuer_control': return t('market.powers.warning');
  }
}

/**
 * The reasons shown for a token's risk codes, one line each: a live mint authority and the program's mint authority say nearly the same, so when both apply they are
 * one line (`mint_authority_both`). The chip's panel and the details section both print these, so they never differ.
 */
export function riskReasons(r: FactRow & { powers?: readonly string[] }): { key: RiskCode | 'mint_authority_both'; text: string }[] {
  const codes = tokenRiskCodes({ facts: r.facts, powers: [...(r.powers ?? [])], ...(r.genesis ? { genesis: r.genesis } : {}) });
  const both = codes.includes('live_mint_authority') && codes.includes('mint_authority');
  const out: { key: RiskCode | 'mint_authority_both'; text: string }[] = [];
  for (const c of codes) {
    if (both && c === 'mint_authority') continue;
    const key = both && c === 'live_mint_authority' ? 'mint_authority_both' : c;
    out.push({ key, text: riskReasonText(r, key) });
  }
  return out;
}

/**
 * The one "High risk" chip (warn tone) of a token whose own facts let someone dilute its supply or freeze / seize balances (token-model.ts `tokenRiskCodes`),
 * and its reason: a tap on the chip opens a small panel that lists them (a tooltip does not exist on a touch screen; the panel is fixed to the viewport so
 * a list row, which cuts what overflows it, does not cut it). A tap closes it again, as do Escape, a tap elsewhere and a scroll. Nothing for other tokens.
 */
export function RiskChip(props: { row: Pick<TokenRow, 'covenantId' | 'facts' | 'powers'> & Partial<Pick<TokenRow, 'genesis'>> }) {
  const r = props.row;
  const codes = tokenRiskCodes(r);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!pos) return;
    const close = () => setPos(null);
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && close();
    const onDown = (e: Event) => {
      const el = e.target as Node | null;
      if (el && (button.current?.contains(el) || (el as HTMLElement).closest?.('[data-risk-panel]'))) return;
      close();
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onDown);
    window.addEventListener('scroll', close, true);
    window.addEventListener('resize', close);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onDown);
      window.removeEventListener('scroll', close, true);
      window.removeEventListener('resize', close);
    };
  }, [pos]);
  if (codes.length === 0) return null;
  const toggle = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
    if (pos) return setPos(null);
    const b = button.current!.getBoundingClientRect();
    const width = Math.min(352, window.innerWidth - 24);
    setPos({ left: Math.max(12, Math.min(b.left, window.innerWidth - width - 12)), top: b.bottom + 6 });
  };
  return (
    <span class="risk-chip" data-testid="badge-high-risk" data-reasons={codes.join(' ')}>
      <button type="button" class="badge badge-warn risk-btn" ref={button} onClick={toggle} aria-expanded={pos ? 'true' : 'false'} aria-label={`${t('market.badge.highRisk')}: ${t('market.risk.open')}`} data-testid="high-risk-toggle">
        {t('market.badge.highRisk')}
      </button>
      {pos ? (
        <div class="risk-panel" data-risk-panel role="note" style={{ left: `${pos.left}px`, top: `${pos.top}px` }} data-testid="high-risk-reasons">
          <strong>{t('market.risk.title')}</strong>
          <ul>
            {riskReasons({ ...r, ticker: '' }).map((x) => (
              <li key={x.key} data-reason={x.key}>{x.text}</li>
            ))}
          </ul>
          <p class="muted small">{t('market.risk.note')}</p>
        </div>
      ) : null}
    </span>
  );
}

export function StatusBadges(props: { row: Pick<TokenRow, 'badges' | 'covenantId' | 'facts'> & { info?: { family: 'kcc20' | 'kron' } | null; powers: readonly string[]; genesis?: TokenRow['genesis'] }; /** more lines for the family chip's tooltip (the program, the decimals) */ familyNote?: string | null }) {
  const r = props.row;
  return (
    <span class="row" data-testid={`token-badges-${r.covenantId}`} style="gap:4px">
      {r.info ? <Badge tone="neutral" title={props.familyNote ? `${t('market.badge.familyHint')}
${props.familyNote}` : t('market.badge.familyHint')} data-testid="badge-family">{t(`market.badge.family.${r.info.family}`)}</Badge> : null}
      {r.badges.map((b) => (
        <StatusBadge key={b.kind} badge={b} />
      ))}
      <RiskChip row={{ covenantId: r.covenantId, facts: r.facts, powers: [...r.powers], ...(r.genesis ? { genesis: r.genesis } : {}) }} />
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
 * The facts of a token that "High risk" states (token-model.ts `riskFactIds`), each with its plain sentence: in the order ticket's panel (`compact`: the badges, the
 * sentences open on a tap, because a tooltip does not exist on a touch screen), or printed (`block`). No other fact is shown by default: the rest of a token's facts
 * (a pass, kascov's remark, a capped public mint, blacklist, an unverified genesis) are in the "Status and checks" section of its details. Several tokens (a pair)
 * name each token before its facts. Renders nothing without facts.
 */
export function TokenFacts(props: { rows: readonly FactRow[]; compact?: boolean; 'data-testid'?: string }) {
  // by default a token shows no status: only what "High risk" states is printed here (the order ticket's panel); every other fact is in the token's details
  const rows = props.rows.map((r) => ({ ...r, facts: riskFactIds({ facts: r.facts, powers: [], ...(r.genesis ? { genesis: r.genesis } : {}) }) })).filter((r) => r.facts.length > 0);
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
  return props.compact ? (
    <details class="tf tf-compact" data-testid={testid}>
      <summary title={rows.map((r) => r.facts.map((id: TokenFactId) => factText(r, id)).join(' ')).join(' ')}>
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

// The DISCLOSURE panel of the ticket: what the planned order costs and does, from `buildDisclosureModel` (derived from the planned state and the
// built transaction, never from the form). Thin renderer.
import type { ComponentChildren } from 'preact';
import { formatDateTime, t } from '../../i18n';
import type { BuiltTx } from '../../kob/types';
import { feeDisclosureOf } from '../confirm/fee-disclosure';
import type { DisclosureModel, DisclosureRow, TimeRow, Txt } from './disclosure-model';

const text = (v: Txt | undefined): string => (v === undefined ? '' : typeof v === 'string' ? v : t(v.key, v.params));

/** One row of a disclosure list (label, value, optional detail line): shared by the order ticket of a KAS market and of a token pair. */
export function DiscRow(props: { testid: string; label: string; value: ComponentChildren; detail?: ComponentChildren; tone?: string }) {
  return (
    <div class={`tk-row tk-${props.tone ?? 'normal'}`} data-testid={props.testid}>
      <dt>{props.label}</dt>
      <dd>
        <span class="tk-value">{props.value}</span>
        {props.detail ? <span class="tk-detail">{props.detail}</span> : null}
      </dd>
    </div>
  );
}

function RowView(props: { row: DisclosureRow }) {
  const r = props.row;
  return <DiscRow testid={`disc-${r.id}`} label={t(r.labelKey, r.labelParams)} value={text(r.value)} detail={r.detail ? text(r.detail) : undefined} tone={r.tone} />;
}

const when = (r: { unix: bigint; utc: string; jst: string }): string => `${formatDateTime(r.unix)} (${r.utc}, ${r.jst})`;

function TimeView(props: { row: TimeRow }) {
  const r = props.row;
  return (
    <>
      <div class="tk-row" data-testid={`disc-${r.id}`}>
        <dt>{t(r.labelKey)}</dt>
        <dd>
          <span class="tk-value">{when(r)}</span>
        </dd>
      </div>
      {r.extra ? (
        <div class="tk-row tk-muted" data-testid={`disc-${r.id}-extra`}>
          <dt>{t(r.extra.labelKey)}</dt>
          <dd>
            <span class="tk-value">{when(r.extra)}</span>
          </dd>
        </div>
      ) : null}
    </>
  );
}

export function DisclosurePanel(props: { model: DisclosureModel; built?: BuiltTx | null }) {
  const m = props.model;
  // the fee rate, its bucket and the notes (fallback to the minimum, caps) of the planned transaction
  const feeInfo = props.built ? feeDisclosureOf(props.built) : null;
  return (
    <section class="tk-disclosure" data-testid="order-disclosure" aria-label={t('ticket.disc.title')}>
      <h3>{t('ticket.disc.title')}</h3>
      <p class="tk-summary" data-testid="disc-summary">
        {t('ticket.disc.summary', { side: t(m.side === 'buy' ? 'common.side.buy' : 'common.side.sell'), amount: m.tokenAmount, ticker: m.ticker })}
      </p>
      <dl class="tk-rows">
        <DiscRow testid="disc-minFill" label={t('ticket.disc.minFill')} value={m.minFill} detail={t('ticket.disc.minFillHint')} tone="muted" />
        {m.minTouch !== null ? <DiscRow testid="disc-minTouch" label={t('ticket.disc.minTouch')} value={m.minTouch} tone="muted" /> : null}
        {m.price.map((r) => (
          <RowView row={r} key={r.id} />
        ))}
        {m.extras.map((r) => (
          <RowView row={r} key={r.id} />
        ))}
        {m.pair?.rows.map((r) => (
          <RowView row={r} key={r.id} />
        ))}
        {m.times.map((r) => (
          <TimeView row={r} key={r.id} />
        ))}
      </dl>

      <h4>{t('ticket.disc.lockedTitle')}</h4>
      <p class="tk-note">{t('ticket.disc.lockedText')}</p>
      <dl class="tk-rows" data-testid="disc-carriers">
        {m.carriers.map((c) => (
          <div class={`tk-row${c.kept ? ' tk-muted' : ''}`} key={c.kind} data-testid={`disc-carrier-${c.kind}`}>
            <dt>{t(c.labelKey)}</dt>
            <dd>
              <span class="tk-value">{c.count > 1 ? t('ticket.disc.each', { each: c.each, count: c.count, total: c.total }) : c.total}</span>
              {c.kept ? <span class="tk-detail">{t('ticket.disc.kept')}</span> : null}
            </dd>
          </div>
        ))}
        <div class="tk-row tk-primary" data-testid="disc-kasLocked">
          <dt>{t('ticket.disc.totalLocked')}</dt>
          <dd>
            <span class="tk-value">{m.kasLocked}</span>
          </dd>
        </div>
        {m.tokensEscrowed ? (
          <div class="tk-row" data-testid="disc-tokensEscrowed">
            <dt>{t('ticket.disc.escrowed')}</dt>
            <dd>
              <span class="tk-value">{m.tokensEscrowed}</span>
              <span class="tk-detail">{t('ticket.disc.escrowedText')}</span>
            </dd>
          </div>
        ) : null}
        {m.reserves.map((r) => (
          <RowView row={r} key={r.id} />
        ))}
        {m.fee ? (
          <div class="tk-row" data-testid="disc-fee">
            <dt>{t('ticket.disc.fee')}</dt>
            <dd>
              <span class="tk-value">{m.fee}</span>
              <span class="tk-detail">{t('ticket.disc.feeText')}</span>
            </dd>
          </div>
        ) : null}
        {feeInfo?.rows.map((r) => (
          <div class="tk-row" key={r.id} data-testid={`disc-${r.id}`}>
            <dt>{r.label}</dt>
            <dd>
              <span class="tk-value">{r.value}</span>
              {r.detail ? <span class="tk-detail">{r.detail}</span> : null}
            </dd>
          </div>
        ))}
      </dl>

      {feeInfo && feeInfo.warnings.length + feeInfo.info.length > 0 ? (
        <ul class="tk-notes" data-testid="disc-fee-notes">
          {[...feeInfo.warnings, ...feeInfo.info].map((f) => (
            <li key={f.code} data-note={f.code}>
              {f.text}
            </li>
          ))}
        </ul>
      ) : null}

      {m.notes.length > 0 ? (
        <ul class="tk-notes" data-testid="disc-notes">
          {m.notes.map((n) => (
            <li key={n.tag} data-note={n.tag}>
              {t(n.key, n.params)}
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}

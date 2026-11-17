// "What's this?" of the ticket's Delivery row: the three deliveries of a buy (kob/orders/delivery.ts), as the founder-reviewed mock-up shows
// them (a buy of 30 filled three times by 10), played step by step in the three lanes at once (delivery-help-steps.ts): the empty order, three
// fills, the order ends. A play / pause button and step buttons drive it; it plays when it opens, or opens paused on the final state with
// reduced motion. The drawing is inline SVG built in JSX (no markup strings), animated by CSS classes and keyframes of ticket.css (`.dh-*`), in
// the app's colour tokens (light and dark). Each lane is its own drawing, so on a phone the lanes stack. The choice in force is highlighted.
// A Modal: Escape and the close button close it (the timer stops), focus returns to the link.
import { useEffect, useId, useState } from 'preact/hooks';
import { t } from '../../i18n';
import type { DeliveryChoice } from '../../kob/orders/delivery';
import { Modal } from '../kit';
import { LAST_STEP, boughtAt, captionOf, delayOf, initialSteps, stepBy, tick, toggle, type StepState } from './delivery-help-steps';

/** The lanes in the order of the mock-up. */
const LANES: readonly DeliveryChoice[] = ['each', 'collected', 'topup'];
/** The comparison rows, the default first. */
const ROWS: readonly DeliveryChoice[] = ['each', 'collected', 'topup'];

const X = [20, 92, 164];
const W = 56;
const H = 28;
const ORDER_Y = 26;
const CUST_Y = 74;
const WALLET_Y = 154;
const MID = X[1]!;

const prefersReducedMotion = (): boolean => {
  try {
    return typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  } catch {
    return false;
  }
};

/** A UTXO box with its amount; a new amount pops (`key` restarts the keyframes). */
function Box(props: { x: number; y: number; w?: number; label: string; cls?: string; pop?: boolean }) {
  const w = props.w ?? W;
  return (
    <g>
      <rect class={props.cls ?? 'dh-box'} x={props.x} y={props.y} width={w} height={H} rx={6} />
      <text key={props.label} class={`dh-num${props.pop ? ' dh-pop' : ''}`} x={props.x + w / 2} y={props.y + 19} text-anchor="middle">{props.label}</text>
    </g>
  );
}

/** `dh-show` once the step reaches `from` (fades and draws in by the stylesheet), else hidden. */
const at = (step: number, from: number, cls: string): string => `${cls} ${step >= from ? 'dh-show' : 'dh-hide'}`;

function Lane(props: { mode: DeliveryChoice; on: boolean; step: number }) {
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, '');
  const arrow = `dh-ah-${uid}`;
  const arrowBuy = `dh-ahb-${uid}`;
  const { mode: m, step } = props;
  const bought = boughtAt(step);
  const ended = step >= LAST_STEP;
  const head = (cls: string, id: string) => (
    <marker id={id} viewBox="0 0 10 10" refX={9} refY={5} markerWidth={7} markerHeight={7} orient="auto-start-reverse">
      <path class={cls} d="M0,0 L10,5 L0,10 z" />
    </marker>
  );
  let drawing;
  if (m === 'each') {
    drawing = (
      <>
        {X.map((x, i) => (
          <g key={x} class={at(step, i + 1, 'dh-step')}>
            <line class="dh-flow dh-draw" x1={110 + i * 10} y1={ORDER_Y + H} x2={x + W / 2} y2={WALLET_Y - 2} marker-end={`url(#${arrow})`} />
            <Box x={x} y={WALLET_Y} label="10" />
            <text class="dh-small" x={x + W / 2} y={WALLET_Y + H + 16} text-anchor="middle">{t('ticket.dhelp.fillN', { n: String(i + 1) })}</text>
          </g>
        ))}
      </>
    );
  } else if (m === 'collected') {
    // one custody in the order that counts up in place, then moves down into the wallet as one UTXO when the order ends
    drawing = (
      <>
        <g class={`dh-step dh-cust-g ${step >= 1 ? 'dh-show' : 'dh-hide'}${ended ? ' dh-moved' : ''}`}>
          <Box x={MID} y={CUST_Y} label={String(bought || 10)} cls={ended ? 'dh-box' : 'dh-cust'} pop={!ended} />
        </g>
        <text class={at(step, 1, 'dh-small dh-step') + (ended ? ' dh-gone' : '')} x={MID + W / 2} y={CUST_Y + H + 14} text-anchor="middle">{t('ticket.dhelp.custody')}</text>
        {/* right of the custody's way down, in two short lines: never past the drawing's edges */}
        <text class={at(step, LAST_STEP, 'dh-small dh-step')} x={MID + W + 8} y={CUST_Y + 22}>{t('ticket.dhelp.endsOrClaim')}</text>
        <text class={at(step, LAST_STEP, 'dh-small dh-step')} x={MID + W + 8} y={CUST_Y + 36}>{t('ticket.dhelp.orClaim')}</text>
      </>
    );
  } else {
    // one wallet UTXO that each fill adds to, for a tip
    drawing = (
      <>
        <line key={`a${step}`} class={at(step, 1, 'dh-flow dh-step') + (step >= 1 && !ended ? ' dh-flash' : '')} x1={120} y1={ORDER_Y + H} x2={MID + W / 2} y2={WALLET_Y - 2} marker-end={`url(#${arrow})`} />
        <g class={at(step, 1, 'dh-step')}>
          <Box x={MID} y={WALLET_Y} label={String(bought || 10)} pop={!ended} />
        </g>
        <text key={`t${step}`} class={`dh-small dh-tip dh-step ${step >= 2 && step <= 3 ? 'dh-show dh-pop' : 'dh-hide'}`} x={MID + W + 6} y={WALLET_Y + 19}>{t('ticket.dhelp.tip')}</text>
        <text class={at(step, 1, 'dh-small dh-step')} x={MID + W / 2} y={WALLET_Y + H + 16} text-anchor="middle">{t('ticket.dhelp.oneUtxo')}</text>
      </>
    );
  }
  return (
    <figure class={`dh-lane${props.on ? ' dh-on' : ''}${m === 'each' ? ' dh-default' : ''}`} data-testid={`delivery-help-lane-${m}`} data-on={props.on ? 'true' : 'false'}>
      <figcaption class="dh-lane-title">
        {t(`ticket.short.delivery.${m}`)}
        {m === 'each' ? <span class="dh-pill">{t('ticket.dhelp.default')}</span> : null}
        {props.on ? <span class="dh-pill dh-pill-on">{t('ticket.dhelp.yours')}</span> : null}
      </figcaption>
      <svg class="dh-svg" viewBox="0 0 240 212" role="img" aria-label={t(`ticket.dhelp.aria.${m}`)}>
        <defs>
          {head('dh-head', arrow)}
          {head('dh-head-buy', arrowBuy)}
        </defs>
        <line class="dh-split" x1={4} y1={128} x2={236} y2={128} />
        <Box x={70} y={ORDER_Y} w={100} label={t('ticket.dhelp.order')} />
        {drawing}
        {/* the zone names last, with a halo: the fills' arrows pass under them */}
        <text class="dh-zone" x={4} y={14}>{t('ticket.dhelp.inOrder')}</text>
        <text class="dh-zone" x={4} y={144}>{t('ticket.dhelp.inWallet')}</text>
      </svg>
      <p class={`dh-result ${ended ? 'dh-show' : 'dh-hide'}`} data-testid={`delivery-help-result-${m}`}>{t(`ticket.dhelp.result.${m}`)}</p>
      <p class="dh-cost">{t(`ticket.dhelp.cost.${m}`)}</p>
    </figure>
  );
}

/** The dialog. `selected`: the delivery the ticket has in force (highlighted). */
export function DeliveryHelp(props: { selected: DeliveryChoice; onClose: () => void }) {
  const [s, setS] = useState<StepState>(() => initialSteps(prefersReducedMotion()));
  // one timer per shown step while playing; closing the dialog (unmount) or pausing clears it
  useEffect(() => {
    if (!s.playing) return;
    const h = setTimeout(() => setS(tick), delayOf(s.step));
    return () => clearTimeout(h);
  }, [s.playing, s.step]);
  const cap = captionOf(s.step);
  const cols = ['wait', 'for', 'tradeoff'] as const;
  return (
    <Modal title={t('ticket.dhelp.title')} onClose={props.onClose} large data-testid="delivery-help">
      <p class="dh-intro">{t('ticket.dhelp.intro')}</p>
      <div class="dh-controls">
        <button type="button" class="dh-ctl" onClick={() => setS((x) => stepBy(x, -1))} disabled={s.step <= 0} aria-label={t('ticket.dhelp.back')} data-testid="delivery-help-back">{'⏮'}</button>
        <button type="button" class="dh-ctl dh-ctl-play" onClick={() => setS(toggle)} aria-label={t(s.playing ? 'ticket.dhelp.pause' : 'ticket.dhelp.play')} aria-pressed={s.playing ? 'true' : 'false'} data-autofocus data-testid="delivery-help-play">{'⏯'}</button>
        <button type="button" class="dh-ctl" onClick={() => setS((x) => stepBy(x, 1))} disabled={s.step >= LAST_STEP} aria-label={t('ticket.dhelp.forward')} data-testid="delivery-help-forward">{'⏭'}</button>
        <span class="dh-caption" aria-live="polite" data-testid="delivery-help-caption" data-step={s.step}>{t(cap.key, cap.params ?? {})}</span>
      </div>
      <div class="dh-lanes" data-step={s.step}>
        {LANES.map((m) => <Lane key={m} mode={m} on={m === props.selected} step={s.step} />)}
      </div>
      <table class="dh-table" data-testid="delivery-help-table">
        <thead>
          <tr>
            <th scope="col">{t('ticket.dhelp.col.name')}</th>
            {cols.map((c) => <th key={c} scope="col">{t(`ticket.dhelp.col.${c}`)}</th>)}
          </tr>
        </thead>
        <tbody>
          {ROWS.map((m) => (
            <tr key={m} class={m === props.selected ? 'dh-on' : undefined} data-testid={`delivery-help-row-${m}`}>
              <th scope="row">{t(`ticket.short.delivery.${m}`)}</th>
              {cols.map((c) => <td key={c} data-label={t(`ticket.dhelp.col.${c}`)}>{t(`ticket.dhelp.${c}.${m}`)}</td>)}
            </tr>
          ))}
        </tbody>
      </table>
    </Modal>
  );
}

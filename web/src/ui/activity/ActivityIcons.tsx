// The activity log's type icons. They are KOB's own outline set with a 24 viewBox, stroke 2 and
// round caps and joins, and they are bare on every page. Shape AND colour tell the kinds apart,
// never colour alone.
// - Placed is a square with a plus, white-ish on dark and gray on light.
// - Fill is two opposite arrows, green when the taker bought and red when the taker sold.
// - Cancel is a circle with a cross, muted.
// - Refund, kill and expiry are an undo arrow, amber.
// - Stop armed and stop triggered are a target, amber.
//
// Each icon has an accessible name. The colours are the `--act-*` variables of activity.css, in
// both themes.
import type { ComponentChildren } from 'preact';
import { t } from '../../i18n';
import type { ActivityKind } from './activity-model';

const PATHS: Record<ActivityKind, ComponentChildren> = {
  placed: (
    <>
      <rect x="3.5" y="3.5" width="17" height="17" rx="4" />
      <path d="M12 8v8M8 12h8" />
    </>
  ),
  fill: <path d="M4 9h15l-3-3M20 15H5l3 3" />,
  cancel: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M9 9l6 6M15 9l-6 6" />
    </>
  ),
  refund: <path d="M9 14L4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />,
  stop: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <circle cx="12" cy="12" r="4" />
      <path d="M12 1.5v3M12 19.5v3M1.5 12h3M19.5 12h3" />
    </>
  ),
};

/** The accessible name of a kind's icon. A fill names the taker's side. */
export function kindLabel(kind: ActivityKind, side?: 'buy' | 'sell' | null): string {
  if (kind === 'fill') return t(side === 'sell' ? 'activity.kind.fillSell' : side === 'buy' ? 'activity.kind.fillBuy' : 'activity.kind.fill');
  return t(`activity.kind.${kind}`);
}

/** The bare icon (`currentColor`), e.g. inside a toggle button. */
export function KindGlyph(props: { kind: ActivityKind; size?: number; label?: string | null }) {
  const size = props.size ?? 16;
  const label = props.label === undefined ? kindLabel(props.kind) : props.label;
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      focusable="false"
      {...(label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': 'true' })}
    >
      {PATHS[props.kind]}
    </svg>
  );
}

/** The icon in its colour, for rows and the legend. */
export function KindIcon(props: { kind: ActivityKind; side?: 'buy' | 'sell' | null; small?: boolean }) {
  const tone = props.kind === 'fill' ? (props.side === 'sell' ? 'sell' : 'buy') : props.kind;
  return (
    <span class={`act-ty act-ty-${tone}${props.small ? ' act-ty-sm' : ''}`} data-kind={props.kind}>
      <KindGlyph kind={props.kind} size={props.small ? 13 : 16} label={kindLabel(props.kind, props.side)} />
    </span>
  );
}
